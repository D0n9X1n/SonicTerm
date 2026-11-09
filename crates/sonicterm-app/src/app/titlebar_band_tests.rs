//! Titlebar double-click counting, on every host.

use super::*;

/// The platform's own click count decides when it has one; otherwise two presses within the
/// system's double-click time and rectangle count, and a slower or farther second press does not.
#[test]
fn a_double_click_needs_two_close_presses_in_time() {
    let start = Instant::now();
    let first = Some((start, (100.0, 10.0)));
    let limits = (Duration::from_millis(500), (4.0, 4.0));
    assert!(is_double_click(Some(2), None, start, (0.0, 0.0), limits));
    assert!(!is_double_click(Some(1), first, start, (100.0, 10.0), limits));
    let soon = start + Duration::from_millis(300);
    assert!(is_double_click(None, first, soon, (102.0, 11.0), limits));
    assert!(!is_double_click(None, first, soon, (110.0, 10.0), limits), "moved too far");
    let wide = (Duration::from_millis(500), (8.0, 2.0));
    assert!(is_double_click(None, first, soon, (107.0, 10.0), wide), "the system width");
    let tall = (Duration::from_millis(500), (2.0, 5.0));
    assert!(!is_double_click(None, first, soon, (104.0, 10.0), tall), "a narrow width refuses");
    assert!(is_double_click(None, first, soon, (101.0, 14.0), tall), "a tall height accepts");
    let late = start + Duration::from_millis(600);
    assert!(!is_double_click(None, first, late, (100.0, 10.0), limits), "too slow");
    let slow = (Duration::from_millis(900), (4.0, 4.0));
    assert!(is_double_click(None, first, late, (100.0, 10.0), slow), "the system time");
    assert!(!is_double_click(None, None, soon, (100.0, 10.0), limits), "no first press");
}

/// Each edge shows the matching two-way resize cursor.
#[test]
fn resize_edges_show_their_cursor() {
    assert_eq!(edge_cursor(ResizeEdge::North), CursorIcon::NsResize);
    assert_eq!(edge_cursor(ResizeEdge::West), CursorIcon::EwResize);
    assert_eq!(edge_cursor(ResizeEdge::NorthEast), CursorIcon::NeswResize);
    assert_eq!(edge_cursor(ResizeEdge::SouthEast), CursorIcon::NwseResize);
}

/// A caption button acts only when released over itself with no overlay opened while it was held;
/// moving off it, releasing over another button, or a palette opening cancels it.
#[test]
fn a_caption_press_acts_only_on_its_own_release() {
    use sonicterm_ui::window_band::CaptionButton::{Close, Maximize};
    assert_eq!(release_action(Some(Close), Some(Close), false), Some(Close));
    assert_eq!(release_action(Some(Close), Some(Maximize), false), None, "another button");
    assert_eq!(release_action(Some(Close), None, false), None, "released off the buttons");
    assert_eq!(release_action(Some(Close), Some(Close), true), None, "a palette opened");
    assert_eq!(
        release_action(None, Some(Close), false),
        None,
        "cancelled by a leave or focus loss"
    );
}

/// An overlay that opens, or a band that moves, cancels a held caption button for good: the press
/// is gone after the overlay closes, so its release acts on nothing. Otherwise the press stands.
#[test]
fn an_overlay_or_a_moved_band_cancels_a_held_caption_button() {
    use sonicterm_ui::window_band::CaptionButton::Close;
    let held = CaptionState { pressed: Some(Close), hovered: Some(Close), ..Default::default() };
    assert_eq!(settle_caption(held, false, false), held, "nothing changed");
    let after_palette = settle_caption(held, true, false);
    assert_eq!((after_palette.pressed, after_palette.hovered), (None, None));
    // The palette closed again: the press does not come back, so the release cannot act.
    let closed = settle_caption(after_palette, false, false);
    assert_eq!(release_action(closed.pressed, Some(Close), false), None);
    assert_eq!(settle_caption(held, false, true).pressed, None, "a moved band");
}

