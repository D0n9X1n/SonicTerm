use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine;
use parking_lot::Mutex;
use sonicterm_render_model::InlineImage;
use sonicterm_types::ResourceAmount;
use sonicterm_vt::vt::{MediaEvent, MediaProtocol};

static NEXT_IMAGE_ID: AtomicU64 = AtomicU64::new(1);
const MAX_INLINE_IMAGE_DECODE_SIDE: u32 = 2048;
const MAX_INLINE_IMAGE_DECODE_PIXELS: u64 =
    MAX_INLINE_IMAGE_DECODE_SIDE as u64 * MAX_INLINE_IMAGE_DECODE_SIDE as u64;
const MAX_RETAINED_INLINE_IMAGES: usize = 128;
pub(super) const MAX_RETAINED_INLINE_IMAGE_BYTES: usize = 64 * 1024 * 1024;

pub(super) fn decode_inline_image(event: &MediaEvent) -> Option<InlineImage> {
    let (width, height, bgra) = match event.protocol {
        MediaProtocol::Iterm2File | MediaProtocol::Kitty => decode_base64_image(&event.data)?,
        MediaProtocol::Sixel => decode_sixel(&event.data)?,
    };

    Some(InlineImage {
        id: NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed),
        row: event.row,
        col: event.col,
        width,
        height,
        bgra: Arc::from(bgra),
    })
}

