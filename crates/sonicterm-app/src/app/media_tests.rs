use super::*;

fn image(id: u64, bytes: usize) -> InlineImage {
    InlineImage { id, row: 0, col: 0, width: 1, height: 1, bgra: Arc::from(vec![0; bytes]) }
}

#[test]
fn decode_dimensions_reject_image_bombs() {
    assert!(inline_image_decode_dimensions_allowed(2048, 2048));
    assert!(!inline_image_decode_dimensions_allowed(2049, 1));
    assert!(!inline_image_decode_dimensions_allowed(1, 2049));
    assert!(!inline_image_decode_dimensions_allowed(u32::MAX, u32::MAX));
}

#[test]
fn retained_inline_images_respect_count_and_byte_budgets() {
    let chunk = MAX_RETAINED_INLINE_IMAGE_BYTES / 2;
    let mut images = vec![image(1, chunk), image(2, chunk), image(3, chunk)];

    trim_inline_images_to(&mut images, MAX_RETAINED_INLINE_IMAGE_BYTES);

    assert_eq!(images.iter().map(|image| image.id).collect::<Vec<_>>(), vec![2, 3]);
    assert!(
        images.iter().map(|image| image.bgra.len()).sum::<usize>()
            <= MAX_RETAINED_INLINE_IMAGE_BYTES
    );
}

/// One pane's inline-media retention is bounded; the sum across panes is not.
///
/// `trim_inline_images_to` enforces [`MAX_RETAINED_INLINE_IMAGE_BYTES`] against a
/// single pane's vector, and each pane owns its own. Nothing composes them, so
/// a session's total is `panes × 64 MiB` with every pane individually
/// compliant. That is the shape behind the reported multi-gigabyte growth:
/// not memory that fails to release, but bounded parts composing without a
/// bound.
///
/// This pins both halves — the per-pane cap holds, and the aggregate is the
/// product — so that wiring a governor above the pane can be verified to
/// change the second number without breaking the first.
#[test]
fn per_pane_media_is_capped_but_the_aggregate_is_not() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    let mut pane: Vec<InlineImage> = Vec::new();
    let mut id = 0u64;
    while retained_inline_media(&pane).bytes < MAX_RETAINED_INLINE_IMAGE_BYTES && id <= 200 {
        id += 1;
        pane.push(image(id, 1024 * 1024));
        trim_inline_images_to(&mut pane, MAX_RETAINED_INLINE_IMAGE_BYTES);
    }

    let per_pane = retained_inline_media(&pane);
    assert!(
        per_pane.bytes <= MAX_RETAINED_INLINE_IMAGE_BYTES,
        "one pane must stay within its own ceiling: {} > {MAX_RETAINED_INLINE_IMAGE_BYTES}",
        per_pane.bytes
    );
    assert!(
        per_pane.bytes >= MAX_RETAINED_INLINE_IMAGE_BYTES / 2,
        "the pane must actually reach a meaningful fraction of its ceiling, got {}",
        per_pane.bytes
    );
    assert_eq!(
        per_pane.items,
        pane.len(),
        "the reported item count must equal the images actually retained"
    );

    // Twenty panes is an ordinary heavy session, not a stress case.
    const PANES: usize = 20;
    let aggregate = per_pane.bytes.saturating_mul(PANES);
    assert!(
        aggregate > 1024 * 1024 * 1024,
        "the point of this test: {PANES} compliant panes exceed a gigabyte together \
         ({aggregate} bytes) with no bound above them"
    );
}

