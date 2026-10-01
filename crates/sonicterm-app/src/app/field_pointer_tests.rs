use std::cell::Cell;

use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::field_geometry::{FieldHit, FieldHitMode};
use sonicterm_ui::search::SearchState;
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
use winit::window::WindowId;

use super::{App, FieldPointerCapture, FieldTarget};

/// Left of the fake field, in physical pixels; each query byte is 10 px wide.
const FIELD_LEFT: f32 = 100.0;
/// Right edge of the fake field's hit area.
const FIELD_RIGHT: f32 = 300.0;

thread_local! {
    /// Number of fake hits still to answer `Stale` before geometry "presents".
    static STALE_HITS: Cell<u32> = const { Cell::new(0) };
    /// Answer every hit `Unavailable`, as a live preedit does.
    static UNAVAILABLE: Cell<bool> = const { Cell::new(false) };
}

/// Query length of the field `target` names in `window_id`.
fn target_len(app: &App, window_id: WindowId, target: FieldTarget) -> usize {
    match target {
        FieldTarget::Palette { .. } => app.command_palette.query().len(),
        FieldTarget::Search(_) => {
            let window = &app.windows[&window_id];
            window.tab_states[window.tabs.active_index()]
                .search
                .as_ref()
                .map_or(0, |search| search.query.len())
        }
    }
}

/// Deterministic presented geometry: a 10 px-per-byte field between 100 and 300 px.
fn fake_hit(
    app: &App,
    window_id: WindowId,
    target: FieldTarget,
    point: (f32, f32),
    mode: FieldHitMode,
) -> FieldHit {
    let inside = (FIELD_LEFT..FIELD_RIGHT).contains(&point.0) && (10.0..30.0).contains(&point.1);
    if mode == FieldHitMode::Press && !inside {
        return FieldHit::Outside;
    }
    if UNAVAILABLE.with(Cell::get) {
        return FieldHit::Unavailable;
    }
    let stale = STALE_HITS.with(|count| {
        let remaining = count.get();
        count.set(remaining.saturating_sub(1));
        remaining > 0
    });
    if stale {
        return FieldHit::Stale;
    }
    let offset = ((point.0 - FIELD_LEFT) / 10.0).round().max(0.0) as usize;
    FieldHit::Offset(offset.min(target_len(app, window_id, target)))
}

fn reset_fake() {
    STALE_HITS.with(|count| count.set(0));
    UNAVAILABLE.with(|flag| flag.set(false));
}

fn left(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left }
}

/// Move the recorded pointer, as `record_window_pointer` does before handlers run.
fn move_to(app: &mut App, window_id: WindowId, pointer_x: f64) -> WindowEvent {
    app.windows.get_mut(&window_id).unwrap().cursor_pos = (pointer_x, 20.0);
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(pointer_x, 20.0),
    }
}

fn dispatch(app: &mut App, window_id: WindowId, event: &WindowEvent) -> bool {
    app.field_pointer_event_with(window_id, event, fake_hit)
}

fn palette_app(query: &str) -> (App, WindowId) {
    reset_fake();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let window_id = app.main_window_id.unwrap();
    app.command_palette.open();
    app.command_palette.set_query(query);
    (app, window_id)
}

fn search_app(query: &str) -> (App, WindowId) {
    reset_fake();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("second");
    let window_id = app.main_window_id.unwrap();
    let window = app.windows.get_mut(&window_id).unwrap();
    let active = window.tabs.active_index();
    let mut search = SearchState::new();
    search.query = query.to_string();
    window.tab_states[active].search = Some(search);
    (app, window_id)
}

fn search_of(app: &App, window_id: WindowId) -> &SearchState {
    let window = &app.windows[&window_id];
    window.tab_states[window.tabs.active_index()].search.as_ref().unwrap()
}

/// Press places the palette caret, drag selects, release ends the gesture: every
/// half of the click is consumed and no row is activated or the palette closed.
#[test]
fn palette_press_drag_release_selects_without_row_activation() {
    let (mut app, window_id) = palette_app("abcdef");
    let press_move = move_to(&mut app, window_id, 120.0);
    let _ = press_move;
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(app.command_palette.cursor(), 2);
    assert_eq!(app.command_palette.selected_range(), None);

    let drag = move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &drag));
    assert_eq!(app.command_palette.selected_range(), Some(2..5));

    // Dragging past the field clamps to the query end.
    let past = move_to(&mut app, window_id, 900.0);
    assert!(dispatch(&mut app, window_id, &past));
    assert_eq!(app.command_palette.selected_range(), Some(2..6));

    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert_eq!(app.field_pointer_capture, None);
    assert!(app.command_palette.is_open(), "release must not activate a row");
    assert_eq!(app.command_palette.query(), "abcdef");
    assert_eq!(app.command_palette.selected_range(), Some(2..6));
}

