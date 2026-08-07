use super::*;

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

use crate::app::{App, WindowId};

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
        + retention.inline_media.bytes
        + retention.pty_output.bytes
        + retention.pty_input.bytes;
    let expected_items = retention.grid_visible.items
        + retention.grid_history.items
        + retention.grid_alternate.items
        + retention.parser.items
        + retention.hyperlinks.items
        + retention.inline_media.items
        + retention.pty_output.items
        + retention.pty_input.items;

    assert_eq!(retention.total().bytes, expected_bytes);
    assert_eq!(retention.total().items, expected_items);
    assert!(retention.grid_visible.bytes > 0, "a live pane must report grid cells");
}

/// Every seam carries weight in the total, including the ones a fresh pane
/// leaves empty.
///
/// The test above measures a live pane, where `pty_input` is zero — nothing is
/// queued toward the shell. A zero term is invisible on both sides of the
/// assertion, so that test omitted `pty_input` entirely and still passed, and
/// would have kept passing if `total()` stopped folding it.
///
/// Constructed rather than measured, with a distinct non-zero value per seam,
/// so dropping any one term from `total()` changes the sum by an amount no
/// other term can account for.
#[test]
fn every_seam_contributes_to_the_total() {
    // Distinct powers of two: any subset sums to a unique value, so a missing
    // term cannot be masked by the others.
    let retention = PaneRetention {
        grid_visible: ResourceAmount { bytes: 1, items: 1 },
        grid_history: ResourceAmount { bytes: 2, items: 2 },
        grid_alternate: ResourceAmount { bytes: 4, items: 4 },
        parser: ResourceAmount { bytes: 8, items: 8 },
        hyperlinks: ResourceAmount { bytes: 16, items: 16 },
        inline_media: ResourceAmount { bytes: 32, items: 32 },
        pty_output: ResourceAmount { bytes: 64, items: 64 },
        pty_input: ResourceAmount { bytes: 128, items: 128 },
    };

    let total = retention.total();
    assert_eq!(
        total.bytes, 255,
        "the total must fold all eight seams; a missing term leaves a gap the \
         other seven cannot produce"
    );
    assert_eq!(total.items, 255, "items must fold the same eight seams as bytes");
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
        pty_output: ResourceAmount { bytes: 8 * 1024, items: 1 },
        pty_input: ResourceAmount { bytes: 128, items: 1 },
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

#[test]
fn measurement_does_not_report_contended_inline_media_as_zero() {
    let pane = pane_with(80, 24);
    let held = pane.inline_images.lock();

    assert!(
        measure_pane(&pane).is_none(),
        "inline-media contention must make the pane partial, not look like zero retained media"
    );

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

/// The seam table must name the same fields, and its sample must be real.
///
/// This guards the table and sample rather than the prose: the seam table once
/// omitted `pty_input_bytes` entirely while the prose counted seven seams and
/// the log line emitted eight. Because `total_bytes` is the sum of all of them,
/// the documented rows did not add up to the documented total — and reconciling
/// seams against the total is exactly the procedure the triage guide teaches.
///
/// Both language halves carry the sample, so the check requires a fenced block
/// that names every field rather than merely finding one somewhere in the page.
#[test]
fn the_seam_table_documents_the_fields_the_memory_log_actually_emits() {
    const WIKI: &str = include_str!("../../../../wiki/Logging.md");
    const SOURCE: &str = include_str!("retention.rs");

    let emitted: Vec<&str> = SOURCE
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let name = line.split(" = ").next()?;
            (name.ends_with("_bytes") && line.contains(" = retention.")).then_some(name)
        })
        .collect();

    assert!(!emitted.is_empty(), "the scan must find emitted fields, or it asserts nothing");

    // Asserted against table rows, not against the file. A whole-file
    // `contains` is satisfied by the sample block below, so it would pass with
    // the table row deleted — the exact omission this test exists to catch.
    // Verified by deleting the row and watching this go RED.
    let table_rows: Vec<&str> =
        WIKI.lines().map(str::trim_start).filter(|line| line.starts_with('|')).collect();
    assert!(!table_rows.is_empty(), "the scan must find table rows, or it asserts nothing");

    for field in &emitted {
        assert!(
            table_rows.iter().any(|row| row.contains(field)),
            "`{field}` is emitted by the memory log but no table row in wiki/Logging.md \
             names it; the seam table must account for every term in total_bytes"
        );
    }

    // The sample log output is quoted as if copied from a real run. A sample
    // missing a field the log line always emits sends a reader looking for a
    // discrepancy that is in the docs, not in their terminal.
    //
    // The block must name every emitted field, not merely mention the phrase.
    // The page also carries shell recipes that grep for `pane retention`, and
    // those blocks contain the phrase without being samples — selecting one of
    // them would assert nothing.
    let sample = WIKI
        .split("```")
        .find(|block| {
            block.contains("total_bytes=") && emitted.iter().all(|field| block.contains(field))
        })
        .expect("wiki/Logging.md must contain a fenced sample `pane retention` block");
    for field in &emitted {
        assert!(
            sample.contains(field),
            "the sample `pane retention` block in wiki/Logging.md omits `{field}`, \
             which the log line always emits"
        );
    }
}

