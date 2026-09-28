//! Doc-hidden test hooks for the command palette, search, notifications, IME, clipboard,
//! and selection.

use super::*;

impl App {
    /// Test-only: resolve a chord string through the App's keymap.
    /// Used by `child_window_tab_actions_dispatch.rs` to
    /// pin down that the chords the child-window handler now dispatches
    /// (cmd+1, cmd+2, cmd+Right, cmd+Left) actually resolve to their
    /// expected Action variants.
    #[doc(hidden)]
    pub fn __test_keymap_lookup(&self, keys: &str) -> Option<Action> {
        self.keymap.lookup(keys).cloned()
    }

    /// Test-only: read the window the command palette is currently
    /// attached to. `None` = main window OR closed; `Some(id)` = that
    /// child window. Used by overlay-routing regression tests.
    #[doc(hidden)]
    pub fn __test_palette_attached_window(&self) -> Option<WindowId> {
        self.palette_attached_window
    }

    /// Test-only: whether the command palette is currently open.
    #[doc(hidden)]
    pub fn __test_palette_open(&self) -> bool {
        self.command_palette.is_open()
    }

    /// Test-only: command palette query text.
    #[doc(hidden)]
    pub fn __test_palette_query(&self) -> &str {
        self.command_palette.query()
    }

    /// Test-only: command palette caret byte offset.
    #[doc(hidden)]
    pub fn __test_palette_cursor(&self) -> usize {
        self.command_palette.cursor()
    }

    /// Test-only: replace the command-palette query and refresh its selection.
    #[doc(hidden)]
    pub fn __test_set_palette_query(&mut self, query: &str) {
        self.palette_pointer_capture = None;
        self.command_palette.set_query(query);
    }

    /// Test-only: drive command-palette core editing without constructing a
    /// platform-private winit `KeyEvent`.
    #[doc(hidden)]
    pub fn __test_command_palette_text_edit(
        &mut self,
        key: &winit::keyboard::Key,
        modifiers: ModifiersState,
    ) -> bool {
        if !self.command_palette.is_open() || self.palette_ime_is_composing() {
            // When: `command_palette` is shut, or an IME preedit owns its input,
            // so a core text edit would corrupt composition or edit nothing.
            return self.command_palette.is_open();
        }
        if self.command_palette.mode()
            == sonicterm_ui::command_palette::CommandPaletteMode::TabColor
        {
            // When: `TabColor` mode owns the keystroke, so it counts as handled
            // without editing the query text behind the picker.
            return true;
        }
        let Some(edit) = text_edit::core_text_edit_for_key(key, modifiers) else {
            // When: `core_text_edit_for_key` maps this key to no edit, so the
            // query is untouched and the key is reported unhandled.
            return false;
        };
        self.command_palette.apply_text_edit(edit);
        self.request_redraw_for_overlay(self.palette_attached_window);
        true
    }

    /// Test-only: enter tab-rename mode with a known value.
    #[doc(hidden)]
    pub fn __test_start_rename_tab(&mut self, title: &str) {
        self.command_palette.start_rename_tab(title);
    }

    /// Test-only: drive command-palette key handling by logical key.
    #[doc(hidden)]
    pub fn __test_command_palette_handle_key(&mut self, key: &winit::keyboard::Key) -> bool {
        self.command_palette_handle_logical_key(key)
    }

    /// Test-only: drive command-palette IME handling.
    #[doc(hidden)]
    pub fn __test_command_palette_handle_ime(&mut self, event: &winit::event::Ime) -> bool {
        self.command_palette_handle_ime(event)
    }

