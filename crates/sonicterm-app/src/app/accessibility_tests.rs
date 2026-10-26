use super::*;
pub(super) use sonicterm_cfg::config::Config;
pub(super) use sonicterm_cfg::keymap::Keymap;
pub(super) use sonicterm_cfg::theme::Theme;
pub(super) use sonicterm_gpu::status_strip::{PresentedStatusSegment, StripHit};
pub(super) use sonicterm_types::Action;
pub(super) use sonicterm_ui::copy_mode::CopyModeState;
pub(super) use sonicterm_ui::tabs::TabMarks;
pub(super) use winit::event::{DeviceId, ElementState, MouseButton};
pub(super) use winit::keyboard::SmolStr;

/// A headless App with the strip on and one torn-out terminal window, which is returned. It has
/// no renderer, so no window is eligible to draw: covered, parked and device-stopped look the same.
pub(super) fn app_with_window() -> (App, WindowId) {
    let mut config = Config::default();
    config.status_line.enabled = true;
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let window_id = app.__test_seed_child_window(&["first", "second"]);
    let _ = app.sync_status_line(Instant::now());
    (app, window_id)
}

/// Commit a presented strip for `window_id` with `segments` as `(id, left px, click)`, as a frame
/// that drew them would.
pub(super) fn present_with(
    app: &mut App,
    window_id: WindowId,
    segments: &[(StatusSegmentId, f32, Option<SegmentClick>)],
) {
    let segments = segments
        .iter()
        .map(|(id, left_px, click)| PresentedStatusSegment {
            id: *id,
            x: *left_px,
            width: 40.0,
            click: click.clone(),
        })
        .collect();
    app.windows.get_mut(&window_id).unwrap().status_strip =
        Some(StripHit { top: 500.0, height: 20.0, segments });
}

/// `present_with` where a clickable segment toggles the strip, so a click is observable.
pub(super) fn present(
    app: &mut App,
    window_id: WindowId,
    segments: &[(StatusSegmentId, f32, bool)],
) {
    let toggle = SegmentClick::Action(Action::ToggleStatusLine);
    let segments: Vec<_> = segments
        .iter()
        .map(|(id, left_px, clickable)| (*id, *left_px, clickable.then(|| toggle.clone())))
        .collect();
    present_with(app, window_id, &segments);
}

/// The window's accessibility state.
pub(super) fn access(app: &App, window_id: WindowId) -> &WindowAccessibility {
    app.accessibility.get(&window_id).expect("terminal windows have accessibility")
}

/// The last tree the window sent.
pub(super) fn last_tree(app: &App, window_id: WindowId) -> TreeUpdate {
    access(app, window_id).sent.last().cloned().expect("a tree was sent")
}

/// The node with `id` in `tree`.
pub(super) fn node(tree: &TreeUpdate, id: NodeId) -> Option<&Node> {
    tree.nodes.iter().find(|(node_id, _)| *node_id == id).map(|(_, node)| node)
}

/// Connect an assistive technology to `window_id` and run the dispatch's flush at `now`.
pub(super) fn activate(app: &mut App, window_id: WindowId, now: Instant) {
    app.handle_accessibility_request(window_id, AccessibilityRequest::InitialTree);
    let _ = app.flush_accessibility(now);
}

/// The announcement node's current text and urgency.
pub(super) fn announced(app: &App, window_id: WindowId) -> Option<Announcement> {
    access(app, window_id).announcement.clone()
}

/// A mouse press of `button` at the window's recorded pointer.
pub(super) fn press(button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Pressed, button }
}

/// The body of `function` in `source`, up to the next function at the same indent.
pub(super) fn body<'source>(source: &'source str, function: &str) -> &'source str {
    let start = source.find(function).unwrap_or_else(|| panic!("{function}"));
    let rest = &source[start + function.len()..];
    // The nearest next function start, whatever its visibility.
    let end = ["\n    fn ", "\n    pub(super) fn ", "\n    pub(in crate::app) fn "]
        .iter()
        .filter_map(|start| rest.find(start))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// A headless App with the strip on and one torn-out terminal window holding `titles` as tabs.
pub(super) fn app_with_tabs(titles: &[&str]) -> (App, WindowId) {
    let mut config = Config::default();
    config.status_line.enabled = true;
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let window_id = app.__test_seed_child_window(titles);
    let _ = app.sync_status_line(Instant::now());
    (app, window_id)
}

/// Seed `count` tabs, connect a reader, mark `marked` tabs with `marks` before the connection,
/// then ring tab `rings`: returns what the reader heard.
pub(super) fn ring_after_marks(
    count: usize,
    marked: usize,
    marks: fn(&mut TabMarks),
    rings: usize,
) -> String {
    let titles: Vec<String> = (0..count).map(|index| format!("tab {index}")).collect();
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    let (mut app, window_id) = app_with_tabs(&titles);
    for tab in 0..marked {
        app.windows.get_mut(&window_id).unwrap().tabs.mark(tab, marks);
    }
    let start = Instant::now();
    activate(&mut app, window_id, start);
    app.windows.get_mut(&window_id).unwrap().tabs.mark(rings, |marks| marks.bell = true);
    let _ = app.flush_accessibility(start + Duration::from_secs(1));
    announced(&app, window_id).map(|said| said.text).unwrap_or_default()
}

/// The model's bytes as allocated storage: hash tables through the repository's shared table
/// estimate, vectors and strings by capacity.
pub(super) fn allocated_bytes(state: &WindowAccessibility) -> usize {
    sonicterm_types::retained_hash_table_bytes::<StatusSegmentId, (Instant, String)>(
        state.announced.capacity(),
    ) + state.announced.values().map(|(_, said)| said.capacity()).sum::<usize>()
        + state.queue.capacity() * std::mem::size_of::<Pending>()
        + state.queue.iter().map(|pending| pending.text.capacity()).sum::<usize>()
        + state.structure.segments.capacity() * std::mem::size_of::<Exposed>()
        + state.announcement.as_ref().map_or(0, |said| said.text.capacity())
        + state.health_text.as_ref().map_or(0, String::capacity)
        + sonicterm_types::retained_hash_table_bytes::<u64, RequestTarget>(
            state.targets.lock().unwrap().capacity(),
        )
}
