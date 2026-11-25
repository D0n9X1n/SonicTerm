//! DECSET 5522 paste events off the event loop. A paste chooses its event recipients, records each
//! one's identity and binds the offer the user pasted, all at the gesture; a disposable capture
//! thread then reads the clipboard offer's version, its type listing and the version again, so an
//! X11 owner that never answers cannot stall the event loop. When the capture settles, each
//! recipient whose identity is unchanged gets its event, and a one-time grant only when the
//! capture's stable version is the offer bound at the gesture.
//!
//! The gesture binds the offer by reading its version where that is a counter read (macOS
//! `changeCount`, Windows `GetClipboardSequenceNumber`). X11 needs a round trip to the owner, so
//! there the capture's first sample stands for the pasted offer only when that capture started at
//! the gesture, with no other capture outstanding and no SonicTerm write in flight, and no write
//! SonicTerm started since then. Otherwise no grant is issued; the event still goes out and the
//! follow-up read asks. A copy another X11 program makes between the gesture and that first sample
//! is not visible to SonicTerm without a round trip.
//!
//! Each gesture takes a new paste generation, recorded per recipient pane, so a newer paste
//! supersedes a capture still in progress for that pane: the older capture's event carries no
//! grant. One capture runs at a time within the reader's deadline and at most
//! [`MAX_PENDING_PASTES`] wait; each capture stamps when it completed, so a result completed at or
//! past its deadline is late even when it is pumped before the deadline timer. Past the deadline,
//! while a capture is wedged, or with the queue full, the event goes out with no listing and no
//! grant.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use sonicterm_vt::vt::KittyClipboardLocation;

use super::osc52_read_grants::{one_time_password, PasteBinding, PasteGrant};
use super::osc52_read_source::{GestureOffer, ReadLocation, TypeListing};
use super::UserEvent;

/// Pastes that may wait for the capture thread; a further one is answered at once, unlisted.
const MAX_PENDING_PASTES: usize = 4;

/// One event recipient, as the paste gesture chose it.
struct Recipient {
    pane_id: u64,
    /// Its identity at the gesture; the event goes out only while the pane still has it.
    binding: Option<PasteBinding>,
}

/// How a gesture bound the offer the user pasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OfferBinding {
    /// The version read at the gesture; a grant needs the capture's stable version to equal it.
    Version(u64),
    /// X11: the capture started at the gesture, so its first sample is the pasted offer while no
    /// write SonicTerm starts moves `writes_started` from this count.
    FirstSample { writes_started: u64 },
    /// Nothing ties the gesture to an offer, so no grant is issued.
    Unbound,
}

/// One paste gesture: its recipients, the offer it bound and its paste generation.
struct Gesture {
    recipients: Vec<Recipient>,
    offer: OfferBinding,
    /// The paste generation each recipient took; a newer paste to the pane supersedes it.
    generation: u64,
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

/// One capture's result and when its thread completed it.
struct Done {
    captured: Captured,
    completed_at: Instant,
}

/// The capture outstanding on its thread.
struct Running {
    gesture: Gesture,
    deadline: Instant,
    /// Answered unlisted at its deadline; its late result is dropped when the thread returns.
    abandoned: bool,
}

/// The paste-capture lane: pastes waiting for the capture thread, the one outstanding, and each
/// recipient pane's newest paste generation.
pub(in crate::app) struct PasteCapture {
    pending: VecDeque<Gesture>,
    running: Option<Running>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
    /// The generation the last gesture took.
    last_generation: u64,
    /// Per recipient pane, the generation of the newest paste that chose it and has not been
    /// sent yet; its entry ends when that paste's event goes out.
    newest: HashMap<u64, u64>,
}

impl Default for PasteCapture {
    fn default() -> Self {
        // One capture runs at a time, so at most one result waits.
        let (done_tx, done_rx) = crossbeam_channel::unbounded();
        Self {
            pending: VecDeque::new(),
            running: None,
            done_tx,
            done_rx,
            last_generation: 0,
            newest: HashMap::new(),
        }
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

    /// Whether `generation` is still pane `pane_id`'s newest paste; its entry ends when it is.
    fn take_newest(&mut self, pane_id: u64, generation: u64) -> bool {
        if self.newest.get(&pane_id) != Some(&generation) {
            // When: newest holds another generation, a later paste superseded this one.
            return false;
        }
        self.newest.remove(&pane_id);
        true
    }
}

impl super::App {
    /// Send each of `panes` its paste event once the offer is captured off the event loop. Each
    /// pane's earlier grant ends now, its identity is recorded now and the offer the user pasted is
    /// bound now, so only the recipients this gesture chose get an event, only while they keep
    /// that identity, and a grant only for that offer.
    pub(in crate::app) fn capture_paste_events(&mut self, panes: &[u64]) {
        let now = Instant::now();
        // A capture that already finished settles first, so the lane is seen as it really is.
        self.pump_paste_capture(now);
        let offer = self.gesture_offer_binding();
        let capture = &mut self.clip_state.reads.paste_capture;
        capture.last_generation += 1;
        let generation = capture.last_generation;
        let mut recipients = Vec::with_capacity(panes.len());
        for &pane_id in panes {
            // A new paste ends the pane's earlier grant and supersedes its capture in progress.
            self.clip_state.reads.paste_grants.revoke(pane_id);
            self.clip_state.reads.paste_capture.newest.insert(pane_id, generation);
            recipients.push(Recipient { pane_id, binding: self.paste_binding(pane_id) });
        }
        let gesture = Gesture { recipients, offer, generation };
        let capture = &mut self.clip_state.reads.paste_capture;
        if capture.wedged() || capture.pending.len() >= MAX_PENDING_PASTES {
            // When: wedged (a capture is past its deadline) or pending is full, the events go out
            // now with no listing and no grant.
            self.send_paste_events(gesture, &Captured::unanswered());
            return;
        }
        capture.pending.push_back(gesture);
        self.start_paste_capture(now);
    }