/// The coverage table must match the charge sites that exist.
///
/// The table is a claim about the code. Without this it is a claim nobody
/// checks — which is how `PtyOutput` came to be recorded as charged before its
/// charge site was written, in this same change.
///
/// Scans the seam-class list rather than trusting the table: a class the
/// retention pass charges must be recorded `Charged`, and a class recorded
/// `Charged` must be one the pass actually charges.
#[test]
fn the_coverage_table_agrees_with_the_charge_sites() {
    use enum_map::Enum;
    use sonicterm_types::{ClassCoverage, ResourceClass};

    let charged_here: Vec<ResourceClass> =
        seam_classes(&PaneRetention::default()).iter().map(|(class, _)| *class).collect();

    for class in &charged_here {
        assert_eq!(
            class.coverage(),
            ClassCoverage::Charged,
            "{class:?} is charged by the retention pass but the coverage table does not \
             record it as charged"
        );
    }

    // And the converse: a class recorded `Charged` must be one this pass
    // charges. The set is read from `seam_classes` rather than listed here,
    // because a list written by hand records what someone believed and keeps
    // reporting it after it stops being true — which is what let two classes
    // stay recorded as charged while nothing read the seam that would have
    // charged them, and then let a later derivation inherit the same claim.
    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        if class.coverage() == ClassCoverage::Charged {
            assert!(
                charged_here.contains(&class),
                "{class:?} is recorded as charged but no production pass charges it"
            );
        }
    }
}

/// The charging pass runs on the sampling interval, not on every wake.
///
/// Its caller is `do_about_to_wait`, which fires on every idle wake — hundreds
/// of times a second under sustained pane output. Each pass walks every pane's
/// grid cell by cell through `retained_amount_by_region`, which is uncached, so
/// an ungated pass repeats that walk at the event loop's spin rate.
///
/// Enters `sample_pane_retention` directly rather than through
/// `__test_sample_pane_retention_now`, which clears the limiter by design and
/// therefore cannot observe a cadence. `now` is supplied explicitly so the
/// interval is crossed without sleeping.
///
/// Removing the cadence check fails this: the second call charges the grown
/// scrollback immediately and the mid-interval figure moves.
#[test]
fn charging_runs_on_the_sampling_interval_rather_than_every_wake() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let window = app.__test_seed_child_window(&["one"]);
    let pane_id = *app
        .__test_child_pane_ids(window)
        .expect("the seeded window exists")
        .first()
        .expect("the window has a pane");

    let start = Instant::now();

    // First wake: nothing has been sampled yet, so this one charges.
    app.sample_pane_retention(start);
    let charged_at_start: usize = app
        .__test_pane_charges(window, pane_id)
        .expect("the pane holds charges")
        .values()
        .map(|amount| amount.bytes)
        .sum();
    assert!(
        charged_at_start > 0,
        "the first wake must charge: a pane holding cells and charged nothing leaves every \
         governor limit with no figure to apply itself to"
    );

    // Grow the pane by enough that a fresh charge could not report the old
    // figure by coincidence.
    {
        let pane = seeded_pane(&app, window, pane_id);
        let mut parser = pane.parser.lock();
        parser.grid_mut().set_scrollback_limit(2_000);
        for line in 0..1_500 {
            parser.advance(format!("line {line} with enough text to occupy cells\r\n").as_bytes());
        }
    }
    let held_now =
        measure_pane(seeded_pane(&app, window, pane_id)).expect("uncontended").total().bytes;
    assert!(
        held_now > charged_at_start,
        "precondition failed: the pane did not grow, so a charge that followed it \
         immediately would be indistinguishable from one that did not"
    );

    // Wakes inside the interval, at the rate the event loop actually delivers
    // them. None may re-walk the panes.
    for offset_ms in [1, 2, 5, 100, 1_000, 29_999] {
        app.sample_pane_retention(start + Duration::from_millis(offset_ms));
        let charged: usize = app
            .__test_pane_charges(window, pane_id)
            .expect("the pane holds charges")
            .values()
            .map(|amount| amount.bytes)
            .sum();
        assert_eq!(
            charged, charged_at_start,
            "a wake {offset_ms} ms into the interval re-charged the pane. The pass walks every \
             pane's grid cell by cell and its caller fires hundreds of times a second, so it \
             must run on the interval rather than on the wake"
        );
    }

    // At the interval, the pass runs and the figure catches up.
    app.sample_pane_retention(start + RETENTION_SAMPLE_INTERVAL);
    let charged_after_interval: usize = app
        .__test_pane_charges(window, pane_id)
        .expect("the pane holds charges")
        .values()
        .map(|amount| amount.bytes)
        .sum();
    assert!(
        charged_after_interval > charged_at_start,
        "the pass must run once the interval elapses: rate-limiting it must delay the figure, \
         never stop maintaining it. {charged_after_interval} !> {charged_at_start}"
    );
}

