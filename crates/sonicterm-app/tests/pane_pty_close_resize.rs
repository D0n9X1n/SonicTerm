#![warn(clippy::min_ident_chars)]

use sonicterm_app::app::App;
use sonicterm_app_core::{AppIntent, PaneId};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_ui::pane::Rect;

fn test_app() -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.testing.test_viewport_override = Some((Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
    app
}

#[test]
fn pty_close_refits_surviving_split_pane_to_full_width() {
    let mut app = test_app();
    let survivor = app.__test_seed_tab("main");

    app.__test_split_active_right();
    let closing = app.__test_active_pane_in_tab(0).expect("split should focus new pane");
    assert_ne!(survivor, closing);
    assert_eq!(app.__test_pane_count_in_tab(0), Some(2));
    assert_eq!(app.__test_pane_grid_size(survivor), Some((40, 24)));

    app.dispatch_intent(AppIntent::PtyExit { pane: PaneId(closing), status: 0 });

    assert_eq!(app.__test_pane_count_in_tab(0), Some(1));
    assert_eq!(app.__test_pane_ids(), vec![survivor]);
    assert_eq!(app.__test_pane_grid_size(survivor), Some((80, 24)));
}

/// `App::redraw_request_count` and `App::reap_call_count` stay public fields of `App` itself, the
/// path code outside the crate reads, even though the rest of the diagnostics live in their own owner.
#[test]
fn diagnostic_counters_keep_their_public_app_paths() {
    let app = test_app();
    let redraws = app.redraw_request_count.load(std::sync::atomic::Ordering::Relaxed);
    let reaps = app.reap_call_count.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!((redraws, reaps), (0, 0));
}