/// A reverse drag selects backward from the press anchor.
#[test]
fn palette_reverse_drag_keeps_the_press_anchor() {
    let (mut app, window_id) = palette_app("abcdef");
    move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    let back = move_to(&mut app, window_id, 110.0);
    assert!(dispatch(&mut app, window_id, &back));
    assert_eq!(app.command_palette.selected_range(), Some(1..5));
    assert_eq!(app.command_palette.cursor(), 1);
}

/// Shift+press extends from the existing caret instead of collapsing it.
#[test]
fn shift_press_extends_from_the_existing_caret() {
    let (mut app, window_id) = palette_app("abcdef");
    app.command_palette.set_cursor(1);
    app.windows.get_mut(&window_id).unwrap().modifiers = winit::keyboard::ModifiersState::SHIFT;
    move_to(&mut app, window_id, 140.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(app.command_palette.selected_range(), Some(1..4));
}

/// A press outside the field is left to the palette modal and terminal handlers.
#[test]
fn press_outside_the_field_is_not_consumed() {
    let (mut app, window_id) = palette_app("abc");
    move_to(&mut app, window_id, 40.0);
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(app.field_pointer_capture, None);
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// A terminal or chrome gesture already held in the window keeps its motion and release.
#[test]
fn held_window_gestures_keep_their_events() {
    let (mut app, window_id) = palette_app("abc");
    move_to(&mut app, window_id, 120.0);
    app.windows.get_mut(&window_id).unwrap().mouse_down = true;
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Released)));
    let window = app.windows.get_mut(&window_id).unwrap();
    window.mouse_down = false;
    window.pressed_tab = Some(0);
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(app.field_pointer_capture, None);
}

/// A press on unpresented geometry is held, not given to the terminal, and
/// resolves at its press point once the field is presented.
#[test]
fn stale_press_waits_for_presented_geometry() {
    let (mut app, window_id) = palette_app("abcdef");
    move_to(&mut app, window_id, 120.0);
    STALE_HITS.with(|count| count.set(2));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(app.command_palette.cursor(), 6, "no caret move before geometry");
    let still_stale = move_to(&mut app, window_id, 140.0);
    assert!(dispatch(&mut app, window_id, &still_stale));
    let presented = move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &presented));
    assert_eq!(app.command_palette.selected_range(), Some(2..5), "anchored at the press point");
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// An unmappable press (including a clip without a fitting boundary) preserves
/// selection while consuming its held motion and exactly its paired release.
#[test]
fn unavailable_press_swallows_only_its_release() {
    let (mut app, window_id) = palette_app("abc");
    app.command_palette.select_all();
    move_to(&mut app, window_id, 120.0);
    UNAVAILABLE.with(|flag| flag.set(true));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(app.command_palette.cursor(), 3);
    let drag = move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &drag), "the held button's motion stays consumed");
    assert_eq!(app.command_palette.cursor(), 3, "swallowed motion never edits");
    assert_eq!(
        app.command_palette.selected_range(),
        Some(0..3),
        "unavailable hit preserves selection"
    );
    assert_eq!(app.field_pointer_capture, Some(FieldPointerCapture::Swallow { window_id }));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Released)));
    let after = move_to(&mut app, window_id, 160.0);
    assert!(!dispatch(&mut app, window_id, &after), "motion after the release is not ours");
}

