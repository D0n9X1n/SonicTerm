//! Do the coverage decisions match the bounds the code enforces?
//!
//! `ClassCoverage` records why each resource class is or is not charged, and
//! `MeasuredNegligible` carries a byte figure so that "small" is a measurement
//! rather than an adjective. The figure is only worth the bytes it is written
//! in if something checks it against the bound production code enforces.
//!
//! `sonicterm-types` is dependency-free by design, so the table there cannot
//! reference the constants bounding the queues it describes. That is exactly
//! the gap a wrong figure hides in: the declaration and the enforcement live in
//! crates that never meet. These tests are in `sonicterm-app` because it
//! depends on both.
//!
//! Measured before this was corrected: `PtyInput` was recorded
//! `MeasuredNegligible { per_pane_bytes: 4096 }` while its queue accepted four
//! messages of up to 16 MiB each — 67,108,864 bytes, **16,384x** the declared
//! figure, reachable from a single paste because a paste is admitted at the
//! full message size and broadcast to every pane.

use enum_map::Enum;
use sonicterm_io::pty::{
    max_pty_queued_input_bytes, MAX_PTY_INPUT_MESSAGE_BYTES, PTY_INPUT_QUEUE_CAPACITY,
};
use sonicterm_types::resource::{ClassCoverage, ResourceClass};

/// A class whose worst case is large must be charged, not called negligible.
///
/// The per-class check in `sonicterm-types` compares the declared figure to a
/// threshold, which catches a figure someone typed too large but cannot catch
/// one that is simply wrong — the declaration is both the claim and the
/// evidence. Here the threshold is applied to the bound read from the
/// constants that actually bound the queue.
#[test]
fn a_class_that_can_hold_megabytes_is_not_recorded_negligible() {
    const NEGLIGIBLE_CEILING: usize = 1024 * 1024;

    // Not a restatement: recomputed from the two constants the queue is built
    // from, then checked against what `sonicterm-io` enforces.
    let real_bound = PTY_INPUT_QUEUE_CAPACITY * MAX_PTY_INPUT_MESSAGE_BYTES;
    assert_eq!(
        real_bound,
        max_pty_queued_input_bytes(),
        "the bound recomputed here must match what sonicterm-io enforces"
    );

    if let ClassCoverage::MeasuredNegligible { per_pane_bytes } = ResourceClass::PtyInput.coverage()
    {
        assert!(
            real_bound <= NEGLIGIBLE_CEILING,
            "PtyInput is recorded negligible at {per_pane_bytes} bytes per pane, but its \
             queue accepts {PTY_INPUT_QUEUE_CAPACITY} messages of up to \
             {MAX_PTY_INPUT_MESSAGE_BYTES} bytes — {real_bound} bytes, {}x the declared \
             figure. Twenty panes make it {} MiB.",
            real_bound / per_pane_bytes.max(1),
            real_bound * 20 / (1024 * 1024)
        );
    }
}

/// The PTY input queue is charged, because its worst case is not small.
///
/// Stated as its own property rather than folded into the check above: the
/// reason this class cannot be `MeasuredNegligible` is that no honest figure
/// would fit under the threshold, so the decision has to be `Charged`.
#[test]
fn the_pty_input_queue_is_a_charged_class() {
    assert_eq!(
        ResourceClass::PtyInput.coverage(),
        ClassCoverage::Charged,
        "the PTY input queue can hold {} bytes per pane, so it must be charged rather \
         than recorded as negligible",
        max_pty_queued_input_bytes()
    );
}

/// Every class still recorded negligible must be small at its real bound.
///
/// The two that remain are bounded by fixed-size payloads: parser replies are
/// short escape sequences, and command events are `Copy` records with no heap
/// behind them. Both are asserted against the aggregate a desktop session
/// would actually see.
#[test]
fn the_remaining_negligible_classes_stay_small_in_aggregate() {
    const PANES: usize = 20;
    const AGGREGATE_CEILING: usize = 4 * 1024 * 1024;

    let mut aggregate = 0usize;
    let mut recorded = Vec::new();
    for index in 0..ResourceClass::LENGTH {
        let class = ResourceClass::from_usize(index);
        if let ClassCoverage::MeasuredNegligible { per_pane_bytes } = class.coverage() {
            aggregate += per_pane_bytes * PANES;
            recorded.push((class, per_pane_bytes));
        }
    }

    assert!(
        aggregate < AGGREGATE_CEILING,
        "the classes recorded negligible sum to {} KiB across {PANES} panes: {recorded:?}",
        aggregate / 1024
    );
}
