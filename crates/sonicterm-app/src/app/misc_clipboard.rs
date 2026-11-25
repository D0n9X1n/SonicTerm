//! Window-local clipboard copy, paste and refusal handling.

use super::*;
use crate::app::clipboard_worker::{TargetOutcome, WriteResult};
use crate::app::osc52_stream::ClipTargets;

/// Whether paste trusts the native clipboard's copied-file list: Windows reads `CF_HDROP` and
/// macOS reads file URLs. The Linux backend decodes `text/uri-list` lossily, dropping entries it
/// cannot decode and misreading a `localhost` authority, so Linux keeps pasting clipboard text.
pub(in crate::app) const NATIVE_CLIPBOARD_FILE_LIST: bool =
    cfg!(any(target_os = "windows", target_os = "macos"));

impl App {
    /// Copy the user's `text` with no follow-up; true when it was written or queued in order.
    pub(in crate::app) fn set_clipboard_text(&mut self, text: String) -> bool {
        self.copy_text_then(text, |_, _| {})
    }

    /// Write `text` to `targets` on the event-loop thread; an empty text clears them. A test
    /// buffer keeps the write off the desktop and never touches PRIMARY.
    pub(in crate::app) fn write_clipboard_inline(
        &mut self,
        text: &str,
        targets: ClipTargets,
    ) -> bool {
        self.write_clipboard_inline_targets(text, targets).written()
    }

    /// [`Self::write_clipboard_inline`] with each selection's outcome: refused when nothing could
    /// be written, unknown when a native call failed after it may have stored the text.
    pub(in crate::app) fn write_clipboard_inline_targets(
        &mut self,
        text: &str,
        targets: ClipTargets,
    ) -> WriteResult {
        #[cfg(any(test, feature = "test-support"))]
        if self.testing.test_clipboard_write_failure {
            // When: `test_clipboard_write_failure` is true, reject at the set_text
            // boundary without changing the in-memory or system clipboard.
            return WriteResult::each(targets, TargetOutcome::Refused);
        }
        if self.test_clipboard_installed() {
            // When: test_clipboard_installed, keep the copy off the system clipboard; like a
            // native copy, the text replaces any copied file list.
            if targets.clipboard {
                self.testing.test_clipboard_text = Some(text.to_owned());
                self.testing.test_clipboard_files = None;
            }
            return WriteResult::each(targets, TargetOutcome::Taken);
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            // When: no clipboard handle was created, no write occurred and callers
            // must preserve any selection that depends on confirmed delivery.
            return WriteResult::each(targets, TargetOutcome::Refused);
        };
        crate::app::clipboard_worker::write_native_targets(clipboard, text, targets)
    }

    /// Whether a test buffer stands in for the native clipboard.
    pub(in crate::app) fn test_clipboard_installed(&self) -> bool {
        self.testing.test_clipboard_text.is_some() || self.testing.test_clipboard_files.is_some()
    }

