use super::*;

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

fn pane_with(cols: u16, rows: u16) -> PaneState {
    PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(cols, rows)))), None)
}

/// A pane reports every seam it holds, and the total is their sum.
///
/// The seams are disjoint by construction — each meters only what it owns — so
/// summing them is meaningful. If a future change made two seams count the
/// same allocation, the total would exceed reality with no test noticing
/// unless the relationship is pinned here.
#[test]
fn a_pane_total_is_exactly_the_sum_of_its_seams() {
    let pane = pane_with(80, 24);
    let retention = measure_pane(&pane).expect("a fresh pane's locks are uncontended");

    let expected_bytes = retention.grid_visible.bytes
        + retention.grid_history.bytes
        + retention.grid_alternate.bytes
        + retention.parser.bytes
        + retention.hyperlinks.bytes
        + retention.inline_media.bytes;
    let expected_items = retention.grid_visible.items
        + retention.grid_history.items
        + retention.grid_alternate.items
        + retention.parser.items
        + retention.hyperlinks.items
        + retention.inline_media.items;

    assert_eq!(retention.total().bytes, expected_bytes);
    assert_eq!(retention.total().items, expected_items);
    assert!(retention.grid_visible.bytes > 0, "a live pane must report grid cells");
}

/// Content written to a pane moves its reported total.
///
/// A measurement that never changes is indistinguishable from one wired to a
/// constant. This drives real bytes through the parser and asserts the figure
/// follows, so the seam is known to be reading live state.
#[test]
fn writing_to_a_pane_moves_its_reported_retention() {
    let pane = pane_with(80, 24);
    let before = measure_pane(&pane).expect("uncontended").total();

    {
        let mut parser = pane.parser.lock();
        parser.grid_mut().set_scrollback_limit(2_000);
        for line in 0..1_500 {
            parser.advance(format!("line {line} with enough text to occupy cells\r\n").as_bytes());
        }
    }

    let after = measure_pane(&pane).expect("uncontended").total();
    assert!(
        after.bytes > before.bytes,
        "scrollback must raise reported retention: {} !> {}",
        after.bytes,
        before.bytes
    );
}

/// Interned hyperlinks show up under the hyperlink seam, not the grid's.
///
/// The registry meters its strings and both `Grid::retained_amount` and
/// `Parser::retained_amount` deliberately exclude them. Attributing them to
/// the wrong seam would point an operator at the wrong subsystem while the
/// total stayed correct — a failure a total-only check cannot catch.
#[test]
fn hyperlink_strings_are_attributed_to_the_hyperlink_seam() {
    let pane = pane_with(80, 24);
    let before = measure_pane(&pane).expect("uncontended");

    {
        let mut parser = pane.parser.lock();
        for index in 0..256 {
            parser.advance(
                format!("\x1b]8;;https://example.com/path/to/resource/{index}\x07x\x1b]8;;\x07")
                    .as_bytes(),
            );
        }
    }

    let after = measure_pane(&pane).expect("uncontended");
    assert!(
        after.hyperlinks.bytes > before.hyperlinks.bytes,
        "interned links must raise the hyperlink seam"
    );
    assert_eq!(after.hyperlinks.items, 256, "each distinct link is one retained item");
    assert_eq!(
        after.parser.bytes, before.parser.bytes,
        "hyperlink strings must not also be charged to the parser seam"
    );
}

/// Panes sum without a bound above them.
///
/// This is the composition behind reported multi-gigabyte growth: each pane
/// stays inside its own ceilings while the session total is the product of
/// pane count and those ceilings. The aggregate exists to make that visible;
/// this pins that it actually composes rather than reporting one pane.
#[test]
fn the_session_total_is_the_sum_over_panes() {
    let panes: Vec<PaneState> = (0..8).map(|_| pane_with(80, 24)).collect();
    let single = measure_pane(&panes[0]).expect("uncontended").total();

    let aggregate = measure_panes(panes.iter());

    assert_eq!(
        aggregate.total().bytes,
        single.bytes * 8,
        "the session total must be the sum over identical panes"
    );
    assert!(
        aggregate.total().bytes > single.bytes,
        "the aggregate must exceed any single pane it contains"
    );
}

