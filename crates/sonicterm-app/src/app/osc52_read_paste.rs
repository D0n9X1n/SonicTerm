//! DECSET 5522 paste events off the event loop. A paste chooses its event recipients and records
//! each one's identity at the gesture; a disposable capture thread then reads the clipboard offer's
//! version, its type listing and the version again, so an X11 owner that never answers cannot
//! stall the event loop. When the capture settles, each recipient whose identity is unchanged gets
//! its event and, with a stable version, its one-time grant. One capture runs at a time within the
//! reader's deadline and at most [`MAX_PENDING_PASTES`] wait; past the deadline, while a capture is
//! wedged, or with the queue full, the event goes out at once with no listing and no grant, so the
//! follow-up read asks.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use sonicterm_vt::vt::KittyClipboardLocation;

use super::osc52_read_grants::{one_time_password, PasteBinding, PasteGrant};
use super::osc52_read_source::{ReadLocation, TypeListing};
use super::UserEvent;

/// Pastes that may wait for the capture thread; a further one is answered at once, unlisted.
const MAX_PENDING_PASTES: usize = 4;

/// One event recipient, as the paste gesture chose it.
struct Recipient {
    pane_id: u64,
    /// Its identity at the gesture; the event goes out only while the pane still has it.
    binding: Option<PasteBinding>,
}

/// What one capture found: the listing, and the offer version when it was stable across it.
struct Captured {
    listing: TypeListing,
    version: Option<u64>,
}

impl Captured {
    /// What a paste no capture answered gets: an empty listing and no version, so no grant.
    fn unanswered() -> Self {
        Self { listing: TypeListing::Failed, version: None }
    }
}

/// The capture outstanding on its thread.
struct Running {
    recipients: Vec<Recipient>,
    deadline: Instant,
    /// Answered unlisted at its deadline; its late result is dropped when the thread returns.
    abandoned: bool,
}

/// The paste-capture lane: pastes waiting for the capture thread and the one outstanding.
pub(in crate::app) struct PasteCapture {
    pending: VecDeque<Vec<Recipient>>,
    running: Option<Running>,
    done_tx: Sender<Captured>,
    done_rx: Receiver<Captured>,
}

impl Default for PasteCapture {
    fn default() -> Self {
        // One capture runs at a time, so at most one result waits.
        let (done_tx, done_rx) = crossbeam_channel::unbounded();
        Self { pending: VecDeque::new(), running: None, done_tx, done_rx }
    }
}

impl PasteCapture {
    /// Whether no paste waits and no capture is outstanding.
    pub(in crate::app) fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.running.is_none()
    }

    /// When the lane next needs a turn: the outstanding capture's deadline, unless it passed.
    pub(in crate::app) fn next_wake(&self) -> Option<Instant> {
        self.running.as_ref().filter(|running| !running.abandoned).map(|running| running.deadline)
    }

    /// Whether a capture is past its deadline and its native calls have not returned.
    fn wedged(&self) -> bool {
        self.running.as_ref().is_some_and(|running| running.abandoned)
    }
}

impl super::App {
    /// Send each of `panes` its paste event once the offer is captured off the event loop. Each
    /// pane's earlier grant ends now and its identity is recorded now, so only the recipients this
    /// gesture chose get an event, and only while they keep that identity.
    pub(in crate::app) fn capture_paste_events(&mut self, panes: &[u64]) {
        let mut recipients = Vec::with_capacity(panes.len());
        for &pane_id in panes {
            // A new paste ends the pane's earlier grant, whatever becomes of this event.
            self.clip_state.reads.paste_grants.revoke(pane_id);
            recipients.push(Recipient { pane_id, binding: self.paste_binding(pane_id) });
        }
        let capture = &mut self.clip_state.reads.paste_capture;
        if capture.wedged() || capture.pending.len() >= MAX_PENDING_PASTES {
            // When: wedged (a capture is past its deadline) or pending is full, the events go out
            // now with no listing and no grant.
            self.send_paste_events(recipients, &Captured::unanswered());
            return;
        }
        capture.pending.push_back(recipients);
        self.pump_paste_capture(Instant::now());
    }

