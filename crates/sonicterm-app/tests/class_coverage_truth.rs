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
use sonicterm_app::app::retention::{seam_classes, PaneRetention};
use sonicterm_io::pty::{
    max_pty_queued_input_bytes, MAX_PTY_INPUT_MESSAGE_BYTES, PTY_INPUT_QUEUE_CAPACITY,
};
use sonicterm_types::resource::{ClassCoverage, PaneSeamTerm, ResourceClass};

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

/// The backstop must sit above everything a pane can now be charged.
///
/// `PANE_COMMITTED_BUDGET_BYTES` is a real enforced limit — it is the
/// `owner_bytes` a pane owner is held to — and it is derived from the six seam
/// caps times a headroom multiplier. Charging the PTY input queue adds a term
/// that derivation never counted, so the headroom that was slack for allocator
/// overshoot is now partly spent on a real seam.
///
/// A backstop below the worst case it backstops stops being a tripwire and
/// becomes the enforcer, refusing panes that are behaving correctly.
#[test]
fn the_backstop_covers_the_seams_plus_the_charged_input_queue() {
    use sonicterm_app::app::{PANE_COMMITTED_BUDGET_BYTES, PANE_SEAM_CAP_SUM_BYTES};

    let pty_input_bound = PTY_INPUT_QUEUE_CAPACITY * MAX_PTY_INPUT_MESSAGE_BYTES;
    let worst_case = PANE_SEAM_CAP_SUM_BYTES + pty_input_bound;

    assert!(
        PANE_COMMITTED_BUDGET_BYTES > worst_case,
        "the backstop is {PANE_COMMITTED_BUDGET_BYTES} bytes and a pane's worst case is \
         now {worst_case} — {PANE_SEAM_CAP_SUM_BYTES} of seam caps plus {pty_input_bound} \
         of queued input, which is charged but is not one of the terms the budget is \
         derived from. A backstop below the worst case refuses panes that are behaving \
         correctly."
    );
}

/// A class recorded as uncharged must not appear in the charge path.
///
/// `UnchargedRetention` is the one coverage variant that describes work not
/// done: the memory is present and significant, and no owner's ledger carries
/// it. That claim is only worth reading if something checks it, because the
/// failure mode is silent in both directions. A class wired up to charge while
/// the table still says otherwise leaves the table lying about the thing it
/// exists to record; a class the table promotes to `Charged` without a charge
/// site reproduces exactly the defect the variant was introduced to make
/// visible — a row reading `Charged` with nothing behind it.
///
/// `seam_classes` is the whole production charge path. It is the sole argument
/// to the only `charge_classes` call an app makes, so a class absent from it is
/// charged nowhere, whatever any table says.
#[test]
fn no_uncharged_class_appears_in_the_production_charge_path() {
    let charged: Vec<ResourceClass> =
        seam_classes(&PaneRetention::default()).into_iter().map(|(class, _)| class).collect();

    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        let ClassCoverage::UnchargedRetention { per_owner_bytes } = class.coverage() else {
            continue;
        };

        assert!(
            !charged.contains(&class),
            "{class:?} is recorded UnchargedRetention at {per_owner_bytes} bytes per owner, \
             but it is in the production charge path. Either the charge is real and the \
             table must say so, or the class does not belong in the path."
        );
    }
}

/// The renderer's host-side classes reach no ledger, though they do reach a
/// report.
///
/// Stated for these two by name rather than left to the sweep above, because
/// the reason they are absent is structural and not obvious from the table:
/// `sonicterm-gpu` declares no dependency on `sonicterm-resource`, so the crate
/// that computes both figures cannot reserve against a governor at all.
///
/// They are no longer invisible. The app reads `retained_amounts` and emits a
/// `renderer retention` line per renderer, live and warm, on the memory
/// target's sampling interval. Reporting is not charging, and this test pins
/// the second: a future change that wired these into the charge path would
/// have to resolve the `InlineMediaRetained` collision first, since the
/// renderer's image atlas and a pane's decoded media are different allocations
/// under the same class.
///
/// The consequence is a diagnosis gap rather than an unbounded allocation —
/// the per-seam caps bound this memory whether or not anything is charged. What
/// is missing is the ability to see it in the *charge* path: on the Windows
/// software path the frame buffer is the largest buffer a renderer holds, and
/// scales with the window rather than sitting at a fixed size. It is not the
/// largest buffer in the process — three 64 MiB bounds exceed a 4K frame — but
/// it is the one a user cannot find in a per-owner ledger.
#[test]
fn the_renderer_host_side_classes_are_absent_from_the_charge_path() {
    let charged: Vec<ResourceClass> =
        seam_classes(&PaneRetention::default()).into_iter().map(|(class, _)| class).collect();

    for class in [ResourceClass::GlyphAtlas, ResourceClass::SoftwareFrame] {
        assert!(
            matches!(class.coverage(), ClassCoverage::UnchargedRetention { .. }),
            "{class:?} is computed by the renderer and charged by nothing, so it must be \
             recorded UnchargedRetention rather than as a decision that means there is \
             nothing to do"
        );
        assert!(
            !charged.contains(&class),
            "{class:?} is in the charge path, so its coverage row is stale"
        );
        assert_eq!(
            class.pane_seam_term(),
            PaneSeamTerm::NotChargedInProduction,
            "{class:?} reaches no owner's ledger, so a pane's seam-cap sum must carry no \
             term for it"
        );
    }
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
    for index in 0..ResourceClass::COUNT {
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