/// The process-wide inline-media ceiling holds across many panes, and the
/// charge is released when a pane's store drops.
///
/// [`MAX_RETAINED_INLINE_IMAGE_BYTES`] bounds one pane at 64 MiB, so ten panes
/// can retain 640 MiB with every pane compliant. That composition is what
/// produced multi-gigabyte growth in the field. This drives ten panes past
/// their individual ceilings and asserts the process total never crosses
/// [`MAX_PROCESS_INLINE_MEDIA_BYTES`] at any point — not merely at the end,
/// since a peak that is trimmed afterwards has already been allocated.
#[test]
fn the_process_wide_media_ceiling_holds_across_panes() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    const PANES: usize = 10;
    const IMAGE_BYTES: usize = 4 * 1024 * 1024;
    const PUSHES_PER_PANE: usize = 24; // 96 MiB offered per pane, above its own cap

    let baseline = process_inline_media_bytes();
    let mut panes: Vec<(Vec<InlineImage>, SharedInlineMediaCharge)> =
        (0..PANES).map(|_| (Vec::new(), new_inline_media_charge())).collect();

    let mut id = 0u64;
    let mut peak = baseline;
    for round in 0..PUSHES_PER_PANE {
        for (images, charge) in &mut panes {
            id += 1;
            images.push(image(id, IMAGE_BYTES));
            drop(trim_inline_images_charged(images, charge));
            peak = peak.max(process_inline_media_bytes());
            assert!(
                process_inline_media_bytes() <= MAX_PROCESS_INLINE_MEDIA_BYTES,
                "process total {} exceeded the ceiling {MAX_PROCESS_INLINE_MEDIA_BYTES} \
                 in round {round}",
                process_inline_media_bytes()
            );
        }
    }

    // Without the aggregate check the panes would together hold far more than
    // the ceiling, so a peak at or below it is the property under test.
    assert!(
        peak <= MAX_PROCESS_INLINE_MEDIA_BYTES,
        "peak {peak} exceeded the ceiling {MAX_PROCESS_INLINE_MEDIA_BYTES}"
    );
    let offered = PANES * PUSHES_PER_PANE * IMAGE_BYTES;
    assert!(
        offered > MAX_PROCESS_INLINE_MEDIA_BYTES * 3,
        "the test must offer substantially more than the ceiling to be meaningful: \
         offered {offered}"
    );

    // Dropping every pane's store must return the charge exactly.
    drop(panes);
    assert_eq!(
        process_inline_media_bytes(),
        baseline,
        "dropping every pane's images must return the process total to its baseline"
    );
}

/// A pane's charge survives its VT worker and is released with the pane.
///
/// The worker ends when its shell exits — an ordinary event, not teardown —
/// while the pane stays on screen with its scrollback and images. A charge
/// owned only by the worker would be returned at that moment, undercounting
/// pixels that are still retained and letting other panes past the true
/// ceiling. Co-ownership means the last holder returns it.
#[test]
fn a_charge_outlives_its_worker_and_is_released_with_the_pane() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    let baseline = process_inline_media_bytes();

    // The pane and its worker both hold the charge.
    let pane_charge = new_inline_media_charge();
    let worker_charge = pane_charge.clone();

    let mut images = vec![image(1, 8 * 1024 * 1024)];
    drop(trim_inline_images_charged(&mut images, &worker_charge));
    let this_pane = retained_inline_media(&images).bytes;
    assert!(
        process_inline_media_bytes() >= baseline + this_pane,
        "retaining an image must charge the process total"
    );

    // Compare deltas, not absolutes: PROCESS_INLINE_MEDIA_BYTES is global and
    // sibling tests charge it concurrently, so a snapshot taken earlier is
    // already stale. What must hold is that *this* pane's contribution does
    // not move when its worker ends.
    let before_worker_exit = process_inline_media_bytes();
    drop(worker_charge);
    assert_eq!(
        process_inline_media_bytes(),
        before_worker_exit,
        "a worker ending must not release a charge for pixels the pane still holds"
    );

    // The pane is finally closed: exactly this pane's bytes come back.
    let before_pane_close = process_inline_media_bytes();
    drop(images);
    drop(pane_charge);
    assert_eq!(
        process_inline_media_bytes(),
        before_pane_close - this_pane,
        "closing the pane must return exactly what it retained"
    );
}

