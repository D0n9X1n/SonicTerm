//! Status selection, IME, resource and interval projection contracts.

use super::super::status_line_tests::*;
use super::*;

/// Each presenter and adapter has its own health label: GDI only where the degrade policy runs on
/// Windows, a CPU adapter also when `software_render_mode = "off"` declines to degrade, forced
/// software on real hardware; and GPU recovery shows only for a window waiting on it.
#[test]
fn health_classifies_each_presenter_and_recovery_per_window() {
    use RenderHealth::{Hardware, SoftwareAdapter, SoftwareForced, SoftwareGdi};
    assert_eq!(classify_render(true, true, true), SoftwareGdi, "Windows CPU adapter, auto");
    assert_eq!(classify_render(true, false, true), SoftwareGdi, "Windows, forced");
    assert_eq!(
        classify_render(true, true, false),
        SoftwareAdapter,
        "Windows CPU adapter, mode off"
    );
    assert_eq!(classify_render(false, true, false), SoftwareAdapter, "CPU adapter, mode off");
    assert_eq!(
        classify_render(false, true, true),
        SoftwareAdapter,
        "CPU adapter, auto, off Windows"
    );
    assert_eq!(classify_render(false, false, true), SoftwareForced, "hardware, forced");
    assert_eq!(classify_render(false, false, false), Hardware);
    assert!(waits_on_gpu_recovery(true, true, false), "stopped window during recovery");
    assert!(waits_on_gpu_recovery(true, false, false), "device refuses work during recovery");
    assert!(!waits_on_gpu_recovery(true, false, true), "a healthy window is not waiting");
    assert!(!waits_on_gpu_recovery(false, true, false), "no recovery in progress");
}

/// A wall clock that ran ahead of the monotonic clock by more than one interval means the machine
/// slept: the history re-aligns and a sample is due at once. Ordinary drift is not a jump.
#[test]
fn wall_clock_jump_after_sleep_resamples() {
    let at = Instant::now();
    let later = at + Duration::from_secs(2);
    assert!(!wall_clock_jumped((at, 1_000_000), later, 1_002_000, 2_000), "in step");
    assert!(
        !wall_clock_jumped((at, 1_000_000), later, 1_003_500, 2_000),
        "drift under an interval"
    );
    assert!(wall_clock_jumped((at, 1_000_000), later, 1_602_000, 2_000), "ten minutes asleep");
    assert!(
        !wall_clock_jumped((at, 1_000_000), later, 900_000, 2_000),
        "a clock set back is not a sleep"
    );
}

/// With the sparkline on and a window shown, the next wake is never later than the next bucket
/// rollover; with it off, nothing wakes without an eligible window.
#[test]
fn sparkline_rollover_bounds_the_wake() {
    let mut app = app_with_strip(true);
    app.config.status_line.preset = sonicterm_cfg::status_line::StatusPreset::Diagnostics;
    assert_eq!(app.sync_status_line(Instant::now()), None, "no eligible window, no rollover wake");
    assert!(app.status_line.resolved.mem.sparkline);
    assert_eq!(app.status_line.mem.history, [None; sonicterm_ui::status_line::SPARK_CELLS]);
}

/// A window selection is counted once per identity: repeating the frame pass or writing rows
/// outside it never recounts, a moved endpoint does, and another pane's selection counts nothing.
#[test]
fn selection_is_counted_once_per_identity() {
    let mut grid = three_rows();
    let selection =
        Selection { start: (0, 0), end: (1, 2), anchored: true, ..Selection::new(0, 0) }
            .with_content_state(7, grid.content_seq(), false, 0);
    let mut status = SelStatus::default();
    let cells_read = refresh_sel_status(&mut status, Some(&selection), None, 7, &grid);
    assert_eq!(cells_read, Some(13), "`abc` and `def` padded to the 10-column first row, then 3");
    let extent = status.counted.unwrap().1;
    assert_eq!((extent.lines, extent.chars, extent.capped), (2, 7, false), "`abc\\ndef`");
    assert!(refresh_sel_status(&mut status, Some(&selection), None, 7, &grid).is_none(), "cached");
    grid.goto(2, 0);
    grid.put_char(
        'X',
        sonicterm_grid::grid::Color::Default,
        sonicterm_grid::grid::Color::Default,
        Default::default(),
    );
    assert!(
        refresh_sel_status(&mut status, Some(&selection), None, 7, &grid).is_none(),
        "output below"
    );
    let moved = Selection { end: (1, 1), ..selection };
    assert!(
        refresh_sel_status(&mut status, Some(&moved), None, 7, &grid).is_some(),
        "new endpoint"
    );
    assert!(refresh_sel_status(&mut status, Some(&moved), None, 8, &grid).is_none());
    assert!(status.counted.is_none(), "a selection of another pane is not this grid's");
}