/// A slow transfer survives the wakes that arrive inside one interval.
///
/// `reclaim_stalled_captures` treats a capture whose progress figure is
/// unchanged across two consecutive samples as abandoned. That inference is
/// only sound if consecutive samples are an interval apart. Called once per
/// wake they are milliseconds apart, and a transfer that is merely slow — an
/// image arriving over a loaded link — looks identical to one that died.
///
/// Drives the wake rate this was measured at: hundreds of wakes inside a single
/// interval, with no bytes arriving between them.
#[test]
fn a_slow_capture_survives_the_wakes_inside_one_interval() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let window = app.__test_seed_child_window(&["one"]);
    let pane_id = *app
        .__test_child_pane_ids(window)
        .expect("the seeded window exists")
        .first()
        .expect("the window has a pane");

    // An APC introducer with payload and no terminator: a transfer still in
    // flight. Nothing here says whether it is slow or dead.
    let mut chunk = Vec::with_capacity(512 * 1024 + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(512 * 1024, b'A');
    {
        let pane = seeded_pane(&app, window, pane_id);
        pane.parser.lock().advance(&chunk);
    }
    assert_eq!(
        app.__test_pane_capture_count(window, pane_id),
        Some(1),
        "precondition: the capture is in flight"
    );

    // One interval's worth of wakes at the measured sustained-output rate.
    // No bytes arrive: the transfer is slow, not dead.
    let start = Instant::now();
    for wake in 0..600u64 {
        app.sample_pane_retention(start + Duration::from_millis(wake * 2));
    }

    assert_eq!(
        app.__test_pane_capture_count(window, pane_id),
        Some(1),
        "600 wakes inside one interval cancelled a live transfer. Two consecutive samples mean \
         two intervals only if the pass is rate-limited; on the wake path they are milliseconds \
         apart, so a slow transfer is destroyed and the reported stall duration is wrong by the \
         ratio between a wake and an interval"
    );
}

