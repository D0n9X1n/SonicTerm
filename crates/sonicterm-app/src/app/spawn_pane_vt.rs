//! VT batch processing and publication without extending parser custody.

use super::*;

/// Publish one completed nonempty batch only after parser, media, and host side effects return.
pub(in crate::app) fn process_pane_vt_batch_and_publish<Bytes: AsRef<[u8]>>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    read_at: Option<Instant>,
    command_started: &mut Option<Instant>,
    sync_latch: &mut SyncLatch,
    proxy: Option<&EventLoopProxy<UserEvent>>,
    send_reply: impl FnMut(Vec<u8>),
) {
    publish_pane_vt_batch_with(
        handles,
        bytes,
        read_at,
        command_started,
        sync_latch,
        super::super::media::decode_inline_image,
        |event| {
            if let Some(proxy) = proxy {
                // When: proxy exists, deliver the typed host event on the app event loop.
                let _ = proxy.send_event(event);
            }
        },
        Instant::now,
        send_reply,
    );
}

/// Parse one batch with the given decoder, event sink and clock, then publish its generation.
// Ordering: output_generation Release pairs with the window's pre-lock Acquire snapshot.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn publish_pane_vt_batch_with<Bytes, Decode, Emit, Now, Send>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    read_at: Option<Instant>,
    command_started: &mut Option<Instant>,
    sync_latch: &mut SyncLatch,
    decode_media: Decode,
    emit_event: Emit,
    now: Now,
    send_reply: Send,
) where
    Bytes: AsRef<[u8]>,
    Decode: FnMut(&MediaEvent) -> Option<InlineImage>,
    Emit: FnMut(UserEvent),
    Now: FnMut() -> Instant,
    Send: FnMut(Vec<u8>),
{
    let nonempty = !bytes.as_ref().is_empty();
    process_pane_vt_batch_with(
        handles,
        bytes,
        read_at,
        command_started,
        sync_latch,
        decode_media,
        emit_event,
        now,
        send_reply,
    );
    if nonempty {
        // The batch's parser and media guards were released inside `process_pane_vt_batch_with`,
        // so a woken waiter's first read never blocks on this worker.
        publish_grid_change(&handles.output_generation, &handles.change_signal);
    }
}

/// Publish one completed change to a pane's grid: bump its output generation, then wake every
/// waiter. Every grid write a reader can wait for goes through here, the VT worker's batches and
/// the app's own status lines alike, with no parser guard held so a woken reader never blocks.
/// The two never overlap: the app writes a pane's status line only on the exit event its worker
/// posts after its last batch, so the worker's echo watch can still predict its own next value.
// Ordering: output_generation Release pairs with every reader's Acquire load of it.
pub(in crate::app) fn publish_grid_change(
    output_generation: &AtomicU64,
    change_signal: &crate::app::PaneSignal,
) {
    output_generation.fetch_add(1, Ordering::Release);
    change_signal.notify();
}

#[cfg(test)]
#[path = "spawn_pane_vt_tests.rs"]
mod spawn_pane_vt_tests;

#[cfg(test)]
#[path = "spawn_pane_vt_clipboard_tests.rs"]
mod spawn_pane_vt_clipboard_tests;

#[cfg(test)]
#[path = "spawn_pane_vt_fuzz_tests.rs"]
mod spawn_pane_vt_fuzz_tests;

impl PaneVtHandles {
    /// Settle the clipboard intake at `now`: abort an OSC 52 stream past its expiry, and finish an
    /// open OSC 5522 write past its deadline or under `deny`, sending its status through
    /// `send_reply`.
    pub(in crate::app) fn settle_clipboard_intake(
        &self,
        now: Instant,
        send_reply: &mut impl FnMut(Vec<u8>),
    ) {
        self.expire_clipboard_stream(now);
        self.service_kitty_clipboard(now, send_reply);
    }

    /// Take one ready output chunk at `now`: settle the clipboard intake first, then `parse` it,
    /// so a terminator or commit the chunk carries never lands on a stream or write that expired
    /// or whose policy generation moved before it was read. The worker loop and its tests run
    /// this one step, so they share its order.
    pub(in crate::app) fn take_ready_output<Reply: FnMut(Vec<u8>)>(
        &self,
        now: Instant,
        send_reply: &mut Reply,
        parse: impl FnOnce(&mut Reply),
    ) {
        self.settle_clipboard_intake(now, send_reply);
        parse(send_reply);
    }

