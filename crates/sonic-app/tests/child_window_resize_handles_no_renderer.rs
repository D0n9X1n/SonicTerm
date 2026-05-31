use std::collections::HashMap;

use sonic_app::app::{
    resize_renderer_and_panes_if_present, set_scale_factor_if_renderer_present, PaneState,
};
use sonic_core::{grid::Grid, vt::Parser};

fn pane(cols: u16, rows: u16) -> (PaneState, std::sync::Arc<parking_lot::Mutex<Parser>>) {
    let parser = std::sync::Arc::new(parking_lot::Mutex::new(Parser::new(Grid::new(cols, rows))));
    (PaneState::new(parser.clone(), None), parser)
}

#[test]
fn resize_and_scale_factor_paths_tolerate_missing_renderer() {
    let mut renderer = None;
    let (pane, parser) = pane(80, 24);
    let mut panes = HashMap::new();
    panes.insert(1, pane);

    let resized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        resize_renderer_and_panes_if_present(&mut renderer, &panes, 1024, 768)
    }))
    .expect("resize path must not panic when WindowState.renderer is None");
    assert!(!resized, "no renderer means the resize path is a no-op");
    assert_eq!(parser.lock().grid().cols, 80);
    assert_eq!(parser.lock().grid().rows, 24);

    let scaled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        set_scale_factor_if_renderer_present(&mut renderer, 2.0)
    }))
    .expect("scale-factor path must not panic when WindowState.renderer is None");
    assert!(!scaled, "no renderer means the scale-factor path is a no-op");
}

#[cfg(target_os = "macos")]
#[test]
fn document_windowstate_none_renderer_resize_scale_paths() {
    use std::sync::Arc;
    use std::time::Instant;

    use sonic_app::app::{
        child_window_resized_handles_no_renderer,
        child_window_scale_factor_changed_handles_no_renderer, WindowRole, WindowState,
    };
    use sonic_ui::ime::ImeState;
    use sonic_ui::tabs::TabBar;
    use winit::event_loop::EventLoop;
    use winit::window::Window;

    let Ok(event_loop) = std::panic::catch_unwind(EventLoop::<()>::new) else {
        return;
    };
    let Ok(event_loop) = event_loop else {
        return;
    };
    #[allow(deprecated)]
    let Ok(window) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event_loop.create_window(Window::default_attributes().with_visible(false))
    })) else {
        return;
    };
    let Ok(window) = window else {
        return;
    };

    let mut child = WindowState {
        role: WindowRole::Terminal,
        window: Some(Arc::new(window)),
        renderer: None,
        tabs: TabBar::new(),
        tab_states: Vec::new(),
        panes: HashMap::new(),
        cursor_pos: (0.0, 0.0),
        mouse_down: false,
        selection: None,
        copy_mode: None,
        modifiers: Default::default(),
        last_render: Instant::now(),
        hover_link: false,
        pressed_tab: None,
        drag_session: None,
        drag_target: None,
        scale_factor: 1.0,
        ime: ImeState::new(),
        ime_cursor_throttle: sonic_ui::ime::ImeCursorThrottle::new(),
        hovered_url: None,
        hidden: false,
        scrollbar_drag: None,
        scrollbar_vis: std::collections::HashMap::new(),
    };

    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        child_window_resized_handles_no_renderer(&mut child, 1024, 768);
        child_window_scale_factor_changed_handles_no_renderer(&mut child, 2.0);
    }))
    .expect("WindowState { renderer: None } resize/scale paths must not panic");
}