/// A new pane still renders images when other panes hold the ceiling.
///
/// The eviction loop's exit condition is a *process-wide* total, but its body
/// can only shrink the *calling* pane. Four panes at the 64 MiB per-pane cap
/// reach the 256 MiB process ceiling exactly, so a fifth pane evicts every
/// image it decodes — down to empty — and still cannot satisfy the condition.
///
/// The consequence is the worst kind: the pane the user is actively looking at
/// renders nothing, permanently, while idle panes they cannot see keep the
/// whole budget. Principle 1 says the active pane must get its share.
#[test]
fn a_new_pane_renders_images_even_when_others_hold_the_ceiling() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    const IMAGE_BYTES: usize = 4 * 1024 * 1024;

    // Four panes fill the process ceiling exactly: 4 x 64 MiB = 256 MiB.
    let mut holders: Vec<(Vec<InlineImage>, SharedInlineMediaCharge)> = Vec::new();
    let mut id = 0u64;
    for _ in 0..4 {
        let mut images = Vec::new();
        let charge = new_inline_media_charge();
        for _ in 0..20 {
            id += 1;
            images.push(image(id, IMAGE_BYTES));
            drop(trim_inline_images_charged(&mut images, &charge));
        }
        holders.push((images, charge));
    }

    // A fifth pane decodes one image. It must be able to show it.
    let mut newcomer = Vec::new();
    let newcomer_charge = new_inline_media_charge();
    id += 1;
    newcomer.push(image(id, IMAGE_BYTES));
    drop(trim_inline_images_charged(&mut newcomer, &newcomer_charge));

    assert!(
        !newcomer.is_empty(),
        "a new pane must retain at least one image; it evicted everything while \
         {} idle panes held {} bytes",
        holders.len(),
        process_inline_media_bytes()
    );

    // And it must keep working, not blank on every subsequent image.
    for _ in 0..5 {
        id += 1;
        newcomer.push(image(id, IMAGE_BYTES));
        drop(trim_inline_images_charged(&mut newcomer, &newcomer_charge));
        assert!(
            !newcomer.is_empty(),
            "the new pane must keep rendering images, not blank on every decode"
        );
    }
}

/// Every pane gets a share; none is starved to nothing.
#[test]
fn many_panes_each_keep_a_share_of_the_media_budget() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    const PANES: usize = 12;
    const IMAGE_BYTES: usize = 4 * 1024 * 1024;

    let mut panes: Vec<(Vec<InlineImage>, SharedInlineMediaCharge)> =
        (0..PANES).map(|_| (Vec::new(), new_inline_media_charge())).collect();

    let mut id = 0u64;
    for _ in 0..24 {
        for (images, charge) in &mut panes {
            id += 1;
            images.push(image(id, IMAGE_BYTES));
            drop(trim_inline_images_charged(images, charge));
        }
    }

    for (index, (images, _)) in panes.iter().enumerate() {
        assert!(
            !images.is_empty(),
            "pane {index} of {PANES} was starved to nothing; every pane must keep a share"
        );
    }
}