/// Copy mode's range has no content check of its own, so rewriting a row inside a stationary
/// range changes its stamp and recounts it.
#[test]
fn stationary_copy_mode_range_is_recounted_when_its_rows_change() {
    let mut grid = three_rows();
    let mut copy_mode = CopyModeState::new_at((0, 0));
    copy_mode.start_select();
    copy_mode.cursor = (2, 1);
    let mut status = SelStatus::default();
    assert!(refresh_sel_status(&mut status, None, Some(&copy_mode), 3, &grid).is_some());
    assert!(refresh_sel_status(&mut status, None, Some(&copy_mode), 3, &grid).is_none());
    grid.goto(1, 0);
    grid.put_char(
        'X',
        sonicterm_grid::grid::Color::Default,
        sonicterm_grid::grid::Color::Default,
        Default::default(),
    );
    assert!(
        refresh_sel_status(&mut status, None, Some(&copy_mode), 3, &grid).is_some(),
        "rewritten row"
    );
    assert_eq!(status.counted.unwrap().1.chars, 7, "`abc\\nXef`");
}

/// A copy result shows for `copied_secs` and wakes once at its end; then the counted extent of a
/// live selection returns, and a cleared selection shows nothing.
#[test]
fn copy_result_shows_until_its_deadline() {
    let grid = three_rows();
    let selection =
        Selection { start: (0, 0), end: (0, 2), anchored: true, ..Selection::new(0, 0) };
    let mut status = SelStatus::default();
    let _ = refresh_sel_status(&mut status, Some(&selection), None, 1, &grid);
    let now = Instant::now();
    let copied_for = Duration::from_secs(2);
    status.last_copy = Some((now, false));
    assert_eq!(
        sel_input(&mut status, true, now, copied_for),
        (Some(SelInput::CopyFailed), Some(now + copied_for))
    );
    let after = now + copied_for;
    assert_eq!(
        sel_input(&mut status, true, after, copied_for),
        (Some(SelInput::Extent { lines: 1, chars: 3 }), None)
    );
    assert_eq!(status.last_copy, None, "the result is consumed at its deadline");
    status.last_copy = Some((after, true));
    assert_eq!(sel_input(&mut status, false, after, copied_for).0, Some(SelInput::Copied));
    assert_eq!(sel_input(&mut status, false, after + copied_for, copied_for), (None, None));
}

/// `copy_to_clipboard` records whether its write reached the clipboard, for the window it copied.
#[test]
fn requested_copy_records_its_result() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    assert!(app.__test_advance_pane_parser(pane_id, b"hello"));
    let selection = {
        let parser = app.main().unwrap().panes[&pane_id].parser.lock();
        let grid = parser.grid();
        Selection { start: (0, 0), end: (0, 4), anchored: true, ..Selection::new(0, 0) }
            .with_content_state(
                pane_id,
                grid.content_seq(),
                grid.is_alt(),
                grid.scrollback_evicted(),
            )
            .with_content_fingerprint(grid)
    };
    app.main_mut().unwrap().selection = Some(selection);
    app.__test_set_memory_clipboard("old");
    app.copy_selection_for_kind(crate::app::FrontmostKind::Main);
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("hello"));
    assert_eq!(app.main().unwrap().sel_status.last_copy.map(|(_, copied)| copied), Some(true));
    app.__test_set_clipboard_write_failure(true);
    app.copy_selection_for_kind(crate::app::FrontmostKind::Main);
    assert_eq!(app.main().unwrap().sel_status.last_copy.map(|(_, copied)| copied), Some(false));
}