/// Losing all eligible boundaries during an anchored drag leaves selection
/// unchanged, consumes motion/release, and can resume at the original anchor.
#[test]
fn anchored_unavailable_drag_preserves_selection_and_consumes_events() {
    let (mut app, window_id) = palette_app("abcdef");
    move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    let drag = move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &drag));
    assert_eq!(app.command_palette.selected_range(), Some(2..5));
    UNAVAILABLE.with(|flag| flag.set(true));
    let missing = move_to(&mut app, window_id, 100.0);
    assert!(dispatch(&mut app, window_id, &missing));
    assert_eq!(app.command_palette.selected_range(), Some(2..5));
    assert_eq!(app.command_palette.cursor(), 2);
    assert!(matches!(
        app.field_pointer_capture,
        Some(FieldPointerCapture::Dragging { anchored: true, .. })
    ));
    UNAVAILABLE.with(|flag| flag.set(false));
    let resumed = move_to(&mut app, window_id, 110.0);
    assert!(dispatch(&mut app, window_id, &resumed));
    assert_eq!(app.command_palette.selected_range(), Some(1..5));
    UNAVAILABLE.with(|flag| flag.set(true));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert_eq!(app.command_palette.selected_range(), Some(1..5));
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// Focus loss cancels the drag without consuming the focus event, and the
/// paired release is still consumed so no half click leaks.
#[test]
fn focus_loss_cancels_but_still_consumes_the_release() {
    let (mut app, window_id) = palette_app("abcdef");
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert!(!dispatch(&mut app, window_id, &WindowEvent::Focused(false)));
    let drag = move_to(&mut app, window_id, 160.0);
    assert!(dispatch(&mut app, window_id, &drag), "the held button's motion stays consumed");
    assert_eq!(app.command_palette.selected_range(), None, "cancelled drag stops selecting");
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert_eq!(app.field_pointer_capture, None);
}

/// Closing the palette mid-drag cancels selection; the held button's motion
/// and release stay consumed so they cannot reach the terminal.
#[test]
fn owner_close_mid_drag_cancels_and_swallows() {
    let (mut app, window_id) = palette_app("abcdef");
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    app.command_palette.close();
    let drag = move_to(&mut app, window_id, 160.0);
    assert!(dispatch(&mut app, window_id, &drag));
    assert_eq!(
        app.field_pointer_capture,
        Some(FieldPointerCapture::Swallow { window_id }),
        "the stale owner becomes a swallowed release"
    );
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert_eq!(app.field_pointer_capture, None);
}

/// A search drag belongs to the tab it started on; switching tabs cancels it
/// and never edits the newly active tab's search.
#[test]
fn search_drag_is_bound_to_its_tab() {
    let (mut app, window_id) = search_app("needle");
    move_to(&mut app, window_id, 110.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(search_of(&app, window_id).cursor(), 1);
    let drag = move_to(&mut app, window_id, 130.0);
    assert!(dispatch(&mut app, window_id, &drag));
    assert_eq!(search_of(&app, window_id).selected_range(), Some(1..3));

    let window = app.windows.get_mut(&window_id).unwrap();
    let other = if window.tabs.active_index() == 0 { 1 } else { 0 };
    window.tabs.activate(other);
    let mut other_search = SearchState::new();
    other_search.query = "hay".to_string();
    window.tab_states[other].search = Some(other_search);
    let after_switch = move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &after_switch));
    assert_eq!(search_of(&app, window_id).selected_range(), None, "other tab untouched");
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// The palette is modal over its window's search, including the colour picker,
/// whose title row is not an editable field.
#[test]
fn palette_blocks_search_in_its_window() {
    let (mut app, window_id) = search_app("needle");
    app.command_palette.start_tab_color_picker("tab", Vec::new());
    move_to(&mut app, window_id, 110.0);
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(search_of(&app, window_id).cursor(), 0);
}