/// A genuinely stalled capture is still reclaimed, once the threshold is met.
///
/// The guard above must not be satisfied by never reclaiming at all. This is
/// the same shape — a capture with no bytes arriving — sampled across enough
/// intervals to meet [`STALL_SAMPLES_BEFORE_CANCEL`], and it must be
/// cancelled.
///
/// The loop counts derive from the constant rather than hardcoding an interval
/// count, so raising the threshold cannot leave this test asserting the old
/// one while still passing.
#[test]
fn a_stalled_capture_is_still_reclaimed_across_the_stall_threshold() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let window = app.__test_seed_child_window(&["one"]);
    let pane_id = *app
        .__test_child_pane_ids(window)
        .expect("the seeded window exists")
        .first()
        .expect("the window has a pane");

    let mut chunk = Vec::with_capacity(512 * 1024 + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(512 * 1024, b'A');
    {
        let pane = seeded_pane(&app, window, pane_id);
        pane.parser.lock().advance(&chunk);
    }

    let start = Instant::now();
    // The first sample only records a figure — there is nothing to compare it
    // against — so reaching the threshold takes one more sample than the
    // threshold itself.
    let samples_to_cancel = u32::from(STALL_SAMPLES_BEFORE_CANCEL) + 1;
    for sample in 0..samples_to_cancel - 1 {
        app.sample_pane_retention(start + RETENTION_SAMPLE_INTERVAL * sample);
        assert_eq!(
            app.__test_pane_capture_count(window, pane_id),
            Some(1),
            "sample {sample} of {samples_to_cancel} must not cancel: below the threshold a \
             merely-slow transfer is indistinguishable from a dead one"
        );
    }

    app.sample_pane_retention(start + RETENTION_SAMPLE_INTERVAL * (samples_to_cancel - 1));
    assert_eq!(
        app.__test_pane_capture_count(window, pane_id),
        Some(0),
        "a capture quiet across the full threshold must still be reclaimed; widening the \
         window must delay reclamation, never remove it"
    );
}

/// Bytes arriving reset the stall count, so silence must be consecutive.
///
/// Without this, a transfer that goes quiet for one interval, delivers a chunk,
/// then goes quiet again would accumulate its way to cancellation despite never
/// being silent for the threshold — the count would measure total quiet samples
/// rather than consecutive ones. That is exactly the trickling-but-alive
/// transfer the threshold exists to protect.
#[test]
fn bytes_arriving_reset_the_stall_count() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let window = app.__test_seed_child_window(&["one"]);
    let pane_id = *app
        .__test_child_pane_ids(window)
        .expect("the seeded window exists")
        .first()
        .expect("the window has a pane");

    let mut chunk = Vec::with_capacity(512 * 1024 + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(512 * 1024, b'A');
    {
        let pane = seeded_pane(&app, window, pane_id);
        pane.parser.lock().advance(&chunk);
    }

    let start = Instant::now();
    let mut elapsed = 0u32;

    // Enough alternating quiet-then-a-byte rounds that a cumulative counter
    // would have cancelled several times over.
    for _round in 0..4 {
        for _ in 0..u32::from(STALL_SAMPLES_BEFORE_CANCEL) {
            app.sample_pane_retention(start + RETENTION_SAMPLE_INTERVAL * elapsed);
            elapsed += 1;
        }
        // One byte: the transfer is slow, not dead.
        {
            let pane = seeded_pane(&app, window, pane_id);
            pane.parser.lock().advance(b"A");
        }
        assert_eq!(
            app.__test_pane_capture_count(window, pane_id),
            Some(1),
            "a transfer still delivering bytes must never be cancelled, however slowly it \
             delivers them"
        );
    }
}

/// Fill a pane the way its PTY thread does: merge, then trim under charge.
///
/// Pushing into `inline_images` directly would move no counter — the charge is
/// applied by the trim, so a test that skipped it would drive a process total
/// that stays at zero and a ceiling that is never crossed.
fn decode_into(pane: &PaneState, id: &mut u64, count: usize, image_bytes: usize) {
    for _ in 0..count {
        *id += 1;
        let evicted = {
            let mut images = pane.inline_images.lock();
            images.push(sonicterm_render_model::InlineImage {
                id: *id,
                row: 0,
                col: 0,
                width: 1,
                height: 1,
                bgra: Arc::from(vec![0u8; image_bytes]),
            });
            crate::app::media::trim_inline_images_charged(&mut images, &pane.inline_media_charge)
        };
        drop(evicted);
    }
}

fn retained_bytes(pane: &PaneState) -> usize {
    crate::app::media::retained_inline_media(&pane.inline_images.lock()).bytes
}