/// The process total stays within a stateable bound when panes are created
/// one at a time and then left idle.
///
/// This is the sequence a real session follows, and it is not what the uniform
/// tests above exercise — they drive every pane on every round, so every pane
/// re-trims against the current budget. In reality a pane fills up, the user
/// moves on, and it never decodes again.
///
/// A fair share alone does not converge under that sequence: only a decoding
/// pane re-trims, so panes admitted earlier keep the larger budget they were
/// admitted under. Measured at **616 MiB for 20 panes** against a 256 MiB
/// ceiling — growth of `ceiling x (1 + ln(N/4))`, which is not a bound.
///
/// Trimming to the floor while over the ceiling makes every decode return
/// memory rather than merely capping the newcomer. The residual is
/// irreducible: principle 1 requires every pane to render at least its newest
/// image, so N panes cost at least N × that image.
///
/// Driven at [`MAX_SINGLE_INLINE_IMAGE_BYTES`] — the largest image any decoder
/// can emit — which is also the floor, because the floor is sized to hold one
/// such image whole. Nothing larger can reach a pane, so nothing larger tells
/// this test anything: an image above the floor is a shape the decoders cannot
/// produce, and a bound widened to admit it is a bound stated against an
/// impossible input.
///
/// # What this bound is worth
///
/// `ceiling + panes × residual` scales with the pane count it is meant to
/// bound, so it admits more memory the more panes exist and grows weaker as N
/// rises. It is kept because the pre-reclamation peak is a real quantity and
/// this is the sequence that produces it, but the assertion that discriminates
/// per pane belongs with the pass that revisits idle panes — this test
/// deliberately never runs that pass, so the panes it leaves behind are still
/// holding budgets from when the session was smaller.
#[test]
fn the_process_total_stays_within_a_stateable_bound_as_panes_accumulate() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    // The largest image any decoder can emit. Anything larger is not a
    // stronger test — it is an input no protocol can deliver.
    const IMAGE_BYTES: usize = MAX_SINGLE_INLINE_IMAGE_BYTES;
    const PANES: usize = 20;

    let baseline = process_inline_media_bytes();
    let mut panes: Vec<(Vec<InlineImage>, SharedInlineMediaCharge)> = Vec::new();
    let mut id = 0u64;
    let mut peak = 0usize;

    for _ in 0..PANES {
        let mut images = Vec::new();
        let charge = new_inline_media_charge();
        // Fill to budget, then never decode again — the idle case.
        for _ in 0..20 {
            id += 1;
            images.push(image(id, IMAGE_BYTES));
            drop(trim_inline_images_charged(&mut images, &charge));
            assert!(
                !images.is_empty(),
                "no pane may be starved to nothing, even under process pressure"
            );
        }
        panes.push((images, charge));
        peak = peak.max(process_inline_media_bytes() - baseline);
    }

    // What this sequence does *not* establish, stated so the bound below is
    // not read as more than it is.
    //
    // A pane only re-trims when it decodes. Panes admitted before the ceiling
    // was reached keep the generous budget they were admitted under, and this
    // test never runs the pass that revisits them — so measured here, the
    // earliest panes sit far above the per-pane residual: 64 MiB against 4 MiB.
    // That is the stale-budget case `trim_panes_over_media_ceiling` exists to
    // relieve, and it is asserted where that pass is driven, not here.
    //
    // The consequence for this test is that the aggregate below is the only
    // claim it can make. It is a real bound on the pre-reclamation peak, and it
    // is weak: the `PANES ×` term grows with the pane count it is meant to
    // bound, so it admits more memory the more panes exist. The assertion that
    // discriminates on a per-pane basis lives with the reclamation pass.
    let worst_pane = panes
        .iter()
        .map(|(images, _)| retained_inline_media(images).bytes)
        .max()
        .expect("at least one pane exists");
    assert!(
        worst_pane > max_pane_residual_bytes(),
        "precondition failed: no pane exceeded the {}-byte residual, so this run did not \
         reproduce the idle-pane case and the aggregate below is being measured against a \
         sequence that never built up a stale budget",
        max_pane_residual_bytes()
    );
    for (index, (images, _)) in panes.iter().enumerate() {
        assert!(
            retained_inline_media(images).bytes > 0,
            "pane {index} of {PANES} was trimmed to nothing; every pane must keep its newest \
             image even under process pressure"
        );
    }

    // Ceiling, plus the largest single image each pane may still be holding.
    let bound = MAX_PROCESS_INLINE_MEDIA_BYTES + PANES * max_pane_residual_bytes();
    assert!(
        peak <= bound,
        "peak {peak} ({} MiB) exceeded the stateable bound {bound} ({} MiB) \
         for {PANES} panes",
        peak / 1024 / 1024,
        bound / 1024 / 1024
    );

    // Guard against the assertion being trivially true. Each pane decoded 20
    // images; if the bound held only because it scaled with decode count it
    // would be meaningless. The peak must stay under what those decodes would
    // cost unbounded, which is what makes trimming observable at all.
    let unbounded = PANES * 20 * IMAGE_BYTES;
    assert!(
        peak < unbounded / 4,
        "trimming must be doing real work: peak {} MiB against an untrimmed {} MiB",
        peak / 1024 / 1024,
        unbounded / 1024 / 1024
    );

    drop(panes);
    assert_eq!(
        process_inline_media_bytes(),
        baseline,
        "dropping every pane must return the process total to its baseline"
    );
}