/// Another window's release neither ends nor steals the source window's gesture.
#[test]
fn release_in_another_window_is_not_consumed() {
    let (mut app, window_id) = palette_app("abcdef");
    let child = app.__test_seed_child_window(&["child"]);
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert!(!dispatch(&mut app, child, &left(ElementState::Released)));
    assert!(matches!(app.field_pointer_capture, Some(FieldPointerCapture::Dragging { .. })));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// Keyboard edits end the drag, so a later move cannot re-extend over typed text.
#[test]
fn keyboard_action_cancels_the_drag() {
    let (mut app, window_id) = palette_app("abcdef");
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    app.cancel_field_pointer(window_id);
    let drag = move_to(&mut app, window_id, 160.0);
    assert!(dispatch(&mut app, window_id, &drag), "the held button's motion stays consumed");
    assert_eq!(app.command_palette.selected_range(), None);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// After an IME, resize or keyboard cancel, the source window's motion, leave and
/// release stay consumed until the release; other windows' events are untouched.
#[test]
fn cancelled_drag_swallows_motion_leave_and_release() {
    let (mut app, window_id) = palette_app("abcdef");
    let child = app.__test_seed_child_window(&["child"]);
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    let resized = WindowEvent::Resized(winit::dpi::PhysicalSize::new(800, 600));
    assert!(!dispatch(&mut app, window_id, &resized), "the resize still reaches its handler");
    let drag = move_to(&mut app, window_id, 160.0);
    assert!(dispatch(&mut app, window_id, &drag));
    let leave = WindowEvent::CursorLeft { device_id: DeviceId::dummy() };
    assert!(dispatch(&mut app, window_id, &leave));
    let elsewhere = move_to(&mut app, child, 160.0);
    assert!(!dispatch(&mut app, child, &elsewhere), "another window's motion is not owed");
    assert!(!dispatch(&mut app, child, &leave));
    assert_eq!(app.command_palette.selected_range(), None, "swallowed motion never selects");
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert_eq!(app.field_pointer_capture, None);
    let free = move_to(&mut app, window_id, 170.0);
    assert!(!dispatch(&mut app, window_id, &free));
}

/// A press in another window neither clears nor steals the source window's drag:
/// the source keeps its motion and its paired release.
#[test]
fn other_window_press_keeps_the_source_capture() {
    let (mut app, window_id) = palette_app("abcdef");
    let child = app.__test_seed_child_window(&["child"]);
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    move_to(&mut app, child, 120.0);
    assert!(!dispatch(&mut app, child, &left(ElementState::Pressed)));
    assert!(
        matches!(
            app.field_pointer_capture,
            Some(FieldPointerCapture::Dragging { window_id: source, .. }) if source == window_id
        ),
        "the source window keeps its gesture"
    );
    let drag = move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &drag));
    assert_eq!(app.command_palette.selected_range(), Some(2..5));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert_eq!(app.field_pointer_capture, None);
}

/// Closing the tab a rename editor captured, while a sibling remains active, turns
/// the drag into a swallowed release instead of editing the orphaned query.
#[test]
fn closed_rename_tab_mid_drag_stops_editing() {
    reset_fake();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("second");
    let window_id = app.main_window_id.unwrap();
    app.start_rename_active_tab();
    assert_eq!(app.command_palette.query(), "second");
    move_to(&mut app, window_id, 110.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert_eq!(app.command_palette.cursor(), 1);
    let active = app.windows[&window_id].tabs.active_index();
    app.close_tab_at(active);
    assert_eq!(app.windows[&window_id].tabs.len(), 1, "the sibling tab remains");
    assert!(app.command_palette.is_open(), "the editor is still showing");
    let drag = move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &drag), "the held button's motion stays consumed");
    assert_eq!(app.command_palette.selected_range(), None, "the orphaned query is not edited");
    assert_eq!(app.field_pointer_capture, Some(FieldPointerCapture::Swallow { window_id }));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// Closing and reopening the same palette mode mid-drag never hands the old
/// gesture to the new editor.
#[test]
fn reopened_palette_does_not_inherit_the_capture() {
    let (mut app, window_id) = palette_app("abcdef");
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    app.command_palette.close();
    app.command_palette.open();
    app.command_palette.set_query("abcdef");
    let drag = move_to(&mut app, window_id, 150.0);
    assert!(dispatch(&mut app, window_id, &drag));
    assert_eq!(app.command_palette.selected_range(), None, "the new editor is not selected");
    assert_eq!(app.field_pointer_capture, Some(FieldPointerCapture::Swallow { window_id }));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
}

/// The shared field handler runs before the palette modal handler in window dispatch.
#[test]
fn field_pointer_runs_before_the_palette_modal() {
    const SRC: &str = include_str!("window_event.rs");
    let field = SRC.find("self.field_pointer_event(win_id, &event)").expect("field hook");
    let modal =
        SRC.find("self.command_palette_handle_pointer_event(win_id, &event)").expect("modal hook");
    assert!(field < modal);
}