    /// Test-only: describe where the main window will anchor the OS IME
    /// candidate area.
    #[doc(hidden)]
    pub fn __test_main_ime_candidate_anchor_kind(&self) -> &'static str {
        if self.command_palette.is_open() && self.palette_attached_window.is_none() {
            "palette"
        } else {
            // When: `command_palette` is shut, or `palette_attached_window` names
            // a child, so the main window's IME anchor is the terminal grid.
            "terminal"
        }
    }

    /// Test-only: read the main window notification bubble message.
    #[doc(hidden)]
    pub fn __test_main_notification_message(&self) -> Option<&str> {
        self.main()
            .and_then(|main| main.notification.as_ref())
            .map(|bubble| bubble.message.as_str())
    }

    /// Test-only: whether the main notification is ongoing.
    #[doc(hidden)]
    pub fn __test_main_notification_ongoing(&self) -> Option<bool> {
        self.main()
            .and_then(|main| main.notification.as_ref())
            .map(|bubble| bubble.expires_at.is_none())
    }

    /// Test-only: install a notification with a specific expiration.
    #[doc(hidden)]
    pub fn __test_show_notification_until(
        &mut self,
        kind: FrontmostKind,
        level: NotificationLevel,
        message: &str,
        expires_at: Option<std::time::Instant>,
    ) {
        self.show_notification_for_kind_until(kind, level, message.to_string(), expires_at);
    }

    /// Test-only: run notification expiry and return the next wake time.
    #[doc(hidden)]
    pub fn __test_expire_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.expire_notifications(now)
    }

    /// Test-only: read a child window notification bubble message.
    #[doc(hidden)]
    pub fn __test_child_notification_message(&self, id: WindowId) -> Option<&str> {
        self.windows
            .get(&id)
            .and_then(|child| child.notification.as_ref())
            .map(|bubble| bubble.message.as_str())
    }

    /// Test-only invoker for `open_search_in_child`. Mirrors the
    /// pattern used by `__test_invoke_close_active_tab_in_child` so
    /// integration tests can assert the stale-id no-op contract for
    /// overlay routing.
    #[doc(hidden)]
    pub fn __test_invoke_open_search_in_child(&mut self, id: WindowId) -> bool {
        self.open_search_in_child(id)
    }

    /// Test-only: open main search and install a known query.
    #[doc(hidden)]
    pub fn __test_set_main_search_query(&mut self, query: &str) -> bool {
        self.open_search();
        let Some(main) = self.main_mut() else {
            // When: `main_mut` resolves nothing, so no window holds the search
            // session the query was meant to seed.
            return false;
        };
        let tab_index = main.tabs.active_index();
        let Some(tab) = main.tab_states.get_mut(tab_index) else {
            // When: `tab_states` has no entry at the active index `tab_index`, so no tab
            // carries the search state to install into.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the seam refuses rather
            // than fabricating a session the user never opened.
            return false;
        };
        let Some(pane) = main.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so there is no grid
            // for `set_query` to match the term against.
            return false;
        };
        search.set_query(query, pane.parser.lock().grid());
        true
    }

    /// Test-only: install a known query in an open child search field.
    #[doc(hidden)]
    pub fn __test_set_child_search_query(&mut self, id: WindowId, query: &str) -> bool {
        if !self.open_search_in_child(id) {
            // When: `open_search_in_child` could not open search for this id, so
            // there is no session for the query to land in.
            return false;
        }
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` no longer tracks this id, so the child vanished
            // between opening search and installing the query.
            return false;
        };
        let tab_index = child.tabs.active_index();
        let Some(tab) = child.tab_states.get_mut(tab_index) else {
            // When: `tab_states` has no entry at the active index `tab_index`, so the
            // child carries no tab to install the query into.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the seam refuses rather
            // than fabricating a session.
            return false;
        };
        let Some(pane) = child.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so there is no grid
            // for `set_query` to match against.
            return false;
        };
        search.set_query(query, pane.parser.lock().grid());
        true
    }

    /// Test-only: apply a core edit to main or child search through the same
    /// shared state operation used by production routing.
    #[doc(hidden)]
    pub fn __test_search_text_edit(
        &mut self,
        id: Option<WindowId>,
        key: &winit::keyboard::Key,
        modifiers: ModifiersState,
    ) -> bool {
        let Some(edit) = text_edit::search_text_edit_for_key(key, modifiers) else {
            // When: `search_text_edit_for_key` maps this key to no edit, so the
            // search term is untouched and the key is reported unhandled.
            return false;
        };
        let target = id.or(self.main_window_id);
        let Some(target) = target else {
            // When: neither the supplied id nor `main_window_id` yields a
            // `target`, so no window owns the search this edit would change.
            return false;
        };
        let Some(window) = self.windows.get_mut(&target) else {
            // When: `windows` no longer tracks `target`, so the window closed
            // between resolving it and applying the edit.
            return false;
        };
        if window.ime.is_composing() {
            // When: `ime` is mid-composition, so the key belongs to the preedit
            // and a core edit would cut the composition in half.
            return true;
        }
        let tab_index = window.tabs.active_index();
        let Some(tab) = window.tab_states.get_mut(tab_index) else {
            // When: `tab_states` has no entry at the active index `tab_index`, so no tab
            // holds the search this edit would change.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the edit is refused
            // rather than opening a session the user did not ask for.
            return false;
        };
        let Some(pane) = window.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so re-matching the
            // term has no grid to search.
            return false;
        };
        search.apply_text_edit(edit, pane.parser.lock().grid());
        true
    }

    /// Test-only: read main or child search query and caret.
    #[doc(hidden)]
    pub fn __test_search_query_cursor(&self, id: Option<WindowId>) -> Option<(&str, usize)> {
        let target = id.or(self.main_window_id)?;
        let window = self.windows.get(&target)?;
        let search = window.tab_states.get(window.tabs.active_index())?.search.as_ref()?;
        Some((search.query.as_str(), search.cursor()))
    }

    /// Test-only: seed main IME preedit state.
    #[doc(hidden)]
    pub fn __test_set_main_ime_preedit(&mut self, text: &str) -> bool {
        let Some(main) = self.main_mut() else {
            // When: `main_mut` resolves nothing, so no window holds the IME state
            // this preedit would seed.
            return false;
        };
        main.ime.handle_preedit(text, Some((text.len(), text.len())));
        true
    }

    /// Test-only: install an in-memory clipboard buffer. This avoids depending
    /// on the OS clipboard in headless integration tests while exercising the
    /// same `set_clipboard_text` / `paste_clipboard` dispatch paths.
    #[doc(hidden)]
    pub fn __test_set_memory_clipboard(&mut self, text: &str) {
        self.test_clipboard_text = Some(text.to_string());
    }

    /// Test-only: read the in-memory clipboard buffer if installed.
    #[doc(hidden)]
    pub fn __test_memory_clipboard(&self) -> Option<String> {
        self.test_clipboard_text.clone()
    }

    /// Test-only: make clipboard writes fail before either clipboard seam changes.
    #[doc(hidden)]
    pub fn __test_set_clipboard_write_failure(&mut self, enabled: bool) {
        self.test_clipboard_write_failure = enabled;
    }

    /// Test-only: exercise file-drop path paste routing without a platform drop event.
    #[doc(hidden)]
    pub fn __test_paste_file_paths_for_kind(
        &mut self,
        kind: FrontmostKind,
        paths: Vec<std::path::PathBuf>,
    ) {
        self.paste_file_paths_for_kind(kind, paths);
    }

    /// Bind an unbound headless selection to a synthetic window's active pane,
    /// matching production mouse selection creation.
    fn bind_test_selection(
        window: &WindowState,
        selection: Option<Selection>,
    ) -> Option<Selection> {
        let mut selection = selection?;
        if selection.pane_id.is_some() {
            // When: `selection` already names a `pane_id`, so rebinding it would
            // move the caller's range onto a different pane.
            return Some(selection);
        }
        let pane_id = window.tab_states.get(window.tabs.active_index())?.active_pane;
        let pane = window.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        selection = selection.with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        Some(selection)
    }

    /// Test-only: set the synthetic main window's selection.
    #[doc(hidden)]
    pub fn __test_set_main_selection(&mut self, selection: Option<Selection>) -> bool {
        let Some(id) = self.main_window_id else {
            // When: `main_window_id` is unset, so no main window exists to carry
            // the selection.
            return false;
        };
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` no longer tracks this id, so the content state the
            // selection binds to cannot be read.
            return false;
        };
        let selection = Self::bind_test_selection(window, selection);
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window disappeared between binding and assignment, so the
            // bound selection has nowhere to be stored.
            return false;
        };
        window.selection = selection;
        true
    }

    /// Test-only: set a synthetic child window's selection.
    #[doc(hidden)]
    pub fn __test_set_child_selection(
        &mut self,
        id: WindowId,
        selection: Option<Selection>,
    ) -> bool {
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` tracks no entry for this id, so the content state
            // the selection binds to cannot be read.
            return false;
        };
        let selection = Self::bind_test_selection(window, selection);
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window disappeared between binding and assignment, so the
            // bound selection has nowhere to be stored.
            return false;
        };
        window.selection = selection;
        true
    }

    /// Test seam: the selection a window currently holds.
    ///
    /// The outer `None` means `id` is unknown; the inner `None` means the
    /// window is tracked but has no selection.
    #[doc(hidden)]
    pub fn __test_window_selection(&self, id: WindowId) -> Option<Option<Selection>> {
        self.windows.get(&id).map(|state| state.selection)
    }

    /// Test-only: seed child IME preedit state.
    #[doc(hidden)]
    pub fn __test_set_child_ime_preedit(&mut self, id: WindowId, text: &str) -> bool {
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no child holds the
            // IME state this preedit would seed.
            return false;
        };
        child.ime.handle_preedit(text, Some((text.len(), text.len())));
        true
    }

    /// Test-only: read whether a child IME composition is active.
    #[doc(hidden)]
    pub fn __test_child_ime_composing(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|child| child.ime.is_composing())
    }
}
