//! Window-local clipboard copy, paste and refusal handling.

use super::*;

impl App {
    #[cfg(target_os = "windows")]
    pub(in crate::app) fn clipboard_text_for_reassert(&mut self) -> Option<String> {
        if let Some(text) = self.test_clipboard_text.as_ref() {
            // When: test_clipboard_text contains text, expose the test clipboard snapshot.
            return Some(text.clone());
        }
        self.clipboard.as_mut().and_then(|clipboard| clipboard.get_text().ok())
    }

    pub(in crate::app) fn set_clipboard_text(&mut self, text: String) -> bool {
        #[cfg(target_os = "windows")]
        {
            self.pending_osc52_reassert = None;
        }
        if text.is_empty() {
            // When: text is empty; writing it would clear the user's clipboard,
            // so leave the previous contents intact.
            return false;
        }
        if self.test_clipboard_write_failure {
            // When: `test_clipboard_write_failure` is true, reject at the set_text
            // boundary without changing the in-memory or system clipboard.
            return false;
        }
        if self.test_clipboard_text.is_some() {
            // When: test_clipboard_text is set; the suite captures the copy here
            // so no real system clipboard is touched by a test run.
            self.test_clipboard_text = Some(text.clone());
            return true;
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            // When: no clipboard handle was created, no write occurred and callers
            // must preserve any selection that depends on confirmed delivery.
            return false;
        };
        if let Err(error) = clipboard.set_text(text.clone()) {
            tracing::warn!("clipboard set failed: {error}");
            false
        } else {
            // When: set_text succeeded; record the byte count so a silent
            // clipboard failure stays distinguishable from a real copy.
            tracing::info!("copied {} bytes", text.len());
            true
        }
    }

    /// Resolve clipboard ownership from the target's open search, even when its active pane is unavailable.
    pub(in crate::app) fn search_paste_window_for_kind(
        &self,
        kind: FrontmostKind,
    ) -> Option<WindowId> {
        let id = match kind {
            FrontmostKind::Child(id) => id,
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                self.main_window_id?
            }
        };
        self.windows
            .get(&id)
            .and_then(|window| window.tab_states.get(window.tabs.active_index()))
            .filter(|tab| tab.search.is_some())
            .map(|_| id)
    }

    /// Read clipboard text once, from the test buffer when installed, else the native clipboard.
    ///
    /// A missing handle or a failed native read returns `None`, which every
    /// paste route treats as nothing to paste rather than an error to surface.
    pub(in crate::app) fn read_clipboard_text(&mut self) -> Option<String> {
        if let Some(text) = self.test_clipboard_text.clone() {
            // When: test_clipboard_text is installed, it stands in for the native clipboard.
            return Some(text);
        }
        self.clipboard.as_mut().and_then(|clipboard| clipboard.get_text().ok())
    }

    /// Paste clipboard text into the source search field or encode it independently for each terminal destination.
    pub(in crate::app) fn paste_clipboard_for_kind(&mut self, kind: FrontmostKind) {
        let Some(text) = self.read_clipboard_text() else {
            // When: text is None; neither the test override nor the system
            // clipboard yielded anything, so there is nothing to paste.
            return;
        };
        if let Some(id) = self.search_paste_window_for_kind(kind) {
            // When: id owns an open search, a failed commit must not turn clipboard text into terminal or broadcast input.
            let _ = self.search_handle_ime_commit(id, &text);
            return;
        }
        let Some(pane_id) = self.active_pane_id_for_kind(kind) else {
            // When: active_pane_id_for_kind finds no pane for this kind; there is
            // no PTY to paste into, so the clipboard text is dropped.
            return;
        };
        let (destinations, refusals) = self.paste_payload_to_destinations(
            pane_id,
            &UserPayload::Text(text),
            super::super::PtyInputSource::Paste,
        );
        self.show_paste_refusals(kind, destinations, &refusals);
    }

    /// Paste one native path list using each admitted destination's shell syntax and paste protocol.
    pub(in crate::app) fn paste_file_paths_for_kind<I>(&mut self, kind: FrontmostKind, paths: I)
    where
        I: IntoIterator<Item = std::path::PathBuf>,
    {
        let Some(pane_id) = self.active_pane_id_for_kind(kind) else {
            // When: active_pane_id_for_kind finds no pane, consume the drop without choosing another window.
            return;
        };
        if !self.admits_new_user_input(pane_id) {
            // When: pane_id belongs to a READONLY window, consume native drops before quoting or broadcast.
            self.note_pane_input(pane_id, InputState::Blocked(1));
            return;
        }
        let paths: Vec<_> = paths.into_iter().collect();
        if paths.is_empty() {
            // When: paths is empty, the drop has no arguments or paste guards to deliver.
            return;
        }
        let (destinations, refusals) = self.paste_payload_to_destinations(
            pane_id,
            &UserPayload::Paths(paths),
            super::super::PtyInputSource::FileDrop,
        );
        self.show_paste_refusals(kind, destinations, &refusals);
    }

    /// Show one source-owned summary of encoding refusals, with no payload or path data.
    pub(super) fn show_paste_refusals(
        &mut self,
        kind: FrontmostKind,
        destinations: usize,
        refusals: &[PasteRefusal],
    ) {
        if refusals.is_empty() {
            // When: refusals is empty, a successful or consumed gesture must not replace an existing notification.
            return;
        }
        let window_id = match kind {
            FrontmostKind::Main => self.main_window_id,
            FrontmostKind::Child(id) => Some(id),
            FrontmostKind::None | FrontmostKind::Other => None,
        };
        if let Some(window_id) = window_id {
            let refused =
                InputState::PasteRefused { refused: refusals.len(), attempted: destinations };
            let now = self.dispatch_now();
            self.note_input(window_id, refused, now);
        }
        let mut groups = std::collections::BTreeMap::new();
        for refusal in refusals {
            let (name, needed) = match *refusal {
                PasteRefusal::NonUnicodePath => ("NonUnicodePath", None),
                PasteRefusal::ControlCharacter => ("ControlCharacter", None),
                PasteRefusal::CmdUnsafeCharacter => ("CmdUnsafeCharacter", None),
                PasteRefusal::TooLarge { needed } => ("TooLarge", Some(needed)),
            };
            *groups.entry((name, needed)).or_insert(0usize) += 1;
        }
        let details = groups.into_iter().map(|((name, needed), count)| {
            let size = needed.map(|needed| format!(", needed: {needed} bytes, maximum: {MAX_PTY_INPUT_MESSAGE_BYTES} bytes"))
                .unwrap_or_default();
            format!("{name} (destinations: {count}{size})")
        }).collect::<Vec<_>>().join("; ");
        self.show_notification_for_kind(
            kind,
            sonicterm_ui::overlays::NotificationLevel::Warning,
            format!(
                "Paste refused for {} of {destinations} destinations: {details}",
                refusals.len()
            ),
        );
    }
}
