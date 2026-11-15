//! The event-loop side of the tools that surface an agent's work to the user: `focus_pane`,
//! `set_title` and `notify`. Each is a control tool, checked by `authorize_control` when it runs;
//! `focus_pane` and `notify` also share one message slot per agent session.
//!
//! Text an agent shows the user is cleaned on the connection's worker by [`display_text`] before
//! it is queued, so the loop only ever receives bounded text without control or format characters.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sonicterm_cfg::config::McpControl;
use winit::window::UserAttentionType;

use super::agents::{Access, AgentSessionId, LiveSessions, MessageAdmission};
use super::control_loop::Refusal;
use super::{LoopReply, McpShared, MAX_PENDING_NOTICES};
use crate::app::App;

/// The most characters a tab title set by `set_title` keeps.
pub(crate) const TITLE_MAX_CHARS: usize = 64;
/// The most characters of an agent's message `notify` shows.
pub(crate) const NOTIFY_MAX_CHARS: usize = 200;

/// `text` without control characters (Unicode Cc) or the format characters (Cf) that reorder or
/// hide text, cut to at most `max_chars` characters, so an agent cannot fake or hide what it shows.
pub(crate) fn display_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control() && !is_format_character(*character))
        .take(max_chars)
        .collect()
}

/// Whether `character` is a Unicode format character (Cf): bidi controls, zero-width marks, the
/// byte-order mark, the soft hyphen, invisible operators, interlinear annotation and tag marks.
fn is_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// One agent message's place in the notice queue; dropping it, with its request, frees the place.
pub(crate) struct NoticeSlot {
    /// The server's pending-notice counter this slot holds one of.
    pending: Arc<AtomicU64>,
}

