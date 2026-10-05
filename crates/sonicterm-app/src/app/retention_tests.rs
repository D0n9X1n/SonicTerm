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

/// Build a detached pane whose inline media charges `pool`.
fn pane_in(pool: &Arc<crate::app::media::InlineMediaPool>, cols: u16, rows: u16) -> PaneState {
    PaneState::new_with_media_pool(
        Arc::new(Mutex::new(Parser::new(Grid::new(cols, rows)))),
        None,
        pool,
    )
}

/// An app whose panes stage captures and charge media in private pools, so a
/// capture a test opens is admitted however many captures sibling tests hold.
fn app_with_private_pools() -> App {
    App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_capture_staging_pool(sonicterm_vt::vt::CaptureStagingPool::new())
    .with_inline_media_pool(crate::app::media::InlineMediaPool::new())
}

#[test]
fn measured_inline_media_mixed_transitions_update_existing_charges() {
    // One larger image and several smaller images move bytes/items in opposite directions through real retention.
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    let pane_id = app.__test_seed_tab("media");
    let window = app.main_window_id.unwrap();
    app.reconcile_pane_owners();
    let owner = app.windows[&window].panes[&pane_id].owner.as_ref().unwrap().id();
    let mut previous = None;
    for sizes in [&[4096][..], &[512, 512, 512][..], &[8192][..]] {
        {
            let pane = &app.windows[&window].panes[&pane_id];
            let mut images = pane.inline_images.lock();
            *images = sizes
                .iter()
                .enumerate()
                .map(|(index, size)| sonicterm_render_model::InlineImage {
                    id: index as u64,
                    row: 0,
                    col: 0,
                    width: (*size / 4) as u32,
                    height: 1,
                    bgra: Arc::from(vec![0; *size]),
                })
                .collect();
        }
        let measured = measure_pane(&app.windows[&window].panes[&pane_id]).unwrap();
        if let Some(old) = previous {
            assert!(!measured.inline_media.component_le(old));
            assert!(!old.component_le(measured.inline_media));
        }
        app.__test_charge_pane_owners();
        app.__test_charge_pane_owners();
        let pane = &app.windows[&window].panes[&pane_id];
        assert_eq!(
            pane.charges[&ResourceClass::InlineMediaRetained].committed_amount(),
            measured.inline_media
        );
        let snapshot = app.governor.snapshot(owner).unwrap();
        assert_eq!(
            snapshot.owner_class_bytes[ResourceClass::InlineMediaRetained],
            measured.inline_media.bytes
        );
        assert_eq!(
            snapshot.owner_class_items[ResourceClass::InlineMediaRetained],
            measured.inline_media.items
        );
        previous = Some(measured.inline_media);
    }
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

/// Each language file documents emitted memory fields and excludes the obsolete aggregate name.
#[test]
fn the_wiki_documents_the_fields_the_memory_log_actually_emits() {
    const PAGES: [(&str, &str); 2] = [
        ("English", include_str!("../../../../wiki/Logging.md")),
        ("Chinese", include_str!("../../../../wiki/Logging-zh-CN.md")),
    ];
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

    for (language, page) in PAGES {
        for field in &emitted {
            assert!(
                page.contains(field),
                "{language} Logging omits emitted memory field `{field}`"
            );
        }
        assert!(
            !page.contains("`grid_bytes`"),
            "{language} Logging documents `grid_bytes`, which the log line no longer emits"
        );
    }
}

/// Each language file independently describes every memory field in its table and fenced sample.
#[test]
fn the_seam_table_documents_the_fields_the_memory_log_actually_emits() {
    const PAGES: [(&str, &str); 2] = [
        ("English", include_str!("../../../../wiki/Logging.md")),
        ("Chinese", include_str!("../../../../wiki/Logging-zh-CN.md")),
    ];
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

    for (language, page) in PAGES {
        let table_rows: Vec<&str> =
            page.lines().map(str::trim_start).filter(|line| line.starts_with('|')).collect();
        assert!(!table_rows.is_empty(), "{language} Logging must contain field tables");
        for field in &emitted {
            assert!(
                table_rows.iter().any(|row| row.contains(field)),
                "{language} Logging table omits emitted memory field `{field}`"
            );
        }
        assert!(
            page.split("```").skip(1).step_by(2).any(|block| {
                block.contains("total_bytes=") && emitted.iter().all(|field| block.contains(field))
            }),
            "{language} Logging must contain a fenced pane retention sample with every emitted field"
        );
    }
}

/// Pane retention charges exactly its contributing classes; transport-owned charges never enter the pane total.
#[test]
fn the_coverage_table_agrees_with_the_charge_sites() {
    use enum_map::Enum;
    use sonicterm_types::{ClassCoverage, PaneSeamTerm, ResourceClass};

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

    for index in 0..ResourceClass::COUNT {
        let class = ResourceClass::from_usize(index);
        match class.pane_seam_term() {
            PaneSeamTerm::Contributes => {
                assert!(charged_here.contains(&class), "{class:?} has no pane charge site");
                assert_eq!(class.coverage(), ClassCoverage::Charged);
            }
            PaneSeamTerm::ChargedToAnotherOwnerKind => {
                assert!(!charged_here.contains(&class), "{class:?} was charged to a pane");
                assert_eq!(class, ResourceClass::ReaperWork);
                assert_eq!(class.coverage(), ClassCoverage::Charged);
            }
            PaneSeamTerm::NotChargedInProduction => {
                assert!(!charged_here.contains(&class), "{class:?} has an unrecorded charge site");
                assert_ne!(class.coverage(), ClassCoverage::Charged);
            }
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

/// Pane migration preserves every committed class without consulting a busy parser.
///
/// The route sequence covers main-to-child, child-to-child, and child-to-main while
/// process and window snapshots prove attribution moves without passing through zero.
#[test]
fn pane_owner_moves_transfer_existing_charges_across_every_window_direction() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let pane_id = app.__test_seed_tab("charged");
    let main = app.__test_main_window_id().expect("the synthetic main window exists");
    let first_child = app.__test_seed_child_window(&[]);
    let second_child = app.__test_seed_child_window(&[]);

    let parser = app
        .windows
        .get(&main)
        .and_then(|window| window.panes.get(&pane_id))
        .expect("the seeded pane exists")
        .parser
        .clone();
    {
        let mut parser = parser.lock();
        parser.grid_mut().set_scrollback_limit(256);
        for line in 0..128 {
            parser.advance(format!("charged line {line}\r\n").as_bytes());
        }
        parser.advance(b"\x1b]8;;https://example.com/charged\x07x\x1b]8;;\x07");
    }
    app.__test_force_retention_sample();

    let expected = app.__test_pane_charges(main, pane_id).expect("the pane is charged");
    assert!(
        expected.len() >= 2,
        "the fixture must charge several classes so a partial transfer is observable"
    );
    let process_before = app.__test_governor_snapshot_root().process_amount;
    let _parser_guard = parser.lock();

    for (source, destination) in
        [(main, first_child), (first_child, second_child), (second_child, main)]
    {
        let old_owner = app.__test_pane_owner(source, pane_id).expect("source pane owner");
        let destination_window_owner =
            app.__test_window_owner(destination).expect("destination window owner");

        assert!(app.__test_move_pane_between_windows(source, destination, pane_id));

        let new_owner =
            app.__test_pane_owner(destination, pane_id).expect("destination pane owner");
        assert_ne!(new_owner, old_owner, "a moved pane needs a destination child owner");
        assert!(!app.__test_owner_is_open(old_owner), "the emptied source owner must close");
        assert_eq!(app.__test_pane_charges(destination, pane_id), Some(expected.clone()));
        let new_owner_snapshot = app.__test_owner_snapshot(new_owner).expect("new owner snapshot");
        assert_eq!(new_owner_snapshot.parent, Some(destination_window_owner));
        for (class, amount) in &expected {
            assert_eq!(new_owner_snapshot.owner_class_bytes[*class], amount.bytes);
            assert_eq!(new_owner_snapshot.owner_class_items[*class], amount.items);
        }
        assert_eq!(
            app.__test_owner_snapshot(
                app.__test_window_owner(source).expect("source window owner")
            )
            .expect("source window snapshot")
            .owner_amount,
            ResourceAmount::default(),
            "the source window must stop carrying the moved pane immediately"
        );
        assert_eq!(
            app.__test_governor_snapshot_root().process_amount,
            process_before,
            "owner movement must not change process totals"
        );
    }
}

/// A rejected owner move removes its provisional owner without splitting charges.
///
/// The target admits `GridVisible` but rejects the later `GridHistory` class, so
/// this exercises whole-batch validation rather than a first-token failure.
#[test]
fn rejected_pane_owner_transfer_preserves_source_and_destination_window() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let pane_id = app.__test_seed_tab("charged");
    let main = app.__test_main_window_id().expect("the synthetic main window exists");
    let destination = app.__test_seed_child_window(&[]);
    {
        let pane = app
            .windows
            .get(&main)
            .and_then(|window| window.panes.get(&pane_id))
            .expect("the seeded pane exists");
        let mut parser = pane.parser.lock();
        parser.grid_mut().set_scrollback_limit(256);
        for line in 0..128 {
            parser.advance(format!("charged line {line}\r\n").as_bytes());
        }
    }
    app.__test_force_retention_sample();

    let source_owner = app.__test_pane_owner(main, pane_id).expect("source pane owner");
    let source_charges = app.__test_pane_charges(main, pane_id).expect("source charges");
    assert!(source_charges.contains_key(&ResourceClass::GridVisible));
    assert!(source_charges.contains_key(&ResourceClass::GridHistory));
    let destination_window_owner =
        app.__test_window_owner(destination).expect("destination window owner");
    let process_before = app.__test_governor_snapshot_root().process_amount;
    let restrictive = sonicterm_types::OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map::enum_map! {
            ResourceClass::GridHistory => 0,
            _ => usize::MAX,
        },
        class_items: enum_map::enum_map! { _ => None },
    };
    let provisional_id = app
        .governor
        .create_child(destination_window_owner, sonicterm_types::OwnerKind::AppPane, restrictive)
        .expect("create restrictive provisional owner");
    let provisional = crate::app::OwnerGuard::new(app.governor.clone(), provisional_id);

    let result = {
        let pane = app
            .windows
            .get_mut(&main)
            .and_then(|window| window.panes.get_mut(&pane_id))
            .expect("source pane");
        crate::app::install_transferred_pane_owner(pane, provisional)
    };

    assert!(result.is_err(), "the GridHistory target limit must reject the batch");
    assert_eq!(app.__test_pane_owner(main, pane_id), Some(source_owner));
    assert_eq!(app.__test_pane_charges(main, pane_id), Some(source_charges));
    assert!(!app.__test_owner_is_open(provisional_id), "the empty provisional owner must close");
    assert!(
        app.__test_owner_is_open(destination_window_owner),
        "a pane-owner rejection must not close its existing destination window"
    );
    assert_eq!(app.__test_governor_snapshot_root().process_amount, process_before);
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
    let mut app = app_with_private_pools();
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
    let mut app = app_with_private_pools();
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
    let mut app = app_with_private_pools();
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
    let pool = crate::app::media::InlineMediaPool::new();
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
    let early: Vec<PaneState> = (0..EARLY).map(|_| pane_in(&pool, 80, 24)).collect();
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
    let late: Vec<PaneState> = (0..LATE).map(|_| pane_in(&pool, 80, 24)).collect();
    for pane in &late {
        decode_into(pane, &mut id, 8, IMAGE_BYTES);
    }

    assert!(
        pool.bytes() > crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES,
        "precondition failed: the process is not over its ceiling, so there is no pressure \
         for the pass to relieve"
    );

    let reclaimed = trim_panes_over_pool_ceiling(&pool, early.iter().chain(late.iter()));

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
    let total = pool.bytes();
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
    let pool = crate::app::media::InlineMediaPool::new();
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
    )
    .with_inline_media_pool(pool.clone());
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
    while pool.bytes() <= crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES && crowd.len() < 256 {
        let pane = pane_in(&pool, 80, 24);
        decode_into(&pane, &mut id, 8, IMAGE_BYTES);
        crowd.push(pane);
    }
    assert!(
        pool.bytes() > crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES,
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
/// while the process is over budget, and `trim_panes_over_pool_ceiling`,
/// which revisits panes that are *idle*. Removing either alone leaves the
/// total bounded — this test fails only when the idle-pane walk goes, because
/// that is the one this fixture's post-decode state depends on. A single test
/// cannot pin both; the decoding-pane cap is pinned separately by the media
/// tests.
#[test]
fn the_session_total_returns_under_the_ceiling_as_panes_accumulate() {
    let pool = crate::app::media::InlineMediaPool::new();
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_inline_media_pool(pool.clone());

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

    let after_decode = pool.bytes();
    assert!(
        after_decode > ceiling,
        "the session must actually get over the ceiling or the reclaim assertion below is \
         satisfied by a session that was never in trouble: {after_decode} !> {ceiling}"
    );

    // The pass the idle-wake path runs. This is what revisits panes that are
    // idle and still holding a budget sized for a smaller session.
    app.sample_pane_retention(Instant::now());
    let after_reclaim = pool.bytes();

    assert!(
        after_reclaim <= ceiling,
        "decoded inline media stayed at {after_reclaim} bytes against a {ceiling}-byte \
         process ceiling after the reclaim pass (before it: {after_decode}). Per-pane \
         budgets bounding each pane while the sum runs away is exactly the composition \
         failure behind the multi-gigabyte growth reports"
    );
}

/// Panes the app seeds or splits charge the app's injected media pool, so a
/// test that measures media observes only its own panes.
#[test]
fn seeded_panes_charge_the_apps_media_pool() {
    let pool = crate::app::media::InlineMediaPool::new();
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_inline_media_pool(pool.clone());
    let main_pane = app.__test_seed_tab("main");
    let (reply_pane, _replies) = app.__test_seed_tab_with_reply("reply");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.split_active_pane_in_child(child, sonicterm_cfg::keymap::Direction::Right));
    assert_eq!(pool.live_charges(), 4, "seeded and split panes all charge the injected pool");

    let main = app.__test_main_window_id().expect("the synthetic main window exists");
    for pane_id in [main_pane, reply_pane] {
        let charge = seeded_pane(&app, main, pane_id).inline_media_charge.lock();
        assert!(Arc::ptr_eq(charge.pool(), &pool), "pane {pane_id} charges the app's pool");
    }

    drop(app);
    assert_eq!(pool.live_charges(), 0, "closing the app releases every charge");
}

/// Panes the app seeds or splits stage their captures in the app's injected
/// staging pool, so a test that needs a capture admitted depends only on the
/// captures it opens.
#[test]
fn seeded_panes_stage_captures_in_the_apps_pool() {
    let pool = sonicterm_vt::vt::CaptureStagingPool::new();
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_capture_staging_pool(pool.clone());
    let main_pane = app.__test_seed_tab("main");
    let (reply_pane, _replies) = app.__test_seed_tab_with_reply("reply");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.split_active_pane_in_child(child, sonicterm_cfg::keymap::Direction::Right));

    // An APC introducer with no terminator opens a capture that stays in flight.
    let main = app.__test_main_window_id().expect("the synthetic main window exists");
    let child_panes = app.__test_child_pane_ids(child).expect("the seeded child window exists");
    let panes = [(main, main_pane), (main, reply_pane)]
        .into_iter()
        .chain(child_panes.iter().copied().map(|pane_id| (child, pane_id)));
    for (window, pane_id) in panes {
        seeded_pane(&app, window, pane_id).parser.lock().advance(b"\x1b_GAAAA");
    }
    assert_eq!(pool.live_captures(), 4, "seeded and split panes all stage in the injected pool");

    drop(app);
    assert_eq!(pool.live_captures(), 0, "closing the app releases every capture");
}

