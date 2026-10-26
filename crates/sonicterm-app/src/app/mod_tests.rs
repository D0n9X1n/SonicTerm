//! App routing, PTY admission, and resize reporting at the app seam.

use super::*;
use sonicterm_cfg::keymap::Direction;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The compatibility helper preserves the shared text encoder's bytes for every guard choice.
#[test]
fn wrap_paste_keeps_shared_text_encoding() {
    for text in ["", "paste", "你好é\r\n\x1b[201~"] {
        for bracketed in [false, true] {
            let expected = sonicterm_types::encode_payload(
                &sonicterm_types::UserPayload::Text(text.into()),
                sonicterm_types::PasteTarget {
                    bracketed,
                    dialect: sonicterm_types::ShellDialect::Unknown,
                },
                usize::MAX,
            )
            .unwrap();
            assert_eq!(wrap_paste(text, bracketed), expected);
        }
    }
}

/// Exit retires hidden main and child panes once, including headless panes without a native transport.
#[test]
fn finish_session_drains_every_window_and_is_idempotent() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(media::InlineMediaPool::new());
    app.__test_seed_tab("hidden main");
    let child = app.__test_seed_child_window(&["child"]);
    app.main_mut().unwrap().hidden = true;
    app.reconcile_pane_owners();
    let owners: Vec<_> = app
        .windows
        .values()
        .flat_map(|window| {
            window.panes.values().filter_map(|pane| pane.owner.as_ref().map(OwnerGuard::id))
        })
        .collect();

    assert!(app.finish_session());
    assert!(app.windows.values().all(|window| window.panes.is_empty()));
    assert!(app.windows.contains_key(&child), "native windows remain owned until App drop");
    assert!(owners.into_iter().all(|owner| !app.__test_owner_is_open(owner)));
    assert!(app.finish_session(), "a repeated finish returns the cached disposition");
    assert_eq!(app.pty_reaper.path_counts(), (0, 0));
}

/// Every permanent close route submits owned native custody, while a live pane transfer preserves its reservation.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_close_paths_take_no_fallback() {
    if isolated() {
        return;
    }
    for route in [
        "pty-main",
        "pty-child",
        "tab-main",
        "pane-main",
        "window-child",
        "straggler-child",
        "tab-child",
        "pane-or-tab-child",
        "pane-child",
        "exit-hidden",
        "accepted-handoff",
        "replaced-main",
        "uninstalled",
    ] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default())
            .with_capture_staging_pool(CaptureStagingPool::new())
            .with_inline_media_pool(media::InlineMediaPool::new());
        let main_pane = app.__test_seed_tab("main");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["child"]);
        let child_pane = app.windows[&child].tab_states[0].active_pane;
        let in_child = route.ends_with("child");
        let (window, pane) = if in_child { (child, child_pane) } else { (main, main_pane) };
        if matches!(route, "pane-main" | "pane-child" | "pane-or-tab-child") {
            let extra = next_pane_id();
            let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
                Grid::new(80, 24),
                None,
                Arc::clone(&app.capture_staging_pool),
            )));
            let extra_pane = PaneState::new_with_media_pool(parser, None, &app.inline_media_pool);
            let state = app.windows.get_mut(&window).unwrap();
            assert!(state.tab_states[0].tree.split(pane, Direction::Right, extra));
            state.panes.insert(extra, extra_pane);
        }
        attach_idle_pty(&mut app, window, pane);
        app.reconcile_pane_owners();
        assert!(app.windows[&window].panes[&pane].reap_slot.is_some(), "{route}");
        let probe = app.windows[&window].panes[&pane].pty.as_ref().unwrap().child_exit_probe();
        let pid = app.windows[&window].panes[&pane].pty.as_ref().unwrap().pid().unwrap();
        let pane_owner = app.windows[&window].panes[&pane].owner.as_ref().unwrap().id();
        match route {
            "pty-main" | "pty-child" => {
                assert!(app.close_pty_pane(pane));
            }
            "tab-main" => app.close_tab_at(0),
            "pane-main" => app.close_active_pane(),
            "window-child" => {
                assert!(app.close_child_window(child));
            }
            "straggler-child" => {
                app.windows.get_mut(&child).unwrap().tabs = TabBar::new();
                app.windows.get_mut(&child).unwrap().tab_states.clear();
                app.reap_empty_child(child);
            }
            "tab-child" => {
                assert!(app.close_tab_at_in_child(child, 0));
            }
            "pane-or-tab-child" => {
                assert!(app.close_active_pane_or_tab_in_child(child));
            }
            "pane-child" => {
                assert!(app.close_active_pane_in_child(child));
            }
            "exit-hidden" => {
                app.main_mut().unwrap().hidden = true;
                assert!(app.finish_session(), "{route}");
            }
            "accepted-handoff" => {
                app.__test_set_os_drag_sink(Arc::new(RetiringSink));
                assert!(app.try_os_drag_handoff(0));
            }
            "replaced-main" => app.retire_previous_main(),
            "uninstalled" => {
                let detached = app.windows.get_mut(&window).unwrap().remove_pane(pane).unwrap();
                app.retire_pane(detached);
            }
            _ => unreachable!(),
        }
        let paths = app.pty_reaper.path_counts();
        let settled = app.finish_session();
        assert_eq!(paths, (1, 0), "{route}");
        assert!(settled, "{route} left native custody unsettled");
        assert!(!app.__test_owner_is_open(pane_owner), "{route} retained the UI owner");
        assert!(probe.has_exited().expect("reaped child observation"), "{route}");
        record_process(pid, false);
    }
}

