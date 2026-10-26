//! Source-window IME routing and commit handling.

use super::*;

impl App {
    /// Where an IME commit in `win_id` goes now, or `None` when the window is gone. The handler and
    /// the status line's `ime` segment both read this, so they cannot name different owners.
    pub(in crate::app) fn ime_route(&self, win_id: WindowId) -> Option<ImeRoute> {
        let window = self.windows.get(&win_id)?;
        Some(ime_route_for(window, self.command_palette_owns_input(win_id)))
    }

    /// Route composition through the source window's palette, search, READONLY, or terminal owner.
    pub(in crate::app) fn handle_window_ime(&mut self, win_id: WindowId, ime_event: Ime) {
        if self.status_focus_ime(win_id, &ime_event) {
            // When: `status_focus_ime` consumed it, the strip owns focus and no composition reaches a pane.
            return;
        }
        let Some(route) = self.ime_route(win_id) else {
            // When: ime_route is None, win_id no longer resolves; a late event must not reach another window.
            return;
        };
        if route == ImeRoute::Palette
            && self.command_palette_handle_ime_in_window(win_id, &ime_event)
        {
            // When: route is Palette and the palette consumed ime_event, no other input owner may receive it.
            return;
        }
        let Some(window) = self.windows.get_mut(&win_id) else {
            // When: win_id no longer resolves, a late IME event must not fall back to another window.
            return;
        };
        // A palette that declined the event leaves the remaining owners in their usual order.
        let route = ime_route_for(window, false);
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
        let active_pane =
            window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane);
        window.request_window_redraw();
        if committed.is_empty() {
            // When: committed is empty, composition changes require redraw but no search or PTY input.
            return;
        }
        match route {
            // An open search owns the commit even when its pane is temporarily missing.
            ImeRoute::Search => {
                self.search_handle_ime_commit(win_id, &committed);
            }
            ImeRoute::CopyMode => {
                // Copy mode owns the keyboard, so the commit is dropped; the status line says so.
                window.ime_discarded_at = Some(std::time::Instant::now());
            }
            ImeRoute::Pane | ImeRoute::Palette => {
                if let Some(active_pane) = active_pane {
                    let bytes = committed.into_bytes();
                    self.write_to_pane(active_pane, bytes.clone(), PtyInputSource::Ime);
                    self.broadcast_from(active_pane, bytes, PtyInputSource::Ime);
                }
            }
        }
    }
}

/// The route for `window`; `palette_owns_input` says the command palette owns this window's input.
pub(in crate::app) fn ime_route_for(
    window: &super::super::window_state::WindowState,
    palette_owns_input: bool,
) -> ImeRoute {
    let search_open =
        window.tab_states.get(window.tabs.active_index()).is_some_and(|tab| tab.search.is_some());
    if palette_owns_input {
        ImeRoute::Palette
    } else if search_open {
        // When: search_open, an open search owns the commit before copy mode or the pane.
        ImeRoute::Search
    } else if window.copy_mode.is_some() {
        // When: copy_mode is active, it owns the keyboard and the commit is dropped.
        ImeRoute::CopyMode
    } else {
        // When: palette_owns_input, search_open and copy_mode are all off, the pane receives it.
        ImeRoute::Pane
    }
}

#[cfg(test)]
#[path = "window_keyboard_ime_tests.rs"]
mod window_keyboard_ime_tests;