    /// How this gesture binds the offer: its version where the source reads one on the event
    /// loop; on X11 the capture's first sample, only when that capture starts now with no
    /// SonicTerm write in flight; otherwise unbound.
    fn gesture_offer_binding(&self) -> OfferBinding {
        let (reads, domain) = (&self.clip_state.reads, &self.clip_state.domain);
        // On X11 the first sample binds only when this capture starts now (is_idle) and no write in
        // flight can land before that sample unseen; otherwise another capture or a write could
        // change the offer first, so nothing binds.
        let quiet = reads.paste_capture.is_idle() && domain.in_flight() == 0;
        // shortcut: a paste reads the standard clipboard only.
        match reads.source.gesture_offer(ReadLocation::Clipboard) {
            GestureOffer::Version(version) => OfferBinding::Version(version),
            GestureOffer::RoundTrip if quiet => {
                // shortcut: on X11 another program's copy between the gesture and the first
                // sample is not seen; binding it needs the owner's timestamp read at the gesture.
                OfferBinding::FirstSample { writes_started: domain.writes_started }
            }
            GestureOffer::RoundTrip | GestureOffer::Unavailable => OfferBinding::Unbound,
        }
    }

    /// Settle the capture lane at `now`: send a finished capture's events, answer one past its
    /// deadline and every waiting paste unlisted, and start the next capture.
    pub(in crate::app) fn pump_paste_capture(&mut self, now: Instant) {
        while let Ok(done) = self.clip_state.reads.paste_capture.done_rx.try_recv() {
            let Some(running) = self.clip_state.reads.paste_capture.running.take() else {
                // When: running is None, no capture is recorded, so nothing is sent.
                continue;
            };
            if running.abandoned {
                // When: running.abandoned, it was answered unlisted at its deadline; its late
                // result is dropped.
                continue;
            }
            if done.completed_at >= running.deadline {
                // When: completed_at reached running.deadline, the result is late even when pumped
                // before the deadline timer: unlisted and no grant, as the deadline sends it.
                self.send_paste_events(running.gesture, &Captured::unanswered());
                continue;
            }
            self.send_paste_events(running.gesture, &done.captured);
        }
        let capture = &mut self.clip_state.reads.paste_capture;
        let overdue = match capture.running.as_mut() {
            Some(running) if !running.abandoned && now >= running.deadline => {
                // Past its deadline the capture is answered unlisted; the thread keeps running.
                running.abandoned = true;
                let gesture = &mut running.gesture;
                let recipients = std::mem::take(&mut gesture.recipients);
                let (offer, generation) = (gesture.offer, gesture.generation);
                let mut late = vec![Gesture { recipients, offer, generation }];
                late.extend(capture.pending.drain(..));
                late
            }
            _ => Vec::new(),
        };
        if !overdue.is_empty() {
            // The owner did not answer within the reader's deadline.
            tracing::warn!(pastes = overdue.len(), "paste event offer capture passed its deadline");
        }
        for gesture in overdue {
            self.send_paste_events(gesture, &Captured::unanswered());
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
        let Some(gesture) = capture.pending.pop_front() else {
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
                // Stamped here, so the lane can tell a late result from one it pumped late.
                let _ = done_tx.send(Done { captured, completed_at: Instant::now() });
                if let Some(proxy) = proxy {
                    // When: proxy is Some, the loop is woken; a closed loop has nothing to wake.
                    let _ = proxy.send_event(UserEvent::ClipboardDone);
                }
            });
        match spawned {
            Ok(_) => {
                // The thread is detached; its result arrives through done_rx.
                let running = Running { gesture, deadline, abandoned: false };
                self.clip_state.reads.paste_capture.running = Some(running);
            }
            Err(error) => {
                tracing::warn!("paste event offer capture could not start: {error}");
                self.send_paste_events(gesture, &Captured::unanswered());
            }
        }
    }

    /// Send each of `gesture`'s recipients still holding its gesture identity its event through
    /// its PTY's sensitive-reply slot, with its own one-time password, and bind a grant to each
    /// event its writer admitted when the capture's stable version is the offer the gesture bound
    /// and no newer paste chose that recipient. A recipient that closed, moved, reset DECSET 5522
    /// or entered READONLY since the gesture gets nothing.
    fn send_paste_events(&mut self, gesture: Gesture, captured: &Captured) {
        // shortcut: a paste reads the standard clipboard only, so its event names that location.
        let location = KittyClipboardLocation::Standard;
        let offer_version = match gesture.offer {
            OfferBinding::Version(version) => {
                captured.version.filter(|sampled| *sampled == version)
            }
            OfferBinding::FirstSample { writes_started } => {
                let unwritten = self.clip_state.domain.writes_started == writes_started;
                captured.version.filter(|_| unwritten)
            }
            OfferBinding::Unbound => None,
        };
        for Recipient { pane_id, binding } in gesture.recipients {
            let capture = &mut self.clip_state.reads.paste_capture;
            let newest = capture.take_newest(pane_id, gesture.generation);
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
            // A superseded paste's event goes out, but only the newest paste may grant.
            let version = offer_version.filter(|_| newest);
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
}