/// The worker's staging vector is bounded by the pane's budget, not by the
/// fixed per-pane constant.
///
/// The VT worker decodes into a local vector and merges it into the pane's
/// charged store only at the end of the batch, so that vector is **uncharged**
/// while it fills. Trimming it against the fixed constant let a pane stage far
/// more than it could ever retain: with many panes live the fair share is a
/// fraction of the constant, so every pane decoding at once could hold
/// hundreds of megabytes that no ceiling saw.
///
/// The staging vector must therefore never hold more than the merge is going
/// to let the pane keep.
#[test]
fn the_staging_vector_is_bounded_by_the_pane_budget() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    const IMAGE_BYTES: usize = 4 * 1024 * 1024;
    const PANES: usize = 20;

    // Enough live charges that the fair share is well below the fixed
    // per-pane constant — otherwise the two are indistinguishable and the
    // test would pass against the defect.
    let charges: Vec<SharedInlineMediaCharge> =
        (0..PANES).map(|_| new_inline_media_charge()).collect();
    let budget = pane_inline_media_budget();
    assert!(
        budget < MAX_RETAINED_INLINE_IMAGE_BYTES,
        "precondition: the fair share ({budget}) must be below the fixed constant \
         ({MAX_RETAINED_INLINE_IMAGE_BYTES}), or this test cannot discriminate"
    );

    // One worker stages a burst, as it would from a single PTY chunk.
    let mut staged: Vec<InlineImage> = Vec::new();
    for id in 1..=32u64 {
        staged.push(image(id, IMAGE_BYTES));
        trim_staged_inline_images(&mut staged);

        let held = retained_inline_media(&staged).bytes;
        assert!(
            held <= budget.max(IMAGE_BYTES),
            "staging held {held} bytes against a {budget}-byte pane budget; \
             uncharged staging must not exceed what the merge will keep"
        );
    }

    assert!(!staged.is_empty(), "staging must still retain the newest image");
    drop(charges);
}

/// The live-charge count does not ratchet, so budgets do not shrink over time.
///
/// The fair share *divides* the process ceiling by this count, which makes an
/// over-count silently harmful in a way an over-count of bytes would not be:
/// every pane's budget shrinks toward the floor and images start being evicted
/// on a machine with plenty of memory free. Nothing reports it, because each
/// pane is dutifully honouring the budget it was given.
///
/// Two creation sites feed the count — `PaneState::new` makes one and
/// `spawn_pane` replaces it with its own — so the field assignment that
/// performs the replacement has to drop the first. This drives that cycle far
/// more often than a session would and asserts the budget comes back.
#[test]
fn the_live_charge_count_does_not_ratchet_across_pane_churn() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    let baseline = pane_inline_media_budget();

    for cycle in 0..500 {
        // The spawn_pane shape: `PaneState::new` makes one charge, spawn
        // makes a second, and the field assignment replaces the first. What
        // matters is that the replaced charge is *dropped*, not that anything
        // reads it — so the first is deliberately only ever overwritten.
        let from_pane_state = new_inline_media_charge();
        let from_spawn = new_inline_media_charge();
        let _worker_clone = from_spawn.clone();
        let held = from_spawn;
        drop(from_pane_state);

        // Retain and release some media through it, as a real pane would.
        let mut images = vec![image(cycle as u64, 1024 * 1024)];
        drop(trim_inline_images_charged(&mut images, &held));
        drop(images);
        drop(held);
    }

    assert_eq!(
        pane_inline_media_budget(),
        baseline,
        "the per-pane budget must return to its baseline after pane churn; a \
         ratcheting charge count silently shrinks every pane's budget toward \
         the floor with no error anywhere"
    );
    assert_eq!(
        process_inline_media_bytes(),
        0,
        "no bytes may remain charged after every pane is dropped"
    );
}

/// Eviction hands the images back instead of freeing them in place.
///
/// Freeing an evicted `InlineImage` releases up to 4 MiB of pixels through the
/// allocator. Measured: evicting 64 of them costs **2.6 ms** when the buffers
/// are actually freed, against **1.9 µs** when they are kept alive elsewhere —
/// a factor of ~1372. The `Vec` shuffle is negligible; the deallocation is the
/// whole cost.
///
/// The pane's image store is locked while eviction runs, and the render path
/// takes that same lock, so freeing inside the critical section puts a
/// multi-millisecond allocator pause squarely inside a 16 ms frame. Returning
/// the images lets the caller drop them after releasing the guard.
///
/// This pins the seam: the evicted images must come back, and their pixel
/// buffers must still be alive when they do.
#[test]
fn eviction_returns_the_images_so_they_can_be_freed_outside_the_lock() {
    let _serialised = MEDIA_COUNTER_LOCK.lock();
    const IMAGE_BYTES: usize = 4 * 1024 * 1024;

    let charge = new_inline_media_charge();
    let mut images: Vec<InlineImage> = Vec::new();
    for id in 1..=24u64 {
        images.push(image(id, IMAGE_BYTES));
    }
    let offered = images.len();

    let evicted = trim_inline_images_charged(&mut images, &charge);

    assert!(
        !evicted.is_empty(),
        "a pane offered {offered} images past its budget must return the evicted ones, \
         not free them while holding the image lock"
    );
    assert_eq!(
        evicted.len() + images.len(),
        offered,
        "every offered image must be either retained or returned — none may vanish"
    );

    // The returned images must still own their pixels: if eviction had freed
    // them in place, what came back would be empty shells and the caller could
    // not have moved the allocator work off the lock.
    for image in &evicted {
        assert_eq!(
            image.bgra.len(),
            IMAGE_BYTES,
            "a returned image must still hold its pixel buffer"
        );
    }

    // The charge reflects what is retained, not what was offered.
    assert_eq!(
        retained_inline_media(&images).bytes,
        process_inline_media_bytes(),
        "the charge must match what the pane actually kept"
    );

    drop(evicted);
    drop(images);
    drop(charge);
    assert_eq!(process_inline_media_bytes(), 0);
}