/// The raw-paste warning needs all of: `when_off`, bracketed paste off, the primary screen, and a
/// confirmed prompt. `Unknown` (no OSC 133) and `Running` never warn.
#[test]
fn paste_warns_only_at_a_confirmed_prompt_without_bracketed_paste() {
    use sonicterm_cfg::status_line::PasteWarning;
    let plain = StatusPaneFacts::default();
    assert!(paste_is_raw(PasteWarning::WhenOff, plain, CommandPhase::Prompt));
    assert!(!paste_is_raw(PasteWarning::Never, plain, CommandPhase::Prompt));
    assert!(!paste_is_raw(PasteWarning::WhenOff, plain, CommandPhase::Unknown));
    let running = CommandPhase::Running { started_at: Instant::now() };
    assert!(!paste_is_raw(PasteWarning::WhenOff, plain, running));
    let bracketed = StatusPaneFacts { bracketed_paste: true, ..plain };
    assert!(!paste_is_raw(PasteWarning::WhenOff, bracketed, CommandPhase::Prompt));
    let alternate = StatusPaneFacts { alt_screen: true, ..plain };
    assert!(!paste_is_raw(PasteWarning::WhenOff, alternate, CommandPhase::Prompt));
}

/// A dropped commit shows for `hold` and wakes once at its end; a composition shows only when
/// its commit goes to search or the palette, never for an ordinary composition into the pane.
#[test]
fn ime_shows_a_dropped_commit_then_only_non_pane_compositions() {
    let now = Instant::now();
    let hold = Duration::from_secs(8);
    let mut discarded_at = Some(now);
    assert_eq!(
        ime_input(&mut discarded_at, false, ImeRoute::CopyMode, now, hold),
        (Some(ImeInput::DiscardedInCopyMode), Some(now + hold))
    );
    assert_eq!(ime_input(&mut discarded_at, false, ImeRoute::Pane, now + hold, hold), (None, None));
    assert_eq!(discarded_at, None, "the warning is consumed at its deadline");
    let composing = |route| ime_input(&mut None, true, route, now, hold).0;
    assert_eq!(composing(ImeRoute::Search), Some(ImeInput::ToSearch));
    assert_eq!(composing(ImeRoute::Palette), Some(ImeInput::ToPalette));
    assert_eq!(composing(ImeRoute::Pane), None);
    assert_eq!(ime_input(&mut None, false, ImeRoute::Search, now, hold).0, None, "not composing");
}

/// The shared route sends a commit in copy mode nowhere and records the drop; without copy mode
/// the same commit reaches the pane and records nothing.
#[test]
fn copy_mode_commit_is_dropped_and_recorded() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    let main_id = app.main_window_id.unwrap();
    app.__test_enable_pty_write_log();
    app.main_mut().unwrap().copy_mode = Some(CopyModeState::new_at((0, 0)));
    assert_eq!(app.ime_route(main_id), Some(ImeRoute::CopyMode));
    app.handle_window_ime(main_id, winit::event::Ime::Commit("你好".into()));
    assert_eq!(app.__test_pty_write_log(), Vec::new(), "copy mode drops the commit");
    assert!(app.main().unwrap().ime_discarded_at.is_some());
    let window = app.main_mut().unwrap();
    window.copy_mode = None;
    window.ime_discarded_at = None;
    assert_eq!(app.ime_route(main_id), Some(ImeRoute::Pane));
    app.handle_window_ime(main_id, winit::event::Ime::Commit("你好".into()));
    assert_eq!(app.__test_pty_write_log(), vec![(pane_id, "你好".as_bytes().to_vec())]);
    assert_eq!(app.main().unwrap().ime_discarded_at, None);
}

/// The first sighting of a pane is its size baseline, so spawn and first layout show nothing; a
/// later resize shows the focused pane's grid until the hold ends, and a focus change to another
/// pane shows nothing.
#[test]
fn size_shows_only_after_a_resize_of_the_focused_pane() {
    let mut seen = HashMap::new();
    let first = StatusPaneFacts {
        pane_id: 1,
        size_generation: 4,
        cols: 80,
        rows: 24,
        ..Default::default()
    };
    assert!(!size_changed(&mut seen, first), "the baseline");
    assert!(!size_changed(&mut seen, first), "unchanged");
    let other = StatusPaneFacts {
        pane_id: 2,
        size_generation: 9,
        cols: 40,
        rows: 24,
        ..Default::default()
    };
    assert!(!size_changed(&mut seen, other), "a newly focused pane is only a baseline");
    let resized = StatusPaneFacts { size_generation: 5, cols: 120, rows: 40, ..first };
    assert!(size_changed(&mut seen, resized));

    let window_id = WindowId::dummy();
    let now = Instant::now();
    let until = now + Duration::from_millis(1500);
    let mut shown = HashMap::from([(window_id, (1, until))]);
    assert_eq!(size_input(&mut shown, window_id, resized, now), (Some((120, 40)), Some(until)));
    assert_eq!(size_input(&mut shown, window_id, other, now), (None, Some(until)), "another pane");
    assert_eq!(size_input(&mut shown, window_id, resized, until), (None, None), "the hold ended");
    assert!(shown.is_empty());
}