    /// Settle the capture lane at `now`: send a finished capture's events, answer one past its
    /// deadline and every waiting paste unlisted, and start the next capture.
    pub(in crate::app) fn pump_paste_capture(&mut self, now: Instant) {
        while let Ok(captured) = self.clip_state.reads.paste_capture.done_rx.try_recv() {
            let Some(running) = self.clip_state.reads.paste_capture.running.take() else {
                // When: running is None, no capture is recorded, so nothing is sent.
                continue;
            };
            // A capture answered unlisted at its deadline sends nothing more when it settles.
            if !running.abandoned {
                self.send_paste_events(running.recipients, &captured);
            }
        }
        let capture = &mut self.clip_state.reads.paste_capture;
        let overdue = match capture.running.as_mut() {
            Some(running) if !running.abandoned && now >= running.deadline => {
                // Past its deadline the capture is answered unlisted; the thread keeps running.
                running.abandoned = true;
                let mut late = vec![std::mem::take(&mut running.recipients)];
                late.extend(capture.pending.drain(..));
                late
            }
            _ => Vec::new(),
        };
        if !overdue.is_empty() {
            // The owner did not answer within the reader's deadline.
            tracing::warn!(pastes = overdue.len(), "paste event offer capture passed its deadline");
        }
        for recipients in overdue {
            self.send_paste_events(recipients, &Captured::unanswered());
        }
        self.start_paste_capture(now);
    }

    /// Start the next waiting capture on a disposable thread, unless one is outstanding.
    fn start_paste_capture(&mut self, now: Instant) {
        let capture = &mut self.clip_state.reads.paste_capture;
        if capture.running.is_some() {
            // When: running is Some, one capture is outstanding, so the next waits.
            return;
        }
        let Some(recipients) = capture.pending.pop_front() else {
            // When: pop_front finds no waiting paste, there is nothing to capture.
            return;
        };
        let done_tx = capture.done_tx.clone();
        let source = Arc::clone(&self.clip_state.reads.source);
        let deadline = now + self.clip_state.reads.reader_deadline;
        let proxy = self.event_loop_proxy.clone();
        let spawned =
            std::thread::Builder::new().name("sonicterm-paste-capture".into()).spawn(move || {
                let capture = || {
                    // shortcut: a paste reads the standard clipboard only.
                    let before = source.offer_version(ReadLocation::Clipboard);
                    let listing = source.list_types(ReadLocation::Clipboard);
                    let after = before.and_then(|_| source.offer_version(ReadLocation::Clipboard));
                    // A version that moved during the listing describes no one offer: none binds.
                    let version = before.filter(|version| after == Some(*version));
                    Captured { listing, version }
                };
                // A panicking native call still reports, so the lane is never left outstanding.
                let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(capture))
                    .unwrap_or_else(|_| Captured::unanswered());
                let _ = done_tx.send(captured);
                if let Some(proxy) = proxy {
                    // When: proxy is Some, the loop is woken; a closed loop has nothing to wake.
                    let _ = proxy.send_event(UserEvent::ClipboardDone);
                }
            });
        match spawned {
            Ok(_) => {
                // The thread is detached; its result arrives through done_rx.
                let running = Running { recipients, deadline, abandoned: false };
                self.clip_state.reads.paste_capture.running = Some(running);
            }
            Err(error) => {
                tracing::warn!("paste event offer capture could not start: {error}");
                self.send_paste_events(recipients, &Captured::unanswered());
            }
        }
    }

    /// Send each of `recipients` still holding its gesture identity its event through its PTY's
    /// sensitive-reply slot, with its own one-time password, and bind a grant to each event its
    /// writer admitted when `captured` has a stable version. A recipient that closed, moved, reset
    /// DECSET 5522 or entered READONLY since the gesture gets nothing.
    fn send_paste_events(&mut self, recipients: Vec<Recipient>, captured: &Captured) {
        // shortcut: a paste reads the standard clipboard only, so its event names that location.
        let location = KittyClipboardLocation::Standard;
        for Recipient { pane_id, binding } in recipients {
            let unchanged = self.takes_paste_event(pane_id)
                && self.admits_new_user_input(pane_id)
                && self.paste_binding(pane_id) == binding;
            if !unchanged {
                // When: unchanged is false, the pane is no longer the recipient the gesture chose.
                tracing::debug!(pane_id, "paste event recipient changed; none sent");
                continue;
            }
            let password = one_time_password();
            let event = super::kitty_clipboard_read::paste_event_reply(
                location,
                password.as_deref(),
                &captured.listing,
            );
            let bytes = event.len();
            let reply = sonicterm_io::pty::SensitiveReply::new(event, Box::new(()));
            // shortcut: a refused event is not retried; the user pastes again.
            if let Err(error) = self.send_read_reply(pane_id, reply) {
                // When: send_read_reply refuses the event (writer busy or stopped), no grant is made.
                tracing::debug!(pane_id, bytes, ?error, "paste event not admitted");
                continue;
            }
            let grant = match (password, binding, captured.version) {
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
}
