//! `App::do_window_event` — the full `WindowEvent` dispatch body,
//! extracted from `ApplicationHandler::window_event` from the monolithic app module.
//!
//! This is mechanically the original body wrapped in a separate `impl App`
//! block; field access works because all referenced `App` fields are
//! `pub(super)`.

use std::sync::atomic::Ordering;
use std::time::Instant;

use sonicterm_cfg::{config::ScrollbarMode, keymap::Action};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::overlays::{
    search_bar_label, search_query_caret_prefix, SearchBarLayout, SEARCH_BAR_ICON_GAP,
    SEARCH_BAR_PAD_LEFT, SEARCH_BAR_PAD_RIGHT,
};
use sonicterm_ui::selection::{plain_text_from_grid_range, SelectMode, Selection};
use sonicterm_ui::tabbar_view::TabBarLayout;
use sonicterm_vt::vt::MouseTracking;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, ModifiersState, NamedKey, PhysicalKey},
    window::{CursorIcon, WindowId},
};

use super::key_encoding::{key_event_to_string, key_event_to_strings};
use super::{
    invalidate_selection_for_content, mark_all_panes_dirty, pane_id_at_point,
    runtime_smoke::{grid_contains_marker, RuntimeSmokeFailure},
    App, FrontmostKind, PointerCell, PointerGesture, PointerGestureOwner, PtyInputSource, TabState,
    WindowState,
};

const SPLITTER_HIT_THICKNESS: f32 = 8.0;
const SEARCH_BADGE_ICON: &str = "";

/// Pointer event encoded for a terminal mouse protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointerReportKind {
    LeftPress,
    LeftRelease,
    HeldLeftMotion,
    NoButtonMotion,
}

/// Pure terminal pointer route produced after ownership and cell resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointerMotionRoute {
    Local,
    None,
    Report { pane_id: u64, sgr: bool, row: u16, col: u16, modifiers: ModifiersState },
}

/// Encode one pane-local pointer report in SGR or classic X10 form.
pub(super) fn pointer_report_bytes(
    sgr: bool,
    kind: PointerReportKind,
    modifiers: ModifiersState,
    row: u16,
    col: u16,
) -> Vec<u8> {
    let mut modifier_bits = 0;
    if modifiers.shift_key() {
        modifier_bits += 4;
    }
    if modifiers.alt_key() || modifiers.super_key() {
        modifier_bits += 8;
    }
    if modifiers.control_key() {
        modifier_bits += 16;
    }
    let base: u32 = match kind {
        PointerReportKind::LeftPress | PointerReportKind::LeftRelease => 0,
        PointerReportKind::HeldLeftMotion => 32,
        PointerReportKind::NoButtonMotion => 35,
    };
    let cb = base + modifier_bits;
    let col = u32::from(col) + 1;
    let row = u32::from(row) + 1;
    if sgr {
        let terminator = match kind {
            PointerReportKind::LeftRelease => 'm',
            PointerReportKind::LeftPress
            | PointerReportKind::HeldLeftMotion
            | PointerReportKind::NoButtonMotion => 'M',
        };
        format!("\x1b[<{cb};{col};{row}{terminator}").into_bytes()
    } else {
        // When: `sgr` is false, legacy release uses no-button base 3 while other events retain `cb`.
        let legacy_cb = match kind {
            PointerReportKind::LeftRelease => 3 + modifier_bits,
            PointerReportKind::LeftPress
            | PointerReportKind::HeldLeftMotion
            | PointerReportKind::NoButtonMotion => cb,
        };
        vec![
            0x1b,
            b'[',
            b'M',
            (legacy_cb + 32).min(255) as u8,
            (col.min(223) + 32) as u8,
            (row.min(223) + 32) as u8,
        ]
    }
}

/// Return an existing terminal press's destinations only for repeat events.
pub(super) fn terminal_repeat_targets(
    pressed_keys: &super::keyboard_protocol::PressedKeys,
    physical_key: PhysicalKey,
    repeat: bool,
) -> Option<super::keyboard_protocol::KeyRoutes> {
    repeat.then(|| pressed_keys.get(&physical_key).cloned()).flatten()
}

/// Choose and latch the owner of a left-button grid press.
pub(super) fn begin_pointer_gesture(
    cell: PointerCell,
    tracking: MouseTracking,
    sgr: bool,
    modifiers: ModifiersState,
    ui_consumed: bool,
) -> Option<PointerGesture> {
    if ui_consumed {
        // When: `ui_consumed` is true, SonicTerm chrome consumed the press and no grid gesture exists.
        return None;
    }
    let owner = match (tracking, modifiers.shift_key()) {
        (MouseTracking::Off, _) | (_, true) => PointerGestureOwner::Local,
        (MouseTracking::Button | MouseTracking::ButtonMotion | MouseTracking::AnyMotion, false) => {
            PointerGestureOwner::Terminal { tracking, sgr }
        }
    };
    Some(PointerGesture { owner, press_pane: cell.pane_id, last_cell: cell })
}

impl WindowState {
    /// Begin one grid press from this window's live modifier state.
    pub(super) fn begin_pointer_press(
        &mut self,
        cell: PointerCell,
        tracking: MouseTracking,
        sgr: bool,
    ) -> Option<Vec<u8>> {
        let gesture = if self.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only) {
            // READONLY selects local ownership only at the press; an earlier terminal hold keeps its route.
            Some(PointerGesture {
                owner: PointerGestureOwner::Local,
                press_pane: cell.pane_id,
                last_cell: cell,
            })
        } else {
            // When: copy_mode is not READONLY, retain the normal Shift and tracking ownership decision.
            begin_pointer_gesture(cell, tracking, sgr, self.modifiers, false)
        };
        self.pointer_gesture = gesture;
        let PointerGestureOwner::Terminal { sgr, .. } = gesture?.owner else {
            // When: `gesture.owner` is Local, preserve the selection and emit no terminal report.
            return None;
        };
        self.selection = None;
        Some(pointer_report_bytes(
            sgr,
            PointerReportKind::LeftPress,
            self.modifiers,
            cell.row,
            cell.col,
        ))
    }
}

/// Route motion for an already-latched left-button gesture.
pub(super) fn route_pressed_pointer_motion(
    gesture: &mut PointerGesture,
    cell: Option<PointerCell>,
    modifiers: ModifiersState,
) -> PointerMotionRoute {
    let PointerGestureOwner::Terminal { tracking, sgr } = gesture.owner else {
        // When: local selection owns the gesture, current parser/modifier state cannot steal it.
        return PointerMotionRoute::Local;
    };
    if let Some(cell) = cell.filter(|cell| cell.pane_id == gesture.press_pane) {
        gesture.last_cell = cell;
    }
    if tracking == MouseTracking::Button {
        // When: Button mode reports transitions only, suppress held motion.
        return PointerMotionRoute::None;
    }
    PointerMotionRoute::Report {
        pane_id: gesture.press_pane,
        sgr,
        row: gesture.last_cell.row,
        col: gesture.last_cell.col,
        modifiers,
    }
}

/// Decide whether the visible native scrollbar owns a pointer in its gutter.
pub(super) fn native_scrollbar_owns_pointer(
    mode: ScrollbarMode,
    pane: sonicterm_ui::pane::Rect,
    x: f32,
    y: f32,
    gutter_width: f32,
    edge_active: bool,
    visible: bool,
) -> bool {
    let interactive = match mode {
        ScrollbarMode::Always => true,
        ScrollbarMode::Auto => edge_active || visible,
        ScrollbarMode::Never => false,
    };
    if !interactive {
        // When: no scrollbar is interactive, terminal motion keeps the pane's whole cell surface.
        return false;
    }
    let width = gutter_width.max(0.0).min(pane.w.max(0.0));
    x >= pane.x + pane.w - width && x < pane.x + pane.w && y >= pane.y && y < pane.y + pane.h
}

/// Inset a pane rect to the content area that owns the rendered scrollbar.
pub(super) fn pointer_scrollbar_content_rect(
    pane: sonicterm_ui::pane::Rect,
    insets: [f32; 4],
    minimum: (f32, f32),
) -> sonicterm_ui::pane::Rect {
    sonicterm_ui::pane::Rect::new(
        pane.x + insets[0],
        pane.y + insets[2],
        (pane.w - insets[0] - insets[1]).max(minimum.0),
        (pane.h - insets[2] - insets[3]).max(minimum.1),
    )
}

/// Build live no-button motion only for the current pane's AnyMotion mode.
pub(super) fn no_button_motion_report(
    cell: PointerCell,
    tracking: MouseTracking,
    sgr: bool,
    modifiers: ModifiersState,
    ui_consumed: bool,
) -> Option<PointerMotionRoute> {
    (!ui_consumed && tracking == MouseTracking::AnyMotion).then_some(PointerMotionRoute::Report {
        pane_id: cell.pane_id,
        sgr,
        row: cell.row,
        col: cell.col,
        modifiers,
    })
}

/// Consume a left-button gesture and return its terminal release route, if any.
pub(super) fn take_pointer_release(
    gesture: &mut Option<PointerGesture>,
    modifiers: ModifiersState,
) -> Option<PointerMotionRoute> {
    let gesture = gesture.take()?;
    let PointerGestureOwner::Terminal { sgr, .. } = gesture.owner else {
        // When: local selection owns the gesture, release stays entirely local.
        return None;
    };
    Some(PointerMotionRoute::Report {
        pane_id: gesture.press_pane,
        sgr,
        row: gesture.last_cell.row,
        col: gesture.last_cell.col,
        modifiers,
    })
}

/// Consume a gesture interrupted by focus loss and release terminal ownership.
pub(super) fn take_focus_loss_pointer_release(
    gesture: &mut Option<PointerGesture>,
    modifiers: ModifiersState,
) -> Option<PointerMotionRoute> {
    take_pointer_release(gesture, modifiers)
}

/// Clear a gesture whose release can no longer arrive.
pub(super) fn cancel_pointer_gesture(gesture: &mut Option<PointerGesture>) -> bool {
    gesture.take().is_some()
}

/// Convert a pure pointer route into one pane-targeted protocol payload.
pub(super) fn pointer_route_bytes(
    route: PointerMotionRoute,
    kind: PointerReportKind,
) -> Option<(u64, Vec<u8>)> {
    let PointerMotionRoute::Report { pane_id, sgr, row, col, modifiers } = route else {
        // When: `route` is Local or None, ownership or mode suppressed the terminal report.
        return None;
    };
    Some((pane_id, pointer_report_bytes(sgr, kind, modifiers, row, col)))
}

