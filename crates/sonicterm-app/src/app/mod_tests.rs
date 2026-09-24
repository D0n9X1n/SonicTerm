//! App routing, PTY admission, and resize reporting at the app seam.

use super::*;
use sonicterm_cfg::keymap::Direction;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type SubmittedInput = Vec<(u64, Vec<u8>)>;

pub(super) fn submission_snapshot(bytes: &[u8]) -> Option<Vec<u8>> {
    PTY_SUBMISSIONS.with_borrow(|slot| slot.as_ref().map(|_| bytes.to_vec()))
}

pub(super) fn record_submission(pane: u64, bytes: Vec<u8>) {
    PTY_SUBMISSIONS.with_borrow_mut(|slot| slot.as_mut().unwrap().push((pane, bytes)));
}

thread_local! {
    static PTY_SUBMISSIONS: std::cell::RefCell<Option<SubmittedInput>> = const { std::cell::RefCell::new(None) };
}

/// Scope successful-submission evidence to one test's event-loop thread.
pub(super) struct PtySubmissions;

impl PtySubmissions {
    pub(super) fn start() -> Self {
        PTY_SUBMISSIONS.with_borrow_mut(|slot| {
            assert!(slot.is_none(), "submission observation cannot nest");
            *slot = Some(Vec::new());
        });
        Self
    }

    pub(super) fn take(&self) -> SubmittedInput {
        PTY_SUBMISSIONS.with_borrow_mut(|slot| std::mem::take(slot.as_mut().unwrap()))
    }
}

impl Drop for PtySubmissions {
    // Lifecycle: every test releases its thread-local observer, including assertion unwinding.
    fn drop(&mut self) {
        PTY_SUBMISSIONS.with_borrow_mut(|slot| *slot = None);
    }
}

/// Install an idle real PTY without a VT worker that could change the test's negotiated modes.
#[cfg(any(windows, unix))]
pub(super) fn attach_idle_pty(app: &mut App, window: WindowId, pane: u64) {
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".into()]);
    app.windows.get_mut(&window).unwrap().panes.get_mut(&pane).unwrap().pty =
        Some(PtyHandle::spawn_with_args(program, &args, 80, 24).expect("idle input PTY"));
}

#[cfg(any(windows, unix))]
fn input_test_app() -> (App, WindowId, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("input");
    let window = app.main_window_id.unwrap();
    attach_idle_pty(&mut app, window, pane);
    (app, window, pane)
}

/// Only successful queue admission is evidence; an oversized real-PTY write contributes nothing.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_excludes_failed_input() {
    let (mut app, _, pane) = input_test_app();
    let submitted = PtySubmissions::start();
    assert!(app.write_to_pane(pane, b"accepted".to_vec(), PtyInputSource::Keyboard));
    assert_eq!(submitted.take(), vec![(pane, b"accepted".to_vec())]);
    let oversized = vec![b'x'; sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES + 1];
    assert!(!app.write_to_pane(pane, oversized, PtyInputSource::Paste));
    assert!(submitted.take().is_empty());
    let pty = app.pane_by_id(pane).unwrap().pty.as_ref().unwrap();
    assert!(App::queue_pty_input(None, pty, pane, PtyInputSource::ScriptDraft, b"draft".to_vec()));
    assert_eq!(submitted.take(), vec![(pane, b"draft".to_vec())]);
    assert!(!App::queue_pty_input(
        None,
        pty,
        pane,
        PtyInputSource::ScriptDraft,
        vec![b'x'; sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES + 1],
    ));
    assert!(submitted.take().is_empty());
}

/// Pending motion is not submission, even though staging returns accepted ownership to the caller.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_excludes_staged_motion() {
    let (mut app, _, pane) = input_test_app();
    app.pane_by_id(pane).unwrap().parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
    let submitted = PtySubmissions::start();
    assert!(app.write_to_pane(pane, b"control".to_vec(), PtyInputSource::Keyboard));
    assert_eq!(submitted.take(), vec![(pane, b"control".to_vec())]);
    assert!(app.write_to_pane(pane, b"\x1b[<35;2;3M".to_vec(), PtyInputSource::PointerMotion));
    assert_ne!(app.pane_by_id(pane).unwrap().pending_pointer_motion.len, 0);
    assert!(submitted.take().is_empty());
}

/// A motion-only flush records exactly the newest coalesced report that the real PTY accepted.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_records_flushed_motion() {
    let (mut app, _, pane) = input_test_app();
    app.pane_by_id(pane).unwrap().parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
    let submitted = PtySubmissions::start();
    for report in [b"\x1b[<35;1;3M", b"\x1b[<35;2;3M"] {
        assert!(app.write_to_pane(pane, report.to_vec(), PtyInputSource::PointerMotion));
    }
    assert!(submitted.take().is_empty());
    assert_eq!(app.flush_pointer_motion(Instant::now()), None);
    assert_eq!(submitted.take(), vec![(pane, b"\x1b[<35;2;3M".to_vec())]);
    assert_eq!(app.pane_by_id(pane).unwrap().pending_pointer_motion.len, 0);
}

/// Motion followed by a discrete input records their actual combined queue payload once, in order.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_submission_observer_records_motion_with_discrete_input() {
    let (mut app, _, pane) = input_test_app();
    app.pane_by_id(pane).unwrap().parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
    let submitted = PtySubmissions::start();
    assert!(app.write_to_pane(pane, b"\x1b[<35;2;3M".to_vec(), PtyInputSource::PointerMotion));
    assert!(submitted.take().is_empty());
    assert!(app.write_to_pane(pane, b"key".to_vec(), PtyInputSource::Keyboard));
    assert_eq!(submitted.take(), vec![(pane, b"\x1b[<35;2;3Mkey".to_vec())]);
    assert_eq!(app.flush_pointer_motion(Instant::now()), None);
    assert!(submitted.take().is_empty());
}

/// Main, child, and sibling windows retain independent real PTYs for admission assertions.
#[cfg(any(windows, unix))]
pub(super) fn input_test_windows() -> (App, [(WindowId, u64); 3]) {
    let (mut app, main, main_pane) = input_test_app();
    let child = app.__test_seed_child_window(&["child"]);
    let sibling = app.__test_seed_child_window(&["sibling"]);
    let child_pane = app.windows[&child].tab_states[0].active_pane;
    let sibling_pane = app.windows[&sibling].tab_states[0].active_pane;
    attach_idle_pty(&mut app, child, child_pane);
    attach_idle_pty(&mut app, sibling, sibling_pane);
    (app, [(main, main_pane), (child, child_pane), (sibling, sibling_pane)])
}