/// A spawned main-tab pane with no destination still retires through the reserved native path.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_uninstalled_main_tab_uses_reaper() {
    if isolated() {
        return;
    }
    let mut config = Config::default();
    config.terminal.shell = Some(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into());
    let mut app = App::new(Theme::default(), config, Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(media::InlineMediaPool::new());
    assert!(app.main_window_id.is_none());
    app.new_tab_with_launch("uninstalled", pane_launch::PaneLaunch::default());
    let paths = app.pty_reaper.path_counts();
    assert!(app.finish_session(), "uninstalled native transport did not settle");
    assert_eq!(paths, (1, 0));
    assert!(app.windows.is_empty());
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
    let source = include_str!("window_setup.rs");
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

/// An enabled status line adds its own height to the initial window, with or without the tab
/// bar, so a window opened with `[window]` rows keeps that many terminal rows.
#[test]
fn configured_window_size_adds_the_status_line_when_enabled() {
    let mut config = Config::default();
    let without = configured_window_size(&config, true);
    config.status_line.enabled = true;
    let strip = f64::from(sonicterm_gpu::status_strip::status_line_height(config.font.size, 1.0));
    assert!(strip >= 20.0);
    assert_eq!(configured_window_size(&config, true).height, without.height + strip);
    assert_eq!(configured_window_size(&config, true).width, without.width);
    config.status_line.enabled = false;
    let bare = configured_window_size(&config, false);
    config.status_line.enabled = true;
    assert_eq!(configured_window_size(&config, false).height, bare.height + strip);
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
                    (0..window.tabs.len())
                        .map(|tab_index| window.tabs.tabs()[tab_index].id)
                        .collect()
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
                    (0..window.tabs.len())
                        .map(|tab_index| window.tabs.tabs()[tab_index].id)
                        .collect()
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

#[path = "test_hooks_app_resize_fixture.rs"]
mod test_hooks_app_resize_fixture;
use test_hooks_app_resize_fixture::*;

#[path = "test_hooks_app_input_fixture.rs"]
mod test_hooks_app_input_fixture;
pub(super) use test_hooks_app_input_fixture::*;

use super::pty_test_support::{isolated, record_process};
