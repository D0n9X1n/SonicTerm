use super::{held_bar_release_wakes, settle_tab_widths, tab_widths_held};
use crate::app::tab_gesture::{TabPress, TabRelease};
use crate::app::App;
use crate::tab_drag::{find_drop_target, DragAction, DragSession, WindowGeom};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_gpu::core::{PresentOutcome, SkipReason, SurfaceRetryReason};
use sonicterm_ui::tabbar_view::{self, TabBarLayout, TabHit};
use sonicterm_ui::tabs::{TabContent, TabId};
use std::collections::HashMap;
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

/// A headless App with a main window and a child window. Each has a title wider than
/// tab_min_width between two short ones, so its tabs have unequal widths.
fn two_windows() -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["zsh", "cargo build --workspace", "vim"]);
    let main = app.main_window_id.expect("the child seed creates a synthetic main window");
    for title in ["htop", "~/work/sonicterm/crates/app", "ssh prod"] {
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

/// Press at `point` the way both production left-button handlers do: route the press against
/// the bar as drawn, then carry out the returned action.
fn press_at(app: &mut App, window: WindowId, point: (f32, f32)) -> TabPress {
    move_pointer(app, window, point);
    let layout = pointer_layout(app, window);
    let state = app.windows.get_mut(&window).expect("window");
    let press = state.route_tab_press(window, &layout, point);
    app.apply_tab_press(window, press);
    press
}

/// Move the pointer to `point` with the button down, through the motion routing both handlers
/// run; returns whether the move belonged to the tab gesture.
fn drag_to(app: &mut App, window: WindowId, point: (f32, f32)) -> bool {
    move_pointer(app, window, point);
    let layout = pointer_layout(app, window);
    let state = app.windows.get_mut(&window).expect("window");
    let motion = state.route_tab_motion(Some(&layout), point);
    app.apply_tab_motion(window, motion, (f64::from(point.0), f64::from(point.1)))
}

/// Release the button the way both handlers do: route the release against the bar as laid
/// out, then carry out the returned drop. A release on the bar never tears the tab out.
fn release_button(app: &mut App, window: WindowId) -> TabRelease {
    let layout = drawn_layout(app, window);
    let state = app.windows.get_mut(&window).expect("window");
    let release = state.route_tab_release(Some(&layout));
    assert!(!state.mouse_down && state.pressed_tab.is_none() && state.drag_session.is_none());
    app.apply_tab_release(release, |_, _, _| panic!("a drop on the bar tore out"));
    release
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
    // Presses, moves and releases go through the routing both production pointer handlers run,
    // in a main and a child window whose tabs have unequal widths: a press selects the tab drawn
    // under the pointer, a drop lands in the drawn gap, and a title change between press and
    // release moves no pressed tab, nor one under the still pointer after the release.
    let (mut app, main, child) = two_windows();
    for window in [main, child] {
        move_pointer(&mut app, window, ON_CONTENT);
        remeasure(&mut app, window);
        let drawn = tab_rects(&drawn_layout(&app, window));
        assert!(drawn[1].2 > drawn[0].2, "the wide title gets a wider tab: {drawn:?}");
        let centre = |position: usize| {
            let (_, left, width) = drawn[position];
            (left + width * 0.5, ON_BAR.1)
        };

        // A click selects the wide tab and leaves every tab in place.
        assert_eq!(press_at(&mut app, window, centre(1)), TabPress::Activate(1));
        assert_eq!(app.windows[&window].tabs.active_index(), 1);
        retitle(&mut app, window, 0, "cargo test --workspace --all-targets");
        remeasure(&mut app, window);
        assert_eq!(tab_rects(&drawn_layout(&app, window)), drawn, "a pressed tab's bar moved");
        let click = release_button(&mut app, window);
        assert!(
            matches!(click, TabRelease::Finish(_, DragAction::ReturnToOriginalBar)),
            "a click moved a tab: {click:?}"
        );
        assert_eq!(app.windows[&window].tabs.active_index(), 1);
        remeasure(&mut app, window);
        assert_eq!(tab_rects(&drawn_layout(&app, window)), drawn, "a still pointer's bar moved");
        assert_pointer_matches_drawing(&app, window);

        // A drag moves the last tab into the drawn gap between the first two.
        let widths = |state: &App| -> HashMap<TabId, Option<f32>> {
            let tabs = state.windows[&window].tabs.tabs();
            tabs.iter().map(|tab| (tab.id, tab.content_width_px())).collect()
        };
        let held = widths(&app);
        let order: Vec<TabId> = app.windows[&window].tabs.tabs().iter().map(|tab| tab.id).collect();
        assert_eq!(press_at(&mut app, window, centre(2)), TabPress::Activate(2));
        assert_eq!(app.windows[&window].tabs.active_index(), 2);
        let gap = ((drawn[0].1 + drawn[0].2 + drawn[1].1) * 0.5, ON_BAR.1);
        assert!(drag_to(&mut app, window, gap), "the drag left the tab gesture");
        let drop = release_button(&mut app, window);
        assert!(
            matches!(drop, TabRelease::Finish(_, DragAction::ReorderTab { to: 1 })),
            "the drop missed the drawn gap: {drop:?}"
        );
        let dropped: Vec<TabId> =
            app.windows[&window].tabs.tabs().iter().map(|tab| tab.id).collect();
        assert_eq!(dropped, vec![order[0], order[2], order[1]], "the drop missed the drawn gap");
        remeasure(&mut app, window);
        assert_eq!(widths(&app), held, "the drop moved a held width");
        assert_pointer_matches_drawing(&app, window);

        move_pointer(&mut app, window, ON_CONTENT);
        remeasure(&mut app, window);
        assert_ne!(widths(&app), held, "the title never laid out");
        assert_pointer_matches_drawing(&app, window);
    }
}

#[test]
fn a_width_limit_reload_reaches_hit_testing_only_with_a_presented_frame() {
    // A tab_min_width reload through config apply marks every window for a redraw, and that
    // redraw lays even a held bar out with the new limit. A frame that does not present keeps
    // the bar on screen, so a click lands on the tab drawn under it, in a main and a child
    // window; the new geometry reaches hit-testing with the next presented frame. The limits
    // stay on this test's thread.
    tabbar_view::with_scoped_tab_width_limits(|| {
        let (mut app, main, child) = two_windows();
        let click = (180.0, ON_BAR.1);
        for window in [main, child] {
            move_pointer(&mut app, window, click);
            remeasure(&mut app, window);
        }
        let on_screen = [main, child].map(|window| tab_rects(&drawn_layout(&app, window)));
        let redraws = [main, child].map(|window| app.windows[&window].redraw.snapshot());

        let mut reloaded = app.config.clone();
        reloaded.tab_min_width = 120.0;
        app.apply_new_config(reloaded);
        assert_eq!(tabbar_view::min_tab_width(), 120.0);

        for (position, window) in [main, child].into_iter().enumerate() {
            assert_ne!(app.windows[&window].redraw.snapshot(), redraws[position]);
            let before_frame = tab_rects(&pointer_layout(&app, window));
            assert_eq!(before_frame, on_screen[position], "the reload moved hit-testing early");
            assert!(app.tab_widths_held_in(window, BAR_BAND), "the pointer rests on the bar");

            // The redraw lays the held bar out with the new limit, and its frame times out.
            let drawn = app.windows[&window].tabs.laid_out_widths();
            remeasure(&mut app, window);
            let reloaded_rects = tab_rects(&drawn_layout(&app, window));
            assert_ne!(reloaded_rects, on_screen[position], "the reload was held");
            let timeout = PresentOutcome::SurfaceRetry(SurfaceRetryReason::Timeout);
            let tabs = &mut app.windows.get_mut(&window).expect("window").tabs;
            settle_tab_widths(tabs, drawn, &timeout);
            assert_eq!(tab_rects(&pointer_layout(&app, window)), on_screen[position]);
            let hit = pointer_layout(&app, window).hit(click.0, click.1);
            assert_eq!(hit, Some(TabHit::Activate(0)), "a click missed the tab drawn under it");
            assert_pointer_matches_drawing(&app, window);

            // The next redraw presents the new geometry, and hit-testing follows it.
            let drawn = app.windows[&window].tabs.laid_out_widths();
            remeasure(&mut app, window);
            let tabs = &mut app.windows.get_mut(&window).expect("window").tabs;
            settle_tab_widths(tabs, drawn, &PresentOutcome::Presented);
            assert_eq!(tab_rects(&pointer_layout(&app, window)), reloaded_rects);
            let hit = pointer_layout(&app, window).hit(click.0, click.1);
            assert_eq!(hit, Some(TabHit::Activate(1)), "the presented geometry was not used");
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
        let prepare = source.find(".begin_frame_fonts()").expect("fonts are prepared");
        let measure = source.find(".measure_tab_widths(").expect("measure");
        let render = source.find(".render_with_outcome(").expect("render call");
        let settle = source
            .find("settle_tab_widths(")
            .unwrap_or_else(|| panic!("{name} settles the widths on the outcome"));
        assert!(hold < drawn && refresh < drawn && drawn < measure, "{name} keeps then measures");
        assert!(measure < render && render < settle, "{name} settles after drawing");
        assert_eq!(source.matches(".measure_tab_widths(").count(), 1, "{name} measures once");
        // Preparation reads the fallback generation once, before any width is measured.
        assert!(drawn < prepare && prepare < measure, "{name} prepares fonts before measuring");
        assert_eq!(source.matches(".begin_frame_fonts()").count(), 1, "{name} prepares once");
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

#[test]
fn leaving_a_bar_that_holds_widths_still_requests_a_redraw() {
    // Tab hover repaints only when the hovered tab changes, so a pointer that
    // leaves from empty bar space no longer asks for a frame through hover. The
    // held widths must still lay out: the pointer record wakes a bar that holds
    // widths once the pointer leaves it, and only then.
    let (mut app, main, child) = two_windows();
    for window in [main, child] {
        // A bar is laid out once before a later title change can be held against it.
        move_pointer(&mut app, window, ON_CONTENT);
        remeasure(&mut app, window);
        move_pointer(&mut app, window, ON_BAR);
        retitle(&mut app, window, 0, "a title measured while the pointer rests on the bar");
        remeasure(&mut app, window);
        assert!(app.windows[&window].tabs.has_held_content_widths(), "the resting pointer holds");
        for leave in [(-1.0, -1.0), (450.0, 300.0)] {
            assert!(held_bar_release_wakes(false, true, leave, BAR_BAND), "{leave:?} wakes");
        }
        assert!(!held_bar_release_wakes(false, true, (450.0, 580.0), BAR_BAND), "still on bar");
        assert!(!held_bar_release_wakes(true, true, (-1.0, -1.0), BAR_BAND), "a drag still holds");
        assert!(!held_bar_release_wakes(false, false, (-1.0, -1.0), BAR_BAND), "nothing held");
    }
    // The record, not the hover update, owns the wake: it runs on every move and
    // leave, before any handler, and requests the window's redraw on the predicate.
    let source = include_str!("tab_widths.rs").replace("\r\n", "\n");
    let start = source.find("pub(super) fn record_window_pointer(").expect("record");
    let end = start + source[start..].find("\n    }\n").expect("body end");
    let record = &source[start..end];
    let guard = record.find("held_bar_release_wakes(").expect("the record tests the predicate");
    let wake = record.find("state.request_window_redraw();").expect("the record wakes the bar");
    assert!(guard < wake);
}