/// New AllTabs keyboard fan-out excludes a READONLY receiver but still admits the source and its other peer.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_alltabs_keyboard_excludes_readonly_receivers() {
    for source_index in 0..3 {
        for read_only in [false, true] {
            let (mut app, windows) = input_test_windows();
            let (_, source) = windows[source_index];
            let (protected_window, protected) = windows[(source_index + 1) % 3];
            let (_, peer) = windows[(source_index + 2) % 3];
            app.windows.get_mut(&protected_window).unwrap().copy_mode = Some(if read_only {
                CopyModeState::read_only_at((0, 0))
            } else {
                CopyModeState::new_at((0, 0))
            });
            app.broadcast =
                BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
            let submitted = PtySubmissions::start();
            let writes = app
                .terminal_key_targets(source)
                .into_iter()
                .map(|pane| {
                    (
                        pane,
                        keyboard_protocol::EncodedKey {
                            bytes: b"k".to_vec(),
                            held: HeldKey::Legacy,
                        },
                    )
                })
                .collect();
            let delivered = app.dispatch_terminal_key_writes(writes);
            let expected = if read_only {
                BTreeSet::from([source, peer])
            } else {
                BTreeSet::from([source, protected, peer])
            };
            let actual = submitted.take();
            assert_eq!(
                actual.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(),
                expected,
                "source={source_index} read_only={read_only}"
            );
            assert!(actual.iter().all(|(_, bytes)| bytes == b"k"));
            assert_eq!(delivered.keys().copied().collect::<BTreeSet<_>>(), expected);
            assert_eq!(app.broadcast_participants(), expected);
            assert_eq!(
                app.broadcast_receivers(),
                expected.difference(&BTreeSet::from([source])).copied().collect()
            );
        }
    }
}

/// The byte fan-out shares the keyboard receiver filter without treating ordinary copy mode as READONLY.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_alltabs_byte_fanout_excludes_readonly_receivers() {
    for source_index in 0..3 {
        let (mut app, windows) = input_test_windows();
        let (_, source) = windows[source_index];
        let (protected_window, _) = windows[(source_index + 1) % 3];
        let (_, peer) = windows[(source_index + 2) % 3];
        app.windows.get_mut(&protected_window).unwrap().copy_mode =
            Some(CopyModeState::read_only_at((0, 0)));
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
        let submitted = PtySubmissions::start();
        app.broadcast_from(source, b"fanout".to_vec(), PtyInputSource::Ime);
        assert_eq!(submitted.take(), vec![(peer, b"fanout".to_vec())]);
    }
}

/// AppKit reports physical size at its current backing scale, even when the stored event scale is older.
#[cfg(target_os = "macos")]
#[test]
fn macos_dpi_transition_preserves_current_native_size() {
    let logical = winit::dpi::LogicalSize::new(800.0, 600.0);
    for (stored, native, destination) in
        [(1.0, 2.0, 2.0), (2.0, 1.0, 1.0), (1.0, 1.5, 1.5), (1.5, 2.0, 1.5), (2.0, 2.0, 2.0)]
    {
        let current = logical.to_physical::<u32>(native);
        let target = dpi_transition_inner_size(
            current,
            dpi_transition_size_scale(stored, native),
            destination,
            winit::dpi::PhysicalSize::new(100, 100),
            winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX),
        );
        assert_eq!(
            target,
            logical.to_physical::<u32>(destination),
            "stored={stored} native={native} destination={destination}"
        );
    }
}

/// Repeated AppKit scale changes preserve the same logical extent instead of compounding native width and height.
#[cfg(target_os = "macos")]
#[test]
fn macos_dpi_transition_does_not_compound_round_trips() {
    let mut logical = winit::dpi::LogicalSize::new(800.0, 600.0);
    let initial = logical;
    let mut stored = 1.0;
    for native in [2.0, 1.0, 2.0, 1.0] {
        let target = dpi_transition_inner_size(
            logical.to_physical::<u32>(native),
            dpi_transition_size_scale(stored, native),
            native,
            winit::dpi::PhysicalSize::new(100, 100),
            winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX),
        );
        logical = target.to_logical(native);
        assert_eq!(logical, initial);
        stored = native;
    }
}

/// Physical-size source domains do not bypass terminal minimums or destination work-area caps.
#[test]
fn dpi_transition_preserves_bounds_in_both_size_domains() {
    let minimum = winit::dpi::PhysicalSize::new(700, 500);
    let available = winit::dpi::PhysicalSize::new(1800, 1000);
    for source in [1.0, 2.0] {
        let undersized = dpi_transition_inner_size(
            winit::dpi::PhysicalSize::new(50, 40),
            source,
            2.0,
            minimum,
            available,
        );
        assert_eq!(undersized, minimum);
        let oversized = dpi_transition_inner_size(
            winit::dpi::PhysicalSize::new(3000, 2400),
            source,
            2.0,
            minimum,
            available,
        );
        assert_eq!(oversized, available);
        let insufficient = dpi_transition_inner_size(
            winit::dpi::PhysicalSize::new(50, 40),
            source,
            2.0,
            minimum,
            winit::dpi::PhysicalSize::new(600, 400),
        );
        assert_eq!(insufficient, minimum);
    }
}

/// Non-AppKit platforms keep the stored-scale contract for their pre-transition native extents.
#[cfg(not(target_os = "macos"))]
#[test]
fn dpi_transition_non_macos_retains_stored_scale() {
    assert_eq!(dpi_transition_size_scale(1.0, 2.0), 1.0);
    assert_eq!(dpi_transition_size_scale(2.0, 1.0), 2.0);
}