/// A pane's media charge keeps its pool as the pane moves between windows in
/// every direction, and a move neither adds nor releases a charge.
#[test]
fn the_media_pool_follows_a_pane_across_window_moves() {
    let pool = crate::app::media::InlineMediaPool::new();
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_inline_media_pool(pool.clone());
    let pane_id = app.__test_seed_tab("moved");
    let main = app.__test_main_window_id().expect("the synthetic main window exists");
    let first_child = app.__test_seed_child_window(&[]);
    let second_child = app.__test_seed_child_window(&[]);

    for (source, destination) in
        [(main, first_child), (first_child, second_child), (second_child, main)]
    {
        assert!(app.__test_move_pane_between_windows(source, destination, pane_id));
        let charge = seeded_pane(&app, destination, pane_id).inline_media_charge.lock();
        assert!(Arc::ptr_eq(charge.pool(), &pool), "the moved pane keeps its pool");
        drop(charge);
        assert_eq!(pool.live_charges(), 1, "a move neither adds nor releases a charge");
    }
}

use crate::app::redraw::redraw_dispatch_tests::{fake_now, set_fake_now, test_base};
use crate::app::redraw::WindowRedrawState;

/// A redraw state natively covered since `since`, untrimmed, unparked and on a live device.
fn covered_since(since: Instant) -> WindowRedrawState {
    let mut redraw = WindowRedrawState::default();
    assert!(!redraw.observe_native_occlusion(true, since), "covering is not a return to view");
    redraw
}