    /// Resolve clipboard ownership from the target's open search, even when its active pane is unavailable.
    pub(in crate::app) fn search_paste_window_for_kind(
        &self,
        kind: FrontmostKind,
    ) -> Option<WindowId> {
        let id = match kind {
            FrontmostKind::Child(id) => id,
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                self.registry.main_window_id?
            }
        };
        self.registry
            .windows
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
        if let Some(text) = self.testing.test_clipboard_text.clone() {
            // When: test_clipboard_text is installed, it stands in for the native clipboard.
            return Some(text);
        }
        if self.testing.test_clipboard_files.is_some() {
            // When: test_clipboard_files is the only override, the test clipboard holds no text and the desktop one is never read.
            return None;
        }
        self.clipboard.as_mut().and_then(|clipboard| clipboard.get_text().ok())
    }

    /// Read the paths of files copied to the clipboard (Explorer on Windows, Finder on macOS).
    ///
    /// Returns an empty list when the clipboard holds no files, the native read fails, or the
    /// platform's file list is not trusted, so callers fall back to text.
    pub(in crate::app) fn read_clipboard_files(&mut self) -> Vec<std::path::PathBuf> {
        if self.testing.test_clipboard_text.is_some() || self.testing.test_clipboard_files.is_some()
        {
            // When: test_clipboard_text or test_clipboard_files is installed, the test clipboard holds only test_clipboard_files.
            return self.testing.test_clipboard_files.clone().unwrap_or_default();
        }
        if !NATIVE_CLIPBOARD_FILE_LIST {
            // When: NATIVE_CLIPBOARD_FILE_LIST is false, the backend's file list can drop or misread entries, so keep text paste.
            return Vec::new();
        }
        self.clipboard
            .as_mut()
            .and_then(|clipboard| clipboard.get().file_list().ok())
            .unwrap_or_default()
    }

    /// Paste clipboard text into the source search field, or send each terminal destination its
    /// paste: a DECSET 5522 paste event to the panes that take one, copied files or text to the rest.
    pub(in crate::app) fn paste_clipboard_for_kind(&mut self, kind: FrontmostKind) {
        if let Some(id) = self.search_paste_window_for_kind(kind) {
            // When: id owns an open search, its query takes text only; a failed commit must not reach terminal or broadcast input.
            if let Some(text) = self.read_clipboard_text() {
                // When: read_clipboard_text yields text, it becomes the search query; copied files are not offered to search.
                let _ = self.search_handle_ime_commit(id, &text);
            }
            return;
        }
        // Recipients are chosen before any clipboard read, so a pane that asked for paste events
        // gets its event even when the offer holds no text, such as an image alone.
        let split = self
            .active_pane_id_for_kind(kind)
            .filter(|pane| self.admits_new_user_input(*pane))
            .map(|source_pane| {
                let (destinations, blocked) = self.paste_destinations(source_pane);
                let (event_panes, text_panes): (Vec<u64>, Vec<u64>) =
                    destinations.into_iter().partition(|pane| self.takes_paste_event(*pane));
                (source_pane, blocked, event_panes, text_panes)
            });
        let Some((source_pane, blocked, event_panes, text_panes)) =
            split.filter(|(_, _, event_panes, _)| !event_panes.is_empty())
        else {
            // When: split is None or its event_panes is empty, the file and text paste runs unchanged.
            self.paste_clipboard_contents_for_kind(kind);
            return;
        };
        self.note_paste_gesture(source_pane, blocked);
        self.send_paste_events(&event_panes);
        if text_panes.is_empty() {
            // When: text_panes is empty, every pane took an event and the clipboard is never read.
            return;
        }
        // Copied files reach text recipients as quoted paths, as a paste alone would send them.
        let files = self.read_clipboard_files();
        let payload = if files.is_empty() {
            self.read_clipboard_text().map(UserPayload::Text)
        } else {
            // When: files is non-empty, the copied files win over the bare name a file copy offers as text.
            Some(UserPayload::Paths(files))
        };
        let Some(payload) = payload else {
            // When: payload is None, the clipboard has no text, so the text recipients get nothing.
            return;
        };
        let paste = super::super::PtyInputSource::Paste;
        let (destinations, refusals) = self.encode_paste_to(&text_panes, &payload, paste);
        self.show_paste_refusals(kind, destinations, &refusals);
    }

    /// Whether `pane_id` gets a paste event instead of its paste: its program set DECSET 5522, read
    /// without the parser lock, and `osc52_read` is not `deny`, under which the program could not
    /// read what the event announces, so the paste stays text.
    fn takes_paste_event(&self, pane_id: u64) -> bool {
        use sonicterm_cfg::config::Osc52ReadPolicy;
        let readable = self.clip_state.staging.read_policy() != Osc52ReadPolicy::Deny;
        readable && self.pane_by_id(pane_id).is_some_and(|pane| pane.paste_events_mode() & 1 == 1)
    }

    /// Send each of `panes` its own paste event through its PTY's sensitive-reply slot, with its
    /// own one-time password, and bind a grant to each event its writer admitted: the recipient's
    /// identity and the offer version read around the listing. A version the platform cannot give,
    /// or one that moved during the listing, issues no grant, so the follow-up read asks.
    fn send_paste_events(&mut self, panes: &[u64]) {
        use crate::app::osc52_read_grants::{one_time_password, PasteGrant};
        use crate::app::osc52_read_source::ReadLocation;
        // shortcut: a paste reads the standard clipboard only, so its event names that location.
        let location = sonicterm_vt::vt::KittyClipboardLocation::Standard;
        let source = Arc::clone(&self.clip_state.reads.source);
        // shortcut: the offer is listed and versioned on the event loop, as a paste's text read is;
        // on X11 each call can wait the 2 s selection wait for an owner that does not answer.
        let before = source.offer_version(ReadLocation::Clipboard);
        let listing = source.list_types(ReadLocation::Clipboard);
        let after = before.and_then(|_| source.offer_version(ReadLocation::Clipboard));
        let version = before.filter(|version| after == Some(*version));
        for &pane_id in panes {
            // A new paste ends the pane's earlier grant, whatever becomes of this event.
            self.clip_state.reads.paste_grants.revoke(pane_id);
            let password = one_time_password();
            let event = crate::app::kitty_clipboard_read::paste_event_reply(
                location,
                password.as_deref(),
                &listing,
            );
            let binding = self.paste_binding(pane_id);
            let bytes = event.len();
            let reply = sonicterm_io::pty::SensitiveReply::new(event, Box::new(()));
            // shortcut: a refused event is not retried; the user pastes again.
            if let Err(error) = self.send_read_reply(pane_id, reply) {
                // When: send_read_reply refuses the event (writer busy or stopped), no grant is made.
                tracing::debug!(pane_id, bytes, ?error, "paste event not admitted");
                continue;
            }
            let grant = match (password, binding, version) {
                (Some(password), Some(binding), Some(version)) => {
                    Some(PasteGrant::new(binding, location, version, password, Instant::now()))
                }
                _ => None,
            };
            tracing::debug!(pane_id, bytes, granted = grant.is_some(), "paste event sent");
            if let Some(grant) = grant {
                // The grant exists only once its event was admitted to the pane's writer.
                self.clip_state.reads.paste_grants.issue(grant);
            }
        }
    }

    /// Paste the clipboard's copied files, else its text, into each terminal destination.
    fn paste_clipboard_contents_for_kind(&mut self, kind: FrontmostKind) {
        let files = self.read_clipboard_files();
        if !files.is_empty() {
            // When: files is non-empty, paste the quoted paths like a drop; a file copy's text form is only a bare name.
            self.paste_paths_with_source(kind, files, super::super::PtyInputSource::Paste);
            return;
        }
        let Some(text) = self.read_clipboard_text() else {
            // When: text is None; neither the test override nor the system
            // clipboard yielded anything, so there is nothing to paste.
            return;
        };
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

    /// Paste one dropped native path list using each admitted destination's shell syntax and paste protocol.
    pub(in crate::app) fn paste_file_paths_for_kind<I>(&mut self, kind: FrontmostKind, paths: I)
    where
        I: IntoIterator<Item = std::path::PathBuf>,
    {
        self.paste_paths_with_source(kind, paths, super::super::PtyInputSource::FileDrop);
    }

    /// Quote `paths` for each destination and attribute the input to `source` (a drop or a clipboard paste).
    fn paste_paths_with_source<I>(
        &mut self,
        kind: FrontmostKind,
        paths: I,
        source: super::super::PtyInputSource,
    ) where
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
        let (destinations, refusals) =
            self.paste_payload_to_destinations(pane_id, &UserPayload::Paths(paths), source);
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
            FrontmostKind::Main => self.registry.main_window_id,
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

#[cfg(test)]
#[path = "misc_clipboard_tests.rs"]
mod misc_clipboard_tests;

#[cfg(test)]
#[path = "misc_clipboard_paste_event_tests.rs"]
mod misc_clipboard_paste_event_tests;

#[cfg(all(test, target_os = "windows"))]
#[path = "misc_clipboard_native_windows_tests.rs"]
mod misc_clipboard_native_windows_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "misc_clipboard_native_macos_tests.rs"]
mod misc_clipboard_native_macos_tests;
