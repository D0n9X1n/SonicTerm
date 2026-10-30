//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::super::retention_tests::*;
use super::*;

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
                    kitty: None,
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
                assert!(
                    matches!(class, ResourceClass::ReaperWork | ResourceClass::AccessibilityTree),
                    "{class:?} is charged to a non-pane owner without being recorded here"
                );
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