/// Reach a seeded pane through the private field rather than a new test seam.
///
/// These tests are in-crate, so nothing has to be made public to drive them.
fn seeded_pane(app: &App, window: WindowId, pane_id: u64) -> &PaneState {
    app.windows
        .get(&window)
        .and_then(|state| state.panes.get(&pane_id))
        .expect("the seeded pane exists")
}

/// A pane that filled early must not keep that budget once panes multiply.
///
/// The budget is the process ceiling divided by the live pane count, so it
/// shrinks as panes arrive — but a pane only recomputes it *while decoding*,
/// on its own PTY thread. A pane that filled up and went idle is never
/// revisited, and keeps a share sized for a session that no longer exists.
///
/// # Why this asserts per pane
///
/// The aggregate bound `ceiling + panes × floor` cannot fail this case: at 64
/// panes it permits 512 MiB against a measured 496 MiB, and stated against the
/// single-image residual it permits 1280 MiB. Both scale with the pane count
/// they are meant to bound, so both stay green on the unfixed code. Two
/// earlier attempts at this test were written that way and passed without
/// reproducing anything.
///
/// The quantity the defect actually moves is **one pane's retained bytes**:
/// 64 MiB held where 4 MiB renders everything it can show. That is asserted
/// here per pane, and the aggregate is kept only as a secondary check.
#[test]
fn an_idle_pane_gives_back_a_budget_sized_for_a_smaller_session() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    const EARLY: usize = 4;
    const LATE: usize = 60;
    // One MiB, so a pane at the 4 MiB floor still holds four whole images and
    // the per-pane bound below is the floor itself rather than one large
    // image standing in for it.
    const IMAGE_BYTES: usize = 1024 * 1024;

    let floor = crate::app::media::MIN_PANE_INLINE_MEDIA_BYTES;
    let mut id = 0u64;

    // Four panes fill at the early, generous budget — then go idle. Nothing
    // decodes into them again for the rest of this test.
    let early: Vec<PaneState> = (0..EARLY).map(|_| pane_with(80, 24)).collect();
    for pane in &early {
        decode_into(pane, &mut id, 128, IMAGE_BYTES);
    }

    let before: Vec<usize> = early.iter().map(retained_bytes).collect();
    for (index, &bytes) in before.iter().enumerate() {
        assert!(
            bytes > floor,
            "precondition failed: early pane {index} filled to {bytes} bytes, at or below the \
             {floor}-byte floor, so this run cannot show a pane coming down from a larger \
             budget — the fill above did not reach the generous share"
        );
    }

    // Many more panes arrive. Each trims itself as it decodes; the early four
    // never decode again, which is exactly the case under test.
    let late: Vec<PaneState> = (0..LATE).map(|_| pane_with(80, 24)).collect();
    for pane in &late {
        decode_into(pane, &mut id, 8, IMAGE_BYTES);
    }

    assert!(
        crate::app::media::process_inline_media_bytes()
            > crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES,
        "precondition failed: the process is not over its ceiling, so there is no pressure \
         for the pass to relieve"
    );

    let reclaimed = trim_panes_over_media_ceiling(early.iter().chain(late.iter()));

    // The assertion that discriminates. Per pane, not aggregate.
    for (index, pane) in early.iter().enumerate() {
        let after = retained_bytes(pane);
        assert!(
            after <= floor.max(IMAGE_BYTES),
            "early pane {index} still holds {after} bytes ({} MiB) after the pass, against a \
             {floor}-byte floor. It was admitted when {EARLY} panes existed and there are now \
             {}; a pane that filled early and went idle is keeping a share the ceiling can no \
             longer honour, because only a decoding pane re-trims",
            after / 1048576,
            EARLY + LATE
        );
        assert!(
            after > 0,
            "early pane {index} was trimmed to nothing; every pane must keep its most recent \
             image, or the pass refuses the user the thing they asked to see"
        );
        assert!(
            after < before[index],
            "early pane {index} did not come down at all: {} bytes before, {after} after",
            before[index]
        );
    }

    assert!(reclaimed > 0, "the pass reported reclaiming nothing while panes were over budget");

    // Secondary, and only that: every pane is entitled to render one image, so
    // this term has to scale with the pane count. It is a real bound but it
    // does not discriminate — it holds on the unfixed code too.
    let total = crate::app::media::process_inline_media_bytes();
    let bound = crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES + (EARLY + LATE) * floor;
    assert!(
        total <= bound,
        "the process holds {} MiB against a stateable bound of {} MiB",
        total / 1048576,
        bound / 1048576
    );
}