/// The age rule: 29 s of cover is too recent, 30 s passes every rule up to the renderer, and the
/// hook's rule ignores the age entirely. A covered window with a live device is eligible.
#[test]
fn a_window_covered_for_thirty_seconds_becomes_eligible_and_not_before() {
    let base = test_base();
    let redraw = covered_since(base);
    let at = |secs| base + Duration::from_secs(secs);
    assert_eq!(
        trim_eligibility(&redraw, Some(true), at(29), TrimRule::After30s),
        Err(TrimSkip::TooRecent)
    );
    assert_eq!(trim_eligibility(&redraw, Some(true), at(30), TrimRule::After30s), Ok(()));
    assert_eq!(trim_eligibility(&redraw, Some(true), at(0), TrimRule::IgnoreAge), Ok(()));
    // Without a renderer every earlier rule passed, so the renderer is the only reason left.
    assert_eq!(
        trim_eligibility(&redraw, None, at(30), TrimRule::After30s),
        Err(TrimSkip::NoRenderer)
    );
}

/// Each exclusion is reported as its own reason, whatever the age: not covered, backend-only
/// cover, already trimmed, parked, stopped and a refusing device.
#[test]
fn every_exclusion_reports_its_own_reason() {
    let base = test_base();
    let late = base + Duration::from_secs(60);
    let check = |redraw: &WindowRedrawState, accepts| {
        trim_eligibility(redraw, accepts, late, TrimRule::After30s)
    };
    assert_eq!(check(&WindowRedrawState::default(), Some(true)), Err(TrimSkip::NotOccluded));

    // Backend-only occlusion never stamps a covered stretch.
    let mut backend = WindowRedrawState::default();
    backend.backend_occluded = true;
    assert_eq!(check(&backend, Some(true)), Err(TrimSkip::NotOccluded));

    let mut trimmed = covered_since(base);
    trimmed.trimmed = true;
    assert_eq!(check(&trimmed, Some(true)), Err(TrimSkip::AlreadyTrimmed));

    let mut parked = covered_since(base);
    parked.parked = true;
    assert_eq!(check(&parked, Some(true)), Err(TrimSkip::Parked));

    let mut stopped = covered_since(base);
    stopped.stopped_generation = Some(4);
    assert_eq!(check(&stopped, Some(true)), Err(TrimSkip::DeviceUnavailable));

    assert_eq!(check(&covered_since(base), Some(false)), Err(TrimSkip::DeviceUnavailable));
}