/// Native size must use the platform-selected scale in both the requested target and its diagnostic projection.
#[test]
fn dpi_transition_handler_uses_observed_size_domain() {
    let source = include_str!("mod.rs");
    let handler = source
        .split("fn apply_window_dpi_transition(")
        .nth(1)
        .unwrap()
        .split("pub fn apply_terminal_window_minimum(")
        .next()
        .unwrap();
    assert!(handler.contains("dpi_transition_size_scale(old_scale, native_scale)"));
    assert!(handler.contains("dpi_transition_inner_size(old_inner, size_scale, dpi_scale"));
    assert!(handler.contains("old_inner.to_logical::<f64>(size_scale.max(0.1))"));
    assert!(!handler.contains("old_inner.to_logical::<f64>(old_scale"));
}

/// Logical size snapshots survive source focus changes and preserve destination-DPI scaling without maximizing it.
#[test]
fn new_window_size_snapshot_validates_and_converts_source_geometry() {
    let physical = winit::dpi::PhysicalSize::new(1400, 875);
    let logical = inherited_window_size(physical, 1.75, false).unwrap();
    assert_eq!(logical, winit::dpi::LogicalSize::new(800.0, 500.0));
    assert_eq!(logical.to_physical::<u32>(1.25), winit::dpi::PhysicalSize::new(1000, 625));
    assert!(inherited_window_size(physical, 1.75, true).is_none());
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(inherited_window_size(physical, scale, false).is_none());
    }
    assert!(inherited_window_size(winit::dpi::PhysicalSize::new(0, 875), 1.0, false).is_none());
}

/// A queued parentless request preserves its captured default even if configuration changes before draining it.
#[test]
fn new_window_request_keeps_captured_fallback() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.config.window.cols = 111;
    let expected = configured_window_size(&app.config, app.tab_bar_visible);
    assert!(app.run_action(&sonicterm_cfg::keymap::Action::NewWindow));
    app.config.window.cols = 55;
    assert_eq!(app.pending_new_window.unwrap().inner_size, expected);
    assert_ne!(app.window_request(None).inner_size, expected);
}

/// Parentless requests use configured startup dimensions rather than the warm pool's internal size.
#[test]
fn new_window_size_fallback_tracks_configured_rows_and_columns() {
    let mut config = Config::default();
    config.window.cols = 100;
    config.window.rows = 40;
    config.font.size = 16.0;
    config.font.line_height = 1.25;
    config.window.padding_left = 8.0;
    config.window.padding_right = 12.0;
    config.window.padding_top = 4.0;
    config.window.padding_bottom = 6.0;
    let size = configured_window_size(&config, false);
    assert_eq!(size, winit::dpi::LogicalSize::new(920.0, 810.0));
    assert_eq!(
        configured_window_size(&config, true).height,
        size.height + f64::from(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT)
    );
}

#[test]
fn broadcast_render_flags_include_fixed_source_and_exclude_unrelated_panes() {
    // Visual membership includes the fixed source without changing delivery or following focus.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_child_split_active_right(child));
    let source = app.__test_child_active_pane(child).unwrap();
    let sibling =
        app.__test_child_pane_ids(child).unwrap().into_iter().find(|id| *id != source).unwrap();
    let flags = |app: &App, window| app.__test_child_broadcast_render_flags(window).unwrap();
    assert!(flags(&app, child).iter().all(|(_, marked)| !marked));
    assert!(
        app.run_action_for_window(&Action::ToggleBroadcast { scope: BroadcastScope::Tab }, child)
    );
    assert!(flags(&app, child).iter().all(|(_, marked)| *marked));
    assert_eq!(flags(&app, app.main_window_id.unwrap()), vec![(main, false)]);
    assert_eq!(app.broadcast_receivers(), std::collections::BTreeSet::from([sibling]));
    app.windows.get_mut(&child).unwrap().tab_states[0].active_pane = sibling;
    assert!(flags(&app, child).iter().all(|(_, marked)| *marked));
    assert_eq!(app.__test_broadcast_source(), Some(source));
    app.__test_enable_pty_write_log();
    app.__test_write_to_pane_with_broadcast(source, b"ping".to_vec());
    let writes = app.__test_pty_write_log();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes.iter().filter(|(id, _)| *id == source).count(), 1);
    assert_eq!(writes.iter().filter(|(id, _)| *id == sibling).count(), 1);
    app.windows.get_mut(&child).unwrap().tab_states[0].active_pane = source;
    assert!(
        app.run_action_for_window(&Action::ToggleBroadcast { scope: BroadcastScope::Tab }, child)
    );
    assert!(flags(&app, child).iter().all(|(_, marked)| !marked));
}

#[test]
fn broadcast_render_flags_cover_all_windows_and_clear_after_source_closes() {
    // All-tabs chrome covers visible participants, but a dead source cannot advertise live fan-out.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app
        .run_action_for_window(&Action::ToggleBroadcast { scope: BroadcastScope::AllTabs }, child));
    for window in [app.main_window_id.unwrap(), child] {
        assert!(app
            .__test_child_broadcast_render_flags(window)
            .unwrap()
            .iter()
            .all(|(_, marked)| *marked));
    }
    app.clear_closed_broadcast_source();
    assert!(app.__test_broadcast_source().is_some(), "a live source must remain armed");
    assert!(app.close_child_window(child));
    assert_eq!(
        app.__test_child_broadcast_render_flags(app.main_window_id.unwrap()).unwrap(),
        vec![(main, false)]
    );
    app.clear_closed_broadcast_source();
    assert_eq!(app.__test_broadcast_source(), None);
    assert!(app.broadcast_receivers().is_empty());
}

#[test]
fn broadcast_render_flags_mark_single_source_without_receivers() {
    // Armed single-pane tab broadcast still marks its source while the fan-out set stays empty.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let source = app.__test_seed_tab("main");
    assert!(app.run_action(&Action::ToggleBroadcast { scope: BroadcastScope::Tab }));
    assert!(app.broadcast_receivers().is_empty());
    assert_eq!(
        app.__test_child_broadcast_render_flags(app.main_window_id.unwrap()).unwrap(),
        vec![(source, true)]
    );
}