/// `History: trimmed` counts budget trims past the larger of a click's acknowledgement and the
/// last scrollback clear; with neither past the count it hides.
#[test]
fn history_trims_are_acknowledged_by_click_or_clear() {
    let facts = StatusPaneFacts { budget_trimmed: 640, ..Default::default() };
    assert_eq!(history_input(facts, 0), Some(640));
    assert_eq!(history_input(facts, 640), None, "acknowledged by a click");
    assert_eq!(history_input(facts, 576), Some(64), "a later trim shows again");
    let cleared = StatusPaneFacts { trim_cleared: 640, ..facts };
    assert_eq!(history_input(cleared, 0), None, "a scrollback clear acknowledges it");
    assert_eq!(history_input(StatusPaneFacts::default(), 0), None, "nothing trimmed");
}

/// A write records its byte count, a failed native write records `Failed`, a refusal leaves the
/// clipboard alone, attempts accumulate within the hold, and the click goes to the source tab.
#[test]
fn clip_records_outcomes_and_goes_to_the_source_tab() {
    use crate::app::spawn_pane::{ClipAttempts, Osc52Refusal};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let source = app.__test_seed_tab("source");
    app.__test_seed_tab("other");
    let main_id = app.main_window_id.unwrap();
    app.__test_set_memory_clipboard("old");
    let accepted = ClipAttempts { total: 1, accepted: 1, refused: 0, latest: None };
    let now = Instant::now();
    let written = app.handle_clipboard_write("hello".into());
    app.note_clip_write(source, 5, written);
    app.note_clip_attempts(source, accepted, now);
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("hello"));
    assert_eq!(app.status_line.clip.unwrap().outcome, ClipOutcome::Written(5));

    let read = ClipAttempts { total: 3, accepted: 0, refused: 3, latest: Some(Osc52Refusal::Read) };
    app.note_clip_attempts(source, read, now);
    let record = app.status_line.clip.unwrap();
    assert_eq!((record.outcome, record.attempts), (ClipOutcome::ReadRefused, 4));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("hello"), "a refusal writes nothing");

    app.__test_set_clipboard_write_failure(true);
    let written = app.handle_clipboard_write("lost".into());
    app.note_clip_write(source, 4, written);
    app.note_clip_attempts(source, accepted, now + Duration::from_secs(60));
    let record = app.status_line.clip.unwrap();
    assert_eq!((record.outcome, record.attempts), (ClipOutcome::Failed, 1), "a new hold");

    app.activate_main_tab(1);
    app.activate_clip_source();
    assert_eq!(app.windows[&main_id].tabs.active_index(), 0, "the source tab");
}