/// The pass runs in a shipped build, where nothing is watching the log.
///
/// Reclamation sits above the `enabled!(target: "memory", DEBUG)` gate in
/// `sample_pane_retention`. Below it, the pass would do nothing in every
/// default session and everything in a session with `memory=debug` — the
/// memory would come back only for users already investigating why it had not.
///
/// No subscriber is installed here, so the gate is closed: this enters the
/// real production path with the level check failing, and asserts the trim
/// happened anyway. Moving the call below the gate fails this test.
///
/// The allocator assertion accepts either one complete measured field set or
/// the explicit unsupported sentinel, never omission or fabricated zeroes.
#[test]
fn production_sampling_persists_breadcrumbs_with_the_memory_log_switched_off() {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-app-breadcrumb-sampling-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let writer = sonicterm_logging::breadcrumbs::BreadcrumbWriter::start(
        &dir,
        "production-sampling",
        sonicterm_logging::breadcrumbs::BreadcrumbLimits::default(),
    )
    .expect("start breadcrumb writer");

    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_seed_child_window(&["one"]);
    app.set_breadcrumb_recorder(writer.recorder());

    assert!(
        !tracing::enabled!(target: "memory", tracing::Level::INFO),
        "precondition: the breadcrumb path must not rely on INFO logging"
    );
    assert!(
        !app.__test_sample_pane_retention_now(),
        "without DEBUG logging the production sampler still reports no detail log sample"
    );
    writer.shutdown().expect("flush breadcrumb writer");

    let written = std::fs::read_to_string(
        sonicterm_logging::breadcrumbs::breadcrumb_path(&dir, "production-sampling")
            .expect("breadcrumb path"),
    )
    .expect("read persisted breadcrumbs");
    for expected in ["event=counts", "event=resource", "event=retention"] {
        assert!(written.contains(expected), "missing {expected:?} in {written:?}");
    }
    assert!(written.contains("windows=2 panes=1"), "wrong app counts: {written}");
    assert!(written.contains("live_renderers="), "missing renderer count: {written}");
    assert!(
        written.contains("allocator=unsupported")
            || (written.contains("allocator_allocated_bytes=")
                && written.contains("allocator_reserved_bytes=")
                && written.contains("allocator_allocations=")
                && written.contains("allocator_blocks=")
                && written.contains("allocator_largest_block_bytes=")),
        "allocator state was omitted or fabricated: {written}"
    );

    std::fs::remove_dir_all(dir).expect("remove breadcrumb scratch directory");
}

#[test]
fn media_is_reclaimed_with_the_memory_log_switched_off() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    const IMAGE_BYTES: usize = 1024 * 1024;

    assert!(
        !tracing::enabled!(target: "memory", tracing::Level::DEBUG),
        "precondition failed: a subscriber is recording `memory` at debug, so this test \
         cannot show behaviour that differs below the gate"
    );

    let floor = crate::app::media::MIN_PANE_INLINE_MEDIA_BYTES;
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let window = app.__test_seed_child_window(&["early"]);
    let pane_ids = app.__test_child_pane_ids(window).expect("the seeded window exists");
    let early_id = *pane_ids.first().expect("the window has a pane");

    let mut id = 0u64;
    decode_into(seeded_pane(&app, window, early_id), &mut id, 128, IMAGE_BYTES);

    let before = retained_bytes(seeded_pane(&app, window, early_id));
    assert!(
        before > floor,
        "precondition failed: the pane filled to {before} bytes, at or below the {floor}-byte \
         floor, so there is no stale budget for the pass to reclaim"
    );

    // Panes keep arriving until the process is over its ceiling. The pane
    // above never decodes again.
    let mut crowd: Vec<PaneState> = Vec::new();
    while crate::app::media::process_inline_media_bytes()
        <= crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES
        && crowd.len() < 256
    {
        let pane = pane_with(80, 24);
        decode_into(&pane, &mut id, 8, IMAGE_BYTES);
        crowd.push(pane);
    }
    assert!(
        crate::app::media::process_inline_media_bytes()
            > crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES,
        "precondition failed: the process never went over its ceiling"
    );

    // The production entry point, with the gate closed.
    let sampled = app.__test_sample_pane_retention_now();
    assert!(!sampled, "no subscriber is installed, so the gated sampling must report not-taken");

    let after = retained_bytes(seeded_pane(&app, window, early_id));
    assert!(
        after <= floor.max(IMAGE_BYTES),
        "the pane still holds {after} bytes with the memory log switched off, down from \
         {before}. Reclamation that only runs when someone is watching the log does not run \
         in a shipped build"
    );
    assert!(after > 0, "the pane was trimmed to nothing; it must keep its most recent image");
}