#[test]
fn pane_resize_helpers_restore_fullscreen_scrolling_after_margin_reset() {
    // Main and child layout helpers must reconcile parser margins before subsequent PTY output.
    for child in [false, true] {
        for rows in [12, 36] {
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            app.__test_seed_tab("main");
            let window_id = if child {
                app.__test_seed_child_window(&["child"])
            } else {
                app.main_window_id.unwrap()
            };
            let pane_id = app.windows[&window_id].tab_states[0].active_pane;
            let parser = app.pane_by_id(pane_id).unwrap().parser.clone();
            parser.lock().advance(b"\x1b[r");
            let viewport = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, rows as f32 * 10.0);
            if child {
                assert!(app.__test_set_child_pane_viewport(window_id, viewport, 10.0, 10.0));
                assert!(app.__test_invoke_activate_tab_in_child(window_id, 0));
            } else {
                assert!(app.__test_set_main_pane_viewport(viewport, 10.0, 10.0));
                app.__test_resize_visible_panes();
            }
            let mut parser = parser.lock();
            assert_eq!((parser.grid().cols, parser.grid().rows), (80, rows));
            parser.advance(format!("\x1b[{rows};1H").as_bytes());
            let before = parser.grid().scrollback_len();
            parser.advance(b"first\r\nsecond\r\n");
            assert_eq!(parser.grid().scrollback_len(), before + 2, "child={child}, rows={rows}");
            assert_eq!(parser.grid().row(rows - 2)[0].ch, 's');
        }
    }
}

#[test]
fn broadcast_panes_keep_scrollback_and_independent_viewports_after_resize() {
    // Broadcast membership cannot couple the resized panes' history or scrolling positions.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.__test_child_active_pane(child).unwrap();
    assert!(app.run_action(&Action::ToggleBroadcast {
        scope: sonicterm_cfg::keymap::BroadcastScope::AllTabs,
    }));
    assert!(app.terminal_key_targets(main_pane).contains(&child_pane));
    for (window, pane) in [(app.main_window_id.unwrap(), main_pane), (child, child_pane)] {
        let parser = app.pane_by_id(pane).unwrap().parser.clone();
        parser.lock().advance(b"\x1b[r");
        resize_all_panes(&app.windows[&window].panes, 80, 12);
        parser.lock().advance(b"\x1b[12;1Hone\r\ntwo\r\nthree\r\n");
        assert_eq!(parser.lock().grid().scrollback_len(), 3);
    }
    app.scroll_pane(main_pane, -2);
    assert_eq!(app.pane_by_id(main_pane).unwrap().viewport_top_abs, Some(1));
    assert_eq!(app.pane_by_id(child_pane).unwrap().viewport_top_abs, None);
    app.__test_child_set_pane_view_top(child, child_pane, 0, 3);
    assert_eq!(app.pane_by_id(child_pane).unwrap().viewport_top_abs, Some(0));
    assert_eq!(app.pane_by_id(main_pane).unwrap().viewport_top_abs, Some(1));
}

#[test]
fn all_panes_resize_restores_scrolling_without_homing_cursor() {
    // Whole-window sizing has the same parser-state contract as per-pane rectangle sizing.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    let parser = app.pane_by_id(pane_id).unwrap().parser.clone();
    parser.lock().advance(b"\x1b[r\x1b[10;3H");
    resize_all_panes(&app.main().unwrap().panes, 80, 12);
    let mut parser = parser.lock();
    assert_eq!(parser.grid().cursor, sonicterm_grid::grid::Pos { row: 9, col: 2 });
    parser.advance(b"\x1b[12;1Hline\r\n");
    assert_eq!(parser.grid().scrollback_len(), 1);
}

#[test]
fn history_search_commit_starts_at_current_viewport_in_main_and_child() {
    // Four earlier matches and two later matches must retain their global indices around the visible fifth match.
    for child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let window = if child {
            app.__test_seed_child_window(&["child"])
        } else {
            app.main_window_id.unwrap()
        };
        let pane_id = app.windows[&window].tab_states[0].active_pane;
        {
            let pane = app.windows.get_mut(&window).unwrap().panes.get_mut(&pane_id).unwrap();
            let mut parser = pane.parser.lock();
            parser.resize(40, 3);
            for row in 0..21 {
                if row > 0 {
                    parser.advance(b"\r\n");
                }
                let line = if row % 3 == 0 { "needle" } else { "other" };
                parser.advance(line.as_bytes());
            }
            assert_eq!(parser.grid().scrollback_len(), 18);
            pane.viewport_top_abs = Some(12);
        }
        if child {
            assert!(app.open_search_in_child(window));
        } else {
            app.open_search();
        }
        assert!(app.search_handle_ime_commit(window, "needle"));
        let state = &app.windows[&window];
        let search = state.tab_states[0].search.as_ref().unwrap();
        assert_eq!(search.matches.len(), 7);
        assert_eq!(search.current, Some(4));
        assert_eq!(sonicterm_ui::overlays::search_bar_label(search, ""), "/ needle · 5/7");
        assert_eq!(state.panes[&pane_id].viewport_top_abs, Some(12));
        assert_eq!(search.requested_scroll_row, None);
    }
}

#[test]
fn terminal_ime_anchor_adds_physical_pane_origin_once() {
    // The native setter receives raster coordinates, including a right/lower pane's origin exactly once.
    let mut throttle = sonicterm_ui::ime::ImeCursorThrottle::new();
    let pane = sonicterm_ui::pane::Rect::new(401.25, 233.75, 360.0, 220.0);
    let mut calls = Vec::new();
    update_terminal_ime_cursor_area(
        &mut throttle,
        (7, pane),
        (2, 3),
        (10.5, 20.25),
        (4.5, 6.25),
        |pos, size| calls.push((pos, size)),
    );
    assert_eq!(
        calls,
        [(winit::dpi::PhysicalPosition::new(437, 280), winit::dpi::PhysicalSize::new(11, 21))]
    );
}

