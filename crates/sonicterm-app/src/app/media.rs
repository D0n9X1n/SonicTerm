use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine;
use parking_lot::Mutex;
use sonicterm_render_model::InlineImage;
use sonicterm_types::ResourceAmount;
use sonicterm_vt::vt::{MediaEvent, MediaProtocol};

static NEXT_IMAGE_ID: AtomicU64 = AtomicU64::new(1);

/// Largest side a *source* image may declare before any pixels are decoded.
///
/// A preflight rejection bound, not a retention bound. It is read from the
/// encoded header and bounds the work the decoder is willing to start; what
/// survives decode is bounded by [`MAX_INLINE_IMAGE_RENDER_SIDE`] below, which
/// is smaller. The two are separate limits and the distance between them is
/// deliberate: a source larger than the rendered cap is downscaled rather than
/// refused, so an oversized image still renders.
const MAX_INLINE_IMAGE_DECODE_SIDE: u32 = 2048;
const MAX_INLINE_IMAGE_DECODE_PIXELS: u64 =
    MAX_INLINE_IMAGE_DECODE_SIDE as u64 * MAX_INLINE_IMAGE_DECODE_SIDE as u64;

/// Largest side an image can occupy *after* decode, and so the side that
/// bounds every retained buffer.
///
/// Both decoders reduce to this: the base64 path resizes anything larger, and
/// the Sixel path rasterises into a buffer of exactly this side and clips
/// beyond it. Named once and shared by both because it was previously written
/// as two independent literals — one per decoder — which is a bound that can
/// drift on one path while the constant derived from it keeps describing the
/// other.
const MAX_INLINE_IMAGE_RENDER_SIDE: u32 = 1024;
const MAX_RETAINED_INLINE_IMAGES: usize = 128;
pub(super) const MAX_RETAINED_INLINE_IMAGE_BYTES: usize = 64 * 1024 * 1024;

// Ordering: NEXT_IMAGE_ID uses Relaxed; it only hands out distinct InlineImage
// ids and publishes no other data.
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
        // When: inline_image_decode_dimensions_allowed refuses encoded_width by
        // encoded_height, the image is rejected before decode allocates pixels.
        tracing::warn!(
            encoded_width,
            encoded_height,
            "inline image rejected before decode: dimensions exceed memory limit"
        );
        return None;
    }
    let mut image = image::load_from_memory(&bytes).ok()?;
    if image.width() > MAX_INLINE_IMAGE_RENDER_SIDE || image.height() > MAX_INLINE_IMAGE_RENDER_SIDE
    {
        image = image.resize(
            MAX_INLINE_IMAGE_RENDER_SIDE,
            MAX_INLINE_IMAGE_RENDER_SIDE,
            image::imageops::FilterType::Lanczos3,
        );
    }
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        // When: width or height decodes to zero there is no pixel data to
        // retain, so the image is dropped rather than stored as an empty buffer.
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

/// Largest decoded image the retention path can be asked to hold.
///
/// Derived from [`MAX_INLINE_IMAGE_RENDER_SIDE`] — the cap on what survives
/// decode — and not from the preflight cap, which is larger and bounds
/// something else. The preflight gate gets to decide whether decoding starts;
/// it does not decide what is retained, because both decoders reduce to the
/// rendered side below it. Deriving this from the preflight side described a
/// resize that runs before anything reaches retention, and overstated the
/// figure by the square of the ratio between the two sides.
///
/// Measured rather than asserted: driving a 2048x2048 PNG through the Kitty
/// and iTerm2 paths, and a Sixel payload addressing 4096x4096, each retains
/// exactly this many bytes.
pub(super) const MAX_SINGLE_INLINE_IMAGE_BYTES: usize =
    MAX_INLINE_IMAGE_RENDER_SIDE as usize * MAX_INLINE_IMAGE_RENDER_SIDE as usize * 4;

/// Smallest budget a pane is guaranteed regardless of how many panes exist.
///
/// A fair share alone would shrink toward zero as panes multiply, and a pane
/// with a budget below one image renders nothing — the failure this floor
/// exists to prevent.
///
/// Sized to hold one whole image at [`MAX_SINGLE_INLINE_IMAGE_BYTES`], which
/// is the largest any decoder produces. The two figures are therefore equal,
/// and the assertion below keeps them that way: a pane's guaranteed budget
/// must cover the largest single image, or the guarantee does not hold for the
/// case it exists to cover.
pub(super) const MIN_PANE_INLINE_MEDIA_BYTES: usize = 4 * 1024 * 1024;

