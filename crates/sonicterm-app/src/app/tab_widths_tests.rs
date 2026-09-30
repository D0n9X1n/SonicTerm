use super::{settle_tab_widths, tab_widths_held};
use crate::app::App;
use crate::tab_drag::{compute_action, find_drop_target, DragSession, WindowGeom};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_gpu::core::{PresentOutcome, SkipReason, SurfaceRetryReason};
use sonicterm_ui::tabbar_view::{self, TabBarLayout, TabHit};
use sonicterm_ui::tabs::TabContent;
use std::time::Instant;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

/// The vertical band a 40 px bar occupies at the bottom of a 600 px window.
const BAR_BAND: Option<(f32, f32)> = Some((560.0, 600.0));
/// A pointer resting on that bar.
const ON_BAR: (f32, f32) = (450.0, 580.0);
/// A pointer over terminal content.
const ON_CONTENT: (f32, f32) = (450.0, 300.0);

/// Stand-in for the tab font: ten pixels per character of the drawn text.
fn ten_px_per_char(content: &TabContent<'_>) -> Option<f32> {
    Some(content.display_text().chars().count() as f32 * 10.0)
}

/// A headless App with a main window and a child window, each with three titled tabs.
fn two_windows() -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["zsh", "cargo build", "vim"]);
    let main = app.main_window_id.expect("the child seed creates a synthetic main window");
    for title in ["htop", "~/work/sonicterm", "ssh prod"] {
        let _ = app.__test_seed_tab(title);
    }
    (app, main, child)
}

/// Deliver `event` to `window` in the dispatcher's order: record the window's pointer, then let
/// an open overlay consume the event. Returns whether the overlay consumed it.
fn dispatch(app: &mut App, window: WindowId, event: &WindowEvent) -> bool {
    app.record_window_pointer(window, event);
    app.command_palette_handle_pointer_event(window, event)
}

/// Move the pointer to `point` in `window`; returns whether an open overlay consumed the move.
fn move_pointer(app: &mut App, window: WindowId, point: (f32, f32)) -> bool {
    let position = PhysicalPosition::new(f64::from(point.0), f64::from(point.1));
    dispatch(app, window, &WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position })
}

/// Measure one window's tabs the way its redraw does, holding them by the same rule.
fn remeasure(app: &mut App, window: WindowId) {
    let hold = app.tab_widths_held_in(window, BAR_BAND);
    let state = app.windows.get_mut(&window).expect("window");
    state.tabs.refresh_content_widths(Instant::now(), false, 1, hold, ten_px_per_char);
}

/// The layout the renderer draws for a 900 × 600 window with its bar at the bottom.
fn drawn_layout(app: &App, window: WindowId) -> TabBarLayout {
    TabBarLayout::compute_with_insertion_slot(&app.windows[&window].tabs, 900.0, 40.0, None)
        .with_top_offset(560.0)
}

/// The layout the pointer, drag, tear-out and snapshot paths compute for the same window.
fn pointer_layout(app: &App, window: WindowId) -> TabBarLayout {
    TabBarLayout::compute_with_height(&app.windows[&window].tabs, 900.0, 40.0)
        .with_top_offset(560.0)
        .with_visible(true)
}

fn tab_rects(layout: &TabBarLayout) -> Vec<(usize, f32, f32)> {
    layout.tabs.iter().map(|tab| (tab.idx, tab.bg_rect.x, tab.bg_rect.w)).collect()
}

fn retitle(app: &mut App, window: WindowId, position: usize, title: &str) {
    let tabs = &mut app.windows.get_mut(&window).expect("window").tabs;
    let id = tabs.tabs()[position].id;
    tabs.set_title(id, title);
}

/// Every drawn tab's centre selects that tab, and every drawn gap is the slot a drop there
/// lands in, locally and through the cross-window drop-target search.
fn assert_pointer_matches_drawing(app: &App, window: WindowId) {
    let drawn = drawn_layout(app, window);
    let pointer = pointer_layout(app, window);
    assert_eq!(tab_rects(&pointer), tab_rects(&drawn));
    let origin = (100, 50);
    let geom = WindowGeom::new(origin, (900, 600));
    for (position, widget) in drawn.tabs.iter().enumerate() {
        let centre_x = widget.bg_rect.x + widget.bg_rect.w * 0.5;
        assert_eq!(pointer.hit(centre_x, 580.0), Some(TabHit::Activate(widget.idx)));
        let Some(next) = drawn.tabs.get(position + 1) else {
            continue;
        };
        let gap_x = (widget.bg_rect.x + widget.bg_rect.w + next.bg_rect.x) * 0.5;
        assert_eq!(pointer.drop_slot(gap_x, 580.0), next.idx);
        assert_eq!(drawn.insertion_x(next.idx), Some(gap_x));
        let target = find_drop_target(
            (origin.0 + gap_x.round() as i32, origin.1 + 580),
            [(window, geom, pointer.clone())],
        )
        .expect("the drop lands on the drawn bar");
        assert_eq!(target.slot, next.idx);
    }
}