/// A cancelled drag whose release never arrives does not block another window's
/// field: the second window starts and ends its own gesture, and the first
/// window's late release is still consumed exactly once.
#[test]
fn lost_release_does_not_block_another_windows_field() {
    let (mut app, window_id) = palette_app("abcdef");
    let child = app.__test_seed_child_window(&["child"]);
    {
        let window = app.windows.get_mut(&child).unwrap();
        let active = window.tabs.active_index();
        let mut search = SearchState::new();
        search.query = "needle".to_string();
        window.tab_states[active].search = Some(search);
    }
    move_to(&mut app, window_id, 120.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    assert!(!dispatch(&mut app, window_id, &WindowEvent::Focused(false)));
    // The release of that press is never delivered to the source window.
    move_to(&mut app, child, 110.0);
    assert!(
        dispatch(&mut app, child, &left(ElementState::Pressed)),
        "another window's field press starts its own gesture"
    );
    let drag = move_to(&mut app, child, 130.0);
    assert!(dispatch(&mut app, child, &drag));
    assert_eq!(search_of(&app, child).selected_range(), Some(1..3));
    let source_motion = move_to(&mut app, window_id, 160.0);
    assert!(dispatch(&mut app, window_id, &source_motion), "the owed window's motion stays ours");
    assert_eq!(app.command_palette.selected_range(), None, "the owed window never selects");
    assert!(dispatch(&mut app, child, &left(ElementState::Released)));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)), "late release consumed");
    assert!(!dispatch(&mut app, window_id, &left(ElementState::Released)), "consumed only once");
    let free = move_to(&mut app, window_id, 170.0);
    assert!(!dispatch(&mut app, window_id, &free), "no debt remains");
}

/// A child window reaped after its last shell exits cleanly takes its field drag
/// with it: no capture or debt survives, the main window's field drag works, and
/// a late `Destroyed` for the reaped child does not cancel main's gesture.
#[test]
fn reaped_child_drops_its_field_capture() {
    let (mut app, window_id) = search_app("needle");
    let child = app.__test_seed_child_window(&["child"]);
    {
        let window = app.windows.get_mut(&child).unwrap();
        let active = window.tabs.active_index();
        let mut search = SearchState::new();
        search.query = "haystack".to_string();
        window.tab_states[active].search = Some(search);
    }
    let child_pane = app.__test_child_active_pane(child).unwrap();
    move_to(&mut app, child, 110.0);
    assert!(dispatch(&mut app, child, &left(ElementState::Pressed)));
    assert!(matches!(app.field_pointer_capture, Some(FieldPointerCapture::Dragging { .. })));

    app.handle_pane_process_exited(child_pane, Some(true));
    assert!(!app.windows.contains_key(&child), "the empty child window is reaped");
    assert_eq!(app.field_pointer_capture, None, "the reaped child's capture is dropped");
    assert!(app.field_owed_releases.is_empty(), "the reaped child owes nothing");

    move_to(&mut app, window_id, 110.0);
    assert!(
        dispatch(&mut app, window_id, &left(ElementState::Pressed)),
        "main's field press works"
    );
    assert!(!dispatch(&mut app, child, &WindowEvent::Destroyed));
    assert!(
        matches!(
            app.field_pointer_capture,
            Some(FieldPointerCapture::Dragging { window_id: source, .. }) if source == window_id
        ),
        "a late Destroyed for the reaped child leaves main's drag"
    );
    let drag = move_to(&mut app, window_id, 130.0);
    assert!(dispatch(&mut app, window_id, &drag));
    assert_eq!(search_of(&app, window_id).selected_range(), Some(1..3));
    assert!(dispatch(&mut app, window_id, &left(ElementState::Released)));
    assert_eq!(app.field_pointer_capture, None);
}

/// A drag held by a window that is now hidden can never deliver its release, so it
/// does not block another window's field press.
#[test]
fn hidden_source_drag_does_not_block_another_window() {
    let (mut app, window_id) = search_app("needle");
    let child = app.__test_seed_child_window(&["child"]);
    {
        let window = app.windows.get_mut(&child).unwrap();
        let active = window.tabs.active_index();
        let mut search = SearchState::new();
        search.query = "haystack".to_string();
        window.tab_states[active].search = Some(search);
    }
    move_to(&mut app, window_id, 110.0);
    assert!(dispatch(&mut app, window_id, &left(ElementState::Pressed)));
    app.windows.get_mut(&window_id).unwrap().hidden = true;
    move_to(&mut app, child, 110.0);
    assert!(dispatch(&mut app, child, &left(ElementState::Pressed)), "child press is not blocked");
    assert!(app.field_owed_releases.is_empty(), "the hidden window keeps no debt");
    assert!(dispatch(&mut app, child, &left(ElementState::Released)));
    assert_eq!(app.field_pointer_capture, None);
}