#[test]
fn terminal_ime_anchor_updates_equal_cells_after_focus_or_geometry_changes() {
    // Native update identity includes the pane and physical rectangle, not just its local cursor cell.
    let mut throttle = sonicterm_ui::ime::ImeCursorThrottle::new();
    let left = sonicterm_ui::pane::Rect::new(0.0, 40.0, 400.0, 240.0);
    let right = sonicterm_ui::pane::Rect::new(400.0, 40.0, 400.0, 240.0);
    let mut calls = Vec::new();
    for (id, rect, cells, padding) in [
        (1, left, (10.0, 20.0), (4.0, 6.0)),
        (1, left, (10.0, 20.0), (4.0, 6.0)),
        (2, right, (10.0, 20.0), (4.0, 6.0)),
        (2, right, (12.0, 22.0), (4.0, 6.0)),
        (2, right, (12.0, 22.0), (7.0, 9.0)),
        (2, left, (12.0, 22.0), (7.0, 9.0)),
    ] {
        update_terminal_ime_cursor_area(
            &mut throttle,
            (id, rect),
            (1, 1),
            cells,
            padding,
            |pos, size| calls.push((pos, size)),
        );
    }
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].0, winit::dpi::PhysicalPosition::new(14, 66));
    assert_eq!(calls[1].0, winit::dpi::PhysicalPosition::new(414, 66));
    assert_eq!(calls[2].1, winit::dpi::PhysicalSize::new(12, 22));
    assert_eq!(calls[4].0, winit::dpi::PhysicalPosition::new(19, 71));
}

#[test]
fn native_drag_keeps_the_pressed_tab_after_an_earlier_tab_closes() {
    // Native completion must resolve the pressed identity, not the tab that inherited its old index.
    for source_is_main in [true, false] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("A");
        app.__test_seed_tab("B");
        app.__test_seed_tab("C");
        let main = app.main_window_id.unwrap();
        let source =
            if source_is_main { main } else { app.__test_seed_child_window(&["A", "B", "C"]) };
        let pressed = app.tab_id_at(source, 1).unwrap();
        app.__test_set_os_drag_source(Some((source, 1)));
        drop(app.detach_from_child(source, 0));
        app.os_drag_pending
            .set_ended(os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos: (120, 240) });

        app.handle_os_drag_ended();

        let pending = app.pending_tear_out.as_ref().expect("the captured tab still exists");
        assert_eq!(pending.source_window, source);
        assert_eq!(pending.source_tab_id, Some(pressed));
        assert_eq!(pending.source_tab_idx, 0);
    }
}

#[test]
fn native_drag_cancels_when_the_pressed_tab_has_closed() {
    // A vanished captured tab cannot promote its neighbor into a native tear-out.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("A");
    app.__test_seed_tab("B");
    app.__test_seed_tab("C");
    let main = app.main_window_id.unwrap();
    let survivor = app.tab_id_at(main, 2).unwrap();
    app.__test_set_os_drag_source(Some((main, 1)));
    drop(app.detach_from_child(main, 1));
    app.os_drag_pending
        .set_ended(os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos: (120, 240) });

    app.handle_os_drag_ended();

    assert!(app.pending_tear_out.is_none());
    assert_eq!(app.tab_id_at(main, 1), Some(survivor));
    assert_eq!(app.main().unwrap().tabs.len(), 2);
}

#[test]
fn native_bar_drop_and_cancel_preserve_captured_identity_after_topology_changes() {
    // Native completion cannot move a neighbor after close/reorder; cancellation leaves the post-mutation topology intact.
    for source_is_main in [true, false] {
        for mutation in 0..4 {
            for cancelled in [false, true] {
                let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
                app.__test_seed_tab("A");
                app.__test_seed_tab("B");
                app.__test_seed_tab("C");
                let main = app.main_window_id.unwrap();
                let source = if source_is_main {
                    main
                } else {
                    app.__test_seed_child_window(&["A", "B", "C"])
                };
                let target = app.__test_seed_child_window(&["destination"]);
                app.__test_set_child_pane_viewport(
                    target,
                    sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
                    10.0,
                    10.0,
                );
                let pressed = app.tab_id_at(source, 1).unwrap();
                let pane = app.windows[&source].tab_states[1].active_pane;
                app.__test_set_os_drag_source(Some((source, 1)));
                match mutation {
                    0 => drop(app.detach_from_child(source, 0)),
                    1 => drop(app.detach_from_child(source, 1)),
                    2 => {
                        assert!(app.windows.get_mut(&source).unwrap().reorder_tab(1, 2));
                    }
                    _ => {
                        app.windows.remove(&source);
                    }
                }
                let before: Vec<_> = app.windows.get(&source).map_or_else(Vec::new, |window| {
                    (0..window.tabs.len()).map(|i| window.tabs.tabs()[i].id).collect()
                });
                let outcome = if cancelled {
                    os_drag::DragOutcome::Cancelled
                } else {
                    os_drag::DragOutcome::DroppedOnBar {
                        target_window: Some(target),
                        target_slot: 0,
                    }
                };
                app.os_drag_pending.set_ended(outcome);

                assert_eq!(app.handle_os_drag_ended(), Some(outcome));
                assert!(app.handle_os_drag_ended().is_none());
                assert!(app.os_drag_source.is_none());
                assert!(app.pending_tear_out.is_none());
                let transferred = !cancelled && matches!(mutation, 0 | 2);
                assert_eq!(app.windows[&target].tabs.len(), if transferred { 2 } else { 1 });
                assert_eq!(app.windows[&target].panes.contains_key(&pane), transferred);
                if transferred {
                    assert_eq!(app.tab_id_at(target, 0), Some(pressed));
                }
                let expected: Vec<_> =
                    before.into_iter().filter(|id| !transferred || *id != pressed).collect();
                let after: Vec<_> = app.windows.get(&source).map_or_else(Vec::new, |window| {
                    (0..window.tabs.len()).map(|i| window.tabs.tabs()[i].id).collect()
                });
                assert_eq!(after, expected);
            }
        }
    }
}