/// Each progress read moves the deadline as `held` requires: idle disarms, an old write shows and
/// reads again on its next whole second, a young one reads again at its threshold, a race retries
/// at 50 ms three times then waits a threshold, and a stopped writer reports loss only on failure.
#[test]
fn held_step_judges_each_progress_read() {
    use sonicterm_io::pty::PtyInputProgress;
    let now = Instant::now() + Duration::from_secs(60);
    let after = Duration::from_millis(1500);
    assert_eq!(held_step(PtyInputProgress::Idle, 0, now, after), HeldStep::Disarm);
    let old = now - Duration::from_millis(4200);
    let oldest =
        |admitted_at, on_reply| PtyInputProgress::Oldest { ordinal: 7, admitted_at, on_reply };
    assert_eq!(
        held_step(oldest(old, true), 2, now, after),
        HeldStep::Show { admitted_at: old, on_reply: true, due: old + Duration::from_secs(5) }
    );
    let young = now - Duration::from_millis(400);
    assert_eq!(held_step(oldest(young, false), 0, now, after), HeldStep::Rearm(young + after));
    for raced in 0..3 {
        assert_eq!(
            held_step(PtyInputProgress::Raced, raced, now, after),
            HeldStep::Raced(now + Duration::from_millis(50))
        );
    }
    assert_eq!(held_step(PtyInputProgress::Raced, 3, now, after), HeldStep::Raced(now + after));
    let stopped = |failed, lost| PtyInputProgress::Terminal { failed, lost };
    assert_eq!(held_step(stopped(true, 3), 0, now, after), HeldStep::Terminal(Some(3)));
    assert_eq!(held_step(stopped(true, 0), 0, now, after), HeldStep::Terminal(None));
    assert_eq!(held_step(stopped(false, 3), 0, now, after), HeldStep::Terminal(None));
    // No step re-arms at or before `now`, even at an exact whole-second age.
    let exact = now - Duration::from_secs(2);
    for step in [
        held_step(oldest(exact, false), 0, now, after),
        held_step(oldest(young, false), 0, now, after),
        held_step(PtyInputProgress::Raced, 0, now, after),
    ] {
        let due = match step {
            HeldStep::Show { due, .. } | HeldStep::Rearm(due) | HeldStep::Raced(due) => due,
            other => panic!("{other:?}"),
        };
        assert!(due > now, "{step:?}");
    }
}

/// `write failed` replaces `held`; `held` outranks any other record and shows the oldest pane's
/// write with its own age; with nothing held, the record shows.
#[test]
fn input_display_prefers_failure_then_held_then_records() {
    let now = Instant::now() + Duration::from_secs(60);
    let record = |state| Some(InputRecord { state, at: now });
    let busy = InputState::NotSent(NotSentReason::Busy);
    let held = [(now - Duration::from_secs(2), false, 0), (now - Duration::from_secs(9), true, 5)];
    assert_eq!(
        input_state(record(InputState::WriteFailed(3)), held.into_iter(), now),
        Some(InputState::WriteFailed(3))
    );
    assert_eq!(
        input_state(record(busy), held.into_iter(), now),
        Some(InputState::Held { secs: 9, on_reply: true, queued_bytes: 5 })
    );
    assert_eq!(input_state(record(busy), std::iter::empty(), now), Some(busy));
    assert_eq!(input_state(None, std::iter::empty(), now), None);
}

/// The rejection event's error text maps to the reason word the strip shows.
#[test]
fn rejection_text_maps_to_its_reason() {
    use sonicterm_io::pty::PtyInputError;
    let reason = |error: PtyInputError| not_sent_reason(&error.to_string());
    assert_eq!(reason(PtyInputError::QueueFull(vec![1])), NotSentReason::Busy);
    assert_eq!(reason(PtyInputError::MessageTooLarge(vec![1])), NotSentReason::TooLarge);
    assert_eq!(reason(PtyInputError::WriterDisconnected(vec![1])), NotSentReason::Closed);
}

/// A successful paste records nothing; a READONLY paste sends nothing and records `Blocked`; an
/// encoding refusal records `paste refused` with its counts; the record ends after `hold_secs`.
#[test]
fn paste_outcomes_are_recorded_for_the_window() {
    let mut app = app_with_strip(true);
    let pane_id = app.__test_seed_tab("main");
    let main_id = app.main_window_id.unwrap();
    let _ = app.sync_status_line(Instant::now());
    app.__test_enable_pty_write_log();
    app.__test_set_memory_clipboard("ls");
    app.paste_clipboard_for_kind(crate::app::FrontmostKind::Main);
    assert_eq!(app.__test_pty_write_log().len(), 1);
    assert_eq!(app.status_line.input.get(&main_id), None, "a successful paste records nothing");

    app.main_mut().unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    app.paste_clipboard_for_kind(crate::app::FrontmostKind::Main);
    assert_eq!(app.__test_pty_write_log().len(), 1, "READONLY sends nothing");
    assert_eq!(app.status_line.input[&main_id].state, InputState::Blocked(1));

    app.main_mut().unwrap().copy_mode = None;
    app.paste_file_paths_for_kind(
        crate::app::FrontmostKind::Main,
        [std::path::PathBuf::from("/tmp/a\u{7}b")],
    );
    let record = app.status_line.input[&main_id];
    assert_eq!(record.state, InputState::PasteRefused { refused: 1, attempted: 1 });
    assert!(app.__test_pty_write_log().iter().all(|(pane, _)| *pane == pane_id));

    let hold = Duration::from_secs(app.status_line.resolved.input.hold_secs);
    let _ = app.sync_status_line(record.at + hold);
    assert_eq!(app.status_line.input.get(&main_id), None, "the hold ended");
}