/// Through the production handlers on a real window entry: leaving the window or losing focus
/// cancels a held caption button, and the release of a press the band took is swallowed (it is
/// consumed and the flag clears) without acting, whatever the pointer is over.
#[test]
fn leave_and_focus_loss_cancel_and_the_band_release_is_swallowed() {
    use sonicterm_ui::window_band::CaptionButton::Close;
    use winit::event::WindowEvent;
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let child = app.__test_seed_child_window(&["work"]);
    let hold = |app: &mut App| {
        let window = app.registry.windows.get_mut(&child).unwrap();
        window.caption.pressed = Some(Close);
        window.caption.hovered = Some(Close);
        window.band_press_consumed = true;
    };
    for event in [
        WindowEvent::CursorLeft { device_id: winit::event::DeviceId::dummy() },
        WindowEvent::Focused(false),
    ] {
        hold(&mut app);
        app.track_window_band(child, &event);
        let caption = app.registry.windows[&child].caption;
        assert_eq!((caption.pressed, caption.hovered), (None, None), "{event:?} cancels");
    }
    hold(&mut app);
    let (consumed, close) = app.release_window_band(child, 0.0, 0.0);
    assert!(consumed, "the band owns the release");
    assert!(!close, "the release acts on nothing it was not pressed over");
    assert!(!app.registry.windows[&child].band_press_consumed, "and is taken once");
    assert_eq!(app.release_window_band(child, 0.0, 0.0), (false, false), "a later release is free");
}

/// The band sees left input before anything else: a release the band owns is taken before overlay
/// and pane routing in the shared handler, and a press goes to the band before child and main
/// chrome, so neither a palette nor a pane can take it first.
#[test]
fn the_band_routes_before_overlays_panes_and_chrome() {
    let shared = include_str!("window_event.rs");
    let take = shared.find("self.take_band_release(").expect("band release");
    let route = shared.find("self.route_shared_pointer(").expect("shared pointer routing");
    assert!(take < route, "the band's release is taken first");
    let child = include_str!("child_window.rs");
    let band = child.find("self.route_band_left_input(").expect("child band press");
    let chrome = child.find("self.handle_child_left_press_chrome(").expect("child chrome");
    assert!(band < chrome, "a child press reaches the band before its chrome");
    let main = include_str!("window_pointer.rs");
    let body = &main[main.find("fn handle_main_left_mouse_input").expect("main left input")..];
    let band = body.find("self.route_band_left_input(").expect("main band press");
    let chrome = body.find("self.handle_main_left_press_chrome(").expect("main chrome");
    assert!(band < chrome, "a main press reaches the band before its chrome");
}

/// A palette open in the window holding a caption button cancels the press at its next event,
/// before a queued release, even if the palette closes again before that release; a palette open
/// in another window leaves the press alone.
#[test]
fn only_the_window_s_own_palette_cancels_its_caption_press() {
    use sonicterm_ui::window_band::CaptionButton::Minimize;
    use winit::event::WindowEvent;
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let first = app.__test_seed_child_window(&["a"]);
    let second = app.__test_seed_child_window(&["b"]);
    let hold = |app: &mut App, id| {
        let window = app.registry.windows.get_mut(&id).unwrap();
        window.caption.pressed = Some(Minimize);
        window.band_press_consumed = true;
    };
    hold(&mut app, second);
    app.open_status_segments(Some(first), None);
    assert_eq!(app.palette_window(), Some(first));
    let moved = WindowEvent::Focused(true);
    app.track_window_band(second, &moved);
    assert_eq!(app.registry.windows[&second].caption.pressed, Some(Minimize), "another window's");
    hold(&mut app, first);
    app.track_window_band(first, &moved);
    assert_eq!(app.registry.windows[&first].caption.pressed, None, "its own palette cancels");
    app.interaction.command_palette.close();
    assert_eq!(
        app.release_window_band(first, 0.0, 0.0),
        (true, false),
        "the release acts on nothing"
    );
}
