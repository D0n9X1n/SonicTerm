//! The `ask` consent page for OSC 52 reads and `osc52 = "ask"` writes: a waiting ticket is shown on
//! the palette's confirm page in its source window, and only Apply on that page's own session
//! approves it. An approved write goes to the clipboard ordering domain; a refused one is dropped.
//!
//! The page opens only while the source window is the OS-frontmost terminal window and the palette
//! is closed, so it never raises a window, moves focus, or reuses or replaces an open palette; until
//! then the ticket keeps waiting, up to its expiry. The title names the tab and pane by position,
//! never by a title a program set, and Deny is highlighted first. Apply consumes the session, and
//! the approved ticket then takes the ordinary revalidated read path. Deny, Escape, an outside
//! click or expiry answer empty, as a rejected prompt is; a lost page or an identity or policy
//! change ends the ticket silently, except an OSC 5522 write's, which is answered `EPERM` while the
//! same PTY and command still own its pane.

use std::time::Instant;

use sonicterm_ui::command_palette::CommandPaletteMode;
use winit::window::WindowId;

use super::osc52_read::TICKET_LIFETIME;
use super::osc52_read_ticket::{CancelReason, ConsentTicket};

/// A waiting ticket shown on the palette's confirm page.
pub(in crate::app) struct PresentedTicket {
    ticket: ConsentTicket,
    /// The palette session that shows the page; Apply from any other session approves nothing.
    session: u64,
    /// The window the page opened in, which held the source pane then.
    window: WindowId,
}

impl PresentedTicket {
    /// The ticket the page asks about.
    pub(in crate::app) fn ticket(&self) -> &ConsentTicket {
        &self.ticket
    }
}

impl super::App {
    /// The window that hosts the palette: its attached child, or else the main window.
    fn palette_host(&self) -> Option<WindowId> {
        self.interaction.palette_attached_window.or(self.registry.main_window_id)
    }

    /// Whether the palette still shows `presented`'s page: open on its session, in its window.
    fn consent_page_shown(&self, presented: &PresentedTicket) -> bool {
        let palette = &self.interaction.command_palette;
        palette.is_open()
            && palette.mode() == CommandPaletteMode::Confirm
            && palette.session() == presented.session
            && self.palette_host() == Some(presented.window)
    }

    /// Close the palette when it is still on `presented`'s session, wherever it is hosted.
    fn dismiss_consent_page(&mut self, presented: &PresentedTicket) {
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
    pub(in crate::app) fn sweep_consent_page(&mut self, now: Instant) {
        let Some(presented) = self.clip_state.reads.presented.take() else {
            // When: no page is presented, there is nothing to sweep.
            return;
        };
        if let Err(reason) = self.revalidate_ticket(presented.ticket.binding(), now, false) {
            // When: revalidate_ticket refuses, the page closes and the ticket ends silently.
            self.dismiss_consent_page(&presented);
            self.cancel_ticket(presented.ticket, reason);
            return;
        }
        if !self.consent_page_shown(&presented) {
            // When: consent_page_shown is false (closed elsewhere, replaced, or moved), it is lost.
            self.dismiss_consent_page(&presented);
            self.cancel_ticket(presented.ticket, CancelReason::PageLost);
            return;
        }
        if now.saturating_duration_since(presented.ticket.binding().created_at) >= TICKET_LIFETIME {
            // When: the page outlived TICKET_LIFETIME unanswered, it closes and answers empty.
            self.dismiss_consent_page(&presented);
            self.refuse_ticket(presented.ticket, now);
            return;
        }
        self.clip_state.reads.presented = Some(presented);
    }

    /// Show the oldest waiting ticket whose source window is frontmost, when no page is up and the
    /// palette is closed; every other ticket keeps waiting.
    pub(in crate::app) fn present_consent_page(&mut self) {
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
        let (title_key, item_keys) = if ticket.is_write() {
            ("clipboard-write-title", ["clipboard-write-replaces", "clipboard-write-expiry"])
        } else {
            // When: the ticket is a read, the page says what the program receives.
            ("clipboard-read-title", ["clipboard-read-sends", "clipboard-read-expiry"])
        };
        let title = self.i18n.t_args(
            title_key,
            Some(&[("tab", tab_number.as_str()), ("pane", pane_number.as_str())]),
        );
        let items = item_keys.into_iter().map(|key| self.i18n.translate(key)).collect();
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
            Some(PresentedTicket { ticket, session, window: slot.window_id });
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// Close a read's consent page with its answer; `false`, changing nothing, when the open page
    /// is not one. Apply approves the ticket only on the page's own session in the window that
    /// showed it, after revalidation; any other close answers empty.
    pub(in crate::app) fn close_consent_page(&mut self, allow: bool) -> bool {
        let session = self.interaction.command_palette.session();
        let Some(presented) =
            self.clip_state.reads.presented.take_if(|presented| presented.session == session)
        else {
            // When: no presented page has this session, the open page belongs to another owner.
            return false;
        };
        let host = self.palette_host();
        self.dismiss_consent_page(&presented);
        let now = Instant::now();
        let ticket = presented.ticket;
        if host != Some(presented.window) {
            // The palette moved to another window under this session, so the answer is void.
            self.cancel_ticket(ticket, CancelReason::PageLost);
        } else if let Err(reason) = self.revalidate_ticket(ticket.binding(), now, false) {
            // When: revalidate_ticket refuses, the answer cannot apply, so the ticket ends silently.
            self.cancel_ticket(ticket, reason);
        } else if allow
            && now.saturating_duration_since(ticket.binding().created_at) < TICKET_LIFETIME
        {
            // When: allow is set within TICKET_LIFETIME, Apply consumed the page's session and
            // the approval outlives the page.
            let mut ticket = ticket;
            ticket.approve();
            tracing::debug!(pane_id = ticket.binding().pane_id, "OSC 52 request approved");
            if ticket.is_write() {
                // Apply hands a write's job to the ordering domain now.
                self.submit_approved_write(ticket);
            } else {
                // When: the ticket is a read, it queues for the one native reader.
                self.clip_state.reads.queued.push_back(ticket);
            }
        } else {
            // When: allow is false (Deny, Escape, an outside click) or it expired, answer empty.
            self.refuse_ticket(ticket, now);
        }
        // The next waiting read can open its own page now.
        self.pump_clipboard_reads();
        true
    }

    /// The clip segment's hint while a read or asked write waits for an answer: the oldest
    /// presented or waiting ticket's pane, recorded at `now` so the hold never ends it early.
    pub(in crate::app) fn waiting_clip_record(
        &self,
        now: Instant,
    ) -> Option<super::status_line::ClipRecord> {
        let reads = &self.clip_state.reads;
        let presented = reads.presented.as_ref().map(PresentedTicket::ticket);
        let ticket = presented.or_else(|| reads.waiting.first())?;
        Some(super::status_line::ClipRecord {
            pane_id: ticket.binding().pane_id,
            outcome: sonicterm_ui::status_line::ClipOutcome::Waiting,
            attempts: 1,
            last_at: now,
        })
    }
}