/// Press the tab drawn under `point`, through the activation and drag-session setup the
/// production left-button handlers run after their hit test.
fn press_tab(app: &mut App, window: WindowId, point: (f32, f32)) -> usize {
    move_pointer(app, window, point);
    let Some(TabHit::Activate(index)) = drawn_layout(app, window).hit(point.0, point.1) else {
        panic!("{point:?} is not on a drawn tab");
    };
    if Some(window) == app.main_window_id {
        assert!(app.activate_main_tab(index));
    } else {
        let state = app.windows.get_mut(&window).expect("child");
        state.tabs.activate(index);
        crate::app::child_window::resize_visible_panes_in_child(state);
    }
    let state = app.windows.get_mut(&window).expect("window");
    state.mouse_down = true;
    state.pressed_tab = Some(index);
    state.drag_session =
        state.tabs.tabs().get(index).map(|tab| DragSession::new(window, tab.id, point));
    index
}

/// Release the pointer at `point`, through the drop decision and drag finish the production
/// left-button handlers run: `compute_action` against the bar as laid out, then
/// `finish_tab_drag`.
fn release_tab(app: &mut App, window: WindowId, point: (f32, f32)) {
    move_pointer(app, window, point);
    let state = app.windows.get_mut(&window).expect("window");
    let mut session = state.drag_session.take().expect("a pressed tab has a drag session");
    let foreign = state.drag_target.take();
    assert!(state.pressed_tab.take().is_some());
    state.mouse_down = false;
    // The motion handlers keep the session at the pointer; the release reads it there.
    session.current_pos = point;
    let layout = pointer_layout(app, window);
    let index = app.tab_index_of_id(window, session.source_tab).expect("the pressed tab is open");
    let action = compute_action(&session, foreign, &layout, index);
    assert!(app.finish_tab_drag(session, action, |_, _, _| panic!("a drop on the bar tore out")));
}

#[test]
fn a_pressed_or_dragged_tab_holds_every_bar_and_a_resting_pointer_holds_its_own() {
    // A drag can end on any window's bar, so a pressed or dragged tab anywhere holds every
    // bar; a pointer resting on a bar holds only that window's bar, read from its own pointer.
    assert!(tab_widths_held(true, false) && tab_widths_held(false, true));
    assert!(!tab_widths_held(false, false));
    let (mut app, main, child) = two_windows();
    move_pointer(&mut app, main, ON_BAR);
    move_pointer(&mut app, child, ON_CONTENT);
    assert!(app.tab_widths_held_in(main, BAR_BAND), "a resting pointer holds its bar");
    assert!(!app.tab_widths_held_in(child, BAR_BAND));
    assert!(!app.tab_widths_held_in(main, None), "a hidden bar holds nothing");

    move_pointer(&mut app, main, ON_CONTENT);
    app.windows.get_mut(&child).expect("child").pressed_tab = Some(1);
    assert!(app.tab_widths_held_in(main, BAR_BAND));
    app.windows.get_mut(&child).expect("child").pressed_tab = None;
    let source_tab = app.windows[&main].tabs.tabs()[0].id;
    app.windows.get_mut(&main).expect("main").drag_session =
        Some(DragSession::new(main, source_tab, ON_BAR));
    assert!(app.tab_widths_held_in(child, BAR_BAND));
    app.windows.get_mut(&main).expect("main").drag_session = None;
    assert!(!app.tab_gesture_active());
    assert!(!app.tab_widths_held_in(main, BAR_BAND));
}

