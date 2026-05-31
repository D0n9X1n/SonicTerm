//! PR-B2-0 (#365): `WindowState.window` was promoted from
//! `Arc<Window>` to `Option<Arc<Window>>` so test seeders can build a
//! `WindowState` without running `do_resumed`. Smoke test pins that
//! the helpers that previously dereferenced the field unconditionally
//! now short-circuit when it's `None` — i.e. no panic.

use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

use sonic_app::app::{WindowRole, WindowState};
use sonic_ui::ime::ImeState;
use sonic_ui::tabs::TabBar;

fn make_ws_with_no_window() -> WindowState {
    WindowState {
        role: WindowRole::Terminal,
        window: None,
        renderer: None,
        tabs: TabBar::new(),
        tab_states: Vec::new(),
        panes: HashMap::new(),
        cursor_pos: (0.0, 0.0),
        mouse_down: false,
        selection: None,
        copy_mode: None,
        modifiers: Default::default(),
        cursor_visible: Arc::new(AtomicBool::new(true)),
        last_render: Instant::now(),
        hover_link: false,
        pressed_tab: None,
        drag_session: None,
        drag_target: None,
        scale_factor: 1.0,
        ime: ImeState::new(),
        hovered_url: None,
    }
}

#[test]
fn windowstate_can_be_constructed_with_no_window() {
    let ws = make_ws_with_no_window();
    assert!(ws.window.is_none());
}

#[test]
fn request_redraw_is_noop_when_window_is_none() {
    let ws = make_ws_with_no_window();
    // Must not panic; nothing to redraw against.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ws.request_redraw();
    }));
    assert!(res.is_ok(), "request_redraw must short-circuit on None");
}

#[test]
fn renderer_accessors_still_panic_when_renderer_none() {
    // Sanity: `window: None` does not silently make the renderer
    // accessors no-op too — they still panic, matching their existing
    // contract (only `do_resumed` populates them in production).
    let mut ws = make_ws_with_no_window();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = ws.renderer();
        }))
        .is_err(),
        "renderer() must still panic when None"
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = ws.renderer_mut();
        }))
        .is_err(),
        "renderer_mut() must still panic when None"
    );
}