/// The dominant seam is reported, so an operator knows where to look.
#[test]
fn the_largest_seam_is_identified_by_name() {
    let retention = PaneRetention {
        grid_visible: ResourceAmount { bytes: 1_000, items: 10 },
        grid_history: ResourceAmount { bytes: 500, items: 5 },
        grid_alternate: ResourceAmount::default(),
        parser: ResourceAmount { bytes: 200, items: 1 },
        hyperlinks: ResourceAmount { bytes: 50, items: 2 },
        inline_media: ResourceAmount { bytes: 64 * 1024 * 1024, items: 3 },
    };

    let (seam, amount) = retention.largest_seam();

    assert_eq!(seam, "inline_media");
    assert_eq!(amount.bytes, 64 * 1024 * 1024);
}

/// Measurement never blocks on a busy VT thread.
///
/// The parser lock is held while output is parsed. A diagnostic that waits for
/// it would stall its caller behind a pane that is streaming — the render path
/// takes this lock with `try_lock` for exactly that reason, and a measurement
/// helper must not reintroduce the stall it avoids.
#[test]
fn measurement_yields_rather_than_waiting_for_the_parser_lock() {
    let pane = pane_with(80, 24);
    let held = pane.parser.lock();

    assert!(measure_pane(&pane).is_none(), "a contended pane must yield, not block");

    drop(held);
    assert!(measure_pane(&pane).is_some(), "measurement resumes once the lock is free");
}

/// An empty session reports zero rather than failing.
#[test]
fn an_empty_session_reports_zero() {
    let aggregate = measure_panes(std::iter::empty());

    assert_eq!(aggregate.total().bytes, 0);
    assert_eq!(aggregate.total().items, 0);
}

/// Sampling is rate-limited, so the idle path cannot be flooded.
///
/// The caller is the idle-wake path, which runs whenever the event loop has
/// nothing to do. Sampling on every wake would walk every pane's seams and
/// take every parser lock at whatever rate the loop happens to spin — the
/// cost this interval exists to avoid.
///
/// Asserted against `retention_sample_due` rather than the logging wrapper.
/// The wrapper is guarded by `tracing::enabled!`, which is false under `cargo
/// test` because no subscriber is installed — a first version of this test
/// sat entirely behind that early return and passed without executing a
/// single assertion.
#[test]
fn retention_sampling_is_rate_limited() {
    let start = Instant::now();
    let mut last: Option<Instant> = None;

    assert!(
        retention_sample_due(&mut last, start),
        "the first call must sample: there is no previous sample to rate-limit against"
    );
    assert_eq!(last, Some(start));

    assert!(
        !retention_sample_due(&mut last, start + Duration::from_secs(1)),
        "a call one second later must be refused"
    );
    assert!(
        !retention_sample_due(
            &mut last,
            start + RETENTION_SAMPLE_INTERVAL - Duration::from_millis(1)
        ),
        "a call just short of the interval must be refused"
    );
    assert_eq!(last, Some(start), "a refused call must not move the timestamp");

    assert!(
        retention_sample_due(&mut last, start + RETENTION_SAMPLE_INTERVAL),
        "a call at exactly the interval must sample"
    );
    assert_eq!(last, Some(start + RETENTION_SAMPLE_INTERVAL));
}

/// A contended pane is skipped, never waited on.
///
/// The sampler runs on the idle-wake path. Blocking there behind a VT thread
/// that is parsing output would stall the event loop to produce a debug line —
/// the diagnostic interfering with the thing it reports on.
///
/// Asserted against `log_sampled_panes` for the same reason as above: the
/// gated wrapper never runs under test.
#[test]
fn sampling_skips_a_contended_pane_rather_than_blocking() {
    let busy = pane_with(80, 24);
    let free = pane_with(80, 24);
    let held = busy.parser.lock();

    // Completing at all is half the assertion: a blocking implementation
    // would deadlock here, since this thread already holds `busy`'s lock.
    let session = log_sampled_panes([("busy", &busy), ("free", &free)]);

    let free_alone = measure_pane(&free).expect("the free pane is uncontended");
    assert_eq!(
        session.total().bytes,
        free_alone.total().bytes,
        "the contended pane must be skipped, not waited on and not counted"
    );
    assert!(session.total().bytes > 0, "the uncontended pane must still be measured");

    drop(held);
}

