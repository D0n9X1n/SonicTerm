use super::tab_widths_held;
use crate::app::App;
use crate::tab_drag::{find_drop_target, DragSession, WindowGeom};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::TabContent;
use std::time::Instant;
use winit::window::WindowId;

/// Stand-in for the tab font: ten pixels per character of the drawn text.
fn ten_px_per_char(content: &TabContent<'_>) -> Option<f32> {
    Some(content.display_text().chars().count() as f32 * 10.0)
}

/// A headless App with a main window and a child window, each with three titled tabs.
fn two_windows() -> (App, WindowId, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["zsh", "cargo build", "vim"]);
    let main = app.main_window_id.expect("the child seed creates a synthetic main window");
    let tabs = &mut app.windows.get_mut(&main).expect("main window").tabs;
    for title in ["htop", "~/work/sonicterm", "ssh prod"] {
        tabs.push(sonicterm_ui::tabs::Tab::new(title));
    }
    (app, main, child)
}

/// Measure one window's tabs the way its redraw does, with the pointer on or off its bar.
fn remeasure(app: &mut App, window: WindowId, pointer_over_bar: bool) {
    let hold = tab_widths_held(app.tab_gesture_active(), pointer_over_bar);
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

#[test]
fn a_pressed_or_dragged_tab_holds_every_bar_and_a_resting_pointer_holds_its_own() {
    // A drag can end on any window's bar, so a pressed or dragged tab anywhere holds every
    // bar, while a pointer resting on a bar holds only that bar.
    let (mut app, main, child) = two_windows();
    assert!(!app.tab_gesture_active());
    assert!(!tab_widths_held(app.tab_gesture_active(), false));
    assert!(tab_widths_held(app.tab_gesture_active(), true));

    app.windows.get_mut(&child).expect("child").pressed_tab = Some(1);
    assert!(app.tab_gesture_active());
    assert!(tab_widths_held(app.tab_gesture_active(), false));

    app.windows.get_mut(&child).expect("child").pressed_tab = None;
    let source_tab = app.windows[&main].tabs.tabs()[0].id;
    app.windows.get_mut(&main).expect("main").drag_session =
        Some(DragSession::new(main, source_tab, (20.0, 580.0)));
    assert!(app.tab_gesture_active());
    app.windows.get_mut(&main).expect("main").drag_session = None;
    assert!(!app.tab_gesture_active());
}

#[test]
fn clicks_and_drops_follow_the_drawn_bar_until_a_held_width_is_released() {
    // Clicks and drops resolve against the drawn bar in main and child windows. A title change
    // under a pressed tab, under a drag in another window, or under a still pointer moves no tab;
    // a release resolves against the held geometry and the new width applies at the next redraw.
    let (mut app, main, child) = two_windows();
    for window in [main, child] {
        remeasure(&mut app, window, false);
        assert_pointer_matches_drawing(&app, window);
    }

    let before = tab_rects(&drawn_layout(&app, child));
    app.windows.get_mut(&child).expect("child").pressed_tab = Some(1);
    retitle(&mut app, child, 0, "cargo build --release --workspace");
    remeasure(&mut app, child, false);
    assert_eq!(tab_rects(&drawn_layout(&app, child)), before, "a pressed tab's bar moved");
    assert_pointer_matches_drawing(&app, child);
    assert_eq!(app.windows_with_held_tab_widths(), vec![child]);

    app.windows.get_mut(&child).expect("child").pressed_tab = None;
    assert_eq!(tab_rects(&pointer_layout(&app, child)), before, "the release saw a new layout");
    remeasure(&mut app, child, false);
    assert_ne!(tab_rects(&drawn_layout(&app, child)), before);
    assert_pointer_matches_drawing(&app, child);
    assert!(app.windows_with_held_tab_widths().is_empty());

    let before = tab_rects(&drawn_layout(&app, main));
    let source_tab = app.windows[&child].tabs.tabs()[2].id;
    app.windows.get_mut(&child).expect("child").drag_session =
        Some(DragSession::new(child, source_tab, (40.0, 580.0)));
    retitle(&mut app, main, 0, "cargo test --workspace --all-targets");
    remeasure(&mut app, main, false);
    assert_eq!(tab_rects(&drawn_layout(&app, main)), before, "a drag elsewhere moved the bar");

    app.windows.get_mut(&child).expect("child").drag_session = None;
    remeasure(&mut app, main, true);
    assert_eq!(tab_rects(&drawn_layout(&app, main)), before, "a still pointer's bar moved");
    remeasure(&mut app, main, false);
    assert_ne!(tab_rects(&drawn_layout(&app, main)), before);
    assert_pointer_matches_drawing(&app, main);
}

#[test]
fn a_font_or_dpi_reload_lays_a_held_bar_out_again() {
    // A font or DPI change gives the bar a new font key, so it lays out again at once even
    // while a tab is pressed, and clicks and drops follow the bar it draws next.
    let (mut app, _, child) = two_windows();
    remeasure(&mut app, child, false);
    let before = tab_rects(&drawn_layout(&app, child));

    app.windows.get_mut(&child).expect("child").pressed_tab = Some(1);
    let hold = tab_widths_held(app.tab_gesture_active(), false);
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
fn only_the_redraw_paths_measure_tab_widths() {
    // The redraw paths measure right before they draw, so the pointer, drag, tear-out and
    // snapshot paths read the widths of the frame on screen and never re-measure text.
    for (name, source) in [
        ("main", include_str!("window_event.rs")),
        ("child", include_str!("child_window_redraw.rs")),
    ] {
        let refresh = source.find("refresh_active_tab_title(").expect("title refresh");
        let measure = source
            .find(".measure_tab_widths(")
            .unwrap_or_else(|| panic!("{name} redraw must measure tab widths before drawing"));
        let render = source.find(".render_with_outcome(").expect("render call");
        assert!(refresh < measure && measure < render, "{name} measures between title and draw");
        assert_eq!(source.matches(".measure_tab_widths(").count(), 1, "{name} measures once");
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