/// A repeated cover keeps the first stamp, so a second event cannot delay the trim; returning to
/// view clears the stamp, the trimmed mark and the released figure, so a later stretch starts over.
#[test]
fn the_covered_stretch_starts_once_and_ends_on_visibility() {
    let base = test_base();
    let mut redraw = covered_since(base);
    redraw.observe_native_occlusion(true, base + Duration::from_secs(20));
    assert_eq!(redraw.occluded_since, Some(base));

    redraw.trimmed = true;
    redraw.trim_released_requested_bytes = 4_096;
    assert!(redraw.observe_native_occlusion(false, base + Duration::from_secs(40)));
    assert_eq!(
        (redraw.occluded_since, redraw.trimmed, redraw.trim_released_requested_bytes),
        (None, false, 0)
    );
}

/// The retention pass reaches the scheduler with no `memory` subscriber installed. A window covered
/// through the App's own occlusion path, on the fake dispatch clock, is too recent at 29 s, passes
/// every rule but the absent renderer at the first pass after 30 s, and a window covered for 20 s
/// then shown again is never reached.
#[test]
fn the_retention_pass_schedules_the_trim_after_thirty_seconds_of_cover() {
    let base = test_base();
    set_fake_now(base);
    let mut app = app_with_private_pools();
    app.__test_set_dispatch_clock(fake_now);
    app.__test_seed_tab("covered");
    let main = app.main_window_id.expect("the seeded main window");
    let child = app.__test_seed_child_window(&["shown again"]);
    app.handle_window_occlusion(main, true);
    app.handle_window_occlusion(child, true);
    set_fake_now(base + Duration::from_secs(20));
    app.handle_window_occlusion(child, false);

    let pass = |app: &mut App, secs: u64| {
        app.test_scheduler_trims.clear();
        app.last_retention_sample = None;
        // The return value reports only the debug walk, which no subscriber here enables; the
        // recorded decisions are what show the pass reached the scheduler.
        app.sample_pane_retention(base + Duration::from_secs(secs));
        let mut decisions = app.test_scheduler_trims.clone();
        decisions.sort_by_key(|(window_id, _)| *window_id != main);
        decisions
    };
    assert_eq!(
        pass(&mut app, 29),
        [
            (main, TrimDecision::Skipped(TrimSkip::TooRecent)),
            (child, TrimDecision::Skipped(TrimSkip::NotOccluded)),
        ]
    );
    assert_eq!(
        pass(&mut app, 30),
        [
            (main, TrimDecision::Skipped(TrimSkip::NoRenderer)),
            (child, TrimDecision::Skipped(TrimSkip::NotOccluded)),
        ]
    );
    assert_eq!(app.trim_seq, 0, "nothing was released without a renderer");
}