fn decode_base64_image(encoded: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .ok()?;
    let (encoded_width, encoded_height) = reader.into_dimensions().ok()?;
    if !inline_image_decode_dimensions_allowed(encoded_width, encoded_height) {
        tracing::warn!(
            encoded_width,
            encoded_height,
            "inline image rejected before decode: dimensions exceed memory limit"
        );
        return None;
    }
    let mut image = image::load_from_memory(&bytes).ok()?;
    const MAX_INLINE_IMAGE_SIDE: u32 = 1024;
    if image.width() > MAX_INLINE_IMAGE_SIDE || image.height() > MAX_INLINE_IMAGE_SIDE {
        image = image.resize(
            MAX_INLINE_IMAGE_SIDE,
            MAX_INLINE_IMAGE_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    }
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    let mut bgra = Vec::with_capacity(width as usize * height as usize * 4);
    for px in image.pixels() {
        let [r, g, b, a] = px.0;
        let alpha = u16::from(a);
        let premul = |channel: u8| ((u16::from(channel) * alpha + 127) / 255) as u8;
        bgra.push(premul(b));
        bgra.push(premul(g));
        bgra.push(premul(r));
        bgra.push(a);
    }

    Some((width, height, bgra))
}

fn inline_image_decode_dimensions_allowed(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && width <= MAX_INLINE_IMAGE_DECODE_SIDE
        && height <= MAX_INLINE_IMAGE_DECODE_SIDE
        && u64::from(width)
            .checked_mul(u64::from(height))
            .is_some_and(|pixels| pixels <= MAX_INLINE_IMAGE_DECODE_PIXELS)
}

/// Process-wide ceiling on decoded inline media across every pane.
///
/// Panes own their image vectors independently, so a fixed per-pane cap
/// composes without a bound above it: at the original 64 MiB per pane, twenty
/// panes retained 1.2 GiB with every pane individually compliant. That
/// composition, not any single unbounded buffer, is the shape behind the
/// reported multi-gigabyte growth.
///
/// A pane's actual budget is now [`pane_inline_media_budget`] — this ceiling
/// divided by the live pane count — so the per-pane and process bounds are one
/// bound rather than two independent constants that happened to multiply out
/// to exactly this figure.
pub(super) const MAX_PROCESS_INLINE_MEDIA_BYTES: usize = 256 * 1024 * 1024;

/// Live decoded inline-media bytes summed over every pane in this process.
static PROCESS_INLINE_MEDIA_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Number of live pane charges, used to divide the process ceiling fairly.
static LIVE_INLINE_MEDIA_CHARGES: AtomicUsize = AtomicUsize::new(0);

/// Smallest budget a pane is guaranteed regardless of how many panes exist.
///
/// A fair share alone would shrink toward zero as panes multiply, and a pane
/// with a budget below one image renders nothing — the failure this floor
/// exists to prevent. Sized to hold one image at the decode limit
/// (1024×1024×4 bytes), so every pane can always show at least the most recent
/// one.
pub(super) const MIN_PANE_INLINE_MEDIA_BYTES: usize = 4 * 1024 * 1024;

/// Read the live process-wide inline-media total.
#[must_use]
pub(super) fn process_inline_media_bytes() -> usize {
    PROCESS_INLINE_MEDIA_BYTES.load(Ordering::Acquire)
}

/// This pane's share of the process-wide media ceiling.
///
/// The per-pane and process ceilings were originally independent constants,
/// and 256 MiB ÷ 64 MiB is exactly 4 — so four panes at their own cap
/// saturated the process ceiling precisely, and a fifth pane could evict every
/// image it decoded, down to empty, without ever satisfying a condition that
/// depends on bytes it does not own. The pane the user was actively looking at
/// rendered nothing while idle panes they could not see held the entire
/// budget.
///
/// Dividing the ceiling by the live pane count makes the two bounds one bound.
/// N panes at `ceiling / N` sum to the ceiling by construction, so no pane has
/// to evict on another's behalf and the pathological loop cannot arise.
#[must_use]
pub(super) fn pane_inline_media_budget() -> usize {
    let live = LIVE_INLINE_MEDIA_CHARGES.load(Ordering::Acquire).max(1);
    // `clamp` cannot panic: the floor is 4 MiB and the ceiling 64 MiB, both
    // compile-time constants with floor < ceiling.
    (MAX_PROCESS_INLINE_MEDIA_BYTES / live)
        .clamp(MIN_PANE_INLINE_MEDIA_BYTES, MAX_RETAINED_INLINE_IMAGE_BYTES)
}

/// Releases a pane's inline-media charge when the pane's image store drops.
///
/// The charge is returned by `Drop` rather than by the code that removes a
/// pane. Panes are torn down at roughly ten call sites across four modules and
/// move between windows during tab tear-out; a decrement that each of those
/// has to remember is a decrement that will eventually be forgotten, and the
/// counter would ratchet upward until inline media stopped rendering
/// process-wide. Tying it to the allocation's own lifetime means there is no
/// site to forget.
///
/// Co-owned by the VT worker and the pane, because their lifetimes differ. A
/// shell exiting ends the worker while the pane stays on screen with its
/// scrollback and images intact, so a charge held only by the worker would be
/// returned while every pixel it accounted for is still retained — an
/// undercount that lets other panes past the true ceiling.
#[derive(Debug, Default)]
pub(crate) struct InlineMediaCharge {
    bytes: usize,
}

/// Shared handle to a pane's charge, held by both the VT worker and the pane.
pub(crate) type SharedInlineMediaCharge = Arc<Mutex<InlineMediaCharge>>;

/// Create a charge handle for a new pane.
pub(crate) fn new_inline_media_charge() -> SharedInlineMediaCharge {
    LIVE_INLINE_MEDIA_CHARGES.fetch_add(1, Ordering::AcqRel);
    Arc::new(Mutex::new(InlineMediaCharge::default()))
}

impl InlineMediaCharge {
    /// Set this pane's charge to `bytes`, applying the difference to the
    /// process total.
    fn set(&mut self, bytes: usize) {
        if bytes >= self.bytes {
            PROCESS_INLINE_MEDIA_BYTES.fetch_add(bytes - self.bytes, Ordering::AcqRel);
        } else {
            PROCESS_INLINE_MEDIA_BYTES.fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        }
        self.bytes = bytes;
    }
}

impl Drop for InlineMediaCharge {
    fn drop(&mut self) {
        PROCESS_INLINE_MEDIA_BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
        LIVE_INLINE_MEDIA_CHARGES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Trim one pane's retained inline media to its share of the process ceiling.
///
/// Eviction is always from the calling pane. Evicting another pane's images to
/// make room would make one busy pane blank out its neighbours, and two panes
/// under pressure would evict each other every frame.
///
/// The budget is [`pane_inline_media_budget`] — the process ceiling divided by
/// the live pane count — rather than a fixed per-pane constant. A single pass
/// against that budget replaces the loop that used to spin against the
/// *process* total: because the loop body could only shrink the calling pane,
/// a pane could evict itself to empty and still not satisfy a condition owned
/// by other panes' bytes. Dividing the ceiling means a pane's own budget is
/// always achievable by trimming itself.
pub(super) fn trim_inline_images_charged(
    images: &mut Vec<InlineImage>,
    charge: &SharedInlineMediaCharge,
) {
    let budget = if process_inline_media_bytes() > MAX_PROCESS_INLINE_MEDIA_BYTES {
        // Over the ceiling: trim to the floor, not to a fair share.
        //
        // A fair share alone does not converge, because only a pane that is
        // *decoding* re-trims. Panes created earlier keep the larger budget
        // they were admitted under and never revisit it while idle, so the
        // total grows as ceiling x (1 + ln(N/4)) — measured at 616 MiB for 20
        // panes against a 256 MiB ceiling. Trimming to the floor under
        // pressure means every decode while over the ceiling returns memory
        // instead of merely capping the newcomer.
        //
        // The residual is irreducible: principle 1 requires every pane to
        // render at least its newest image, so N panes cost at least N x the
        // floor. That is a bound that can be stated, rather than a curve.
        MIN_PANE_INLINE_MEDIA_BYTES
    } else {
        pane_inline_media_budget()
    };
    let before = retained_inline_media(images);
    trim_inline_images_to(images, budget);
    let after = retained_inline_media(images);

    // Charge exactly what the reporting seam reports, so the figure the
    // governor would see and the figure enforced here cannot drift apart.
    charge.lock().set(after.bytes);

    if after.items < before.items {
        tracing::warn!(
            target: "memory",
            evicted_images = before.items - after.items,
            evicted_bytes = before.bytes.saturating_sub(after.bytes),
            pane_retained_bytes = after.bytes,
            pane_budget_bytes = budget,
            live_panes = LIVE_INLINE_MEDIA_CHARGES.load(Ordering::Acquire),
            process_retained_bytes = process_inline_media_bytes(),
            ceiling = MAX_PROCESS_INLINE_MEDIA_BYTES,
            "inline media evicted to hold the process-wide ceiling"
        );
    }
}

/// Trim a VT worker's staging vector to what its pane could actually keep.
///
/// The worker decodes into a local vector and merges it into the pane's
/// charged store only at the end of the batch, so that vector is **uncharged**
/// while it fills. Trimming it against the fixed per-pane constant let a pane
/// stage far more than it would be allowed to retain: at twenty panes the fair
/// share is ~12 MiB while staging still admitted 64 MiB, so panes decoding
/// concurrently could hold roughly 1.2 GiB that no ceiling ever saw — the
/// multi-gigabyte shape this work exists to remove, reintroduced behind the
/// merge.
///
/// Trimming to the budget the merge will apply means the staging vector never
/// holds bytes the pane is about to discard, so the transient peak stays
/// inside the same bound as the steady state.
pub(super) fn trim_staged_inline_images(images: &mut Vec<InlineImage>) {
    trim_inline_images_to(images, pane_inline_media_budget());
}

/// Drop oldest images until the vector fits `byte_budget` and the count cap.
///
/// Always retains the newest image even when it alone exceeds the budget: a
/// pane that renders nothing is a worse outcome than one that briefly holds a
/// single oversized image, and the decode-side dimension check already bounds
/// how large that one image can be.
fn trim_inline_images_to(images: &mut Vec<InlineImage>, byte_budget: usize) {
    let mut retained_bytes =
        images.iter().fold(0usize, |total, image| total.saturating_add(image.bgra.len()));
    while images.len() > MAX_RETAINED_INLINE_IMAGES
        || (retained_bytes > byte_budget && images.len() > 1)
    {
        let removed = images.remove(0);
        retained_bytes = retained_bytes.saturating_sub(removed.bgra.len());
    }
}

/// Bytes and images this pane retains as decoded inline media.
///
/// This is the same figure [`trim_inline_images_to`] enforces against
/// [`MAX_RETAINED_INLINE_IMAGE_BYTES`], exposed so a governor charges what the
/// pane actually admits rather than a second estimate of it.
///
/// Counts the pixel allocation each [`InlineImage`] owns. `bgra` is an
/// `Arc<[u8]>` shared with the renderer, so this is the *allocation* size, not
/// a per-holder copy: an owner that also counted the renderer's clone of the
/// same image would charge one buffer twice.
#[must_use]
pub(super) fn retained_inline_media(images: &[InlineImage]) -> ResourceAmount {
    ResourceAmount {
        bytes: images.iter().fold(0usize, |total, image| total.saturating_add(image.bgra.len())),
        items: images.len(),
    }
}

fn decode_sixel(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const MAX_SIDE: usize = 1024;
    let mut palette = [[0u8, 0, 0, 255]; 256];
    palette[0] = [0, 0, 0, 255];
    palette[1] = [255, 255, 255, 255];

    let mut color_idx = 1usize;
    let mut pixels = vec![0u8; MAX_SIDE * MAX_SIDE * 4];
    let mut x = 0usize;
    let mut y = 0usize;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut repeat = 1usize;
    let mut i = 0usize;

    while i < data.len() {
        match data[i] {
            b'"' => {
                i += 1;
                skip_sixel_params(data, &mut i);
            }
            b'#' => {
                i += 1;
                let idx = parse_sixel_number(data, &mut i).unwrap_or(0).min(255) as usize;
                color_idx = idx;
                if data.get(i) == Some(&b';') {
                    i += 1;
                    let mode = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if data.get(i) == Some(&b';') {
                        i += 1;
                    }
                    let a = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if data.get(i) == Some(&b';') {
                        i += 1;
                    }
                    let b = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if data.get(i) == Some(&b';') {
                        i += 1;
                    }
                    let c = parse_sixel_number(data, &mut i).unwrap_or(0);
                    if mode == 2 {
                        palette[idx] = [percent_to_u8(a), percent_to_u8(b), percent_to_u8(c), 255];
                    }
                }
            }
            b'!' => {
                i += 1;
                repeat = parse_sixel_number(data, &mut i).unwrap_or(1).max(1) as usize;
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                x = 0;
                y = y.saturating_add(6);
                i += 1;
            }
            byte @ b'?'..=b'~' => {
                let bits = byte - 63;
                for dx in 0..repeat {
                    let px = x + dx;
                    if px >= MAX_SIDE {
                        continue;
                    }
                    for bit in 0..6 {
                        if bits & (1 << bit) == 0 {
                            continue;
                        }
                        let py = y + bit as usize;
                        if py >= MAX_SIDE {
                            continue;
                        }
                        let off = (py * MAX_SIDE + px) * 4;
                        let [r, g, b, a] = palette[color_idx];
                        pixels[off] = b;
                        pixels[off + 1] = g;
                        pixels[off + 2] = r;
                        pixels[off + 3] = a;
                        max_x = max_x.max(px + 1);
                        max_y = max_y.max(py + 1);
                    }
                }
                x = x.saturating_add(repeat);
                repeat = 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    if max_x == 0 || max_y == 0 {
        return None;
    }

    let mut packed = Vec::with_capacity(max_x * max_y * 4);
    for row in 0..max_y {
        let off = row * MAX_SIDE * 4;
        packed.extend_from_slice(&pixels[off..off + max_x * 4]);
    }
    Some((max_x as u32, max_y as u32, packed))
}

fn skip_sixel_params(data: &[u8], i: &mut usize) {
    while *i < data.len() {
        match data[*i] {
            b'0'..=b'9' | b';' => *i += 1,
            _ => break,
        }
    }
}

fn parse_sixel_number(data: &[u8], i: &mut usize) -> Option<u32> {
    let start = *i;
    let mut value = 0u32;
    while *i < data.len() {
        match data[*i] {
            b'0'..=b'9' => {
                value = value.saturating_mul(10).saturating_add(u32::from(data[*i] - b'0'));
                *i += 1;
            }
            _ => break,
        }
    }
    (*i > start).then_some(value)
}

fn percent_to_u8(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;