/// An accepted write arms one deadline per pane, only while a shown strip lists `input`, and a
/// pane without a writer drops its deadline at the next sync.
#[test]
fn accepted_writes_arm_one_deadline_per_pane() {
    let mut app = app_with_strip(true);
    let pane_id = app.__test_seed_tab("main");
    let now = Instant::now();
    let _ = app.sync_status_line(now);
    app.arm_held_input(pane_id);
    let due = app.status_line.held[&pane_id].due;
    assert!(due > now);
    app.arm_held_input(pane_id);
    assert_eq!(app.status_line.held[&pane_id].due, due, "an armed deadline is kept");
    // The seeded pane has no PTY, so nothing it was sent can be outstanding.
    let _ = app.sync_status_line(now);
    assert!(app.status_line.held.is_empty(), "a pane without a writer has nothing outstanding");

    let mut off = app_with_strip(false);
    let pane_id = off.__test_seed_tab("main");
    let _ = off.sync_status_line(now);
    off.arm_held_input(pane_id);
    assert!(off.status_line.held.is_empty(), "nothing is armed while no shown strip lists `input`");
}

/// A count below `min_count` hides the segment; a lower bound obeys it too; `count capped` has
/// no figure to compare, so it always shows.
#[test]
fn glyphs_input_hides_below_min_count() {
    use sonicterm_gpu::missing_glyphs::{MissingGlyphValue, MissingGlyphs};
    let missing = |value| MissingGlyphs { value, sample: Vec::new() };
    assert_eq!(glyphs_input(&missing(MissingGlyphValue::Exact(0)), 1), None);
    assert_eq!(glyphs_input(&missing(MissingGlyphValue::Exact(3)), 1), Some(GlyphsInput::Exact(3)));
    assert_eq!(glyphs_input(&missing(MissingGlyphValue::Exact(3)), 4), None);
    assert_eq!(
        glyphs_input(&missing(MissingGlyphValue::AtLeast(1000)), 5),
        Some(GlyphsInput::AtLeast(1000))
    );
    assert_eq!(glyphs_input(&missing(MissingGlyphValue::Capped), 1000), Some(GlyphsInput::Capped));
}

/// The click lists the window's codepoints in one notification and writes no clipboard; a
/// window with nothing counted shows nothing.
#[test]
fn glyphs_click_lists_codepoints_without_touching_the_clipboard() {
    let mut app = app_with_strip(true);
    app.__test_seed_tab("main");
    let main_id = app.main_window_id.unwrap();
    app.__test_set_memory_clipboard("kept");
    app.list_missing_glyphs(main_id);
    assert_eq!(app.__test_main_notification_message(), None, "nothing counted, nothing listed");
    app.status_line.glyph_sample.insert(main_id, vec!['\u{378}', '\u{379}']);
    app.list_missing_glyphs(main_id);
    let message = app.__test_main_notification_message().unwrap().to_owned();
    assert!(message.contains("U+0378") && message.contains("U+0379"), "{message}");
    assert!(message.contains("[font].family"));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("kept"));
}

/// `tab_mem` sums cached charges: plain when every charge is fresh, with the oldest age when one
/// is older than two retention intervals, and hidden when a pane was never charged.
#[test]
fn tab_mem_reads_cached_charges_with_their_freshness() {
    let now = Instant::now() + Duration::from_secs(600);
    let fresh = Some(now - Duration::from_secs(5));
    let stale = Some(now - Duration::from_secs(80));
    assert_eq!(
        tab_mem_input([(10, fresh), (20, fresh)].into_iter(), now),
        Some(TabMemInput { bytes: 30, stale_secs: None })
    );
    assert_eq!(
        tab_mem_input([(10, fresh), (20, stale)].into_iter(), now),
        Some(TabMemInput { bytes: 30, stale_secs: Some(80) })
    );
    assert_eq!(tab_mem_input([(10, fresh), (20, None)].into_iter(), now), None);
    assert_eq!(tab_mem_input(std::iter::empty(), now), None);
}

