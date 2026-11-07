//! Pins the child window's no-button motion routing against the pane geometry it reads: in a
//! split child, motion inside a pane's scrollbar gutter belongs to the scrollbar and reaches no
//! program, and motion just left of the gutter is reported, for each pane of the split. The pane
//! rect comes from the one-pane layout lookup, so a wrong pane or an already-inset rect moves
//! the gutter and fails here.

use std::sync::atomic::Ordering;

use sonicterm_cfg::config::{Config, ScrollbarMode};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_vt::vt::{MouseTracking, PointerModes};
use winit::dpi::PhysicalPosition;

use crate::app::App;

#[test]
fn child_motion_over_each_panes_gutter_belongs_to_the_scrollbar() {
    // Ordering: pointer_input stores Relaxed; this thread is the handler's only reader.
    let mut config = Config::default();
    config.appearance.scrollbar = ScrollbarMode::Always;
    let mut app = App::new(Theme::default(), config.clone(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    // A 10×10-pixel cell grid over 805 px, so the right pane keeps a fractional remainder.
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 805.0, 240.0);
    assert!(app.__test_set_child_pane_viewport(child, outer, 10.0, 10.0));
    assert!(app.__test_child_split_active_right(child));
    app.__test_enable_pty_write_log();
    let tracking = PointerModes::new(MouseTracking::AnyMotion, true, false, false);
    for pane in app.windows[&child].panes.values() {
        pane.pointer_input.store(tracking.bits(), Ordering::Relaxed);
    }
    let rects = App::compute_pane_rects_for(&app.windows[&child]);
    assert_eq!(rects.len(), 2);
    let gutter = crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX;
    for (pane_id, rect) in rects {
        let row_y = rect.y + 15.0;
        let inside = rect.x + rect.w - gutter / 2.0;
        let outside = rect.x + rect.w - gutter - 6.0;
        for (pixel_x, owned) in [(inside, true), (outside, false)] {
            let before = app.__test_pty_write_log().len();
            app.handle_child_cursor_moved(
                child,
                PhysicalPosition::new(f64::from(pixel_x), f64::from(row_y)),
                &config,
            );
            let reported = app.__test_pty_write_log().len() > before;
            assert_eq!(reported, !owned, "pane {pane_id} at x {pixel_x}: scrollbar owned {owned}");
        }
    }
}