#[test]
fn a_still_pointer_holds_its_bar_through_a_modal_rename() {
    // Rename Tab consumes pointer events while its editor is open. The pointer moves onto the
    // bar during the edit and a much longer title is submitted without moving: no tab moves
    // under the still pointer, and the title lays out once the pointer leaves the bar.
    let (mut app, main, child) = two_windows();
    for (window, frontmost) in [(main, None), (child, Some(child))] {
        move_pointer(&mut app, window, ON_CONTENT);
        remeasure(&mut app, window);
        app.__test_set_frontmost_window(frontmost);
        app.start_rename_active_tab();
        assert!(app.__test_palette_open());
        assert!(move_pointer(&mut app, window, ON_BAR), "the editor consumes the move");
        let held = tab_rects(&drawn_layout(&app, window));

        app.__test_set_palette_query("cargo test --workspace --all-targets --release");
        assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
        assert!(!app.__test_palette_open());
        remeasure(&mut app, window);
        assert_eq!(tab_rects(&drawn_layout(&app, window)), held, "a tab moved under the pointer");
        assert_pointer_matches_drawing(&app, window);

        move_pointer(&mut app, window, ON_CONTENT);
        remeasure(&mut app, window);
        assert_ne!(tab_rects(&drawn_layout(&app, window)), held, "the title never laid out");
        assert_pointer_matches_drawing(&app, window);
    }
}

#[test]
fn every_pointer_move_and_leave_is_recorded_before_any_handler() {
    // The dispatcher records the window's own pointer on every CursorMoved and CursorLeft
    // before an overlay, a modal or a window handler can consume the event or return early.
    let (mut app, main, child) = two_windows();
    for window in [main, child] {
        move_pointer(&mut app, window, ON_BAR);
        assert_eq!(app.windows[&window].cursor_pos, (450.0, 580.0));
        dispatch(&mut app, window, &WindowEvent::Focused(true));
        assert_eq!(app.windows[&window].cursor_pos, (450.0, 580.0), "only pointer events record");
        dispatch(&mut app, window, &WindowEvent::CursorLeft { device_id: DeviceId::dummy() });
        assert_eq!(app.windows[&window].cursor_pos, (-1.0, -1.0));
    }
    let source = include_str!("window_event.rs");
    let record = source
        .find("self.record_window_pointer(win_id, &event);")
        .expect("the dispatcher records the pointer");
    let overlay =
        source.find("self.command_palette_handle_pointer_event(win_id, &event)").expect("overlay");
    let child_route =
        source.find("self.handle_child_window_event(event_loop, win_id, event)").expect("child");
    assert!(record < overlay && overlay < child_route, "the pointer is recorded first");
}

#[test]
fn a_frame_that_does_not_present_keeps_the_drawn_widths() {
    // A redraw lays a changed title out before it draws. If its frame does not present, the
    // screen still shows the old bar, so clicks and drops keep resolving against it; the
    // new width applies with the next frame that presents.
    let (mut app, main, _) = two_windows();
    move_pointer(&mut app, main, ON_CONTENT);
    remeasure(&mut app, main);
    let on_screen = tab_rects(&drawn_layout(&app, main));
    let outcomes = [
        PresentOutcome::AtlasRetry,
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Timeout),
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Occluded),
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Outdated),
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::SurfaceLost),
        PresentOutcome::Skipped(SkipReason::NoPanes),
        PresentOutcome::CachedReblit,
        PresentOutcome::Failed(anyhow::anyhow!("frame failed")),
    ];
    for (round, outcome) in outcomes.into_iter().enumerate() {
        let drawn = app.windows[&main].tabs.laid_out_widths();
        retitle(&mut app, main, 0, &format!("cargo test --workspace --all-targets {round}"));
        remeasure(&mut app, main);
        assert_ne!(tab_rects(&drawn_layout(&app, main)), on_screen, "{outcome:?} laid nothing out");
        let tabs = &mut app.windows.get_mut(&main).expect("main").tabs;
        settle_tab_widths(tabs, drawn, &outcome);
        assert_eq!(tab_rects(&pointer_layout(&app, main)), on_screen, "{outcome:?} moved the bar");
        assert_pointer_matches_drawing(&app, main);
    }
    let drawn = app.windows[&main].tabs.laid_out_widths();
    remeasure(&mut app, main);
    let tabs = &mut app.windows.get_mut(&main).expect("main").tabs;
    settle_tab_widths(tabs, drawn, &PresentOutcome::Presented);
    assert_ne!(tab_rects(&pointer_layout(&app, main)), on_screen, "a presented frame reverted");
    assert_pointer_matches_drawing(&app, main);
}