/// The hook and the scheduler share one body: the hook applies every rule but the age, reports each
/// exclusion as `Skipped` and releases nothing, and runs no retention pass, so the sample clock and
/// every window's panes are exactly as they were.
#[test]
fn the_trim_hook_shares_the_scheduler_rules_and_runs_no_retention_pass() {
    let base = test_base();
    set_fake_now(base);
    let mut app = app_with_private_pools();
    app.__test_set_dispatch_clock(fake_now);
    app.__test_seed_tab("covered");
    let main = app.main_window_id.expect("the seeded main window");
    let sampled_at = base - Duration::from_secs(5);
    app.last_retention_sample = Some(sampled_at);
    let panes_before: Vec<u64> = app.windows[&main].panes.keys().copied().collect();

    assert_eq!(app.__trim_covered_now(main), TrimDecision::Skipped(TrimSkip::NotOccluded));
    assert_eq!(
        app.__trim_covered_now(WindowId::from(7)),
        TrimDecision::Skipped(TrimSkip::NoWindow)
    );
    app.handle_window_occlusion(main, true);
    // Covered this instant: the hook ignores the age, so only the absent renderer remains.
    assert_eq!(app.__trim_covered_now(main), TrimDecision::Skipped(TrimSkip::NoRenderer));
    app.windows.get_mut(&main).expect("main").redraw.trimmed = true;
    assert_eq!(app.__trim_covered_now(main), TrimDecision::Skipped(TrimSkip::AlreadyTrimmed));

    assert_eq!(app.trim_seq, 0);
    assert_eq!(app.last_trim_source, None);
    assert!(app.test_scheduler_trims.is_empty(), "the hook is not the scheduler");
    assert_eq!(app.last_retention_sample, Some(sampled_at), "no retention pass ran");
    let panes_after: Vec<u64> = app.windows[&main].panes.keys().copied().collect();
    assert_eq!(panes_after, panes_before);
    assert_eq!(app.windows.len(), 1, "no window was created or removed");
}