/// A `side`x`side` PNG with incompressible pixels, so the encoded payload is a
/// realistic size rather than a solid-colour degenerate case.
fn png_of_side(side: u32) -> Vec<u8> {
    let mut buffer = image::RgbaImage::new(side, side);
    for (x, y, px) in buffer.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 239) as u8, 255]);
    }
    let mut encoded = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png)
        .expect("encoding a PNG in memory cannot fail");
    encoded
}

fn base64_media_event(protocol: MediaProtocol, png: &[u8]) -> MediaEvent {
    MediaEvent {
        protocol,
        row: 0,
        col: 0,
        metadata: String::new(),
        data: base64::engine::general_purpose::STANDARD.encode(png).into_bytes(),
    }
}

/// A Sixel payload that paints every pixel of a `width`x`height` region.
fn sixel_covering(width: usize, height: usize) -> MediaEvent {
    let mut data = Vec::new();
    for _ in 0..height.div_ceil(6) {
        data.extend_from_slice(b"#1!");
        data.extend_from_slice(width.to_string().as_bytes());
        // `~` is 0x7E: all six rows of the band set.
        data.extend_from_slice(b"~-");
    }
    MediaEvent { protocol: MediaProtocol::Sixel, row: 0, col: 0, metadata: String::new(), data }
}

/// The single-image bound must equal what the decoders actually produce.
///
/// This is the assertion the constant lacked. Stated as an **equality** rather
/// than a bound, because a bound is what let the figure drift: derived from the
/// preflight side of 2048 it read 16 MiB, four times the largest image any
/// decoder can emit, and every `<=` check still passed. Only pinning it to the
/// measured maximum fails when the two disagree.
///
/// Driven through real payloads on every protocol rather than through
/// synthetic `Vec`s, because the question is what the *decoders* emit. A
/// synthetic image can be any size the test chooses and would pin nothing.
///
/// The three shapes are the ones that could each bound retention differently:
/// a source above the rendered cap (resized down), a source at it exactly (no
/// resize runs), and a Sixel addressing far beyond its buffer (clipped). The
/// figure asserted is `bgra.len()`, which is what `retained_inline_media`
/// sums and what the charge is set from, so this pins the number the budget is
/// actually enforced against.
#[test]
fn no_decoder_can_emit_an_image_larger_than_the_single_image_bound() {
    let largest = |label: &str, event: &MediaEvent| -> usize {
        let decoded =
            decode_inline_image(event).unwrap_or_else(|| panic!("{label} must decode to an image"));
        assert!(
            decoded.width <= MAX_INLINE_IMAGE_RENDER_SIDE
                && decoded.height <= MAX_INLINE_IMAGE_RENDER_SIDE,
            "{label} decoded to {}x{}, beyond the {MAX_INLINE_IMAGE_RENDER_SIDE}px rendered cap",
            decoded.width,
            decoded.height
        );
        decoded.bgra.len()
    };

    // A source at the largest side the preflight gate admits: resized down.
    let at_preflight_cap = png_of_side(MAX_INLINE_IMAGE_DECODE_SIDE);
    let kitty = largest(
        "Kitty at the preflight cap",
        &base64_media_event(MediaProtocol::Kitty, &at_preflight_cap),
    );
    let iterm = largest(
        "iTerm2 at the preflight cap",
        &base64_media_event(MediaProtocol::Iterm2File, &at_preflight_cap),
    );

    // A source exactly at the rendered cap: no resize runs, so this is the
    // path that reaches retention untouched.
    let at_render_cap = png_of_side(MAX_INLINE_IMAGE_RENDER_SIDE);
    let unresized = largest(
        "Kitty at the rendered cap",
        &base64_media_event(MediaProtocol::Kitty, &at_render_cap),
    );

    // Sixel rasterises into its own buffer and clips, so drive it well past
    // the edge rather than at it.
    let sixel = largest(
        "Sixel addressing beyond its buffer",
        &sixel_covering(
            MAX_INLINE_IMAGE_RENDER_SIDE as usize * 4,
            MAX_INLINE_IMAGE_RENDER_SIDE as usize * 4,
        ),
    );

    let measured = [kitty, iterm, unresized, sixel].into_iter().max().expect("array is non-empty");
    assert_eq!(
        measured,
        MAX_SINGLE_INLINE_IMAGE_BYTES,
        "the largest image any decoder emits is {measured} bytes ({} MiB), but \
         MAX_SINGLE_INLINE_IMAGE_BYTES claims {MAX_SINGLE_INLINE_IMAGE_BYTES} ({} MiB). \
         Every bound derived from that constant is wrong by the difference — it is the \
         residual `max_pane_residual_bytes` reports and the term the aggregate bound is \
         stated in",
        measured / 1048576,
        MAX_SINGLE_INLINE_IMAGE_BYTES / 1048576
    );

    // A source beyond the preflight gate is refused outright rather than
    // clamped, which is what keeps the gate a separate limit from the cap
    // above: it decides whether decoding starts, not what survives it.
    assert!(
        decode_inline_image(&base64_media_event(
            MediaProtocol::Kitty,
            &png_of_side(MAX_INLINE_IMAGE_DECODE_SIDE + 1)
        ))
        .is_none(),
        "a source past the preflight cap must be rejected before any pixels are decoded"
    );
}