#[test]
fn clicks_and_drops_land_on_the_drawn_bar_in_main_and_child_windows() {
    // A click selects the tab drawn under the pointer and a drop lands in the drawn gap, in a
    // main and a child window. A title change between press and release moves no pressed or
    // dragged tab, nor one under the still pointer after the release.
    let (mut app, main, child) = two_windows();
    for window in [main, child] {
        move_pointer(&mut app, window, ON_CONTENT);
        remeasure(&mut app, window);
        let drawn = tab_rects(&drawn_layout(&app, window));
        let centre = |position: usize| {
            let (_, left, width) = drawn[position];
            (left + width * 0.5, ON_BAR.1)
        };

        assert_eq!(press_tab(&mut app, window, centre(2)), 2);
        retitle(&mut app, window, 0, "cargo test --workspace --all-targets");
        remeasure(&mut app, window);
        assert_eq!(tab_rects(&drawn_layout(&app, window)), drawn, "a pressed tab's bar moved");
        release_tab(&mut app, window, centre(2));
        assert_eq!(app.windows[&window].tabs.active_index(), 2);
        remeasure(&mut app, window);
        assert_eq!(tab_rects(&drawn_layout(&app, window)), drawn, "a still pointer's bar moved");
        assert_pointer_matches_drawing(&app, window);

        let order: Vec<_> = app.windows[&window].tabs.tabs().iter().map(|tab| tab.id).collect();
        assert_eq!(press_tab(&mut app, window, centre(2)), 2);
        let gap = ((drawn[0].1 + drawn[0].2 + drawn[1].1) * 0.5, ON_BAR.1);
        release_tab(&mut app, window, gap);
        let dropped: Vec<_> = app.windows[&window].tabs.tabs().iter().map(|tab| tab.id).collect();
        assert_eq!(dropped, vec![order[0], order[2], order[1]], "the drop missed the drawn gap");
        remeasure(&mut app, window);
        assert_eq!(tab_rects(&drawn_layout(&app, window)), drawn, "the drop moved a held width");

        move_pointer(&mut app, window, ON_CONTENT);
        remeasure(&mut app, window);
        assert_ne!(tab_rects(&drawn_layout(&app, window)), drawn, "the title never laid out");
        assert_pointer_matches_drawing(&app, window);
    }
}

#[test]
fn a_width_limit_reload_lays_held_bars_out_again_and_redraws() {
    // A tab_min_width or tab_max_width reload through config apply lays every bar out again at
    // once, even a held one, and marks every window for a redraw. The limits are scoped to this
    // test's thread, so no other test sees them.
    tabbar_view::with_scoped_tab_width_limits(|| {
        let (mut app, main, child) = two_windows();
        for window in [main, child] {
            move_pointer(&mut app, window, ON_BAR);
            remeasure(&mut app, window);
        }
        app.windows.get_mut(&child).expect("child").pressed_tab = Some(0);
        let before = [main, child].map(|window| tab_rects(&drawn_layout(&app, window)));
        let redraws = [main, child].map(|window| app.windows[&window].redraw.snapshot());

        let mut reloaded = app.config.clone();
        reloaded.tab_min_width = 120.0;
        reloaded.tab_max_width = 200.0;
        app.apply_new_config(reloaded);

        assert_eq!((tabbar_view::min_tab_width(), tabbar_view::max_tab_width()), (120.0, 200.0));
        for (position, window) in [main, child].into_iter().enumerate() {
            assert!(app.tab_widths_held_in(window, BAR_BAND));
            assert_ne!(tab_rects(&drawn_layout(&app, window)), before[position]);
            assert_ne!(app.windows[&window].redraw.snapshot(), redraws[position]);
            assert_pointer_matches_drawing(&app, window);
        }
    });
}

#[test]
fn a_font_reload_redraws_every_window() {
    // Config apply marks every window for a redraw, and that redraw measures the tabs with the
    // new font key, which lays even a held bar out again at once.
    let (mut app, main, child) = two_windows();
    let redraws = [main, child].map(|window| app.windows[&window].redraw.snapshot());
    let mut reloaded = app.config.clone();
    reloaded.font.size += 2.0;
    app.apply_new_config(reloaded);
    for (position, window) in [main, child].into_iter().enumerate() {
        assert_ne!(app.windows[&window].redraw.snapshot(), redraws[position]);
    }
}