/// The floor must hold the largest decodable image whole.
///
/// If the rendered side grows without this floor growing with it, a pane's
/// guaranteed budget would no longer fit one image, and the residual bound
/// below — which is stated as the floor — would understate what a pane can
/// legitimately retain. Failing at compile time is the point: the two are
/// derived from different places and nothing else ties them together.
const _: () = assert!(
    MIN_PANE_INLINE_MEDIA_BYTES >= MAX_SINGLE_INLINE_IMAGE_BYTES,
    "the per-pane floor must hold one whole image at the rendered cap"
);

/// Worst-case bytes one pane retains under process pressure.
///
/// Trimming reduces a pane to its most recent image and no further, so a pane
/// can sit above its budget by at most one image. Since the floor is sized to
/// hold the largest such image whole, the floor *is* that worst case — there
/// is no second, larger term to take a maximum against.
#[must_use]
pub(super) const fn max_pane_residual_bytes() -> usize {
    MIN_PANE_INLINE_MEDIA_BYTES
}

/// Read the live process-wide inline-media total.
// Ordering: PROCESS_INLINE_MEDIA_BYTES is read with Acquire so the total seen
// here reflects the AcqRel updates that published it.
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
// Ordering: LIVE_INLINE_MEDIA_CHARGES is read with Acquire, pairing with the
// AcqRel updates that add and remove pane charges.
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
// Ordering: LIVE_INLINE_MEDIA_CHARGES uses AcqRel so this increment is ordered
// against the Drop decrement that returns the same pane's slot.
pub(crate) fn new_inline_media_charge() -> SharedInlineMediaCharge {
    LIVE_INLINE_MEDIA_CHARGES.fetch_add(1, Ordering::AcqRel);
    Arc::new(Mutex::new(InlineMediaCharge::default()))
}

impl InlineMediaCharge {
    /// Set this pane's charge to `bytes`, applying the difference to the
    /// process total.
    // Ordering: PROCESS_INLINE_MEDIA_BYTES uses AcqRel so each pane's delta is
    // ordered against every other pane's and the running total stays exact.
    fn set(&mut self, bytes: usize) {
        if bytes >= self.bytes {
            PROCESS_INLINE_MEDIA_BYTES.fetch_add(bytes - self.bytes, Ordering::AcqRel);
        } else {
            // When: bytes falls below the previous charge the difference is
            // subtracted instead, keeping the total equal to the live sum.
            PROCESS_INLINE_MEDIA_BYTES.fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        }
        self.bytes = bytes;
    }
}

// Lifecycle: dropping InlineMediaCharge returns this pane's bytes to
// PROCESS_INLINE_MEDIA_BYTES and releases its LIVE_INLINE_MEDIA_CHARGES slot.
impl Drop for InlineMediaCharge {
    // Ordering: PROCESS_INLINE_MEDIA_BYTES and LIVE_INLINE_MEDIA_CHARGES both
    // use AcqRel so the bytes and the slot are returned as one ordered pair.
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
/// Trim one pane's retained inline media to its share of the process ceiling,
/// returning the evicted images for the caller to drop.
///
/// The return value is the point: freeing the evicted pixel buffers costs
/// ~2.6 ms for 64 images, measured, and this runs with the pane's image store
/// locked while the render path waits on that same lock. The caller drops them
/// after releasing it.
#[must_use = "the evicted images must be dropped after releasing the image \
              lock, or a multi-millisecond allocator pause lands on the frame"]
pub(super) fn trim_inline_images_charged(
    images: &mut Vec<InlineImage>,
    charge: &SharedInlineMediaCharge,
) -> Vec<InlineImage> {
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
        // When: process_inline_media_bytes sits under
        // MAX_PROCESS_INLINE_MEDIA_BYTES each pane trims to its fair share.
        pane_inline_media_budget()
    };
    let before = retained_inline_media(images);
    let evicted = take_trimmed_inline_images(images, budget);
    let after = retained_inline_media(images);