#[test]
fn an_absurd_sixel_repeat_decodes_to_the_same_image_as_a_clamped_one() {
    // The repeat count is clamped to the raster width. The claim that matters
    // is not that the clamp is faster — it is that it changes nothing about
    // the output, so a payload asking for billions of repeats must decode to
    // exactly the image a sensible one produces.
    //
    // Every column past MAX_SIDE is discarded by the bounds test inside the
    // paint loop, and `x` saturates past it, so no later byte writes anything
    // either. A test asserting only that the decode returns quickly would
    // pass against a clamp that silently truncated real output.
    let absurd = decode_sixel(b"!4294967295~").expect("an absurd repeat still decodes");
    let clamped = decode_sixel(b"!1024~").expect("a repeat at the limit decodes");

    assert_eq!(absurd, clamped, "clamping the repeat must not change the decoded image");

    // And the clamp did not swallow the picture: `~` is 0x7E, so
    // `bits = 0x7E - 63 = 0b111111` paints all six rows of every column.
    let (w, h, pixels) = absurd;
    assert_eq!(h, 6, "one sixel is six rows tall");
    assert!(w > 1, "the repeat still widened the image, got {w}");
    assert_eq!(pixels.len(), (w as usize) * (h as usize) * 4, "packed BGRA is width * height * 4");
}

#[test]
fn a_repeat_below_the_clamp_is_untouched() {
    // The clamp must not disturb ordinary payloads, or it would buy the
    // pathological case by truncating valid images.
    let (w, h, _) = decode_sixel(b"!8~").expect("a small repeat decodes");
    assert_eq!((w, h), (8, 6), "eight columns of a full six-row sixel");
}

#[test]
fn an_absurd_sixel_repeat_decodes_in_bounded_time() {
    // The equivalence tests above pin what the clamp produces. This pins what
    // it costs, which is the actual defect: without the clamp a twelve-byte
    // payload drives ~4.29 billion no-op iterations.
    //
    // Asserted as wall-clock against a deliberately loose ceiling. A tight
    // bound would be flaky on a loaded machine; a loose one still separates
    // "bounded by the raster" from "bounded by u32::MAX", which are four
    // billion iterations apart and cannot be confused at this resolution.
    let start = std::time::Instant::now();
    let decoded = decode_sixel(b"!4294967295~");
    let elapsed = start.elapsed();

    assert!(decoded.is_some(), "the payload still decodes");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "an absurd repeat took {elapsed:?}; the count is not bounded by the raster width"
    );
}