#[test]
fn local_drag_routes_resolve_identity_after_close_reorder_or_source_loss() {
    // All local release routes use the captured window/tab even after a close or reorder changes vector slots.
    for source_is_main in [true, false] {
        for mutation in 0..4 {
            for route in 0..3 {
                let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
                app.__test_seed_tab("A");
                app.__test_seed_tab("B");
                app.__test_seed_tab("C");
                let main = app.main_window_id.unwrap();
                let source = if source_is_main {
                    main
                } else {
                    app.__test_seed_child_window(&["A", "B", "C"])
                };
                let target = app.__test_seed_child_window(&["destination"]);
                app.__test_set_child_pane_viewport(
                    target,
                    sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
                    10.0,
                    10.0,
                );
                let pressed = app.tab_id_at(source, 1).unwrap();
                let pressed_pane = app.windows[&source].tab_states[1].active_pane;
                let session = crate::tab_drag::DragSession::new(source, pressed, (0.0, 0.0));
                match mutation {
                    0 => {
                        drop(app.detach_from_child(source, 0));
                    }
                    1 => {
                        drop(app.detach_from_child(source, 1));
                    }
                    2 => {
                        app.windows.get_mut(&source).unwrap().reorder_tab(1, 2);
                    }
                    _ => {
                        app.windows.remove(&source);
                    }
                }
                let action =
                    match route {
                        0 => crate::tab_drag::DragAction::ReorderTab { to: 1 },
                        1 => crate::tab_drag::DragAction::MergeIntoWindow(
                            crate::tab_drag::DropTarget { window: target, slot: 0 },
                        ),
                        _ => crate::tab_drag::DragAction::TearOutToNewWindow {
                            drop_local: (0.0, 100.0),
                        },
                    };
                let mut torn = None;
                let completed = app.finish_tab_drag(session, action, |app, window, index| {
                    torn = app.detach_from_child(window, index);
                });
                assert_eq!(completed, mutation == 0 || mutation == 2);
                if completed {
                    match route {
                        0 => {
                            assert_eq!(app.tab_id_at(source, 1), Some(pressed));
                            assert_eq!(
                                app.windows[&source].tab_states[1].active_pane,
                                pressed_pane
                            );
                        }
                        1 => {
                            assert_eq!(app.tab_id_at(target, 0), Some(pressed));
                            assert!(app.windows[&target].panes.contains_key(&pressed_pane));
                        }
                        _ => {
                            let (tab, state, panes) = torn.as_ref().unwrap();
                            assert_eq!(tab.id, pressed);
                            assert_eq!(state.active_pane, pressed_pane);
                            assert!(panes.contains_key(&pressed_pane));
                        }
                    }
                } else {
                    assert_eq!(app.windows[&target].tabs.len(), 1);
                    assert!(torn.is_none());
                }
            }
        }
    }
}

#[test]
fn transferred_nested_tabs_resize_visible_leaves_and_preserve_zoom_hidden_sizes() {
    // Actual transfer routes must emit only final per-pane sizes; unzoom sizes hidden siblings before display.
    for route in 0..3 {
        for zoomed in [false, true] {
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            app.__test_seed_tab("main");
            let main = app.main_window_id.unwrap();
            let child = app.__test_seed_child_window(&["child"]);
            let sibling = app.__test_seed_child_window(&["sibling"]);
            let (source, target) = match route {
                0 => (main, child),
                1 => (child, main),
                _ => (child, sibling),
            };
            let outer = sonicterm_ui::pane::Rect::new(0.0, 50.0, 1200.0, 640.0);
            app.__test_set_main_pane_viewport(outer, 12.5, 20.0);
            app.__test_set_child_pane_viewport(child, outer, 12.5, 20.0);
            app.__test_set_child_pane_viewport(sibling, outer, 12.5, 20.0);
            let calls = Arc::new(Mutex::new(Vec::new()));
            let ids = [next_pane_id(), next_pane_id(), next_pane_id()];
            let mut panes = HashMap::new();
            for id in ids {
                let (mut pane, _, fail) = pane_with_failing_resize();
                fail.store(false, Ordering::Relaxed);
                let pty = pane.pty.as_mut().unwrap();
                let original = std::mem::replace(&mut pty.resize, Box::new(|_, _| Ok(())));
                let calls = calls.clone();
                pty.resize = Box::new(move |cols, rows| {
                    calls.lock().push((id, cols, rows));
                    original(cols, rows)
                });
                panes.insert(id, pane);
            }
            let mut tree = sonicterm_ui::pane::PaneTree::leaf(ids[0]);
            assert!(tree.split(ids[0], Direction::Right, ids[1]));
            assert!(tree.split(ids[1], Direction::Down, ids[2]));
            let split_layout = tree.layout(outer);
            if zoomed {
                assert!(tree.toggle_zoom(ids[1]));
            }
            let layout = tree.layout(outer);
            let tab = sonicterm_ui::tabs::Tab::new("incoming");
            let tab_id = tab.id;
            let window = app.windows.get_mut(&source).unwrap();
            window.tabs.insert(0, tab);
            window.tab_states.insert(0, TabState::new(tree, ids[1]));
            window.panes.extend(panes);

            let source_kind = (source != main).then_some(source);
            let target_kind = (target != main).then_some(target);
            app.transfer_tab(source_kind, 0, target_kind, 1).unwrap();

            assert_eq!(app.tab_id_at(target, 1), Some(tab_id));
            assert_eq!(app.windows[&target].tabs.active_index(), 1);
            let mut expected_calls: Vec<_> = layout
                .iter()
                .map(|(id, rect)| {
                    (*id, (rect.w / 12.5).floor() as u16, (rect.h / 20.0).floor() as u16)
                })
                .collect();
            let mut actual = calls.lock().clone();
            actual.sort_unstable();
            expected_calls.sort_unstable();
            assert_eq!(actual, expected_calls, "route={route}, zoomed={zoomed}");
            for id in ids {
                let parser = app.windows[&target].panes[&id].parser.lock();
                let expected = expected_calls
                    .iter()
                    .find(|call| call.0 == id)
                    .map(|call| (call.1, call.2))
                    .unwrap_or((80, 24));
                assert_eq!((parser.grid().cols, parser.grid().rows), expected);
            }
            if zoomed {
                calls.lock().clear();
                assert!(app.windows.get_mut(&target).unwrap().tab_states[1]
                    .tree
                    .toggle_zoom(ids[1]));
                if target == main {
                    app.resize_visible_panes();
                } else {
                    super::child_window::resize_visible_panes_in_child(
                        app.windows.get_mut(&target).unwrap(),
                    );
                }
                let mut expected: Vec<_> = split_layout
                    .iter()
                    .map(|(id, rect)| {
                        (*id, (rect.w / 12.5).floor() as u16, (rect.h / 20.0).floor() as u16)
                    })
                    .collect();
                let mut actual = calls.lock().clone();
                actual.sort_unstable();
                expected.sort_unstable();
                assert_eq!(actual, expected);
            }
        }
    }
}