#[test]
fn a_font_or_dpi_reload_lays_a_held_bar_out_again() {
    // A font or DPI change gives the bar a new font key, so it lays out again at once even
    // while a tab is pressed, and clicks and drops follow the bar it draws next.
    let (mut app, _, child) = two_windows();
    move_pointer(&mut app, child, ON_CONTENT);
    remeasure(&mut app, child);
    let before = tab_rects(&drawn_layout(&app, child));

    app.windows.get_mut(&child).expect("child").pressed_tab = Some(1);
    let hold = app.tab_widths_held_in(child, BAR_BAND);
    assert!(hold, "a pressed tab holds the bar");
    // A larger font triples every drawn advance, so the longest title passes tab_min_width,
    // and changes the font key from 1 to 2.
    let tabs = &mut app.windows.get_mut(&child).expect("child").tabs;
    tabs.refresh_content_widths(Instant::now(), false, 2, hold, |content| {
        ten_px_per_char(content).map(|width_px| width_px * 3.0)
    });

    assert_ne!(tab_rects(&drawn_layout(&app, child)), before, "a font change was held");
    assert_pointer_matches_drawing(&app, child);
    assert!(app.windows_with_held_tab_widths().is_empty());
}

#[test]
fn a_dpi_change_reaches_the_renderer_that_measures_the_tabs() {
    // winit alone builds a ScaleFactorChanged event, so this pins its route: both windows hand
    // the new scale to the renderer, whose tab font key includes it, and the dispatcher marks
    // the window for the redraw that measures the tabs again.
    for (name, source) in
        [("main", include_str!("window_event.rs")), ("child", include_str!("child_window.rs"))]
    {
        let arm = source
            .find("WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale")
            .expect("scale arm");
        let transition = source[arm..].find("apply_window_dpi_transition(").expect("transition");
        let next_arm = source[arm + 1..].find("WindowEvent::").expect("next arm");
        assert!(transition < next_arm, "{name} applies the DPI transition in its scale arm");
    }
    assert!(
        include_str!("window_setup.rs").contains("renderer.set_scale_factor(dpi_scale as f32);")
    );
    let dispatcher = include_str!("window_event.rs");
    let mark = dispatcher
        .find("window.mark_redraw(super::redraw::RedrawCause::Input);")
        .expect("input mark");
    let marked = &dispatcher[..mark];
    let list = marked.rfind("matches!(").expect("marked event list");
    assert!(marked[list..].contains("WindowEvent::ScaleFactorChanged { .. }"));
}

#[test]
fn only_the_redraw_paths_measure_tab_widths() {
    // Each redraw path holds by its own window's pointer, keeps the drawn widths, measures once
    // right before drawing and settles the widths on the frame's outcome, so the pointer, drag,
    // tear-out and snapshot paths read the widths of the frame on screen.
    for (name, source) in [
        ("main", include_str!("window_event.rs")),
        ("child", include_str!("child_window_redraw.rs")),
    ] {
        let hold = source
            .find("self.tab_widths_held_in(win_id, tab_bar_band)")
            .unwrap_or_else(|| panic!("{name} holds by its own window's pointer"));
        let refresh = source.find("refresh_active_tab_title(").expect("title refresh");
        let drawn = source
            .find(".laid_out_widths();")
            .unwrap_or_else(|| panic!("{name} keeps the drawn widths"));
        let measure = source.find(".measure_tab_widths(").expect("measure");
        let render = source.find(".render_with_outcome(").expect("render call");
        let settle = source
            .find("settle_tab_widths(")
            .unwrap_or_else(|| panic!("{name} settles the widths on the outcome"));
        assert!(hold < drawn && refresh < drawn && drawn < measure, "{name} keeps then measures");
        assert!(measure < render && render < settle, "{name} settles after drawing");
        assert_eq!(source.matches(".measure_tab_widths(").count(), 1, "{name} measures once");
        assert!(!source.contains("pointer_over_tab_bar"), "{name} reads a renderer hover");
    }
    for (name, source) in [
        ("window_pointer", include_str!("window_pointer.rs")),
        ("child_window_pointer", include_str!("child_window_pointer.rs")),
        ("child_window", include_str!("child_window.rs")),
        ("os_drag", include_str!("os_drag.rs")),
        ("drag_target", include_str!("tear_out/drag_target.rs")),
    ] {
        assert!(!source.contains("measure_tab_widths"), "{name} must read the stored widths");
    }
}