    /// The worker's wait at `now`: `flush_wait`, shortened to the open clipboard stream's expiry.
    pub(in crate::app) fn clipboard_wait(&self, now: Instant, flush_wait: Duration) -> Duration {
        let osc52 = self.clipboard.borrow().deadline();
        let kitty = self.kitty_clipboard.borrow().deadline();
        match osc52.into_iter().chain(kitty).min() {
            Some(deadline) => flush_wait.min(deadline.saturating_duration_since(now)),
            None => flush_wait,
        }
    }

    /// Finish an open OSC 5522 write whose deadline passed (`EBUSY`) or whose policy is now `deny`
    /// (`EPERM`), sending its status through `send_reply`; runs on every worker wake.
    pub(in crate::app) fn service_kitty_clipboard(
        &self,
        now: Instant,
        send_reply: &mut impl FnMut(Vec<u8>),
    ) {
        let reply = self.kitty_clipboard.borrow_mut().service(now);
        if let Some(reply) = reply {
            send_reply(reply);
        }
    }

    /// Run one section's OSC 5522 requests: post committed writes now and return the section's
    /// status replies and the consent tickets to post once its command epoch is published.
    pub(in crate::app) fn run_kitty_section(
        &self,
        inbox: Option<&sonicterm_vt::vt::ClipboardInbox>,
        requests: Vec<super::super::kitty_clipboard_tx::KittySectionRequest>,
        emit: &mut impl FnMut(UserEvent),
    ) -> (Vec<u8>, Vec<super::super::osc52_read_ticket::ConsentTicket>) {
        use super::super::kitty_clipboard_tx::KittyStep;
        let mut status = Vec::new();
        let mut tickets = Vec::new();
        if requests.is_empty() {
            // When: requests is empty, the section raised no OSC 5522 packet or RIS and the
            // transaction is not touched.
            return (status, tickets);
        }
        let steps = self.kitty_clipboard.borrow_mut().run(inbox, requests, Instant::now());
        for step in steps {
            match step {
                KittyStep::Silent => {
                    // When: the step is Silent, the packet produced nothing to send.
                }
                KittyStep::Reply(reply) => status.extend(reply),
                KittyStep::Post(job) => {
                    let job = super::super::osc52_stream::ClipboardJobHandle::new(job);
                    emit(UserEvent::ClipboardWrite { pane_id: self.pane_id, job });
                }
                KittyStep::Ticket(ticket) => tickets.push(ticket),
            }
        }
        (status, tickets)
    }

    /// Post the OSC 5522 consent tickets a section admitted, after its command epoch.
    pub(in crate::app) fn post_kitty_tickets(
        &self,
        tickets: Vec<super::super::osc52_read_ticket::ConsentTicket>,
        emit: &mut impl FnMut(UserEvent),
    ) {
        for ticket in tickets {
            let ticket = super::super::osc52_read_ticket::ClipboardTicketHandle::new(ticket);
            emit(UserEvent::ClipboardTicket { pane_id: self.pane_id, ticket });
        }
    }

    /// Abort a clipboard stream open past its expiry, in the intake and the parser, releasing its
    /// lease and queued chunks; the parser swallows the rest of that OSC.
    // Lock order: the clipboard borrow ends before the parser lock is taken.
    pub(in crate::app) fn expire_clipboard_stream(&self, now: Instant) {
        let Some(stream) = self.clipboard.borrow_mut().expire(now) else {
            // When: no stream expired, the parser is not locked.
            return;
        };
        let mut parser = self.parser.lock();
        if parser.open_clipboard_stream() == Some(stream) {
            // The parser still holds the expired stream, so it aborts it too.
            parser.abort_clipboard_stream();
        }
    }

    /// Publish whether the program asked for a steady cursor, so blink deadlines stop without
    /// locking the parser; `None` restores the configured cursor, which may blink.
    // Ordering: cursor_steady is a self-contained Relaxed word this worker alone writes.
    pub(super) fn publish_cursor_style(&self, style: Option<sonicterm_types::CursorRequest>) {
        let steady = style.is_some_and(|request| request.steady);
        self.cursor_steady.store(steady, std::sync::atomic::Ordering::Relaxed);
    }
}
