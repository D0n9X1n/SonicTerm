use super::*;

/// Serialises the tests that assert on `PROCESS_INLINE_MEDIA_BYTES`.
///
/// The counter is process-global by design — that is the property under test —
/// so two tests charging it concurrently make each other's absolute
/// assertions meaningless. Measured at roughly one failure in twelve runs
/// before this guard: the ceiling test would see a sibling's 8 MiB and report
/// the ceiling breached when its own panes were within it.
static MEDIA_COUNTER_LOCK: Mutex<()> = Mutex::new(());

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

    trim_inline_images(&mut images);

    assert_eq!(images.iter().map(|image| image.id).collect::<Vec<_>>(), vec![2, 3]);
    assert!(
        images.iter().map(|image| image.bgra.len()).sum::<usize>()
            <= MAX_RETAINED_INLINE_IMAGE_BYTES
    );
}

/// One pane's inline-media retention is bounded; the sum across panes is not.
///
/// `trim_inline_images` enforces [`MAX_RETAINED_INLINE_IMAGE_BYTES`] against a
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
        trim_inline_images(&mut pane);
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
            trim_inline_images_charged(images, charge);
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
    trim_inline_images_charged(&mut images, &worker_charge);
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