/// The session total returns under the process ceiling as panes accumulate.
///
/// This is the shape behind the incidents this milestone exists to close:
/// growth to 80 GB on macOS and 51 GB on Windows with every individual seam
/// inside its own ceiling. Per-seam bounds do not compose, and the session
/// figure was *reported* and never *asserted* — reporting a leak and
/// preventing one are different claims.
///
/// **Panes are created one at a time, each decoding before the next exists.**
/// That is what produces the divergence: a pane only re-evaluates its budget
/// while *decoding*, so pane 0 is admitted under a nearly-whole-ceiling budget
/// and then goes idle holding it while the pane count climbs underneath. Two
/// earlier versions filled a pre-seeded set of panes instead; each pane then
/// got the same small share, the total never crossed the ceiling at all, and
/// both passed with the convergence deliberately removed. They proved nothing.
///
/// The figure before the pass is asserted to be *over* the ceiling, so the
/// test cannot be satisfied by a session that was never in trouble — that
/// precondition is what the earlier versions silently lacked.
///
/// **Two independent mechanisms hold this bound**, which falsification
/// established and is worth stating: the over-ceiling floor in
/// `trim_inline_images_charged`, which caps every pane that is *decoding*
/// while the process is over budget, and `trim_panes_over_media_ceiling`,
/// which revisits panes that are *idle*. Removing either alone leaves the
/// total bounded — this test fails only when the idle-pane walk goes, because
/// that is the one this fixture's post-decode state depends on. A single test
/// cannot pin both; the decoding-pane cap is pinned separately by the media
/// tests.
#[test]
fn the_session_total_returns_under_the_ceiling_as_panes_accumulate() {
    // The charge counters are process-global and this test holds 24 panes'
    // worth of them. Sibling tests measure the per-pane budget derived from
    // that count, so without this they see a budget shrunk by panes they
    // never created and report a defect that is not there.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );

    const PANES: usize = 24;
    const IMAGE_BYTES: usize = 4 * 1024 * 1024;
    const IMAGES_PER_PANE: usize = 20;

    let ceiling = crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES;
    let mut next_id = 0u64;

    // One window per pane, seeded and filled before the next is created, so
    // each decodes against the live count as it was at that moment.
    for index in 0..PANES {
        let title = format!("pane{index}");
        let window = app.__test_seed_child_window(&[title.as_str()]);
        let pane_id = *app
            .__test_child_pane_ids(window)
            .expect("the seeded window exists")
            .first()
            .expect("the window has a pane");
        let pane = seeded_pane(&app, window, pane_id);
        decode_into(pane, &mut next_id, IMAGES_PER_PANE, IMAGE_BYTES);
    }

    let after_decode = crate::app::media::process_inline_media_bytes();
    assert!(
        after_decode > ceiling,
        "the session must actually get over the ceiling or the reclaim assertion below is \
         satisfied by a session that was never in trouble: {after_decode} !> {ceiling}"
    );

    // The pass the idle-wake path runs. This is what revisits panes that are
    // idle and still holding a budget sized for a smaller session.
    app.sample_pane_retention(Instant::now());
    let after_reclaim = crate::app::media::process_inline_media_bytes();

    assert!(
        after_reclaim <= ceiling,
        "decoded inline media stayed at {after_reclaim} bytes against a {ceiling}-byte \
         process ceiling after the reclaim pass (before it: {after_decode}). Per-pane \
         budgets bounding each pane while the sum runs away is exactly the composition \
         failure behind the multi-gigabyte growth reports"
    );
}