/// A saved primary screen is charged to `GridAlternate`, not folded into
/// history.
///
/// Before this split every grid byte — visible, history and saved primary
/// alike — was charged to `GridHistory`. The total was right and the
/// attribution was wrong, which matters because the remedy differs: history
/// shrinks by lowering `scrollback`, while a saved primary is memory held for
/// a screen the user is not looking at and which frees itself when the
/// full-screen program exits.
#[test]
fn a_saved_primary_screen_is_charged_to_its_own_class() {
    let pane = pane_with(80, 24);
    {
        let mut parser = pane.parser.lock();
        for _ in 0..300 {
            parser.advance(b"scrollback content for the primary screen\r\n");
        }
    }

    let before = measure_pane(&pane).expect("uncontended");
    assert_eq!(
        before.grid_alternate,
        ResourceAmount::default(),
        "precondition: no alternate screen is active"
    );
    assert!(before.grid_history.bytes > 0, "precondition: the primary has history");

    // Enter the alternate screen the way a full-screen program does.
    pane.parser.lock().advance(b"\x1b[?1049h");
    let during = measure_pane(&pane).expect("uncontended");

    assert!(during.grid_alternate.bytes > 0, "the saved primary must be charged to GridAlternate");

    // The classes charged must reflect it.
    let classes = seam_classes(&during);
    let alternate = classes
        .iter()
        .find(|(class, _)| *class == ResourceClass::GridAlternate)
        .expect("GridAlternate must be among the charged classes");
    assert_eq!(alternate.1, during.grid_alternate);

    // And the total must not have moved by the re-attribution alone — the
    // whole point is that this changes where bytes are charged, not how many.
    let sum: usize = classes.iter().map(|(_, amount)| amount.bytes).sum();
    assert_eq!(sum, during.total().bytes, "re-attribution must not change the total charged");
}

/// Every grid class the inventory names must be charged, not just history.
#[test]
fn all_three_grid_classes_appear_among_the_charged_seams() {
    let retention = PaneRetention::default();
    let charged: Vec<ResourceClass> =
        seam_classes(&retention).iter().map(|(class, _)| *class).collect();

    for class in
        [ResourceClass::GridVisible, ResourceClass::GridHistory, ResourceClass::GridAlternate]
    {
        assert!(charged.contains(&class), "{class:?} must have a production charge site");
    }
}

/// The wiki's field table must name the fields the log line emits.
///
/// `wiki/Logging.md` documented `grid_bytes` after that field was split into
/// three, so a user following the documentation would look for a field that no
/// longer exists. Nothing catches that: the log line compiles, the wiki
/// renders, and the mismatch surfaces only when someone tries to use it.
///
/// Both language sections are checked, because a bilingual page drifts one
/// half at a time.
#[test]
fn the_wiki_documents_the_fields_the_memory_log_actually_emits() {
    const WIKI: &str = include_str!("../../../../wiki/Logging.md");
    const SOURCE: &str = include_str!("retention.rs");

    // Fields the log line emits, scraped from the emitting source.
    let emitted: Vec<&str> = SOURCE
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let name = line.split(" = ").next()?;
            (name.ends_with("_bytes") && line.contains(" = retention.")).then_some(name)
        })
        .collect();

    assert!(!emitted.is_empty(), "the scan must find emitted fields, or it asserts nothing");

    for field in &emitted {
        let count = WIKI.matches(field).count();
        assert!(
            count >= 2,
            "`{field}` is emitted by the memory log but appears {count} time(s) in \
             wiki/Logging.md; both the English and 中文 tables must name it"
        );
    }

    // And the reverse: the table must not document a field that no longer
    // exists. `grid_bytes` was split into three by the class-attribution work,
    // and the wiki kept naming it — a user would look for a field the log line
    // stopped emitting.
    assert!(
        !WIKI.contains("`grid_bytes`"),
        "wiki/Logging.md still documents `grid_bytes`, which the log line no longer emits"
    );
}