/// Snapshot the terminal's tracking mode and SGR/legacy encoding profile.
pub(super) fn parser_mouse_profile(parser: &sonicterm_vt::vt::Parser) -> (MouseTracking, bool) {
    (parser.mouse_tracking(), parser.mouse_sgr_enabled())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WheelRoute {
    MouseReport,
    CursorKeys,
    LocalScrollback,
}

/// Select terminal wheel reports or the screen's untracked fallback.
pub(super) fn wheel_route(tracking: MouseTracking, is_alt: bool) -> WheelRoute {
    if tracking != MouseTracking::Off {
        WheelRoute::MouseReport
    } else if is_alt {
        // When: is_alt is true without tracking, pagers receive terminal cursor keys instead of local history motion.
        WheelRoute::CursorKeys
    } else {
        // When: is_alt is false and tracking is Off, only the terminal's local primary-screen scrollback moves.
        WheelRoute::LocalScrollback
    }
}

/// Encode `count` mouse-wheel reports for an app that has mouse tracking on.
/// Wheel buttons per xterm: 64 = up, 65 = down (press only, no release).
/// `sgr` true → SGR encoding `ESC[<Btn;col;row M` (1-based, unbounded).
/// `sgr` false → legacy X10 `ESC[M` + 3 bytes (button+32, col+32, row+32),
/// each byte clamped to the classic 223 ceiling (col/row+32 ≤ 255).
/// `col`/`row` are 1-based cell coordinates of the cell under the cursor.
pub(super) fn wheel_report_bytes(sgr: bool, up: bool, col: u32, row: u32, count: usize) -> Vec<u8> {
    let btn: u32 = if up { 64 } else { 65 };
    let mut out = Vec::new();
    for _ in 0..count {
        if sgr {
            out.extend_from_slice(format!("\x1b[<{btn};{col};{row}M").as_bytes());
        } else {
            // When: sgr is false, clamp each legacy X10 coordinate to one byte.
            // X10: parameters are value+32, capped so col/row+32 fit a byte.
            let cb = (btn + 32).min(255) as u8;
            let cx = (col.min(223) + 32) as u8;
            let cy = (row.min(223) + 32) as u8;
            out.extend_from_slice(&[0x1b, b'[', b'M', cb, cx, cy]);
        }
    }
    out
}

/// Decide whether a key chord should trigger the quit confirmation guard.
///
/// `key_str` is the normalized chord (e.g. `"super+q"`) and `bound` is the
/// action the active keymap maps it to, if any. Cmd+Q is a macOS system
/// chord, so on macOS an *unbound* `super+q` still quits — a user's edited or
/// symlinked keymap frequently omits it, and falling through to the PTY (which
/// types a literal `q`) is never what the user wants. An explicit `quit_app`
/// binding is honored on every platform. If the user deliberately rebound
/// `super+q` to some other action, we stand down and let that action run.
pub(super) fn is_quit_chord(key_str: &str, bound: Option<&Action>) -> bool {
    if matches!(bound, Some(Action::QuitApp)) {
        // When: matches finds bound is Some(Action::QuitApp), trigger the quit guard.
        return true;
    }
    cfg!(target_os = "macos") && key_str == "super+q" && bound.is_none()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowKeyOwner {
    Palette,
    Composition,
    Search,
    Copy,
    Terminal,
}

impl App {
    fn window_key_owner(&self, win_id: WindowId) -> Option<WindowKeyOwner> {
        let window = self.windows.get(&win_id)?;
        Some(if self.command_palette_owns_input(win_id) {
            WindowKeyOwner::Palette
        } else if window.ime.is_composing() {
            // When: ime is composing, native preedit owns keys before any non-modal editor.
            WindowKeyOwner::Composition
        } else if window
            .tab_states
            .get(window.tabs.active_index())
            .is_some_and(|tab| tab.search.is_some())
        {
            // When: the active tab has search, its editor takes precedence over copy navigation.
            WindowKeyOwner::Search
        } else if window.copy_mode.is_some() {
            // When: copy_mode is present without an editor, it blocks terminal input.
            WindowKeyOwner::Copy
        } else {
            // When: no palette, composition, search, or copy_mode owns input, the terminal may receive it.
            WindowKeyOwner::Terminal
        })
    }

    fn copy_mode_allows_keymap(&self, win_id: WindowId) -> bool {
        self.windows
            .get(&win_id)
            .and_then(|window| window.copy_mode.as_ref())
            .is_some_and(|copy| copy.quick_select.is_none())
    }

    /// Route native keys through source-window owners while retaining accepted terminal holds.
    pub(super) fn handle_window_keyboard(
        &mut self,
        win_id: WindowId,
        event: &KeyEvent,
        is_synthetic: bool,
    ) {
        let Some(modifiers) = self.windows.get(&win_id).map(|window| window.modifiers) else {
            // When: win_id is absent, a stale native event cannot borrow the main window's state.
            return;
        };
        if event.state == ElementState::Released {
            // When: event is Released, complete only destinations that admitted its physical press.
            let targets = self.windows.get_mut(&win_id).and_then(|window| {
                super::keyboard_protocol::take_release_routes(
                    &mut window.pty_pressed_keys,
                    event.physical_key,
                    is_synthetic,
                )
            });
            if let Some(mut targets) = targets {
                let writes = self.encoded_terminal_key_writes(
                    event,
                    modifiers,
                    &targets.keys().copied().collect(),
                    Some(&mut targets),
                    is_synthetic,
                );
                self.dispatch_terminal_key_writes(writes);
            }
            return;
        }
        if let Some(mut targets) = self.windows.get(&win_id).and_then(|window| {
            terminal_repeat_targets(&window.pty_pressed_keys, event.physical_key, event.repeat)
        }) {
            // When: targets retains an accepted hold, local overlays cannot redirect its repeats.
            let writes = self.encoded_terminal_key_writes(
                event,
                modifiers,
                &targets.keys().copied().collect(),
                Some(&mut targets),
                is_synthetic,
            );
            if let Some(window) = self.windows.get_mut(&win_id) {
                window.pty_pressed_keys.insert(event.physical_key, targets);
            }
            self.dispatch_terminal_key_writes(writes);
            return;
        }
        if let Some(chord) = key_event_to_string(event, modifiers) {
            // When: key_event_to_string resolves a chord, check quit before any local input owner.
            if is_quit_chord(&chord, self.keymap.lookup(&chord)) {
                // When: chord requests quit, its confirmation belongs to the originating window.
                self.on_quit_chord_pressed(win_id, event.repeat);
                return;
            }
        }
        match self.window_key_owner(win_id) {
            Some(WindowKeyOwner::Palette) => {
                // When: Palette owns this window, only its toggle bypasses palette editing.
                let toggle = key_event_to_string(event, modifiers)
                    .and_then(|chord| self.keymap.lookup(&chord))
                    == Some(&Action::OpenCommandPalette);
                if toggle {
                    self.run_action_for_window(&Action::OpenCommandPalette, win_id);
                } else {
                    // When: toggle is false, the attached palette consumes native text and edits.
                    self.command_palette_handle_key(event);
                }
                return;
            }
            Some(WindowKeyOwner::Composition) => {
                // When: Composition owns input, the OS IME supplies text and Escape only cancels preedit.
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    if let Some(window) = self.windows.get_mut(&win_id) {
                        window.ime.cancel();
                    }
                }
                return;
            }
            Some(WindowKeyOwner::Search) => {
                // When: Search owns input, field edits precede non-edit keymap actions and READONLY navigation.
                let text_edit = super::text_edit::search_text_edit_for_event(event, modifiers)
                    .is_some()
                    || super::text_edit::printable_event_text(event, modifiers).is_some();
                if !text_edit {
                    // When: text_edit is absent, search permits a non-edit binding without typing its key.
                    if let Some(action) = key_event_to_string(event, modifiers)
                        .and_then(|chord| self.keymap.lookup(&chord))
                        .filter(|action| !matches!(action, Action::OpenSearch))
                        .cloned()
                    {
                        // When: action is not a search edit or toggle, dispatch it without terminal fallback.
                        self.run_action_for_window(&action, win_id);
                        return;
                    }
                }
                self.search_handle_key(win_id, event, modifiers);
                return;
            }
            Some(WindowKeyOwner::Copy) => {
                // When: Copy owns input, quick-select hints remain local and other copy modes permit only safe actions.
                if self.copy_mode_allows_keymap(win_id) {
                    // When: copy_mode_allows_keymap excludes quick-select, safe actions can precede navigation.
                    for chord in key_event_to_strings(event, modifiers) {
                        if let Some(action) = self.keymap.lookup(&chord).cloned() {
                            // When: keymap resolves action, only the READONLY whitelist may bypass copy mode.
                            if super::keymap_dispatch::read_only_allows_action(&action)
                                && self.run_action_for_window(&action, win_id)
                            {
                                // When: action is permitted and consumed, it cannot also navigate copy mode.
                                return;
                            }
                        }
                    }
                }
                self.handle_window_copy_key(win_id, &event.logical_key);
                return;
            }
            Some(WindowKeyOwner::Terminal) => {
                // When: Terminal owns this window, configured bindings still precede protocol encoding.
            }
            None => {
                // When: win_id disappeared, do not substitute another input owner.
                return;
            }
        }
        for chord in key_event_to_strings(event, modifiers) {
            if let Some(action) = self.keymap.lookup(&chord).cloned() {
                // When: keymap resolves action, dispatch only after preserving platform passthrough bindings.
                if super::keymap_dispatch::terminal_input_passthrough_binding(&chord, &action) {
                    // When: chord is a platform passthrough, retain it for the terminal encoder.
                    continue;
                }
                if self.run_action_for_window(&action, win_id) {
                    // When: action consumed input, terminal encoding must not duplicate it.
                    return;
                }
            }
        }
        if event.repeat {
            // When: repeat has no accepted hold, it cannot acquire the currently active terminal.
            return;
        }
        let Some(active_pane) = self.windows.get(&win_id).and_then(|window| {
            window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane)
        }) else {
            // When: win_id has no active tab, there is no source pane for input or broadcast.
            return;
        };
        let targets = self.terminal_key_targets(active_pane);
        let writes =
            self.encoded_terminal_key_writes(event, modifiers, &targets, None, is_synthetic);
        let delivered = self.dispatch_terminal_key_writes(writes);
        if delivered.is_empty() {
            // When: delivered is empty, refused input must not clear selection or move the viewport.
            return;
        }
        let Some(window) = self.windows.get_mut(&win_id) else {
            // When: win_id disappeared, accepted peer routes cannot mutate a replacement window.
            return;
        };
        window.pty_pressed_keys.insert(event.physical_key, delivered);
        let mut dirty = false;
        if event.logical_key == Key::Named(NamedKey::Enter) && !modifiers.shift_key() {
            if let Some(pane) = window.panes.get_mut(&active_pane) {
                dirty |= pane.viewport_top_abs.take().is_some();
            }
        }
        if !matches!(event.logical_key, Key::Named(key) if super::key_encoding::is_modifier_key(key))
        {
            dirty |= window.selection.take().is_some();
        }
        if dirty {
            mark_all_panes_dirty(&window.panes);
        }
    }

    /// Apply source modifiers and refresh that window's existing target hover.
    pub(super) fn handle_window_modifiers_changed(
        &mut self,
        win_id: WindowId,
        modifiers: ModifiersState,
    ) {
        let Some(window) = self.windows.get_mut(&win_id) else {
            // When: win_id is gone, its modifiers cannot affect the main window or a peer.
            return;
        };
        window.modifiers = modifiers;
        self.refresh_target_hover(win_id);
        if let Some(window) = self.windows.get(&win_id) {
            window.request_redraw();
        }
    }

    /// Apply native focus and release only the source window's held input owners.
    pub(super) fn handle_window_focus_changed(&mut self, win_id: WindowId, focused: bool) {
        if !self.windows.contains_key(&win_id) {
            // When: win_id is gone, late focus cannot publish reducer state or touch another terminal.
            return;
        }
        if Some(win_id) == self.main_window_id {
            // Only main contributes this compatibility observation; live child focus stays in WindowState.
            let window = sonicterm_types::WindowKey::new(0);
            let intent = if focused {
                sonicterm_app_core::AppIntent::WindowFocused { window }
            } else {
                // When: focused is false, preserve the main compatibility blur observation.
                sonicterm_app_core::AppIntent::WindowBlurred { window }
            };
            self.observe_intent(intent);
        }
        if !focused {
            self.release_window_native_keys(win_id);
        }
        let mut pointer_release = None;
        let mut focus_report = None;
        if let Some(window) = self.windows.get_mut(&win_id) {
            if focused {
                window.ime_cursor_throttle.reset();
                self.frontmost_window = Some(win_id);
            } else if self.frontmost_window == Some(win_id) {
                // When: win_id still owns focus, clear it without cancelling a sibling's newer focus event.
                self.frontmost_window = None;
            }
            window.ime.cancel();
            if !focused {
                // Native button-up may never arrive after blur; consume the source's latched gesture.
                pointer_release =
                    take_focus_loss_pointer_release(&mut window.pointer_gesture, window.modifiers)
                        .and_then(|route| {
                            pointer_route_bytes(route, PointerReportKind::LeftRelease)
                        });
                window.scrollbar_drag = None;
                window.splitter_drag = None;
                window.mouse_down = false;
                window.invalidate_path_hover();
            }
            if let Some(renderer) = window.renderer.as_mut() {
                renderer.set_window_focused(focused);
            }
            if window.test_renderer_focus_marker.is_some() {
                window.test_renderer_focus_marker = Some(focused);
            }
            mark_all_panes_dirty(&window.panes);
            if let Some(active_pane) =
                window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane)
            {
                let enabled = window
                    .panes
                    .get(&active_pane)
                    .is_some_and(|pane| pane.parser.lock().focus_reporting_enabled());
                if enabled {
                    let bytes: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
                    focus_report = Some((active_pane, bytes.to_vec()));
                }
            }
            // IME stays enabled; focus-in resets only its caret throttle to avoid native context churn.
            window.request_redraw();
        }
        if let Some((pane_id, bytes)) = pointer_release {
            self.write_to_pane(pane_id, bytes, PtyInputSource::PointerButton);
        }
        if let Some((pane_id, bytes)) = focus_report {
            self.write_to_pane(pane_id, bytes, PtyInputSource::FocusReport);
        }
    }

    /// Route composition through the source window's palette, search, READONLY, or terminal owner.
    pub(super) fn handle_window_ime(&mut self, win_id: WindowId, ime_event: Ime) {
        if self.windows.contains_key(&win_id)
            && self.command_palette_handle_ime_in_window(win_id, &ime_event)
        {
            // When: win_id is live and its palette consumes ime_event, no other input owner may receive it.
            return;
        }
        let Some(window) = self.windows.get_mut(&win_id) else {
            // When: win_id no longer resolves, a late IME event must not fall back to another window.
            return;
        };
        let committed = match ime_event {
            Ime::Enabled => {
                window.ime.handle_enabled();
                String::new()
            }
            Ime::Disabled => {
                window.ime.handle_disabled();
                String::new()
            }
            Ime::Preedit(text, cursor) => {
                window.ime.handle_preedit(&text, cursor);
                String::new()
            }
            Ime::Commit(text) => {
                window.ime.handle_commit(&text);
                window.ime.take_commits()
            }
        };
        let active_tab = window.tab_states.get(window.tabs.active_index());
        let search_open = active_tab.is_some_and(|tab| tab.search.is_some());
        let active_pane = active_tab.map(|tab| tab.active_pane);
        let copy_mode = window.copy_mode.is_some();
        window.request_redraw();
        if committed.is_empty() {
            // When: committed is empty, composition changes require redraw but no search or PTY input.
            return;
        }
        if search_open {
            // An open search owns the commit even when its pane is temporarily missing.
            self.search_handle_ime_commit(win_id, &committed);
        } else if !copy_mode {
            // When: copy_mode is absent, the source window's active terminal may receive the commit.
            if let Some(active_pane) = active_pane {
                let bytes = committed.into_bytes();
                self.write_to_pane(active_pane, bytes.clone(), PtyInputSource::Ime);
                self.broadcast_from(active_pane, bytes, PtyInputSource::Ime);
            }
        }
    }

    // Ordering: pty_burst_gen uses Acquire; cursor_visible and the coherent keyboard_input word are Relaxed snapshots.
    pub(super) fn do_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        // mark any user-driven event so the next
        // RedrawRequested bypasses the vsync coalescing gate. This
        // covers main and child windows uniformly. PTY-byte
        // redraws (the high-volume path) arrive as RedrawRequested
        // with this flag still false and continue to coalesce.
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::ModifiersChanged(_)
                | WindowEvent::Ime(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(_)
        ) {
            self.input_dirty = true;
        }
        if self.is_warm_window_id(win_id) {
            // When: is_warm_window_id identifies win_id as unpromoted, ignore its event.
            return;
        }
        if !self.windows.contains_key(&win_id) {
            // When: win_id is stale, no event may reach the legacy main-window fallback.
            return;
        }
        if matches!(
            &event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::Ime(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(false)
        ) {
            if let Some(window) = self.windows.get_mut(&win_id) {
                window.invalidate_path_hover();
            }
        }
        if self.command_palette_handle_pointer_event(win_id, &event) {
            // When: `command_palette_handle_pointer_event` consumes input, keep both window handlers from receiving it.
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                self.drain_pending_window_creates(el);
            }
            return;
        }
        match event {
            WindowEvent::Ime(ime_event) => {
                // When: Ime arrives, one source-window owner handles composition before child dispatch.
                self.handle_window_ime(win_id, ime_event);
                return;
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
                // When: KeyboardInput arrives, source ownership precedes deferred creation and source redraw.
                self.handle_window_keyboard(win_id, &event, is_synthetic);
                self.drain_pending_window_creates(el);
                if let Some(window) = self.windows.get(&win_id) {
                    window.request_redraw();
                }
                return;
            }
            WindowEvent::Focused(focused) => {
                // When: Focused arrives, shared cleanup preserves held-input and native caret ownership.
                self.handle_window_focus_changed(win_id, focused);
                return;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                // When: ModifiersChanged arrives, update only its source window and existing hover state.
                self.handle_window_modifiers_changed(win_id, modifiers.state());
                return;
            }
            _ => {
                // When: event is not shared native input, retain the existing presentation and pointer routing.
            }
        }
        // Tear-out child windows: route to the dedicated handler so
        // each child renders/handles input on its own surface.
        // the main window also lives in `self.windows`
        // now (shadow entry, `Some(main_window_id)`), but its events
        // must continue to flow through the legacy `App.*` paths
        // below. Skip the main window's id explicitly.
        if self.windows.contains_key(&win_id) && Some(win_id) != self.main_window_id {
            // When: windows contains win_id and main_window_id differs, delegate to child state.
            self.handle_child_window_event(el, win_id, event);
            return;
        }
        match event {
            WindowEvent::DroppedFile(path) => {
                self.paste_file_paths_for_kind(FrontmostKind::Main, [path]);
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }
            WindowEvent::CloseRequested => {
                if let Some(window) = self.window_key(win_id) {
                    self.observe_intent(sonicterm_app_core::AppIntent::WindowCloseRequested {
                        window,
                    });
                }
                // If child windows still own tabs, hide the main
                // window instead of exiting the app — the children
                // are independent live terminals and must keep
                // running. When no child remains, closing the main
                // leaves no active terminal window, so quit.
                if self.child_window_count() == 0 {
                    self.hide_main_window();
                    el.exit();
                } else {
                    // When: child_window_count remains nonzero, hide main while child terminals stay live.
                    self.hide_main_window();
                }
            }

            WindowEvent::RedrawRequested => {
                // When: `RedrawRequested` arrives, collect one parser snapshot before resolving hover and presenting.
                let process_privileged = self.process_privilege.is_privileged();
                let was_dirty = self.input_dirty;
                let pty_burst_snapshot = self.pty_burst_gen.load(Ordering::Acquire);
                let pty_burst = pty_burst_snapshot != self.last_seen_burst_gen;
                // Perf audit #9: if we already rendered within the
                // current vsync window, defer this redraw until the
                // next monitor refresh boundary. `about_to_wait` will
                // see `pending_redraw` and call
                // `set_control_flow(WaitUntil(last_render +
                // frame_period))`; `new_events`' ResumeTimeReached arm
                // then re-requests the redraw. Net effect: bursty PTY
                // output coalesces into one frame per vsync instead of
                // burning the GPU at the VT thread's 16ms tick rate.
                // Input-driven redraws must be immediate — gating them
                // on the vsync deadline adds
                // perceptible latency to typing/resize/theme changes.
                // Only redraws that arrive purely from streaming PTY
                // bytes (input_dirty stays false) get coalesced.
                let last_render = self.main().map(|ws| ws.last_render).unwrap_or_else(Instant::now);
                // while composing an IME preedit on the software
                // rasterizer, drop to a lower frame cap so a long pinyin run
                // doesn't drive a full-surface raster at full cadence.
                let composing = self.main().map(|ws| ws.ime.is_composing()).unwrap_or(false);
                let frame_period = crate::app::effective_frame_period(
                    self.software_render_degrade,
                    composing,
                    self.frame_period,
                );
                if self.main().is_some_and(|window| {
                    window.contention_blocks_redraw(Instant::now(), frame_period)
                }) || crate::app::should_defer_streaming_redraw(
                    was_dirty,
                    pty_burst,
                    self.software_render_degrade,
                    last_render.elapsed(),
                    frame_period,
                ) {
                    // When: should_defer_streaming_redraw is true, schedule the frame for the next refresh.
                    self.pending_redraw = true;
                    return;
                }
                let mut timing = crate::app::render_timing::RenderTiming::start("main");
                self.pending_redraw = false;
                let main_id_opt = self.main_window_id;
                if let Some(id) = main_id_opt {
                    if let Some(ws) = self.windows.get_mut(&id) {
                        ws.tabs.clear_expired_command_badges(Instant::now());
                    }
                }
                self.poll_command_events_for_all_tabs();
                if let Some(id) = main_id_opt {
                    if let Some(ws) = self.windows.get_mut(&id) {
                        crate::app::refresh_window_tab_privileges(
                            &mut ws.tabs,
                            &ws.tab_states,
                            &mut ws.panes,
                            !pty_burst,
                        );
                    }
                }
                if let Some(t) = timing.as_mut() {
                    t.lap("poll");
                }
                let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                // Compute per-pane rects in window pixels so the renderer can
                // draw a border around each one (and a brighter one around
                // the focused pane). The active pane's grid is rendered into
                // the full content area; per-pane Buffer rendering is v0.4.
                let pane_rects: Vec<(u64, sonicterm_ui::pane::Rect)> = self
                    .main_tab_states()
                    .and_then(|ts| ts.get(tab_idx))
                    .map(|st| {
                        if let Some(r) = self.main_renderer() {
                            // Renderer geometry lays out every pane in the drawable content area.
                            let (w, h) = r.logical_size();
                            let top = (r.top_inset() - r.padding_top_px()).max(0.0);
                            let bottom = r.bottom_inset();
                            let outer = sonicterm_ui::pane::Rect::new(
                                0.0,
                                top,
                                w.max(0.0),
                                (h - top - bottom).max(0.0),
                            );
                            st.tree.layout(outer)
                        } else {
                            // When: main_renderer is absent, no pane rectangles can be derived.
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                let active_id = self
                    .main_tab_states()
                    .and_then(|ts| ts.get(tab_idx))
                    .map(|st| st.active_pane)
                    .unwrap_or(0);
                let broadcast_participants = self.broadcast_participants();
                if let Some(t) = timing.as_mut() {
                    t.lap("layout");
                }

                // per-pane scrollbar visibility/fade tick.
                // Built BEFORE the try_lock pass since it only needs
                // logical-px rects (already in `pane_rects`) and the
                // already-captured cursor pos / scrollbar_drag — no
                // parser lock needed. Result feeds each PaneRender's
                // `scrollbar_alpha` below.
                let scrollbar_now = Instant::now();
                let scrollbar_motion = crate::app::scrollbar_visibility::window_scrollbar_motion(
                    self.main_renderer().map(GpuRenderer::is_software_render_degraded),
                    self.software_render_degrade,
                );
                let scrollbar_alpha_map: std::collections::HashMap<u64, f32> = {
                    let mode = self.config.appearance.scrollbar;
                    let drag_pane =
                        self.main().and_then(|ws| ws.scrollbar_drag.as_ref().map(|s| s.pane_id));
                    let (cx, cy) = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                    let cursor = (cx as f32, cy as f32);
                    let rects: Vec<(u64, f32, f32, f32, f32)> =
                        pane_rects.iter().map(|(id, r)| (*id, r.x, r.y, r.w, r.h)).collect();
                    if let Some(ws) = self.main_mut() {
                        crate::app::scrollbar_visibility::update_and_collect(
                            &mut ws.scrollbar_vis,
                            &rects,
                            cursor,
                            active_id,
                            drag_pane,
                            mode,
                            scrollbar_motion,
                            scrollbar_now,
                        )
                    } else {
                        // When: main_mut is None, no scrollbar visibility state can be collected.
                        std::collections::HashMap::new()
                    }
                };
                // Keep redrawing while any pane's scrollbar fade is
                // animating so a paused mouse-leave still completes the
                // 300 ms fade-out (otherwise the bar would stay frozen
                // mid-fade until the next external event).
                let scrollbar_needs_more_frames = {
                    let mode = self.config.appearance.scrollbar;
                    let drag_pane =
                        self.main().and_then(|ws| ws.scrollbar_drag.as_ref().map(|s| s.pane_id));
                    self.main()
                        .map(|ws| {
                            ws.scrollbar_vis.iter().any(|(id, st)| {
                                crate::app::scrollbar_visibility::is_animating(
                                    st,
                                    mode,
                                    drag_pane == Some(*id),
                                    scrollbar_motion,
                                    scrollbar_now,
                                )
                            })
                        })
                        .unwrap_or(false)
                };
                if let Some(t) = timing.as_mut() {
                    t.lap("scrollbar");
                }
                if scrollbar_needs_more_frames {
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }

                // Fix 1: try_lock EVERY pane in the tab and pass
                // them ALL through to the renderer. The previous single-
                // element slice meant the per-pane loop inside
                // `GpuRenderer::render` never iterated inactive panes in
                // production frames — that was the visible "right pane
                // empty after split" bug.
                //
                // Strategy: clone every pane's parser Arc, try to lock
                // all of them in one pass. If ANY lock fails, defer the
                // redraw (§4 land-mine) and bail — partial frames are
                // not allowed because the renderer needs a coherent
                // multi-pane view, and a re-locked sub-pane would
                // produce torn output. Order is pane_rects order;
                // active position is recorded separately.
                let main_panes_for_arcs = self.main_panes();
                // `try_lock`, never a blocking `lock`: the VT worker holds
                // this while merging a decoded batch, and blocking here would
                // stall the event loop behind it. On contention this defers
                // the redraw exactly as the parser locks below do — the
                // renderer needs a coherent view of every pane, so reusing a
                // stale image list for one pane while the rest advance would
                // tear the frame rather than merely delay it.
                let mut inline_images_by_pane: std::collections::HashMap<
                    u64,
                    Vec<sonicterm_render_model::InlineImage>,
                > = std::collections::HashMap::new();
                let mut inline_images_locked = true;
                if let Some(panes) = main_panes_for_arcs {
                    // When: main_panes_for_arcs is Some, snapshot each pane's inline images.
                    for (id, pane) in panes.iter() {
                        match pane.inline_images.try_lock() {
                            Some(images) => {
                                // Available image locks contribute to the coherent frame snapshot.
                                inline_images_by_pane.insert(*id, images.clone());
                            }
                            None => {
                                // When: try_lock returns None, reject the partial multi-pane snapshot.
                                inline_images_locked = false;
                                break;
                            }
                        }
                    }
                }
                if !inline_images_locked {
                    // When: inline_images_locked is false, release snapshots and retry the frame later.
                    drop(inline_images_by_pane);
                    self.defer_redraw_on_lock_contention(was_dirty);
                    return;
                }
                let parser_arcs: Vec<(
                    u64,
                    std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>,
                    sonicterm_ui::pane::Rect,
                )> = pane_rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        main_panes_for_arcs
                            .and_then(|panes| panes.get(id))
                            .map(|p| (*id, std::sync::Arc::clone(&p.parser), *rect))
                    })
                    .collect();
                if let Some(t) = timing.as_mut() {
                    t.lap("inline_images");
                }
                let mut guards: Vec<(
                    u64,
                    parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser>,
                    sonicterm_ui::pane::Rect,
                )> = Vec::with_capacity(parser_arcs.len());
                let mut all_locked = true;
                for (id, arc, rect) in &parser_arcs {
                    match arc.try_lock() {
                        Some(g) => {
                            // When: try_lock returns Some(g), retain its guard for the coherent frame.

                            // Available parser locks retain their guards for the coherent pane frame.
                            // Extending the guard's lifetime to the outer scope is
                            // valid because `arc` lives in `parser_arcs`, which is
                            // dropped strictly after `guards`, so the underlying
                            // Mutex outlives every guard. parking_lot guards carry
                            // a `*const Mutex` internally and no `'a` tied to `arc`.
                            let g_ext: parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser> =
                                // SAFETY: parser_arcs outlives guards, preserving every guard's backing Mutex.
                                unsafe { std::mem::transmute(g) };
                            guards.push((*id, g_ext, *rect));
                        }
                        None => {
                            // When: try_lock returns None, reject the partial multi-pane frame.
                            all_locked = false;
                            break;
                        }
                    }
                }
                if let Some(t) = timing.as_mut() {
                    t.lap("try_lock");
                }
                if !all_locked {
                    // When: all_locked is false, release every parser guard before deferring.
                    drop(guards);
                    drop(parser_arcs);
                    self.defer_redraw_on_lock_contention(was_dirty);
                    return;
                }

                self.refresh_target_hover_from_parsers(
                    win_id,
                    guards.iter().map(|(id, parser, _)| (*id, &**parser)),
                );
                if let Some(window) = self.main_mut() {
                    window.coherent_frame_collected();
                }
                let marker_observed = self.runtime_smoke.as_ref().is_some_and(|smoke| {
                    smoke.is_waiting_for_marker()
                        && guards.iter().any(|(_, parser, _)| {
                            grid_contains_marker(parser.grid(), smoke.marker())
                        })
                });
                if marker_observed {
                    let baseline =
                        self.main_renderer().map(GpuRenderer::successful_frame_count).unwrap_or(0);
                    if let Some(smoke) = self.runtime_smoke.as_mut() {
                        smoke.begin_present_wait(baseline);
                    }
                }
                let smoke_waiting_for_present =
                    self.runtime_smoke.as_ref().is_some_and(|smoke| smoke.is_waiting_for_present());
                let mut smoke_presented_count = None;

                if let Some(r) = self.main_renderer_mut() {
                    r.set_inactive_pane_cursors(Vec::new());
                }

                // lift the main window Arc clone before the
                // mut borrow on `self.renderer` below, so the IME
                // cursor-area branch can still touch
                // `ws.ime_cursor_throttle` (mut) without re-borrowing
                // `self`.
                let main_window_for_ime = self.main_window().cloned();
                let main_palette_ime_area = main_window_for_ime.as_ref().and_then(|w| {
                    let r = self.main_renderer()?;
                    if self.palette_attached_window.is_some() || !self.command_palette.is_open() {
                        // When: palette_attached_window is Some or command_palette is closed, main has no IME anchor.
                        return None;
                    }
                    self.command_palette_ime_cursor_area(
                        w.inner_size().width as f32,
                        w.inner_size().height as f32,
                        self.config.appearance.panel_padding,
                        r.scale_factor(),
                        r.font_size() * r.scale_factor(),
                        r.cell_w,
                    )
                });
                // Search-bar IME geometry: the full marker-free label drives
                // box width, but the caret/candidate-window anchor must follow
                // the current query caret, not the end of the label. Produce
                // both strings from the same state so the OS candidate area
                // agrees with the renderer-owned block cursor.
                let (search_ime_label, search_ime_prefix) = self
                    .main()
                    .and_then(|ws| {
                        let preedit = ws.ime.preedit();
                        let i = ws.tabs.active_index();
                        ws.tab_states.get(i).and_then(|st| st.search.as_ref()).map(|s| {
                            (search_bar_label(s, preedit), search_query_caret_prefix(s, preedit))
                        })
                    })
                    .unzip();
                // Borrow-split: pull the renderer out via direct
                // map-lookup on `self.windows` (NOT through `main_renderer_mut`,
                // which would borrow all of `self`). That keeps
                // `self.command_palette`, `self.ime` available for the
                // disjoint mut borrows the render call needs in the same
                // expression scope.
                // panes now live in `ws` too, so they're
                // pulled from the same field-disjoint split borrow.
                let main_id_opt = self.main_window_id;
                let mut ws_opt = main_id_opt.and_then(|id| self.windows.get_mut(&id));
                if let Some(ws) = ws_opt.as_deref_mut() {
                    if let Some(active_pos) = guards.iter().position(|(id, _, _)| *id == active_id)
                    {
                        invalidate_selection_for_content(
                            &mut ws.selection,
                            &mut ws.select_anchor,
                            active_id,
                            guards[active_pos].1.grid(),
                        );
                        if self.command_palette.is_open() && self.palette_attached_window.is_none()
                        {
                            self.command_palette.set_context(
                                super::overlays::command_palette_context(
                                    ws,
                                    Some(guards[active_pos].1.grid()),
                                ),
                            );
                            self.command_palette.set_tabs(&ws.tabs, &self.i18n);
                        }
                    }
                }
                #[allow(clippy::type_complexity)]
                let (
                    renderer_opt,
                    tabs_opt,
                    tab_states_opt,
                    panes_opt,
                    cursor_visible_now,
                    last_render_slot,
                    ws_selection_ref,
                    ws_copy_mode_ref,
                    ws_ime_ref,
                    ws_ime_throttle_ref,
                    ws_viewport_tops,
                    ws_hovered_url_cells,
                    ws_notification_ref,
                    ws_link_preview_ref,
                ): (
                    Option<&mut GpuRenderer>,
                    Option<&mut sonicterm_ui::tabs::TabBar>,
                    Option<&mut Vec<TabState>>,
                    Option<&mut std::collections::HashMap<u64, crate::app::PaneState>>,
                    bool,
                    Option<&mut Instant>,
                    Option<&Selection>,
                    Option<&CopyModeState>,
                    Option<&sonicterm_ui::ime::ImeState>,
                    Option<&mut sonicterm_ui::ime::ImeCursorThrottle>,
                    std::collections::HashMap<u64, Option<u64>>,
                    Option<sonicterm_render_model::inputs::HoveredUrlCells>,
                    Option<&sonicterm_ui::overlays::NotificationBubble>,
                    Option<&sonicterm_render_model::inputs::LinkPreview>,
                ) = match ws_opt {
                    Some(ws) => {
                        // Split the available WindowState render inputs into disjoint borrows.
                        // cursor_visible is now per-pane; read
                        // it from the active pane before splitting the
                        // mut borrow of `ws.panes`. Bool read, no
                        // lasting borrow.
                        let cv = ws
                            .panes
                            .get(&active_id)
                            .map(|p| p.cursor_visible.load(std::sync::atomic::Ordering::Relaxed))
                            .unwrap_or(true);
                        // selection + copy_mode now live on
                        // `ws`. Pull immutable refs disjoint from the mut
                        // borrows of `ws.{renderer,tabs,tab_states,panes,last_render}`.
                        // ime + ime_cursor_throttle also live
                        // on `ws`; split-borrow disjointly too.
                        let sel_ref = ws.selection.as_ref();
                        let cm_ref = ws.copy_mode.as_ref();
                        // Shared URI and OSC 8 fragments retain hint/accent state independently of glyph-row dirt.
                        let hovered_url_cells = ws.hovered_url.as_ref().map(|h| h.to_cells());
                        let notification_ref = ws.notification.as_ref();
                        let viewport_tops = ws
                            .panes
                            .iter()
                            .map(|(id, pane)| (*id, pane.viewport_top_abs))
                            .collect();
                        (
                            ws.renderer.as_mut(),
                            Some(&mut ws.tabs),
                            Some(&mut ws.tab_states),
                            Some(&mut ws.panes),
                            cv,
                            Some(&mut ws.last_render),
                            sel_ref,
                            cm_ref,
                            Some(&ws.ime),
                            Some(&mut ws.ime_cursor_throttle),
                            viewport_tops,
                            hovered_url_cells,
                            notification_ref,
                            ws.link_preview.as_ref(),
                        )
                    }
                    None => {
                        // An absent WindowState produces empty render inputs without borrowing self.
                        (
                            None,
                            None,
                            None,
                            None,
                            true,
                            None,
                            None,
                            None,
                            None,
                            None,
                            std::collections::HashMap::new(),
                            None,
                            None,
                            None,
                        )
                    }
                };
                if let (Some(r), Some(pane), Some(tabs_mref), Some(tab_states_mref)) = (
                    renderer_opt,
                    panes_opt.and_then(|p| p.get_mut(&active_id)),
                    tabs_opt,
                    tab_states_opt,
                ) {
                    // When: renderer_opt, pane, tabs_mref, and tab_states_mref are Some, render one coherent frame.
                    let (cursor_rc, cursor_pane_rect) = {
                        // Fix 1: the active pane's parser guard is
                        // already in `guards` from the global try_lock pass
                        // above; locking it again here would AB-BA deadlock
                        // (we already hold it). Find the active guard via
                        // a mut borrow over `guards`.
                        let active_pos = guards
                            .iter()
                            .position(|(id, _, _)| *id == active_id)
                            // PANIC: `active_id` must be a live visible leaf; `guards` covers the successfully locked layout.
                            .expect("active pane guard collected above");
                        // Wezterm-style tab title: `#N icon parent/leaf`.
                        // Pull cwd from OSC 7, the foreground process from
                        // the pid probe (macOS only for now), and the OSC
                        // 0/2 title as the last-resort body (so `ssh
                        // user@host` still labels itself).
                        //
                        // Shared with `app/child_window.rs` via
                        // `refresh_active_tab_title` so Cmd+N / tear-out
                        // windows pick up cwd-based titles too (was
                        // previously stuck on the literal "shell N"
                        // placeholder set at spawn time).
                        let _ = crate::app::refresh_active_tab_title(
                            tabs_mref,
                            pane,
                            &guards[active_pos].1,
                            tab_idx,
                            !pty_burst,
                        );
                        if let Some(search) =
                            tab_states_mref.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                        {
                            let grid = guards[active_pos].1.grid();
                            let view_top = GpuRenderer::resolved_view_top_abs_legacy(
                                grid,
                                pane.viewport_top_abs,
                            );
                            super::search_handle::prepare_search(search, active_id, grid, view_top);
                        }
                        let search = tab_states_mref.get(tab_idx).and_then(|t| t.search.as_ref());
                        // Fix 1: build the slice from ALL panes
                        // (was previously a single-element slice for the
                        // active pane only). The renderer's per-pane loop
                        // now actually iterates every pane in production
                        // frames, so split panes paint.
                        let mut panes_slice: Vec<sonicterm_render_model::PaneRender<'_>> = guards
                            .iter_mut()
                            .map(|(id, g, rect)| sonicterm_render_model::PaneRender {
                                id: *id,
                                rect_px: sonicterm_render_model::geometry::PixelRect {
                                    x: rect.x as i32,
                                    y: rect.y as i32,
                                    w: rect.w as u32,
                                    h: rect.h as u32,
                                },
                                grid: g.grid_mut(),
                                viewport_top_abs: ws_viewport_tops.get(id).copied().flatten(),
                                is_active: *id == active_id,
                                cursor_style: sonicterm_render_model::CursorStyle::default(),
                                is_broadcast_participant: broadcast_participants.contains(id),
                                scrollbar_alpha: scrollbar_alpha_map
                                    .get(id)
                                    .copied()
                                    .unwrap_or(0.0),
                                inline_images: inline_images_by_pane
                                    .get(id)
                                    .cloned()
                                    .unwrap_or_default(),
                            })
                            .collect();
                        r.set_render_timing_label("main");
                        if let Err(e) = r.render(
                            &mut panes_slice,
                            &self.theme,
                            cursor_visible_now
                                && !(self.command_palette.is_open()
                                    && self.palette_attached_window.is_none()),
                            ws_selection_ref,
                            ws_copy_mode_ref,
                            tabs_mref,
                            process_privileged,
                            search,
                            // Feed the palette only to its attached window;
                            // `None` denotes main, and routing it elsewhere
                            // would paint the overlay on the wrong window.
                            self.palette_attached_window
                                .is_none()
                                .then_some(&mut self.command_palette),
                            ws_ime_ref,
                            pane.viewport_top_abs,
                            ws_notification_ref,
                            ws_hovered_url_cells,
                            ws_link_preview_ref,
                        ) {
                            tracing::warn!("render error: {e}");
                            if smoke_waiting_for_present {
                                smoke_presented_count = Some(Err(RuntimeSmokeFailure::Present));
                            }
                        } else if smoke_waiting_for_present {
                            // When: `smoke_waiting_for_present` is true, retain the post-render native-present count.
                            smoke_presented_count = Some(Ok(r.successful_frame_count()));
                        }
                        if let Some(t) = timing.as_mut() {
                            t.lap("render");
                        }
                        self.input_dirty = false;
                        // mark only the generation sampled at
                        // the start of this RedrawRequested as seen.
                        // A burst arriving during render keeps the
                        // counter ahead of last_seen_burst_gen so the
                        // next redraw bypasses the vsync gate.
                        self.last_seen_burst_gen = pty_burst_snapshot;
                        if let Some(lr) = last_render_slot {
                            *lr = Instant::now();
                        }
                        let g = guards[active_pos].1.grid_mut();
                        ((g.cursor.row, g.cursor.col), guards[active_pos].2)
                    };
                    // refresh the OS-drag tab bar
                    // snapshot so cross-window drop hit-tests see the
                    // current layout. Cross-window drops read this; an
                    // empty registry means every drop resolves to
                    // `DroppedOnEmpty` instead of a concrete destination.
                    // (moved after the renderer borrow scope below)
                    // Tell the OS where the active text cursor lives so the
                    // IME candidate window (pinyin candidates, Japanese
                    // romaji selector, Korean Hangul composer) appears
                    // immediately below the cell being edited — not
                    // pinned to the top-left corner of the screen as
                    // happens when the area is never set.
                    if let Some(w) = main_window_for_ime {
                        let mut ws_ime_throttle_ref = ws_ime_throttle_ref;
                        if main_palette_ime_area.is_some() || search_ime_label.is_some() {
                            if let Some(throttle) = ws_ime_throttle_ref.as_deref_mut() {
                                throttle.reset();
                            }
                        }
                        if let Some((pos, size)) = main_palette_ime_area {
                            // The main-hosted palette anchors the candidate window to its caret.
                            w.set_ime_cursor_area(pos, size);
                        } else if let Some(search_label) = search_ime_label.as_ref() {
                            // When: search_ime_label is Some, derive the candidate anchor from its query caret.
                            let window_size = w.inner_size();
                            // window_size + the SearchBarLayout it feeds are
                            // physical px, so every logical-px term here must be
                            // scaled by the renderer's scale factor or the IME
                            // caret rect drifts on HiDPI displays.
                            let scale = r.scale_factor();
                            let font_size = r.font_size() * scale;
                            let icon_w = r.measure_overlay_text_width(SEARCH_BADGE_ICON, font_size);
                            let content_w = icon_w
                                + SEARCH_BAR_ICON_GAP * scale
                                + r.measure_overlay_text_width(search_label, font_size);
                            let row =
                                u8::from(ws_copy_mode_ref.is_some_and(|cm| cm.is_read_only()));
                            let layout = SearchBarLayout::compute_at_row(
                                window_size.width as f32,
                                window_size.height as f32,
                                content_w,
                                row,
                                scale,
                            );
                            let text_x = layout.border.x
                                + SEARCH_BAR_PAD_LEFT * scale
                                + icon_w
                                + SEARCH_BAR_ICON_GAP * scale;
                            // Right inner edge: the candidate window must never
                            // push past the box padding.
                            let right_edge = (layout.border.x + layout.border.w
                                - SEARCH_BAR_PAD_RIGHT * scale)
                                .max(text_x);
                            // Anchor the OS candidate window at the END OF THE
                            // QUERY (`text_x + width("/ " + query)`), matching
                            // the inline preedit caret, then clamp to the right
                            // inner edge. `font_size` already folds in `scale`.
                            let prefix_w = search_ime_prefix
                                .as_ref()
                                .map(|p| r.measure_overlay_text_width(p, font_size))
                                .unwrap_or(0.0);
                            let caret_x = (text_x + prefix_w).clamp(text_x, right_edge);
                            let pos = winit::dpi::PhysicalPosition::new(
                                caret_x as i32,
                                layout.border.y as i32,
                            );
                            let size = winit::dpi::PhysicalSize::new(
                                r.cell_w.ceil() as u32,
                                layout.border.h.ceil() as u32,
                            );
                            w.set_ime_cursor_area(pos, size);
                        } else if let Some(throttle) = ws_ime_throttle_ref {
                            // When: ws_ime_throttle_ref is Some(throttle), use terminal cell IME geometry.
                            if let Some([x, y]) = r.pane_grid_origin(active_id) {
                                // IME follows the planned text origin rather than raw pane padding.
                                let rect = sonicterm_ui::pane::Rect::new(
                                    x,
                                    y,
                                    cursor_pane_rect.w,
                                    cursor_pane_rect.h,
                                );
                                super::update_terminal_ime_cursor_area(
                                    throttle,
                                    (active_id, rect),
                                    cursor_rc,
                                    (r.cell_w, r.cell_h),
                                    (0.0, 0.0),
                                    |pos, size| w.set_ime_cursor_area(pos, size),
                                );
                            }
                        }
                    }
                }
                if let Some(presented) = smoke_presented_count {
                    // When: `smoke_presented_count` contains `presented`, classify the marker-bearing frame.
                    match presented {
                        Ok(count) => {
                            // Compare the successful presentation count with the frozen baseline.
                            let advanced = self
                                .runtime_smoke
                                .as_mut()
                                .is_some_and(|smoke| smoke.observe_presented_frame(count));
                            if advanced {
                                // The marker-bearing main frame has now reached native presentation.
                                tracing::info!("runtime smoke main presentation exercised");
                            }
                        }
                        Err(failure) => {
                            // When: `presented` is `Err(failure)`, retain presentation as the failed boundary.
                            if let Some(smoke) = self.runtime_smoke.as_mut() {
                                smoke.fail(failure);
                            }
                            el.exit();
                            return;
                        }
                    }
                }
                // refresh OS-drag tab bar snapshot
                // for the main window. Outside the renderer borrow scope
                // so the immutable self borrow doesn't conflict with `r`.
                self.publish_main_window_tab_bar();
                if let Some(t) = timing {
                    t.finish();
                }
            }

            WindowEvent::Resized(size) => {
                // When: WindowEvent::Resized supplies size, update geometry before scheduling.

                // Resized updates renderer and pane geometry before scheduling the replacement frame.
                if self
                    .main_renderer_mut()
                    .is_some_and(|renderer| !renderer.try_resize(size.width, size.height))
                {
                    // When: try_resize returns false for size, retain the previous surface.
                    tracing::warn!(
                        width = size.width,
                        height = size.height,
                        "main window resize ignored after renderer safety rejection"
                    );
                    return;
                }
                // Notify the reducer of the new logical grid dimensions.
                // Derive cols/rows from
                // the renderer's cell size; fall back to zero when
                // unavailable (smoke-test environments). The
                // reducer's `WindowResize` Effect is observability-
                // only — the boundary above already drove the wgpu
                // resize, and the existing `request_redraw` below is
                // the production paint path.
                let (cols_u16, rows_u16) = {
                    let cell = self.main_renderer().map(GpuRenderer::cell_size);
                    match cell {
                        Some((cw, ch)) if cw > 0.0 && ch > 0.0 => (
                            ((size.width as f32 / cw).floor() as u32).min(u16::MAX as u32) as u16,
                            ((size.height as f32 / ch).floor() as u32).min(u16::MAX as u32) as u16,
                        ),
                        _ => (0u16, 0u16),
                    }
                };
                self.observe_intent(sonicterm_app_core::AppIntent::WindowResized {
                    window: sonicterm_types::WindowKey::new(0),
                    cols: cols_u16,
                    rows: rows_u16,
                });
                // Per-pane sizing: each pane's grid + PTY is resized to
                // its own PaneRect within the new window content area,
                // never to the whole window's dimensions.
                let rects = self.compute_active_pane_rects();
                let metrics = self.main_renderer().map(|r| {
                    (
                        r.cell_size(),
                        [
                            r.padding_left_px(),
                            r.padding_right_px(),
                            r.padding_top_px(),
                            r.padding_bottom_px(),
                        ],
                    )
                });
                if let (Some(((cw, ch), inset)), Some(panes)) = (metrics, self.main_panes()) {
                    crate::app::resize_panes_to_rects(panes, &rects, cw, ch, inset);
                }
                // Cell geometry changed — force the next render to
                // re-publish the IME cursor area even if (row, col) is
                // unchanged, otherwise the OS candidate window stays
                // pinned to the pre-resize pixel location.
                if let Some(ws) = self.main_mut() {
                    ws.ime_cursor_throttle.reset();
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale, mut inner_size_writer } => {
                // When: ScaleFactorChanged arrives, synchronously bind native and renderer geometry to one physical target.
                if let Some(id) = self.main_window_id {
                    // When: main_window_id identifies the live main window, update exactly that state.
                    if let Some(ws) = self.windows.get_mut(&id) {
                        // When: windows still contains id, apply the shared transition before winit commits WM_DPICHANGED.
                        let _ = crate::app::apply_window_dpi_transition(
                            ws,
                            dpi_scale,
                            &mut inner_size_writer,
                        );
                    }
                }
            }

            // -- Mouse --
            WindowEvent::CursorLeft { .. } => {
                let mut redraw = false;
                if let Some(r) = self.main_renderer_mut() {
                    redraw = r.set_hover_cursor(None);
                }
                if let Some(ws) = self.main_mut() {
                    ws.splitter_hover = None;
                    ws.cursor_pos = (-1.0, -1.0);
                }
                if let Some(window_id) = self.main_window_id {
                    self.clear_target_hover(window_id);
                }
                if let Some(w) = self.main_window() {
                    w.set_cursor(CursorIcon::Default);
                }
                if self.clear_scrollbar_hover() {
                    redraw = true;
                }
                if redraw {
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // When: WindowEvent::CursorMoved supplies position, update pointer interaction.

                // CursorMoved refreshes pointer-driven overlays, drags, selection, and cross-window targets.
                let (lx, ly) = (position.x as f32, position.y as f32);
                if let Some(ws) = self.main_mut() {
                    ws.cursor_pos = (position.x, position.y);
                }
                // Notify the reducer so its compatibility state tracks the
                // cursor. Its effects are discarded, so hover repaints come from
                // the app's own checks below.
                self.observe_intent(sonicterm_app_core::AppIntent::MouseMove {
                    window: sonicterm_types::WindowKey::new(0),
                    pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                });
                let mut hover_redraw = false;
                if let Some(r) = self.main_renderer_mut() {
                    hover_redraw = r.set_hover_cursor(Some((lx, ly)));
                }
                if hover_redraw {
                    // A bare hover-move over the tab bar must repaint —
                    // otherwise the muted × → bright × transition lags
                    // until the next unrelated event.
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
                // Auto scrollbar hover is also pure cursor state. Terminal
                // cursor moves from normal PTY output do not wake the renderer,
                // so request a frame exactly when the pointer crosses the
                // right-edge proximity threshold.
                let _ = self.refresh_scrollbar_hover_from_cursor();
                if self.apply_splitter_drag(lx, ly) {
                    // When: apply_splitter_drag consumes the motion, skip tab and text drag routing.
                    return;
                }
                // Update the live drag session position so the chip
                // can follow the cursor in the renderer overlay.
                let drag_snapshot = self.main_mut().and_then(|ws| {
                    ws.drag_session.as_mut().map(|s| {
                        s.current_pos = (lx, ly);
                        *s
                    })
                });
                let resolved_drag = drag_snapshot.and_then(|session| {
                    self.tab_index_of_id(session.source_window, session.source_tab)
                        .map(|index| (index, session))
                });
                if drag_snapshot.is_some() && resolved_drag.is_none() {
                    // When: `resolved_drag` cannot find the captured tab, cancel the existing gesture before any pointer fallthrough.
                    self.cancel_drag_session();
                    return;
                }
                if let Some((press_idx, session_snapshot)) = resolved_drag {
                    let title = self
                        .main_tabs()
                        .and_then(|t| t.tabs().get(press_idx).map(|tab| tab.title.clone()))
                        .unwrap_or_default();
                    let window_width =
                        self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                    let (bar_h, top_off, visible) = self
                        .main_renderer()
                        .map(|r| {
                            (r.tab_bar_logical_height(), r.tab_bar_y_offset(), r.tab_bar_visible())
                        })
                        .unwrap_or((sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT, 0.0, true));
                    let empty_tabs = sonicterm_ui::tabs::TabBar::new();
                    let layout = TabBarLayout::compute_with_height(
                        self.main_tabs().unwrap_or(&empty_tabs),
                        window_width,
                        bar_h,
                    )
                    .with_top_offset(top_off)
                    .with_visible(visible);
                    let chip = crate::tab_drag::build_drag_chip_overlay(
                        &session_snapshot,
                        &layout,
                        press_idx,
                        title,
                    );
                    if let Some(r) = self.main_renderer_mut() {
                        r.set_drag_chip(chip);
                    }
                }
                // Cross-window drag-merge: if a tab is held, update the
                // pending drop target based on the global cursor
                // position. The actual decision (tear / merge / cancel)
                // is deferred to mouse-up via `compute_action`.
                let (mouse_down, has_press) = self
                    .main()
                    .map(|ws| (ws.mouse_down, ws.pressed_tab.is_some()))
                    .unwrap_or((false, false));
                if mouse_down && has_press {
                    // When: mouse_down and has_press are true, update the tab drop target and OS handoff.
                    let target = self.compute_main_drag_target((position.x, position.y));
                    if let Some(ws) = self.main_mut() {
                        ws.drag_target = target;
                    }
                    // start the OS-level
                    // drag session AS SOON AS the cursor crosses the
                    // drag-start threshold from its press point, not on
                    // mouse-release. Windows `DoDragDrop` needs the live
                    // button state for cursor capture. The current macOS
                    // backend is pasteboard-only, but shares this trigger so
                    // the payload is published once per gesture. The `os_drag_handoff_started` flag
                    // ensures we only attempt the handoff once per
                    // gesture; if it succeeds the backend owns the
                    // gesture end-to-end (Windows) or has already
                    // written the pasteboard (macOS).
                    if !self.os_drag_handoff_started {
                        // When: os_drag_handoff_started is false, test whether the gesture crossed the threshold.
                        let started_idx = self.main().and_then(|ws| {
                            ws.drag_session
                                .as_ref()
                                .filter(|s| crate::tab_drag::drag_moved_enough(s))
                                .and_then(|s| self.tab_index_of_id(s.source_window, s.source_tab))
                        });
                        if let Some(idx) = started_idx {
                            // When: started_idx is Some, transfer this tab gesture to the OS backend once.
                            self.os_drag_handoff_started = true;
                            let _ = self.try_os_drag_handoff(idx);
                        }
                    }
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                if self.main().map(|ws| ws.mouse_down).unwrap_or(false) {
                    // When: mouse_down is true, apply scrollbar, terminal, or text-selection drag semantics.

                    let cell = self
                        .main_renderer()
                        .and_then(|r| r.pixel_to_pane_cell(lx, ly))
                        .map(|(pane_id, row, col)| PointerCell { pane_id, row, col });
                    let gesture_route = self.main_mut().and_then(|ws| {
                        let modifiers = ws.modifiers;
                        ws.pointer_gesture
                            .as_mut()
                            .map(|gesture| route_pressed_pointer_motion(gesture, cell, modifiers))
                    });
                    if let Some(route) = gesture_route {
                        // When: `gesture_route` exists, its press-time owner wins before local continuation paths.
                        match route {
                            PointerMotionRoute::Local => {
                                // When: `route` is Local, continue into SonicTerm's selection or scrollbar motion.
                            }
                            PointerMotionRoute::None => {
                                // When: `route` is None, Button mode suppresses held motion and consumes the move.
                                return;
                            }
                            report @ PointerMotionRoute::Report { .. } => {
                                // When: `route` carries Report data, encode and enqueue held-left motion.
                                if let Some((pane_id, bytes)) =
                                    pointer_route_bytes(report, PointerReportKind::HeldLeftMotion)
                                {
                                    self.write_to_pane(
                                        pane_id,
                                        bytes,
                                        PtyInputSource::PointerMotion,
                                    );
                                }
                                return;
                            }
                        }
                    }

                    // scrollbar drag takes priority over
                    // selection extension while a thumb is held. Match
                    // CLAUDE.md §4 — keep this branch fast; no parser
                    // lock is needed (geometry was snapshotted at press).
                    if let Some((pane_id, new_view_top)) = self.scrollbar_drag_apply(lx, ly) {
                        // When: scrollbar_drag_apply returns pane_id and new_view_top, update that viewport.

                        // Resolve `live_top` for the dragged pane (not
                        // necessarily the active one — keep the gesture
                        // pinned to the press pane even if focus shifted).
                        let live_top_opt = self.main().and_then(|ws| {
                            ws.panes.get(&pane_id).and_then(|p| {
                                p.parser.try_lock().map(|parser| {
                                    let g = parser.grid();
                                    g.scrollback_len() as u64
                                })
                            })
                        });
                        if let Some(live_top) = live_top_opt {
                            // The parser snapshot clamps the dragged viewport against live output.
                            if let Some(ws) = self.main_mut() {
                                if let Some(pane) = ws.panes.get_mut(&pane_id) {
                                    pane.viewport_top_abs = if new_view_top >= live_top {
                                        // Reaching current output resumes following the live bottom.
                                        None
                                    } else {
                                        // When: new_view_top is below live_top, retain its absolute history position.
                                        Some(new_view_top)
                                    };
                                }
                                super::mark_all_panes_dirty(&ws.panes);
                                if let Some(w) = ws.window.as_ref() {
                                    w.request_redraw();
                                }
                            }
                        }
                        // drag also counts as scrollbar activity.
                        self.mark_scrollbar_active(pane_id);
                        return;
                    }
                    if let Some(r) = self.main_renderer() {
                        if let Some((row, col)) =
                            r.pixel_to_cell(position.x as f32, position.y as f32)
                        {
                            // WezTerm-style drag granularity.
                            // The press recorded `select_mode` + `select_anchor`;
                            // extend by Cell / Word / Line accordingly.
                            //
                            // Word/Line need the live grid, so compute the
                            // replacement Selection up front while we still
                            // hold only &self (via try_lock inside the helper,
                            // which drops the grid lock before we redraw —
                            // CLAUDE.md §4). `r`'s last use was pixel_to_cell,
                            // so the &self / &mut self borrows below are fine.
                            let (mode, anchor) = self
                                .main()
                                .map(|ws| (ws.select_mode, ws.select_anchor))
                                .unwrap_or((SelectMode::Cell, (0, 0)));
                            // Some(Some(_)) = recomputed region; Some(None) =
                            // parser was busy → SKIP this move (a cell-extend
                            // would shrink the word/line region); None = Cell
                            // mode (handled by the extend branch below).
                            // `anchor.0` is ABSOLUTE; the helpers convert the
                            // viewport `row` to absolute internally.
                            let replacement = match mode {
                                SelectMode::Word => {
                                    Some(self.word_drag_selection_at(anchor, row, col))
                                }
                                SelectMode::Line => {
                                    Some(self.line_drag_selection_at(anchor.0, row))
                                }
                                SelectMode::Cell => None,
                            };
                            let cell_replacement = if matches!(mode, SelectMode::Cell) {
                                self.cell_drag_selection_at(anchor, row, col)
                            } else {
                                // When: `matches!(mode, SelectMode::Cell)` is false, `replacement` owns the word/line range.
                                None
                            };
                            // selection lives on WindowState.
                            // Split-borrow `ws.selection` and `ws.panes`
                            // disjointly.
                            if let Some(ws) = self.main_mut() {
                                if let Some(sel) = ws.selection.as_mut() {
                                    match ws.select_mode {
                                        SelectMode::Cell => {
                                            if !sel.anchored {
                                                if let Some(new_sel) = cell_replacement {
                                                    *sel = new_sel;
                                                    mark_all_panes_dirty(&ws.panes);
                                                    if let Some(w) = ws.window.as_ref() {
                                                        w.request_redraw();
                                                    }
                                                }
                                            }
                                        }
                                        SelectMode::Word | SelectMode::Line => {
                                            // Replace with the recomputed union
                                            // / row-span; on Some(None) (busy
                                            // parser) skip — never shrink below
                                            // the anchor word/line.
                                            if let Some(Some(new_sel)) = replacement {
                                                *sel = new_sel;
                                                mark_all_panes_dirty(&ws.panes);
                                                if let Some(w) = ws.window.as_ref() {
                                                    w.request_redraw();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // When: mouse_down is false, update SonicTerm hover owners before terminal motion.

                    // Splitters own the pointer ahead of terminal target hints and modifier-authorized actions.
                    let splitter_hover = self.refresh_splitter_hover(lx, ly);
                    if !splitter_hover {
                        self.refresh_hovered_url();
                    } else if let Some(window_id) = self.main_window_id {
                        // When: splitter hover owns the pointer in `window_id`, clear any terminal target beneath it.
                        self.clear_target_hover(window_id);
                    }
                    let scrollbar_owned = self
                        .pane_at_cursor(lx, ly)
                        .and_then(|pane_id| {
                            let pane = self
                                .compute_active_pane_rects()
                                .into_iter()
                                .find_map(|(id, rect)| (id == pane_id).then_some(rect))?;
                            let (edge_active, visible) = self.main().map_or((false, false), |ws| {
                                ws.scrollbar_vis.get(&pane_id).map_or((false, false), |state| {
                                    (
                                        state.mouse_near_right_edge,
                                        state.alpha
                                            > crate::app::scrollbar_visibility::ALPHA_EMIT_FLOOR,
                                    )
                                })
                            });
                            let (content, gutter_width) = self.main_renderer().map_or(
                                (pane, crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX),
                                |renderer| {
                                    let content = pointer_scrollbar_content_rect(
                                        pane,
                                        [
                                            renderer.padding_left_px(),
                                            renderer.padding_right_px(),
                                            renderer.padding_top_px(),
                                            renderer.padding_bottom_px(),
                                        ],
                                        renderer.cell_size(),
                                    );
                                    (
                                        content,
                                        crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX
                                            * renderer.scale_factor(),
                                    )
                                },
                            );
                            Some(native_scrollbar_owns_pointer(
                                self.config.appearance.scrollbar,
                                content,
                                lx,
                                ly,
                                gutter_width,
                                edge_active,
                                visible,
                            ))
                        })
                        .unwrap_or(false);
                    let target_hover =
                        self.main().is_some_and(|ws| ws.hovered_url.is_some() || ws.hover_link);
                    // Only unheld motion checks READONLY here; latched gestures were routed above.
                    let read_only = self.main().is_some_and(|window| {
                        window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only)
                    });
                    let ui_consumed_motion =
                        splitter_hover || scrollbar_owned || target_hover || read_only;
                    if !ui_consumed_motion {
                        let pointer_cell = self
                            .main_renderer()
                            .and_then(|r| r.pixel_to_pane_cell(lx, ly))
                            .map(|(pane_id, row, col)| PointerCell { pane_id, row, col });
                        let pointer_profile = pointer_cell.and_then(|cell| {
                            self.main().and_then(|ws| ws.panes.get(&cell.pane_id)).map(|pane| {
                                let parser = pane.parser.lock();
                                let (tracking, sgr) = parser_mouse_profile(&parser);
                                (cell, tracking, sgr)
                            })
                        });
                        if let Some((cell, tracking, sgr)) = pointer_profile {
                            // The parser snapshot ends before the bounded PTY effect path.
                            let modifiers = self
                                .main()
                                .map(|ws| ws.modifiers)
                                .unwrap_or_else(ModifiersState::empty);
                            if let Some(route) =
                                no_button_motion_report(cell, tracking, sgr, modifiers, false)
                            {
                                if let Some((pane_id, bytes)) =
                                    pointer_route_bytes(route, PointerReportKind::NoButtonMotion)
                                {
                                    self.write_to_pane(
                                        pane_id,
                                        bytes,
                                        PtyInputSource::PointerMotion,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // route wheel events to the pane under the cursor.
                // Default 3 lines per LineDelta tick (matches stock GTK
                // / Cocoa wheel feel). PixelDelta divides by the live
                // cell height so trackpad scrolls match font size.
                let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                let (lx, ly) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
                let cell_h = self
                    .main_renderer()
                    .map(|r| r.cell_size().1)
                    .filter(|h| *h > 0.0)
                    .unwrap_or(16.0);
                let lines_per_tick: f32 = 3.0;
                let delta_lines_f: f32 = match delta {
                    // winit's y is positive when scrolling UP (away from
                    // user); we want negative delta_lines for "scroll
                    // back into history".
                    MouseScrollDelta::LineDelta(_x, y) => -y * lines_per_tick,
                    MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32) / cell_h,
                };
                // Round away from zero so a tiny trackpad nudge still
                // produces at least one line of motion.
                let delta_lines = if delta_lines_f >= 0.0 {
                    // Positive wheel motion rounds upward to preserve a fractional tick.
                    delta_lines_f.ceil() as i32
                } else {
                    // When: delta_lines_f is negative, round downward to preserve a fractional tick.
                    delta_lines_f.floor() as i32
                };
                if delta_lines != 0 {
                    // Nonzero wheel motion routes to the hovered pane.
                    if let Some(pane_id) = self.pane_at_cursor(lx, ly) {
                        // Tracking owns wheel input on either screen; snapshot modes before releasing the parser lock for PTY admission.
                        let cell = self.main_renderer().and_then(|r| r.pixel_to_cell(lx, ly));
                        let (is_alt, tracking, sgr, app_cursor) = self
                            .main()
                            .and_then(|ws| ws.panes.get(&pane_id))
                            .map(|pane| {
                                let parser = pane.parser.lock();
                                let is_alt = parser.grid().is_alt();
                                let (tracking, sgr) = parser_mouse_profile(&parser);
                                let app_cursor = parser.application_cursor_keys();
                                (is_alt, tracking, sgr, app_cursor)
                            })
                            .unwrap_or((false, MouseTracking::Off, false, false));
                        // READONLY keeps wheel input local even when tracking or an alternate screen requests bytes.
                        let route = if self.admits_new_user_input(pane_id) {
                            wheel_route(tracking, is_alt)
                        } else {
                            // When: admits_new_user_input rejects this pane, wheel cannot produce reports or cursor keys.
                            WheelRoute::LocalScrollback
                        };
                        if route == WheelRoute::MouseReport {
                            // MouseReport routes negotiated tracking to the PTY before screen-specific fallbacks.
                            // App wants mouse events: emit one wheel report per
                            // line of motion at the cell under the cursor.
                            let up = delta_lines < 0;
                            let (col1, row1) =
                                cell.map(|(r, c)| (c as u32 + 1, r as u32 + 1)).unwrap_or((1, 1));
                            let count = delta_lines.unsigned_abs() as usize;
                            let payload = wheel_report_bytes(sgr, up, col1, row1, count);
                            self.write_to_pane(pane_id, payload, PtyInputSource::Wheel);
                        } else if route == WheelRoute::CursorKeys {
                            // When: route is CursorKeys, untracked alternate-screen wheel motion becomes arrows.

                            // Build the arrow sequence: ESC O A/B in
                            // application-cursor-keys mode, else ESC [ A/B.
                            // Up when scrolling back into history
                            // (delta_lines < 0), down otherwise. Emit one
                            // copy per line of motion.
                            let up = delta_lines < 0;
                            let seq: &[u8] = match (app_cursor, up) {
                                (true, true) => b"\x1bOA",
                                (true, false) => b"\x1bOB",
                                (false, true) => b"\x1b[A",
                                (false, false) => b"\x1b[B",
                            };
                            let count = delta_lines.unsigned_abs() as usize;
                            let mut payload = Vec::with_capacity(seq.len() * count);
                            for _ in 0..count {
                                payload.extend_from_slice(seq);
                            }
                            self.write_to_pane(pane_id, payload, PtyInputSource::Wheel);
                        } else {
                            // When: route is LocalScrollback, move the untracked primary-screen viewport.
                            self.scroll_pane(pane_id, delta_lines);
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // When: MouseInput uses MouseButton::Left, route state to primary-pointer interaction.
                match state {
                    ElementState::Pressed => {
                        // When: state is ElementState::Pressed, begin primary-pointer interaction.

                        // Notify the reducer of the press transition so selection
                        // observability emits Render(Selection).
                        {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                            self.observe_intent(sonicterm_app_core::AppIntent::MouseButton {
                                window: sonicterm_types::WindowKey::new(0),
                                pressed: true,
                                button: sonicterm_app_core::MouseButton::Left,
                                mods: sonicterm_types::ModKey::empty(),
                                pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                            });
                        }
                        let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        if self.dismiss_notification_at(
                            FrontmostKind::Main,
                            cursor_pos.0 as f32,
                            cursor_pos.1 as f32,
                        ) {
                            // When: dismiss_notification_at returns true, consume the press before terminal interaction.
                            return;
                        }
                        if let Some(ws) = self.main_mut() {
                            ws.mouse_down = true;
                            ws.pointer_gesture = None;
                        }
                        // re-arm the OS-drag
                        // handoff gate so the CursorMoved threshold check
                        // can fire once for the new gesture.
                        self.os_drag_handoff_started = false;
                        let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        let (px, py) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
                        let window_width =
                            self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                        let empty_tabs2 = sonicterm_ui::tabs::TabBar::new();
                        let layout = TabBarLayout::compute_with_height(
                            self.main_tabs().unwrap_or(&empty_tabs2),
                            window_width,
                            self.main_renderer()
                                .map(|r| r.tab_bar_logical_height())
                                .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
                        )
                        .with_top_offset(
                            self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                        )
                        .with_visible(self.tab_bar_visible);
                        let tab_action = layout.hit(px, py);
                        if tab_action.is_some() {
                            // When: tab_action is Some, activate or close it before pane input.
                            match tab_action {
                                Some(sonicterm_ui::tabbar_view::TabHit::Activate(i)) => {
                                    self.activate_main_tab(i);
                                    // Record the press so a subsequent drag
                                    // below the tab bar can be promoted to a
                                    // tear-out gesture.
                                    if let Some(ws) = self.main_mut() {
                                        ws.pressed_tab = Some(i);
                                        ws.drag_session = ws.tabs.tabs().get(i).map(|tab| {
                                            crate::tab_drag::DragSession::new(
                                                win_id,
                                                tab.id,
                                                (px, py),
                                            )
                                        });
                                    }
                                }
                                Some(sonicterm_ui::tabbar_view::TabHit::Close(i)) => {
                                    self.close_tab_at(i)
                                }
                                Some(sonicterm_ui::tabbar_view::TabHit::Overflow) => {
                                    // When: `Overflow` is clicked, open the selector without starting a main-window tab drag.
                                    if let Some(window) = self.windows.get_mut(&win_id) {
                                        window.mouse_down = false;
                                        window.pressed_tab = None;
                                        window.drag_session = None;
                                    }
                                    self.open_tab_selector(win_id);
                                    return;
                                }
                                None => unreachable!("tab_action.is_some() checked above"),
                            }
                            if self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) {
                                // Empty main_tabs hides main and exits only if no child terminal survives.
                                if self.child_window_count() == 0 {
                                    self.hide_main_window();
                                    el.exit();
                                } else {
                                    // When: child_window_count is nonzero, keep the app alive after hiding main.
                                    self.hide_main_window();
                                }
                            }
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                            // Keep mouse_down=true when we recorded a tab
                            // press so cursor-move can promote it to a
                            // tear-out. Close hits consume the click fully.
                            if let Some(ws) = self.main_mut() {
                                if ws.pressed_tab.is_none() {
                                    ws.mouse_down = false;
                                }
                            }
                            return;
                        }
                        if let Some(hit) = self.splitter_hit_at(px, py) {
                            // When: splitter_hit_at returns hit, capture a resize gesture instead of selecting text.
                            if let Some(ws) = self.main_mut() {
                                ws.splitter_drag = Some(super::SplitterDragState {
                                    splitter: hit.id,
                                    axis: hit.axis,
                                    last_pos: (px, py),
                                });
                                ws.selection = None;
                            }
                            self.set_splitter_cursor(hit.axis);
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                            return;
                        }
                        // B1b borrow-split: snapshot renderer geometry up front so the
                        // pane-rect compute can run alongside `self.tab_states.get_mut()`
                        // and the hyperlink path can re-borrow `self`.
                        let renderer_geom = self.main_renderer().map(|r| {
                            let (w, h) = r.logical_size();
                            (
                                w,
                                h,
                                (r.top_inset() - r.padding_top_px()).max(0.0),
                                0.0,
                                0.0,
                                r.bottom_inset(),
                                0.0,
                            )
                        });
                        let pixel_target = {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            self.main_renderer()
                                .and_then(|r| r.pixel_to_pane_cell(cp.0 as f32, cp.1 as f32))
                        };
                        // scrollbar input has priority over
                        // selection start. Done BEFORE the pane-focus switch
                        // and selection-anchor path so a thumb-drag never
                        // doubles as a text drag. `scrollbar_hit_at` returns
                        // `Miss` for any click outside the active pane's bar,
                        // including clicks on inactive panes' bars (those
                        // need a focus-switch click first — matches the
                        // behaviour of other terminals).
                        {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                            match self.scrollbar_hit_at(lx, ly) {
                                crate::app::scrollbar_input::HitOutcome::Miss => {
                                    // When: HitOutcome::Miss leaves the press for pane selection routing.
                                }
                                crate::app::scrollbar_input::HitOutcome::StartDrag(state) => {
                                    // When: HitOutcome::StartDrag carries state, capture the scrollbar drag.
                                    if let Some(ws) = self.main_mut() {
                                        ws.scrollbar_drag = Some(state);
                                        // Suppress the residual selection-drag
                                        // path: mouse_down stays true (so
                                        // CursorMoved routes here) but no
                                        // Selection was created.
                                    }
                                    if let Some(w) = self.main_window() {
                                        w.request_redraw();
                                    }
                                    return;
                                }
                                crate::app::scrollbar_input::HitOutcome::PageUp => {
                                    // When: HitOutcome::PageUp pages the track toward older scrollback.
                                    self.scrollbar_track_page(false);
                                    return;
                                }
                                crate::app::scrollbar_input::HitOutcome::PageDown => {
                                    // When: HitOutcome::PageDown pages the track toward live output.
                                    self.scrollbar_track_page(true);
                                    return;
                                }
                            }
                        }
                        let mut pane_focus_change = None;
                        if let Some((w, h, top, pl, pr_pad, bottom, pb)) = renderer_geom {
                            // When: renderer_geom is Some, derive pane hit regions for focus and selection.
                            let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                            let pane_rects = self
                                .main_tab_states()
                                .and_then(|ts| ts.get(tab_idx))
                                .map(|st| {
                                    let outer = sonicterm_ui::pane::Rect::new(
                                        pl,
                                        top,
                                        (w - pl - pr_pad).max(0.0),
                                        (h - top - bottom - pb).max(0.0),
                                    );
                                    st.tree.layout(outer)
                                })
                                .unwrap_or_default();
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let geometry_pane =
                                pane_id_at_point(&pane_rects, cp.0 as f32, cp.1 as f32);
                            let clicked_pane = pixel_target
                                .and_then(|(pane_id, _, _)| (pane_id != 0).then_some(pane_id))
                                .or(geometry_pane);
                            if pane_rects.len() > 1 {
                                if let (Some(target), Some(window)) =
                                    (clicked_pane, self.main_mut())
                                {
                                    pane_focus_change =
                                        window.begin_pointer_pane_focus_change(target);
                                }
                            }
                            if let Some((_, row, col)) = pixel_target {
                                // When: `pixel_target` identifies a rendered cell, pair its coordinates with the same-snapshot pane before activation.
                                let opened = self.main_window_id.zip(clicked_pane).is_some_and(
                                    |(window_id, pane_id)| {
                                        self.activate_target_at(window_id, pane_id, row, col)
                                    },
                                );
                                if opened {
                                    // When: `opened` is true, consume the target click after presenting any focus change for the clicked pane.
                                    if let Some(ws) = self.main_mut() {
                                        ws.mouse_down = false;
                                        if let Some(change) = pane_focus_change.take() {
                                            ws.finish_pane_focus_change(change);
                                        }
                                    }
                                    return;
                                }
                                let pointer_cell =
                                    clicked_pane.map(|pane_id| PointerCell { pane_id, row, col });
                                if let Some(cell) = pointer_cell {
                                    // When: `pointer_cell` resolves the rendered grid, snapshot that exact pane's protocol profile.
                                    let profile = self
                                        .main()
                                        .and_then(|ws| ws.panes.get(&cell.pane_id))
                                        .map(|pane| {
                                            let parser = pane.parser.lock();
                                            parser_mouse_profile(&parser)
                                        })
                                        .unwrap_or((MouseTracking::Off, false));
                                    let terminal_press = self.main_mut().and_then(|window| {
                                        window.begin_pointer_press(cell, profile.0, profile.1)
                                    });
                                    if let Some(bytes) = terminal_press {
                                        // When: `terminal_press` contains bytes, the window latched terminal ownership before the unguarded enqueue.
                                        self.write_to_pane(
                                            cell.pane_id,
                                            bytes,
                                            PtyInputSource::PointerButton,
                                        );
                                        if let Some(change) = pane_focus_change {
                                            if let Some(window) = self.main_mut() {
                                                window.finish_pane_focus_change(change);
                                            }
                                        }
                                        return;
                                    }
                                }
                                // Multi-click selection: 1 = point, 2 = word,
                                // 3 = line. Record the click against the main
                                // window's streak state, then build the right
                                // Selection. word_at/line_at need the grid; the
                                // helpers below lock the parser only to read it
                                // and return an owned (Copy) Selection, so no
                                // grid lock is held across selection_set/redraw
                                // (CLAUDE.md §4).
                                let click_count = self
                                    .main_mut()
                                    .map(|ws| ws.register_click(row, col))
                                    .unwrap_or(1);
                                // Resolve the absolute row and content baseline
                                // under one parser lock. A selection must not be
                                // born with dirty/content state older than itself.
                                let selection_state = self.viewport_row_selection_state(row);
                                let abs_row = selection_state.map_or(row as u64, |state| state.0);
                                let sel = match click_count {
                                    2 => self.word_selection_at(abs_row, col),
                                    3 => self.line_selection_at(abs_row),
                                    _ => selection_state.map_or_else(
                                        || Selection::new(abs_row, col),
                                        |(_, pane_id, seq, is_alt, evicted)| {
                                            Selection::new(abs_row, col)
                                                .with_content_state(pane_id, seq, is_alt, evicted)
                                        },
                                    ),
                                };
                                // Record the WezTerm-style drag granularity +
                                // anchor cell so a subsequent CursorMoved (button
                                // held) extends by word / line / cell. The anchor
                                // is the press cell (ABSOLUTE row); word/line drags
                                // recompute the anchor word/line from it on each
                                // move.
                                if let Some(ws) = self.main_mut() {
                                    ws.select_mode = match click_count {
                                        2 => SelectMode::Word,
                                        3 => SelectMode::Line,
                                        _ => SelectMode::Cell,
                                    };
                                    ws.select_anchor = (abs_row, col);
                                }
                                self.selection_set(Some(sel));
                                if pane_focus_change.is_none() {
                                    if let Some(panes) = self.main_panes() {
                                        mark_all_panes_dirty(panes);
                                    }
                                }
                            }
                        }
                        if let Some(change) = pane_focus_change {
                            if let Some(window) = self.main_mut() {
                                window.finish_pane_focus_change(change);
                            }
                        } else if let Some(w) = self.main_window() {
                            // When: `pane_focus_change` is `None` and `main_window`
                            // exists, no transition owns the final redraw request.
                            w.request_redraw();
                        }
                    }
                    ElementState::Released => {
                        // When: `state` is Released, commit or cancel the press-latched pointer gesture.

                        // Notify the reducer of the release transition so selection
                        // observability emits Render(Selection).
                        {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                            self.observe_intent(sonicterm_app_core::AppIntent::MouseButton {
                                window: sonicterm_types::WindowKey::new(0),
                                pressed: false,
                                button: sonicterm_app_core::MouseButton::Left,
                                mods: sonicterm_types::ModKey::empty(),
                                pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                            });
                        }
                        let (terminal_owned, pointer_release) = self
                            .main_mut()
                            .map(|ws| {
                                let terminal_owned = matches!(
                                    ws.pointer_gesture.as_ref().map(|gesture| gesture.owner),
                                    Some(PointerGestureOwner::Terminal { .. })
                                );
                                let modifiers = ws.modifiers;
                                let release =
                                    take_pointer_release(&mut ws.pointer_gesture, modifiers);
                                (terminal_owned, release)
                            })
                            .unwrap_or((false, None));
                        // Clear before enqueue: saturation or disconnect must not
                        // leave the completed gesture latched for a later event.
                        if let Some(route) = pointer_release {
                            if let Some((pane_id, bytes)) =
                                pointer_route_bytes(route, PointerReportKind::LeftRelease)
                            {
                                self.write_to_pane(pane_id, bytes, PtyInputSource::PointerButton);
                            }
                        }
                        if terminal_owned {
                            // When: `terminal_owned` is true, release skips selection, tab-drag, and chrome cleanup.
                            if let Some(ws) = self.main_mut() {
                                ws.mouse_down = false;
                            }
                            return;
                        }
                        // end any active scrollbar drag — do this
                        // unconditionally on release so a drag that ended
                        // outside the bar still clears state.
                        if let Some(ws) = self.main_mut() {
                            ws.scrollbar_drag = None;
                            ws.splitter_drag = None;
                            ws.splitter_hover = None;
                        }
                        // Commit-on-release: read the live drag session and
                        // foreign drop target, decide what to do via the
                        // pure compute_action helper, then execute.
                        let (session, foreign, pressed) = self
                            .main_mut()
                            .map(|ws| {
                                let s = ws.drag_session.take();
                                let f = ws.drag_target.take();
                                let p = ws.pressed_tab.take();
                                ws.mouse_down = false;
                                (s, f, p)
                            })
                            .unwrap_or((None, None, None));
                        if let Some(r) = self.main_renderer_mut() {
                            r.set_drag_chip(None);
                        }
                        if let (Some(s), Some(_)) = (session, pressed) {
                            // When: both `session` and `pressed` survived, resolve the captured tab before computing release semantics.
                            let Some(idx) = self.tab_index_of_id(s.source_window, s.source_tab)
                            else {
                                // When: `s.source_tab` no longer exists in `source_window`, the release must not move another tab.
                                self.cancel_drag_session();
                                return;
                            };
                            let window_width = self
                                .main_window()
                                .map(|w| w.inner_size().width as f32)
                                .unwrap_or(0.0);
                            let empty_tabs3 = sonicterm_ui::tabs::TabBar::new();
                            let layout = TabBarLayout::compute_with_height(
                                self.main_tabs().unwrap_or(&empty_tabs3),
                                window_width,
                                self.main_renderer()
                                    .map(|r| r.tab_bar_logical_height())
                                    .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
                            )
                            .with_top_offset(
                                self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                            );
                            let action = crate::tab_drag::compute_action(&s, foreign, &layout, idx);
                            self.finish_tab_drag(s, action, |app, _, index| {
                                app.tear_out_tab(el, index);
                            });
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                        }
                        if let Some(sel_present) =
                            self.main().map(|ws| ws.selection.as_ref().map(|s| s.is_empty()))
                        {
                            // Main selection presence distinguishes no selection from an empty range.
                            if sel_present == Some(true) {
                                // An empty completed selection is cleared instead of rendered.
                                self.selection_set(None);
                                if let Some(panes) = self.main_panes() {
                                    mark_all_panes_dirty(panes);
                                }
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                            }
                        }
                        let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        if !self.refresh_splitter_hover(cp.0 as f32, cp.1 as f32) {
                            self.refresh_hovered_url();
                        }
                    }
                }
            }

            _ => {
                // When: event matches no handled WindowEvent variant, leave application state unchanged.
            }
        }
    }
}
impl App {
    fn handle_window_copy_key(&mut self, win_id: WindowId, key: &Key) {
        let Some(window) = self.windows.get_mut(&win_id) else {
            // When: win_id has disappeared, copy navigation cannot select a peer's grid.
            return;
        };
        let Some(mut state) = window.copy_mode.take() else {
            // When: copy_mode is absent, this window has no copy input owner.
            return;
        };
        let Some(active_pane) =
            window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane)
        else {
            // When: the active tab is absent, retain state until the source topology is available.
            window.copy_mode = Some(state);
            return;
        };
        let Some(pane) = window.panes.get_mut(&active_pane) else {
            // When: active_pane is absent, preserve copy ownership rather than dropping it or choosing another pane.
            window.copy_mode = Some(state);
            return;
        };
        let guard = pane.parser.lock();
        let grid = guard.grid();
        let mut should_copy = false;
        let mut should_exit = false;
        let mut copied_text = None;
        if let Some(quick_select) = state.quick_select.as_ref() {
            // When: quick_select owns key input, non-hint keys retain its table instead of navigating the grid.
            match key {
                Key::Named(NamedKey::Escape) => should_exit = true,
                Key::Character(text) => {
                    if let Some(value) =
                        text.chars().next().and_then(|ch| quick_select.text_for_hint(ch))
                    {
                        copied_text = Some(value.to_owned());
                        should_exit = true;
                    }
                }
                _ => {}
            }
        } else {
            // When: quick_select is absent, copy navigation uses the source pane's retained grid.
            match key {
                Key::Named(NamedKey::Escape) => should_exit = true,
                Key::Named(NamedKey::Enter) if !state.is_read_only() => should_copy = true,
                Key::Named(NamedKey::ArrowLeft) => state.move_left(grid),
                Key::Named(NamedKey::ArrowRight) => state.move_right(grid),
                Key::Named(NamedKey::ArrowUp) => state.move_up(grid),
                Key::Named(NamedKey::ArrowDown) => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("h") => state.move_left(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("j") => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("k") => state.move_up(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("l") => state.move_right(grid),
                Key::Character(s) if s == "v" && !state.is_read_only() => state.start_select(),
                Key::Character(s) if s == "y" && !state.is_read_only() => should_copy = true,
                Key::Character(s) if s == "w" => state.move_word_fwd(grid),
                Key::Character(s) if s == "b" => state.move_word_back(grid),
                Key::Character(s) if s == "0" => state.move_line_start(grid),
                Key::Character(s) if s == "$" => state.move_line_end(grid),
                Key::Character(s) if s == "g" => state.move_top(grid),
                Key::Character(s) if s == "G" => state.move_bottom(grid),
                _ => {
                    // When: key has no copy binding, keep its state without forwarding to the terminal.
                }
            }
            if should_copy {
                copied_text = copy_mode_selected_text(&state, grid);
                should_exit = true;
            } else {
                // When: should_copy is false, follow the source copy cursor without changing another viewport.
                pane.viewport_top_abs = GpuRenderer::copy_mode_view_top_after_move_legacy(
                    &state,
                    grid,
                    pane.viewport_top_abs,
                );
            }
        }
        drop(guard);
        if !should_exit {
            window.copy_mode = Some(state);
        }
        mark_all_panes_dirty(&window.panes);
        if let Some(text) = copied_text {
            // Parser and window borrows end before the shared clipboard boundary.
            self.set_clipboard_text(text);
        }
    }
}

impl App {
    fn main_pane_outer_rect(&self) -> Option<sonicterm_ui::pane::Rect> {
        let r = self.main_renderer()?;
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0)))
    }

    fn splitter_hit_at(&self, x: f32, y: f32) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.main_pane_outer_rect()?;
        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        self.main_tab_states()
            .and_then(|states| states.get(tab_idx))
            .and_then(|state| state.tree.hit_splitter(outer, SPLITTER_HIT_THICKNESS, x, y))
    }

    fn set_splitter_cursor(&self, axis: sonicterm_ui::pane::SplitAxis) {
        if let Some(w) = self.main_window() {
            let icon = match axis {
                sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
            };
            w.set_cursor(icon);
        }
    }

    fn refresh_splitter_hover(&mut self, x: f32, y: f32) -> bool {
        if self.main().and_then(|ws| ws.splitter_drag.as_ref()).is_some() {
            // When: splitter_drag is Some, preserve its resize cursor and consume hover routing.
            return true;
        }
        let Some(hit) = self.splitter_hit_at(x, y) else {
            // When: splitter_hit_at returns None, clear any stale splitter hover cursor.
            let was_splitter =
                self.main_mut().map(|ws| ws.splitter_hover.take().is_some()).unwrap_or(false);
            if was_splitter {
                if let Some(w) = self.main_window() {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            return false;
        };
        if let Some(ws) = self.main_mut() {
            ws.hovered_url = None;
            ws.hover_link = false;
            ws.splitter_hover = Some(hit.axis);
        }
        self.set_splitter_cursor(hit.axis);
        true
    }

    fn apply_splitter_drag(&mut self, x: f32, y: f32) -> bool {
        let Some(drag) = self.main().and_then(|ws| ws.splitter_drag.clone()) else {
            // When: splitter_drag is None, this motion is not a splitter gesture.
            return false;
        };
        let Some(outer) = self.main_pane_outer_rect() else {
            // When: main_pane_outer_rect is None, splitter geometry cannot be updated.
            return false;
        };
        let dx = x - drag.last_pos.0;
        let dy = y - drag.last_pos.1;
        if dx == 0.0 && dy == 0.0 {
            // When: dx and dy are both zero, consume the gesture without resizing.
            return true;
        }

        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        let changed = self
            .main_tab_states_mut()
            .and_then(|states| states.get_mut(tab_idx))
            .map(|state| state.tree.resize_splitter_by_delta(&drag.splitter, outer, dx, dy))
            .unwrap_or(false);

        if changed {
            if let Some(((cell_w, cell_h), inset)) = self.main_renderer().map(|r| {
                (
                    r.cell_size(),
                    [
                        r.padding_left_px(),
                        r.padding_right_px(),
                        r.padding_top_px(),
                        r.padding_bottom_px(),
                    ],
                )
            }) {
                let rects = self
                    .main_tab_states()
                    .and_then(|states| states.get(tab_idx))
                    .map(|state| state.tree.layout(outer))
                    .unwrap_or_default();
                if let Some(panes) = self.main_panes() {
                    crate::app::resize_panes_to_rects(panes, &rects, cell_w, cell_h, inset);
                }
            }
        }

        if let Some(ws) = self.main_mut() {
            if let Some(active) = ws.splitter_drag.as_mut() {
                active.last_pos = (x, y);
            }
            if changed {
                mark_all_panes_dirty(&ws.panes);
            }
        }
        self.set_splitter_cursor(drag.axis);
        if changed {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
        true
    }
}

fn copy_mode_selected_text(state: &CopyModeState, grid: &Grid) -> Option<String> {
    let (start, end) = state.selected_range()?;
    if start == end {
        // When: start equals end, the copy-mode range contains no text.
        return None;
    }
    let out = plain_text_from_grid_range(grid, (start.0, start.1 as u64), (end.0, end.1 as u64));
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
#[path = "window_event_tests.rs"]
mod window_event_tests;