/// Captures the text of every named field of `memory` events, by name; the last event wins.
#[derive(Clone, Default)]
struct MemoryFields(Arc<std::sync::Mutex<Vec<(String, String)>>>);

/// Records every field of one event as text.
#[derive(Default)]
struct FieldTexts(Vec<(String, String)>);

impl tracing::field::Visit for FieldTexts {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_string(), format!("{value:?}")));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for MemoryFields {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "memory" {
            // When: the target differs, the event is not a memory snapshot.
            return;
        }
        let mut texts = FieldTexts::default();
        event.record(&mut texts);
        *self.0.lock().expect("the fields lock") = texts.0;
    }
}

impl MemoryFields {
    /// The last captured event's `name` field, as text.
    fn text(&self, name: &str) -> Option<String> {
        let fields = self.0.lock().expect("the fields lock");
        fields.iter().find(|(field, _)| field == name).map(|(_, value)| value.clone())
    }
}

/// Captures the `renderer_total_bytes` of every INFO `memory snapshot` line.
#[derive(Clone, Default)]
struct RendererTotals(Arc<std::sync::Mutex<Vec<u64>>>);

/// Reads one event's `renderer_total_bytes` field.
#[derive(Default)]
struct RendererTotalField(Option<u64>);

impl tracing::field::Visit for RendererTotalField {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "renderer_total_bytes" {
            // When: the field is the renderer total, it is the figure the pass reported.
            self.0 = Some(value);
        }
    }

    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RendererTotals {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() != "memory" {
            // When: the target differs, the event is not a memory snapshot.
            return;
        }
        let mut total = RendererTotalField::default();
        event.record(&mut total);
        if let Some(bytes) = total.0 {
            self.0.lock().expect("the totals lock").push(bytes);
        }
    }
}