#[test]
fn attaching_a_split_tab_uses_only_final_destination_pane_sizes() {
    // Capture the production resize callback so an intermediate whole-window SIGWINCH cannot hide.
    for direction in [Direction::Right, Direction::Down] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("destination");
        app.__test_set_main_pane_viewport(
            sonicterm_ui::pane::Rect::new(0.0, 40.0, 1200.0, 600.0),
            10.0,
            20.0,
        );
        let (first, _, first_fail) = pane_with_failing_resize();
        let (second, _, second_fail) = pane_with_failing_resize();
        first_fail.store(false, Ordering::Relaxed);
        second_fail.store(false, Ordering::Relaxed);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ids = [next_pane_id(), next_pane_id()];
        let mut panes = HashMap::new();
        for (id, mut pane) in ids.into_iter().zip([first, second]) {
            let pty = pane.pty.as_mut().unwrap();
            let original = std::mem::replace(&mut pty.resize, Box::new(|_, _| Ok(())));
            let calls = calls.clone();
            pty.resize = Box::new(move |cols, rows| {
                calls.lock().push((id, cols, rows));
                original(cols, rows)
            });
            panes.insert(id, pane);
        }
        let mut tree = sonicterm_ui::pane::PaneTree::leaf(ids[0]);
        assert!(tree.split(ids[0], direction, ids[1]));
        let state = TabState::new(tree, ids[0]);
        let tab = sonicterm_ui::tabs::Tab::new("incoming");
        let expected = state.tree.layout(sonicterm_ui::pane::Rect::new(0.0, 40.0, 1200.0, 600.0));

        app.attach_tab_state(1, tab, state, panes).unwrap();

        let main = app.main().unwrap();
        assert_eq!(main.tabs.active_index(), 1);
        let mut expected_calls = Vec::new();
        for (id, rect) in expected {
            let size = ((rect.w / 10.0).floor() as u16, (rect.h / 20.0).floor() as u16);
            let grid = main.panes[&id].parser.lock();
            assert_eq!((grid.grid().cols, grid.grid().rows), size);
            expected_calls.push((id, size.0, size.1));
        }
        let mut actual = calls.lock().clone();
        actual.sort_unstable();
        expected_calls.sort_unstable();
        assert_eq!(actual, expected_calls);
    }
}

#[test]
fn composed_topology_changes_preserve_geometry_owners_and_input_identity() {
    // Main and child operation chains must settle the same derived state before a later transfer uses it.
    let outer = sonicterm_ui::pane::Rect::new(0.0, 40.0, 1200.0, 600.0);
    let assert_window = |app: &App, id: WindowId| {
        let window = &app.windows[&id];
        assert_eq!(window.tabs.len(), window.tab_states.len());
        let mut leaves: Vec<_> =
            window.tab_states.iter().flat_map(|tab| tab.tree.leaves()).collect();
        leaves.sort_unstable();
        let mut live: Vec<_> = window.panes.keys().copied().collect();
        live.sort_unstable();
        assert_eq!(leaves, live);
        for tab in &window.tab_states {
            assert!(tab.tree.leaves().contains(&tab.active_pane));
            assert!(tab.tree.zoomed_pane_id().is_none_or(|pane| pane == tab.active_pane));
        }
        let active = &window.tab_states[window.tabs.active_index()];
        let visible = active.tree.layout(outer);
        assert!(visible.iter().any(|(pane, _)| *pane == active.active_pane));
        for (pane, rect) in visible {
            let parser = window.panes[&pane].parser.lock();
            assert_eq!(
                (parser.grid().cols, parser.grid().rows),
                ((rect.w / 10.0).floor() as u16, (rect.h / 20.0).floor() as u16)
            );
            assert!(parser.grid().dirty_rows().count() > 0);
        }
        let parent = window.owner.as_ref().unwrap().id();
        for pane in window.panes.values() {
            let owner = pane.owner.as_ref().expect("completion registers new panes").id();
            assert_eq!(app.governor.snapshot(owner).unwrap().parent, Some(parent));
        }
    };
    for child_source in [false, true] {
        let mut config = Config::default();
        config.terminal.shell = Some(
            std::env::current_exe()
                .unwrap()
                .join("unavailable-shell")
                .to_string_lossy()
                .into_owned(),
        );
        let mut app = App::new(Theme::default(), config, Keymap::default());
        app.__test_seed_tab("first");
        app.__test_seed_tab("second");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["first", "second"]);
        app.__test_set_main_pane_viewport(outer, 10.0, 20.0);
        app.__test_set_child_pane_viewport(child, outer, 10.0, 20.0);
        app.resize_visible_panes();
        super::child_window::resize_visible_panes_in_child(app.windows.get_mut(&child).unwrap());
        let (source, target) = if child_source { (child, main) } else { (main, child) };
        let tab = app.windows[&source].tabs.active().unwrap().id;
        let area = sonicterm_ui::ime::ImeCursorArea {
            pane_id: app.windows[&source].tab_states[0].active_pane,
            position: (10, 40),
            size: (10, 20),
        };
        for action in [
            Action::SplitRight,
            Action::SplitDown,
            Action::FocusPane(Direction::Up),
            Action::TogglePaneZoom,
            Action::SplitRight,
            Action::ClosePane,
            Action::NextTab,
            Action::PrevTab,
        ] {
            let window = app.windows.get_mut(&source).unwrap();
            window.ime_cursor_throttle.should_update(area);
            assert!(!window.ime_cursor_throttle.should_update(area));
            assert!(app.run_action_for_window(&action, source));
            assert!(app.windows.get_mut(&source).unwrap().ime_cursor_throttle.should_update(area));
            assert_window(&app, source);
            assert_window(&app, target);
        }
        let index = app.tab_index_of_id(source, tab).unwrap();
        assert!(app.windows.get_mut(&source).unwrap().reorder_tab(index, 1 - index));
        assert_eq!(app.windows[&source].tabs.active().unwrap().id, tab);
        assert_window(&app, source);
        app.__test_charge_pane_owners();
        let before = app.governor.snapshot(app.governor.root_owner()).unwrap().process_amount;
        let index = app.tab_index_of_id(source, tab).unwrap();
        app.transfer_tab(Some(source), index, Some(target), 1).unwrap();
        assert_eq!(app.windows[&target].tabs.active().unwrap().id, tab);
        assert_window(&app, source);
        assert_window(&app, target);
        assert_eq!(
            app.governor.snapshot(app.governor.root_owner()).unwrap().process_amount,
            before
        );
    }
}

