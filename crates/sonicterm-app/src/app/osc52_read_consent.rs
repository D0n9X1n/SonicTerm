//! The `ask` consent page for OSC 52 reads: a waiting ticket is shown on the palette's confirm page
//! in its source window, and only Apply on that page's own session approves it.
//!
//! The page opens only while the source window is the OS-frontmost terminal window and the palette
//! is closed, so it never raises a window, moves focus, or reuses or replaces an open palette; until
//! then the ticket keeps waiting, up to its expiry. The title names the tab and pane by position,
//! never by a title a program set, and Deny is highlighted first. Apply consumes the session, and
//! the approved ticket then takes the ordinary revalidated read path. Deny, Escape, an outside
//! click or expiry answer empty, as a rejected prompt is; a lost page or an identity or policy
//! change ends the ticket silently.

use std::time::Instant;

use sonicterm_ui::command_palette::CommandPaletteMode;
use winit::window::WindowId;

use super::osc52_read::TICKET_LIFETIME;
use super::osc52_read_ticket::{CancelReason, ReadTicket};

/// A waiting ticket shown on the palette's confirm page.
pub(in crate::app) struct PresentedRead {
    ticket: ReadTicket,
    /// The palette session that shows the page; Apply from any other session approves nothing.
    session: u64,
    /// The window the page opened in, which held the source pane then.
    window: WindowId,
}

impl PresentedRead {
    /// The ticket the page asks about.
    pub(in crate::app) fn ticket(&self) -> &ReadTicket {
        &self.ticket
    }
}

impl super::App {
    /// The window that hosts the palette: its attached child, or else the main window.
    fn palette_host(&self) -> Option<WindowId> {
        self.interaction.palette_attached_window.or(self.registry.main_window_id)
    }

    /// Whether the palette still shows `presented`'s page: open on its session, in its window.
    fn read_page_shown(&self, presented: &PresentedRead) -> bool {
        let palette = &self.interaction.command_palette;
        palette.is_open()
            && palette.mode() == CommandPaletteMode::Confirm
            && palette.session() == presented.session
            && self.palette_host() == Some(presented.window)
    }

    /// Close the palette when it is still on `presented`'s session, wherever it is hosted.
    fn close_read_page(&mut self, presented: &PresentedRead) {
        if !self.interaction.command_palette.is_open()
            || self.interaction.command_palette.session() != presented.session
        {
            // When: the palette is closed or shows another session, that page is not ours to close.
            return;
        }
        let host = self.interaction.palette_attached_window;
        self.interaction.palette_pointer_capture = None;
        self.interaction.command_palette.close();
        self.interaction.palette_attached_window = None;
        self.request_redraw_for_overlay(host);
    }

    /// End the page whose ticket no longer matches, whose page was lost, or which expired: an
    /// expired one closes and answers empty, any other ends silently.
    pub(in crate::app) fn sweep_read_page(&mut self, now: Instant) {
        let Some(presented) = self.clip_state.reads.presented.take() else {
            // When: no page is presented, there is nothing to sweep.
            return;
        };
        if let Err(reason) = self.revalidate_read(presented.ticket.binding(), now, false) {
            // When: revalidate_read refuses, the page closes and the ticket ends silently.
            self.close_read_page(&presented);
            self.cancel_read(presented.ticket, reason);
            return;
        }
        if !self.read_page_shown(&presented) {
            // When: read_page_shown is false (closed elsewhere, replaced, or moved), it is lost.
            self.close_read_page(&presented);
            self.cancel_read(presented.ticket, CancelReason::PageLost);
            return;
        }
        if now.saturating_duration_since(presented.ticket.binding().created_at) >= TICKET_LIFETIME {
            // When: the page outlived TICKET_LIFETIME unanswered, it closes and answers empty.
            self.close_read_page(&presented);
            self.answer_read_empty(presented.ticket, now);
            return;
        }
        self.clip_state.reads.presented = Some(presented);
    }