/// Draw one frame of `rows` on window `window_id`'s renderer, outside any frame cadence.
fn draw_window(app: &mut App, window_id: WindowId, rows: &str) -> Result<(), String> {
    let mut grid = Grid::new(40, 8);
    for (row, line) in (0u16..).zip(rows.lines()) {
        grid.goto(row, 0);
        for character in line.chars() {
            grid.put_char(
                character,
                sonicterm_grid::grid::Color::Default,
                sonicterm_grid::grid::Color::Default,
                sonicterm_grid::grid::CellFlags::empty(),
            );
        }
    }
    let renderer = app.__test_window_renderer_mut(window_id).ok_or("the window has a renderer")?;
    let frames = renderer.successful_frame_count();
    let mut panes = [sonicterm_render_model::PaneRender {
        id: 1,
        rect_px: sonicterm_render_model::PixelRect { x: 0, y: 0, w: 400, h: 200 },
        grid: &mut grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: sonicterm_render_model::CursorStyle::BlockSteady,
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Vec::new(),
    }];
    renderer
        .render(
            &mut panes,
            &sonicterm_cfg::theme::Theme::default(),
            false,
            None,
            None,
            &sonicterm_ui::tabs::TabBar::new(),
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .map_err(|error| error.to_string())?;
    if renderer.successful_frame_count() == frames {
        // When: no frame presented, the renderer holds nothing a trim could release.
        return Err(String::from("the frame did not present"));
    }
    Ok(())
}

/// A renderer for a live window, presenting through wgpu (WARP on the hosted runner).
fn live_renderer(
    event_loop: &winit::event_loop::ActiveEventLoop,
    visible: bool,
    role: &'static str,
) -> Result<(Arc<winit::window::Window>, sonicterm_gpu::core::GpuRenderer), String> {
    use sonicterm_cfg::config::{ScrollbarMode, SoftwareRenderMode};
    use sonicterm_gpu::core::{GlyphAtlasStart, GpuRenderer, RendererSettings, SurfaceAppearance};
    let window = Arc::new(
        event_loop
            .create_window(
                winit::window::Window::default_attributes()
                    .with_visible(visible)
                    .with_inner_size(winit::dpi::PhysicalSize::new(400, 200)),
            )
            .map_err(|error| error.to_string())?,
    );
    let font_dirs =
        [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    let settings = RendererSettings {
        font_family: "Rec Mono St.Helens",
        font_dirs: &font_dirs,
        font_size: 16.0,
        line_height_mult: 1.0,
        font_weight_scale: 1.0,
        subpixel_aa: Default::default(),
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: 1.0,
            scrollbar: ScrollbarMode::Never,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Off,
        },
        role,
        glyph_atlas_start: GlyphAtlasStart::Normal,
    };
    let renderer = GpuRenderer::new(
        window.clone(),
        event_loop,
        &sonicterm_cfg::theme::Theme::default(),
        settings,
    )
    .map_err(|error| error.to_string())?;
    Ok((window, renderer))
}

/// Run one retention pass at `secs` after `base` and return its scheduler decision for `window_id`.
fn pass_at(app: &mut App, base: Instant, secs: u64, window_id: WindowId) -> Option<TrimDecision> {
    app.test_scheduler_trims.clear();
    app.last_retention_sample = None;
    app.sample_pane_retention(base + Duration::from_secs(secs));
    app.test_scheduler_trims
        .iter()
        .find(|(decided, _)| *decided == window_id)
        .map(|(_, decision)| *decision)
}

/// Every committed charge of `pane_id`, summed in bytes.
fn charged_bytes(app: &App, window_id: WindowId, pane_id: u64) -> usize {
    app.__test_pane_charges(window_id, pane_id)
        .map(|charges| charges.values().map(|amount| amount.bytes).sum())
        .unwrap_or(0)
}

/// The live scheduler on a real renderer: nothing is trimmed at 29 s of cover; the first pass at
/// 30 s trims once, and its INFO snapshot reports lower renderer retention than the snapshot just
/// before it while the pane's charges stay the same; a later pass in the same stretch skips it.
/// After the window is shown, drawn and covered again, a pass with no subscriber at all trims it
/// a second time. After the first trim the App's snapshot and a tagged checkpoint line both carry
/// the trim. A warm spare is never trimmed, by the hook or by the pass.
#[cfg_attr(not(windows), allow(dead_code))]
fn run_live_scheduler(event_loop: &winit::event_loop::ActiveEventLoop) -> Result<(), String> {
    use tracing_subscriber::layer::SubscriberExt;
    let base = test_base();
    set_fake_now(base);
    let mut app = app_with_private_pools();
    app.__test_set_dispatch_clock(fake_now);
    let pane_id = app.__test_seed_tab("covered");
    let main = app.main_window_id.ok_or("the seeded main window")?;
    let (window, renderer) = live_renderer(event_loop, true, "live-scheduler")?;
    if !app.__test_attach_window_renderer(main, window, renderer) {
        return Err(String::from("the renderer attaches"));
    }
    draw_window(&mut app, main, "first row of text\nsecond row of text\nthird row")?;
    if tracing::enabled!(target: "memory", tracing::Level::INFO) {
        return Err(String::from("precondition: no memory subscriber outside a capture"));
    }
    app.handle_window_occlusion(main, true);

    let early = pass_at(&mut app, base, 29, main);
    if early != Some(TrimDecision::Skipped(TrimSkip::TooRecent)) {
        return Err(format!("29 s of cover is too recent: {early:?}"));
    }
    let charges_before = charged_bytes(&app, main, pane_id);
    let totals = RendererTotals::default();
    let subscriber = tracing_subscriber::Registry::default()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(totals.clone());
    let before_bytes = app.build_memory_snapshot().renderer_bytes() as u64;
    let eligible = sonicterm_logging::test_capture::with_default(subscriber, || {
        pass_at(&mut app, base, 30, main)
    });
    if eligible != Some(TrimDecision::Trimmed { trim_seq: 1 }) {
        return Err(format!("the first pass at 30 s trims once: {eligible:?}"));
    }
    let reported = totals.0.lock().expect("the totals lock").clone();
    if reported.len() != 1 || reported[0] >= before_bytes {
        return Err(format!(
            "the pass's snapshot reports lower renderer retention: {before_bytes} -> {reported:?}"
        ));
    }
    if charged_bytes(&app, main, pane_id) != charges_before {
        return Err(String::from("the trim leaves the pane's charges unchanged"));
    }
    let renderer = app.__test_window_renderer_mut(main).ok_or("the renderer")?;
    if !renderer.__frame_texture_trimmed() {
        return Err(String::from("the renderer's frame texture was trimmed"));
    }
    if (app.trim_seq, app.last_trim_source) != (1, Some(TrimSource::Scheduler)) {
        return Err(format!("one scheduler trim: {:?}", (app.trim_seq, app.last_trim_source)));
    }
    // The window's trim state reaches the App's own snapshot: its renderer summary says trimmed and
    // carries the released request size, and a tagged checkpoint line names the scheduler's trim.
    let snapshot = app.build_memory_snapshot();
    let main_label = format!("{main:?}");
    let summary = snapshot
        .renderers
        .iter()
        .find(|summary| summary.label == main_label)
        .ok_or("the snapshot reports the main renderer")?;
    if !summary.trimmed || summary.gpu_released_requested_bytes == 0 {
        return Err(format!(
            "the summary carries the trim: trimmed={} released={}",
            summary.trimmed, summary.gpu_released_requested_bytes
        ));
    }
    let checkpoint = MemoryFields::default();
    let subscriber = tracing_subscriber::Registry::default()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(checkpoint.clone());
    sonicterm_logging::test_capture::with_default(subscriber, || {
        app.__perf_checkpoint_memory(0, "covered", 1);
    });
    let tags =
        (checkpoint.text("trimmed"), checkpoint.text("trim_source"), checkpoint.text("trim_seq"));
    let expected = (Some("true".to_string()), Some("scheduler".to_string()), Some("1".to_string()));
    if tags != expected {
        return Err(format!("the checkpoint line carries the scheduler's trim: {tags:?}"));
    }
    let breakdown = checkpoint.text("renderers").unwrap_or_default();
    let released = format!(
        "renderer_trimmed=true renderer_gpu_released_requested_bytes={}",
        summary.gpu_released_requested_bytes
    );
    if !breakdown.contains(&released) {
        return Err(format!("the checkpoint's renderer entry carries the trim: {breakdown}"));
    }
    let later = pass_at(&mut app, base, 60, main);
    if later != Some(TrimDecision::Skipped(TrimSkip::AlreadyTrimmed)) || app.trim_seq != 1 {
        return Err(format!("one trim per covered stretch: {later:?}, trim_seq {}", app.trim_seq));
    }

    // Shown, drawn and covered again, then a pass with no subscriber installed: still trimmed.
    set_fake_now(base + Duration::from_secs(70));
    app.handle_window_occlusion(main, false);
    draw_window(&mut app, main, "regrown row\nanother regrown row")?;
    app.handle_window_occlusion(main, true);
    let unlogged = pass_at(&mut app, base, 100, main);
    if unlogged != Some(TrimDecision::Trimmed { trim_seq: 2 }) {
        return Err(format!("the pass trims with logging off: {unlogged:?}"));
    }

    // A warm spare holds a live renderer but is never shown: neither the hook nor a pass trims it.
    let (spare_window, spare_renderer) = live_renderer(event_loop, false, "live-scheduler-spare")?;
    let spare = spare_window.id();
    let spare_before = spare_renderer.retained_amounts();
    app.warm_window_pool.push(crate::app::WarmWindow {
        window: spare_window,
        renderer: spare_renderer,
        created_at: Instant::now(),
    });
    if app.__trim_covered_now(spare) != TrimDecision::Skipped(TrimSkip::Warm) {
        return Err(String::from("the hook skips a warm spare"));
    }
    let _ = pass_at(&mut app, base, 200, main);
    if app.test_scheduler_trims.iter().any(|(decided, _)| *decided == spare) {
        return Err(String::from("the pass never visits a warm spare"));
    }
    let spare_after = &app.warm_window_pool[0].renderer;
    if spare_after.retained_amounts() != spare_before || spare_after.__frame_texture_trimmed() {
        return Err(String::from("the warm spare's renderer is untouched"));
    }
    Ok(())
}

/// The live scheduler and the warm-spare exclusion on a real renderer, in an isolated process with
/// its own event loop.
#[cfg(windows)]
#[test]
fn the_live_scheduler_trims_a_covered_renderer_once_and_never_a_warm_spare() {
    use crate::app::pty_test_support::isolated;
    use winit::{
        application::ApplicationHandler,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    if isolated() {
        return;
    }
    struct Probe {
        outcome: Option<Result<(), String>>,
    }
    impl ApplicationHandler<crate::app::UserEvent> for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.outcome = Some(run_live_scheduler(event_loop));
            event_loop.exit();
        }
        fn window_event(
            &mut self,
            _: &ActiveEventLoop,
            _: winit::window::WindowId,
            _: winit::event::WindowEvent,
        ) {
        }
    }
    let event_loop = EventLoop::<crate::app::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .expect("Windows event loop");
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).expect("live scheduler event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