    // A pane is never trimmed below its most recent image, so a pane holding
    // one image at the rendered cap legitimately sits above `budget`. Anything
    // beyond that is not the residual — it is a trim that failed to run.
    debug_assert!(
        after.bytes <= budget.max(max_pane_residual_bytes()),
        "a trimmed pane retained {} bytes, above both its {budget}-byte budget and the \
         {}-byte single-image residual",
        after.bytes,
        max_pane_residual_bytes()
    );

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
    evicted
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

/// Drop oldest images until the vector fits `byte_budget` and the count cap,
/// **returning** the evicted images rather than freeing them.
///
/// Returning them looks like a needless allocation until you measure what
/// freeing them costs. Each `InlineImage` holds an `Arc<[u8]>` over up to
/// 4 MiB of pixels, and releasing the last reference calls the allocator.
/// Evicting 64 of them takes **2.6 ms**, against **1.9 µs** for the same
/// eviction when the buffers stay alive — a factor of ~1372. The `Vec` shuffle
/// is not the cost; the deallocation is.
///
/// That matters because the pane's image store is locked while this runs and
/// the render path takes the same lock. Freeing inside the critical section
/// puts a multi-millisecond allocator pause directly in a 16 ms frame budget.
/// Handing the images back lets the caller drop them after releasing the lock.
///
/// Always retains the newest image even when it alone exceeds the budget: a
/// pane that renders nothing is a worse outcome than one that briefly holds a
/// single oversized image, and [`MAX_SINGLE_INLINE_IMAGE_BYTES`] already bounds
/// how large that one image can be.
#[must_use = "the evicted images must be dropped outside the lock, or the \
              allocator pause this exists to avoid happens anyway"]
fn take_trimmed_inline_images(
    images: &mut Vec<InlineImage>,
    byte_budget: usize,
) -> Vec<InlineImage> {
    let mut retained_bytes =
        images.iter().fold(0usize, |total, image| total.saturating_add(image.bgra.len()));
    let mut evict = 0usize;
    while images.len() - evict > MAX_RETAINED_INLINE_IMAGES
        || (retained_bytes > byte_budget && images.len() - evict > 1)
    {
        retained_bytes = retained_bytes.saturating_sub(images[evict].bgra.len());
        evict += 1;
    }
    // One `drain` rather than repeated `remove(0)`: the shuffle is cheap
    // either way, but this keeps the evicted images together so they can be
    // carried out of the critical section in one move.
    images.drain(0..evict).collect()
}

fn trim_inline_images_to(images: &mut Vec<InlineImage>, byte_budget: usize) {
    drop(take_trimmed_inline_images(images, byte_budget));
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
    // The same rendered-side cap the base64 path resizes to: this buffer is
    // what a Sixel payload rasterises into, and anything addressing beyond it
    // is clipped below.
    const MAX_SIDE: usize = MAX_INLINE_IMAGE_RENDER_SIDE as usize;
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
                // Clamped to the raster width. Every iteration past `MAX_SIDE`
                // is discarded by the `px >= MAX_SIDE` test below, and once `x`
                // has advanced that far no later byte writes anything either —
                // so the clamp changes how long the decode takes, not what it
                // produces. Without it a twelve-byte payload (`!4294967295m`)
                // buys about 4.29 billion no-op iterations, which any process
                // that can write to the terminal could trigger.
                repeat =
                    (parse_sixel_number(data, &mut i).unwrap_or(1).max(1) as usize).min(MAX_SIDE);
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
                // When: byte falls in the sixel data range its low six bits
                // paint one column strip of six vertical pixels.
                let bits = byte - 63;
                for dx in 0..repeat {
                    let px = x + dx;
                    if px >= MAX_SIDE {
                        // When: px has reached MAX_SIDE the column lies outside
                        // the raster, so the rest of the repeat run writes nothing.
                        continue;
                    }
                    for bit in 0..6 {
                        if bits & (1 << bit) == 0 {
                            // When: this bit is clear in bits the pixel stays
                            // background, so no colour is written for that row.
                            continue;
                        }
                        let py = y + bit as usize;
                        if py >= MAX_SIDE {
                            // When: py has reached MAX_SIDE the row lies below
                            // the raster, so the remaining bits are discarded.
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
        // When: max_x or max_y stayed at zero no sixel byte painted anything,
        // so there is no image to hand back.
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
            _ => {
                // When: data holds a byte outside digits and ';' the parameter
                // list has ended, so scanning stops with i on that byte.
                break;
            }
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
            _ => {
                // When: data holds a byte outside the digit range the number is
                // complete, so i stops there and the digits so far form value.
                break;
            }
        }
    }
    (*i > start).then_some(value)
}

fn percent_to_u8(v: u32) -> u8 {
    ((v.min(100) * 255 + 50) / 100) as u8
}

/// Serialises every test that asserts on the process-wide media counters.
///
/// [`PROCESS_INLINE_MEDIA_BYTES`] and [`LIVE_INLINE_MEDIA_CHARGES`] are
/// process-global by design — that is the property under test — so two tests
/// charging them concurrently make each other's absolute assertions
/// meaningless. Measured at roughly one failure in twelve runs before this
/// guard: the ceiling test would see a sibling's 8 MiB and report the ceiling
/// breached when its own panes were within it.
///
/// Lives beside the counters rather than in one test file because the panes
/// that charge them are driven from two: the media tests exercise the trim
/// directly, and the retention tests exercise the pass that walks every pane.
/// A second, independent lock would serialise each file against itself and
/// neither against the other.
///
/// A lock rather than `--test-threads=1`, because a suite that only works
/// under a flag is a suite that will eventually run without it.
#[cfg(test)]
pub(super) static MEDIA_COUNTER_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;