    /// Show the oldest waiting ticket whose source window is frontmost, when no page is up and the
    /// palette is closed; every other ticket keeps waiting.
    pub(in crate::app) fn present_read_page(&mut self) {
        if self.clip_state.reads.presented.is_some() || self.interaction.command_palette.is_open() {
            // When: presented is Some or command_palette is_open, nothing is reused or replaced.
            return;
        }
        let Some(frontmost) = self.registry.frontmost_window else {
            // When: no SonicTerm window is frontmost, no page opens and nothing is raised.
            return;
        };
        let waiting = &self.clip_state.reads.waiting;
        let Some(index) = waiting.iter().position(|ticket| {
            self.registry.window_of_pane(ticket.binding().pane_id) == Some(frontmost)
        }) else {
            // When: no waiting ticket's pane is in the frontmost window, every ticket waits.
            return;
        };
        let pane_id = waiting[index].binding().pane_id;
        let Some(slot) = self.registry.locate_pane(pane_id) else {
            // When: locate_pane finds no tab holding the pane (mid-move), it waits a turn.
            return;
        };
        let window = &self.registry.windows[&slot.window_id];
        if window.hidden {
            // When: the window is hidden, nothing can be shown in it.
            return;
        }
        // Positions, not titles: a program can set a tab's title, so it never reaches the page.
        let tab_number = (slot.tab_index + 1).to_string();
        let pane_number = window.tab_states[slot.tab_index]
            .tree
            .leaves()
            .iter()
            .position(|leaf| *leaf == pane_id)
            .map_or_else(|| "1".to_string(), |position| (position + 1).to_string());
        let ticket = self.clip_state.reads.waiting.remove(index);
        let title = self.i18n.t_args(
            "clipboard-read-title",
            Some(&[("tab", tab_number.as_str()), ("pane", pane_number.as_str())]),
        );
        let items = ["clipboard-read-sends", "clipboard-read-expiry"]
            .into_iter()
            .map(|key| self.i18n.translate(key))
            .collect();
        self.interaction.palette_pointer_capture = None;
        self.interaction.command_palette.start_confirm(
            title,
            self.i18n.translate("clipboard-read-allow"),
            self.i18n.translate("clipboard-read-deny"),
            items,
        );
        self.interaction.palette_attached_window =
            (Some(slot.window_id) != self.registry.main_window_id).then_some(slot.window_id);
        let session = self.interaction.command_palette.session();
        tracing::debug!(pane_id, "OSC 52 read consent page shown");
        self.clip_state.reads.presented =
            Some(PresentedRead { ticket, session, window: slot.window_id });
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// Close a read's consent page with its answer; `false`, changing nothing, when the open page
    /// is not one. Apply approves the ticket only on the page's own session in the window that
    /// showed it, after revalidation; any other close answers empty.
    pub(in crate::app) fn close_read_consent_page(&mut self, allow: bool) -> bool {
        let session = self.interaction.command_palette.session();
        let Some(presented) =
            self.clip_state.reads.presented.take_if(|presented| presented.session == session)
        else {
            // When: no presented page has this session, the open page belongs to another owner.
            return false;
        };
        let host = self.palette_host();
        self.close_read_page(&presented);
        let now = Instant::now();
        let ticket = presented.ticket;
        if host != Some(presented.window) {
            // The palette moved to another window under this session, so the answer is void.
            self.cancel_read(ticket, CancelReason::PageLost);
        } else if let Err(reason) = self.revalidate_read(ticket.binding(), now, false) {
            // When: revalidate_read refuses, the answer cannot apply, so the ticket ends silently.
            self.cancel_read(ticket, reason);
        } else if allow
            && now.saturating_duration_since(ticket.binding().created_at) < TICKET_LIFETIME
        {
            // When: allow is set within TICKET_LIFETIME, Apply consumed the page's session and
            // the approval outlives the page.
            let mut ticket = ticket;
            ticket.approve();
            tracing::debug!(pane_id = ticket.binding().pane_id, "OSC 52 read approved");
            self.clip_state.reads.queued.push_back(ticket);
        } else {
            // When: allow is false (Deny, Escape, an outside click) or it expired, answer empty.
            self.answer_read_empty(ticket, now);
        }
        // The next waiting read can open its own page now.
        self.pump_clipboard_reads();
        true
    }
}