/// Accumulates subscriber output so a test can assert on emitted warnings.
#[derive(Clone, Default)]
struct ResizeWarningLog(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for ResizeWarningLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `action` under a local `warn` subscriber and return what it logged.
fn capture_resize_warnings(action: impl FnOnce()) -> String {
    let log = ResizeWarningLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, action);
    let output = log.0.lock().clone();
    String::from_utf8(output).unwrap()
}

/// How many `pty resize failed` warnings `log` contains.
fn warning_count(log: &str) -> usize {
    log.matches("pty resize failed").count()
}

/// A pane whose PTY resize fails on demand, counting native attempts.
///
/// The spawned handle's own resize closure is the sole owner of the PTY master
/// — teardown closes the master by dropping that closure — so the probe moves
/// it into the replacement rather than assigning over it. Dropping it here
/// would release the master before `PtyHandle::drop`, which on Windows races
/// `ClosePseudoConsole` against the drain. The success path delegates to it, so
/// the original stays live and the native resize still happens.
fn pane_with_failing_resize() -> (PaneState, Arc<AtomicUsize>, Arc<AtomicBool>) {
    #[cfg(unix)]
    let (cmd, args) = ("/bin/cat", Vec::<String>::new());
    #[cfg(windows)]
    let (cmd, args) = ("cmd.exe", Vec::<String>::new());
    let mut pty =
        sonicterm_io::pty::PtyHandle::spawn_with_args(cmd, &args, 80, 24).expect("spawn probe pty");

    let calls = Arc::new(AtomicUsize::new(0));
    let fail = Arc::new(AtomicBool::new(true));
    let seen = calls.clone();
    let should_fail = fail.clone();
    let original = std::mem::replace(&mut pty.resize, Box::new(|_, _| Ok(())));
    pty.resize = Box::new(move |cols, rows| {
        seen.fetch_add(1, Ordering::Relaxed);
        if should_fail.load(Ordering::Relaxed) {
            // When: `should_fail` is set, report a native refusal without touching the pty.
            return Err(anyhow::anyhow!("native resize refused"));
        }
        original(cols, rows)
    });

    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    (PaneState::new(parser, Some(pty)), calls, fail)
}

/// A pane with no PTY is not a resize failure and must not warn.
#[test]
fn a_pane_without_a_pty_is_not_a_resize_failure() {
    let pane = PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(80, 24)))), None);

    let log = capture_resize_warnings(|| pane.resize_pty(7, 100, 30));

    assert_eq!(warning_count(&log), 0, "a pane with no PTY has no native geometry to fail at");
    assert!(!pane.resize_warned.load(Ordering::Relaxed), "an absent PTY must not latch");
}

/// The first failure warns once with pane id, requested geometry, and error.
#[test]
fn the_first_failure_warns_once_with_metadata_and_no_payload() {
    let (pane, calls, _fail) = pane_with_failing_resize();
    // A sentinel the warning must never carry: resize reporting is metadata-only.
    pane.pty
        .as_ref()
        .expect("probe pty")
        .send_input_nonblocking(b"SONICTERM_PAYLOAD_SENTINEL\n".to_vec())
        .expect("queue probe input");

    let log = capture_resize_warnings(|| pane.resize_pty(7, 100, 30));

    assert_eq!(warning_count(&log), 1, "the first failure reports exactly once");
    assert!(log.contains("pane_id=7"), "the warning names the pane:\n{log}");
    assert!(log.contains("cols=100"), "the warning names the requested columns:\n{log}");
    assert!(log.contains("rows=30"), "the warning names the requested rows:\n{log}");
    assert!(log.contains("native resize refused"), "the warning carries the error:\n{log}");
    assert!(
        !log.contains("SONICTERM_PAYLOAD_SENTINEL"),
        "the warning must carry no terminal payload:\n{log}"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the native call was attempted");
}

/// Repeated failures warn once while every request still reaches the native call.
#[test]
fn repeated_failures_warn_once_but_never_stop_retrying() {
    let (pane, calls, _fail) = pane_with_failing_resize();

    let log = capture_resize_warnings(|| {
        pane.resize_pty(7, 100, 30);
        pane.resize_pty(7, 110, 32);
        pane.resize_pty(7, 120, 34);
    });

    assert_eq!(warning_count(&log), 1, "a failing run reports once, not once per request:\n{log}");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        3,
        "suppression is warning-side only; every request must still reach native"
    );
}

/// A success clears the latch, so the next failure warns a second time.
#[test]
fn a_success_clears_the_latch_so_a_later_failure_warns_again() {
    let (pane, calls, fail) = pane_with_failing_resize();

    let log = capture_resize_warnings(|| {
        pane.resize_pty(7, 100, 30);
        fail.store(false, Ordering::Relaxed);
        pane.resize_pty(7, 110, 32);
        fail.store(true, Ordering::Relaxed);
        pane.resize_pty(7, 120, 34);
    });

    assert_eq!(
        warning_count(&log),
        2,
        "the failure after a success is a new run and reports again:\n{log}"
    );
    assert!(log.contains("cols=100"), "the first run's geometry is reported:\n{log}");
    assert!(log.contains("cols=120"), "the second run's geometry is reported:\n{log}");
    assert_eq!(calls.load(Ordering::Relaxed), 3, "each distinct size reached native");
}

/// The grid keeps the requested geometry when the native resize fails.
///
/// Exercised through `resize_all_panes` rather than `resize_pty` directly,
/// because the ordering under test — grid first, native second, no rollback —
/// belongs to the caller.
#[test]
fn a_failed_native_resize_leaves_the_grid_committed() {
    let (pane, calls, _fail) = pane_with_failing_resize();
    let parser = pane.parser.clone();
    let mut panes = HashMap::new();
    panes.insert(7u64, pane);

    let log = capture_resize_warnings(|| resize_all_panes(&panes, 100, 30));

    let (cols, rows) = {
        let guard = parser.lock();
        let grid = guard.grid();
        (grid.cols, grid.rows)
    };
    assert_eq!((cols, rows), (100, 30), "the grid keeps the geometry the user asked for");
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the native call was attempted and failed");
    assert_eq!(warning_count(&log), 1, "the failure was reported once:\n{log}");
}