// Lifecycle: NoticeSlot drop frees its place in pending_notices however its request ends: shown,
// refused, revoked, or dropped with the inbox when the server stops.
impl Drop for NoticeSlot {
    // Ordering: pending AcqRel pairs with the admitting increment in `admit_notice`.
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

impl McpShared {
    /// Take a place in the notice queue for an agent message; `None` when [`MAX_PENDING_NOTICES`]
    /// notices already wait, so messages cannot grow the inbox.
    // Ordering: pending_notices AcqRel makes the increment the single admission point, as `notify`'s.
    pub(crate) fn admit_notice(&self) -> Option<NoticeSlot> {
        if self.pending_notices.fetch_add(1, Ordering::AcqRel) >= MAX_PENDING_NOTICES {
            // When: `MAX_PENDING_NOTICES` already wait, the message is refused and nothing queued.
            self.pending_notices.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(NoticeSlot { pending: Arc::clone(&self.pending_notices) })
    }
}

/// How `focus_pane` brings a window forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RaiseRequest {
    /// SonicTerm is the active application: the window is asked to take focus.
    Focus,
    /// Another application is active: the OS is only asked for the user's attention, so keys
    /// meant for that application never reach an agent's pane.
    Attention,
}

#[cfg(test)]
thread_local! {
    /// The raise requests made on this thread, so a headless test sees which one was chosen.
    pub(crate) static RAISE_REQUESTS: std::cell::RefCell<Vec<RaiseRequest>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The prefix every agent message is shown under: SonicTerm's own words and the pane, never the
/// client's self-reported name, so an agent cannot pose as SonicTerm.
fn agent_message(pane_id: u64, text: &str) -> String {
    format!("Agent message (pane {pane_id}): {text}")
}

/// Spend `session`'s message slot now, or the refusal to answer with.
fn admit_agent_message(sessions: &LiveSessions, session: AgentSessionId) -> Result<(), LoopReply> {
    match sessions.admit_message(session, Instant::now()) {
        MessageAdmission::Admitted => Ok(()),
        MessageAdmission::TooSoon => Err(Refusal::RateLimited.reply()),
        MessageAdmission::Closed => {
            Err(LoopReply::Refused("revoked", "the session closed before the request ran".into()))
        }
    }
}

impl App {
    /// `focus_pane` on the loop: select the pane's tab and the pane within it, then raise its
    /// window while SonicTerm is active, or else request the user's attention.
    pub(crate) fn mcp_focus_pane(
        &mut self,
        sessions: &LiveSessions,
        access: Access,
        pane_id: u64,
    ) -> LoopReply {
        if let Err(refusal) = self.authorize_control(access, pane_id, false) {
            // When: `authorize_control` refused the pane, nothing is selected.
            return refusal.reply();
        }
        let Some(location) = self.locate_pane(pane_id) else {
            // When: `locate_pane` finds no tab for the pane, nothing is selected.
            return Refusal::PaneGone.reply();
        };
        let zoomed_elsewhere = self
            .registry
            .windows
            .get(&location.window_id)
            .and_then(|window| window.tab_states.get(location.tab_index))
            .and_then(|tab| tab.tree.zoomed_pane_id())
            .is_some_and(|zoomed| zoomed != pane_id);
        if zoomed_elsewhere {
            // When: the tab is `zoomed_elsewhere`, selecting the pane would undo the user's zoom.
            return Refusal::Zoomed.reply();
        }
        if let Err(reply) = admit_agent_message(sessions, access.session) {
            // When: the session's message slot is spent or the session is gone, nothing moves.
            return reply;
        }
        if location.is_main {
            self.activate_main_tab(location.tab_index);
        } else {
            // When: `location.is_main` is false, the pane's child window selects the tab.
            self.activate_tab_in_child(location.window_id, location.tab_index);
        }
        if let Some(window) = self.registry.windows.get_mut(&location.window_id) {
            // An already active pane begins no change, so its focus feedback does not restart.
            if let Some(change) = window.begin_pane_focus_change(pane_id) {
                window.finish_pane_focus_change(change);
            }
        }
        let raised = self.surface_window(location.window_id);
        tracing::info!(target: "mcp", pane = pane_id, raised, "agent focused pane");
        LoopReply::Focused { raised }
    }

    /// Bring `window_id` forward: focus it while a SonicTerm window is frontmost, else only ask
    /// the OS for attention. `true` when the window was asked to take focus; best effort, since
    /// Wayland and minimized windows may refuse.
    fn surface_window(&self, window_id: crate::app::WindowId) -> bool {
        let app_active = self
            .registry
            .frontmost_window
            .is_some_and(|frontmost| self.registry.windows.contains_key(&frontmost));
        let request = if app_active { RaiseRequest::Focus } else { RaiseRequest::Attention };
        #[cfg(test)]
        RAISE_REQUESTS.with(|requests| requests.borrow_mut().push(request));
        let Some(native) =
            self.registry.windows.get(&window_id).and_then(|window| window.window.as_ref())
        else {
            // When: the window has no `native` window (headless), there is nothing to raise.
            return false;
        };
        match request {
            RaiseRequest::Focus => {
                native.focus_window();
                true
            }
            RaiseRequest::Attention => {
                native.request_user_attention(Some(UserAttentionType::Informational));
                false
            }
        }
    }

    /// `set_title` on the loop: set or clear the custom title of the pane's tab. Under
    /// `control = "agent"` every pane in the tab must be one the session may control.
    pub(crate) fn mcp_set_title(&mut self, access: Access, pane_id: u64, title: &str) -> LoopReply {
        if let Err(refusal) = self.authorize_control(access, pane_id, false) {
            // When: `authorize_control` refused the pane, no title changes.
            return refusal.reply();
        }
        let Some(location) = self.locate_pane(pane_id) else {
            // When: `locate_pane` finds no tab for the pane, no title changes.
            return Refusal::PaneGone.reply();
        };
        let Some(leaves) = self
            .registry
            .windows
            .get(&location.window_id)
            .and_then(|window| window.tab_states.get(location.tab_index))
            .map(|tab| tab.tree.leaves())
        else {
            // When: the pane's tab is gone, no title changes.
            return Refusal::PaneGone.reply();
        };
        if access.control == McpControl::Agent {
            // When: control is `Agent`, a tab shared with a pane out of scope keeps its title.
            for leaf in leaves {
                if let Err(refusal) = self.authorize_control(access, leaf, false) {
                    // When: `authorize_control` refuses any `leaf`, the tab keeps its title.
                    return refusal.reply();
                }
            }
        }
        let Some(window) = self.registry.windows.get_mut(&location.window_id) else {
            // When: the pane's window is gone, no title changes.
            return Refusal::PaneGone.reply();
        };
        let Some(tab_id) = window.tabs.tabs().get(location.tab_index).map(|tab| tab.id) else {
            // When: the tab bar holds no tab at the pane's index, no title changes.
            return Refusal::PaneGone.reply();
        };
        window.tabs.set_custom_title(tab_id, title);
        window.request_window_redraw();
        tracing::info!(target: "mcp", pane = pane_id, "agent set tab title");
        // A blank title clears the custom title, as the rename command does.
        LoopReply::Titled((!title.trim().is_empty()).then(|| title.to_owned()))
    }

    /// `notify` on the loop: show `text` about `pane_id` under SonicTerm's own prefix.
    pub(crate) fn mcp_notify(
        &mut self,
        sessions: &LiveSessions,
        access: Access,
        pane_id: u64,
        text: &str,
    ) -> LoopReply {
        if let Err(refusal) = self.authorize_control(access, pane_id, false) {
            // When: `authorize_control` refused the pane, nothing is shown.
            return refusal.reply();
        }
        if let Err(reply) = admit_agent_message(sessions, access.session) {
            // When: the session's message slot is spent or the session is gone, nothing is shown.
            return reply;
        }
        self.show_notification_for_kind(
            self.frontmost_kind(),
            sonicterm_ui::overlays::NotificationLevel::Info,
            agent_message(pane_id, text),
        );
        tracing::info!(target: "mcp", pane = pane_id, "agent message shown");
        LoopReply::Notified
    }
}

#[cfg(test)]
#[path = "control_loop_ui_tests.rs"]
mod control_loop_ui_tests;
