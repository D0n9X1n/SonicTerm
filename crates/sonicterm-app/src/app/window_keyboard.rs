//! Source-window keyboard, IME, focus, modifier and copy-mode key routing
//! shared by the main and child windows.

use sonicterm_cfg::keymap::Action;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::selection::plain_text_from_grid_range;
use winit::{
    event::{ElementState, Ime, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
    window::WindowId,
};

use super::key_encoding::{key_event_to_string, key_event_to_strings};
use super::window_event::{
    is_quit_chord, pointer_route_bytes, take_focus_loss_pointer_release, terminal_repeat_targets,
    PointerReportKind,
};
use super::{mark_all_panes_dirty, App, PtyInputSource};

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
                use super::field_input::FieldCommand;
                let command = super::field_input::field_command_for_event(event, modifiers);
                if let Some(command @ (FieldCommand::SelectAll | FieldCommand::Extend(_))) = command
                {
                    // When: command only selects, apply it without a rescan or keymap dispatch.
                    self.search_apply_selection_command(win_id, command);
                    return;
                }
                let text_edit = command.is_some()
                    || super::text_edit::printable_event_text(event, modifiers).is_some();
                if !text_edit {
                    // When: text_edit is absent, search permits a non-edit binding without typing its key.
                    use super::field_input::FieldBinding;
                    // Clipboard may match any alias; other actions keep the first-alias rule, and an
                    // earlier non-clipboard match keeps precedence over a later clipboard alias.
                    let binding = match super::field_input::first_field_binding(
                        &self.keymap,
                        event,
                        modifiers,
                    ) {
                        FieldBinding::Clipboard { chord, action } => Some((chord, action)),
                        FieldBinding::Other { chord, action, primary: true }
                            if !matches!(action, Action::OpenSearch) =>
                        {
                            Some((chord, action))
                        }
                        FieldBinding::Other { .. } | FieldBinding::Unbound => None,
                    };
                    if let Some((chord, action)) = binding {
                        // When: binding resolves chord to a non-search action, search dispatches it instead of typing its key.
                        if super::keymap_dispatch::terminal_input_passthrough_binding(
                            &chord, &action,
                        ) {
                            // When: terminal_input_passthrough_binding accepts chord, search consumes Alt+V without paste or text.
                            return;
                        }
                        // The action dispatches on the source window without terminal fallback.
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
                dirty |= pane.release_viewport();
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
            window.request_window_redraw();
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
                let enabled = window.panes.get(&active_pane).is_some_and(|pane| {
                    crate::app::frame_counters::lock_parser(&pane.parser).focus_reporting_enabled()
                });
                if enabled {
                    let bytes: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
                    focus_report = Some((active_pane, bytes.to_vec()));
                }
            }
            // IME stays enabled; focus-in resets only its caret throttle to avoid native context churn.
            window.request_window_redraw();
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
        window.request_window_redraw();
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
        let guard = crate::app::frame_counters::lock_parser(&pane.parser);
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
                        text.chars().next().and_then(|hint| quick_select.text_for_hint(hint))
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
                Key::Character(text) if text.eq_ignore_ascii_case("h") => state.move_left(grid),
                Key::Character(text) if text.eq_ignore_ascii_case("j") => state.move_down(grid),
                Key::Character(text) if text.eq_ignore_ascii_case("k") => state.move_up(grid),
                Key::Character(text) if text.eq_ignore_ascii_case("l") => state.move_right(grid),
                Key::Character(text) if text == "v" && !state.is_read_only() => {
                    state.start_select()
                }
                Key::Character(text) if text == "y" && !state.is_read_only() => should_copy = true,
                Key::Character(text) if text == "w" => state.move_word_fwd(grid),
                Key::Character(text) if text == "b" => state.move_word_back(grid),
                Key::Character(text) if text == "0" => state.move_line_start(grid),
                Key::Character(text) if text == "$" => state.move_line_end(grid),
                Key::Character(text) if text == "g" => state.move_top(grid),
                Key::Character(text) if text == "G" => state.move_bottom(grid),
                _ => {
                    // When: key has no copy binding, keep its state without forwarding to the terminal.
                }
            }
            if should_copy {
                copied_text = copy_mode_selected_text(&state, grid);
                should_exit = true;
            } else {
                // When: should_copy is false, follow the source copy cursor without changing another viewport.
                let current = pane.resolved_viewport(grid);
                let next = GpuRenderer::copy_mode_view_top_after_move_legacy(&state, grid, current);
                if next != current {
                    // Only a real move repins, so a suspended primary pin survives copy mode.
                    let at = super::viewport_anchor::ViewportBaseline::of(grid);
                    pane.viewport_anchor.set(&mut pane.viewport_top_abs, next, at);
                }
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
#[path = "window_keyboard_tests.rs"]
mod window_keyboard_tests;