/// `out` turns counting on, reads each pane's bytes once per interval as a rate, keeps showing
/// through its hold, and turns counting off again when it stops.
#[test]
fn out_rate_counts_each_panes_bytes_once_per_interval() {
    use std::sync::atomic::Ordering;
    let mut app = app_with_strip(true);
    app.config.status_line.preset = sonicterm_cfg::status_line::StatusPreset::Diagnostics;
    let pane_id = app.__test_seed_tab("main");
    let main_id = app.main_window_id.unwrap();
    let now = Instant::now();
    let _ = app.sync_status_line(now);
    app.sample_out(now);
    assert!(app.pane_by_id(pane_id).unwrap().output_counting.load(Ordering::Relaxed));
    app.pane_by_id(pane_id).unwrap().output_bytes.fetch_add(3 << 20, Ordering::Relaxed);
    app.sample_out(now + Duration::from_secs(1));
    assert_eq!(app.status_line.out_shown[&main_id], Some(3 << 20));
    app.sample_out(now + Duration::from_secs(2));
    assert_eq!(app.status_line.out_shown[&main_id], Some(0), "held through the low samples");
    app.stop_out_sampling();
    assert!(!app.pane_by_id(pane_id).unwrap().output_counting.load(Ordering::Relaxed));
    assert!(app.status_line.out_shown.is_empty());
}

/// GDI only on Windows with the degrade on; elsewhere wgpu on a CPU or hardware adapter.
#[test]
fn presenter_kind_names_the_presenter_on_each_host() {
    assert_eq!(presenter_kind(true, true, true), PresenterKind::Gdi);
    assert_eq!(presenter_kind(true, false, true), PresenterKind::SoftwareAdapter);
    assert_eq!(
        presenter_kind(false, true, true),
        PresenterKind::SoftwareAdapter,
        "never GDI off Windows"
    );
    assert_eq!(presenter_kind(false, false, false), PresenterKind::Hardware);
}

/// `mem` subtracts only the graphics the footprint is charged with, never the uncharged figure.
#[test]
fn mem_subtracts_the_graphics_charged_to_the_footprint() {
    let graphics = task_vm_graphics(MemoryMetric::Bytes(40 << 20));
    assert_eq!(
        mem_without_graphics(MemoryMetric::Bytes(100 << 20), &graphics),
        MemoryMetric::Bytes(60 << 20)
    );
}

/// A graphics figure larger than the footprint (two samples taken apart) floors `mem` at zero.
#[test]
fn mem_without_graphics_never_goes_below_zero() {
    let graphics = task_vm_graphics(MemoryMetric::Bytes(120 << 20));
    assert_eq!(
        mem_without_graphics(MemoryMetric::Bytes(100 << 20), &graphics),
        MemoryMetric::Bytes(0)
    );
}

/// An unsupported footprint stays unsupported; an unsupported graphics figure keeps the footprint.
#[test]
fn mem_without_graphics_keeps_unsupported_and_missing_graphics() {
    let graphics = task_vm_graphics(MemoryMetric::Bytes(40 << 20));
    assert_eq!(
        mem_without_graphics(MemoryMetric::Unsupported, &graphics),
        MemoryMetric::Unsupported
    );
    let unread = task_vm_graphics(MemoryMetric::Unsupported);
    assert_eq!(
        mem_without_graphics(MemoryMetric::Bytes(100 << 20), &unread),
        MemoryMetric::Bytes(100 << 20)
    );
}

/// DXGI does not say how much of Windows private commit is video memory, and other sources offer
/// no figure, so neither changes `mem`.
#[test]
fn mem_without_graphics_leaves_other_sources_alone() {
    use sonicterm_logging::graphics_memory::{GraphicsMemory, GraphicsSource};
    let mut dxgi = GraphicsMemory::unavailable(GraphicsSource::Dxgi);
    dxgi.dedicated = MemoryMetric::Bytes(30 << 20);
    dxgi.shared = MemoryMetric::Bytes(20 << 20);
    dxgi.footprint = MemoryMetric::Bytes(10 << 20);
    let footprint = MemoryMetric::Bytes(100 << 20);
    assert_eq!(mem_without_graphics(footprint, &dxgi), footprint);
    let none = GraphicsMemory::unavailable(GraphicsSource::None);
    assert_eq!(mem_without_graphics(footprint, &none), footprint);
}
