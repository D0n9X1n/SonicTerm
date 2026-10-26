//! Synchronized-output publication, batch coalescing and redraw delivery.

use super::*;

impl OutputFlush {
    /// A decision with nothing pending, whose latch has seen the pane's published resets.
    pub(in crate::app) fn new(pane_id: u64, handles: &PaneVtHandles) -> Self {
        // A pane runs exactly one worker, so an update already open when it starts was opened by a
        // parser this worker never published for; it has no deadline of its own and never holds.
        let sync_latch = SyncLatch::for_pane(handles);
        Self {
            pane_id,
            pending: false,
            pending_since: None,
            pending_bytes: 0,
            released_epoch: sync_latch.epoch,
            sync_latch,
            held: None,
            redraw_probe: crate::app::invariants::RedrawCoalescerProbe::new(),
        }
    }

    /// How long the worker waits for more output at `now`.
    pub(in crate::app) fn wait(&self, now: Instant) -> Duration {
        if let Some((_, deadline)) = self.held {
            // When: `self.held` names an open update, wake at its deadline so a silent one is released.
            return deadline.saturating_duration_since(now);
        }
        if self.pending {
            crate::app::PTY_REDRAW_QUIESCENT
        } else {
            // When: `self.pending` is false and nothing is held, park until output or exit arrives.
            PANE_IDLE_WAIT
        }
    }

    /// Note a batch of `byte_count` bytes received at `now`, before it is parsed.
    pub(in crate::app) fn receive(&mut self, byte_count: usize, now: Instant) {
        if byte_count > 0 {
            self.pending_bytes = self.pending_bytes.saturating_add(byte_count);
            self.pending_since.get_or_insert(now);
        }
    }

    /// Decide after a batch has been parsed and published.
    ///
    /// A reset anywhere in the batch flushes at once; an open update holds until its deadline;
    /// otherwise output flushes on size or age, else coalesces.
    pub(in crate::app) fn after_batch(
        &mut self,
        handles: &PaneVtHandles,
        now: Instant,
        send: impl FnOnce(),
    ) {
        if self.sync_latch.reset_pending {
            // When: `reset_pending` latched a reset in any section, the published batch flushes now.
            send();
            self.sync_latch.reset_pending = false;
            self.clear_pending();
            self.refresh_hold(handles);
            return;
        }
        self.refresh_hold(handles);
        if let Some((epoch, deadline)) = self.held {
            // When: `self.held` names an open update, keep coalescing and send only at its deadline.
            self.pending = true;
            if now >= deadline {
                // The update outlived its bound; present what it has.
                self.release_timeout(handles, epoch, send);
            }
            return;
        }
        let pending_for = self
            .pending_since
            .map(|since| now.saturating_duration_since(since))
            .unwrap_or(Duration::ZERO);
        if crate::app::should_flush_pending_pty_redraw(self.pending_bytes, pending_for) {
            // should_flush_pending_pty_redraw accepted pending_bytes and pending_for: dispatch the coalesced frame.
            send();
            let reason = if self.pending_bytes >= crate::app::PTY_REDRAW_FLUSH_BYTES {
                crate::app::invariants::FlushReason::Buffer
            } else {
                crate::app::invariants::FlushReason::Interval
            };
            self.redraw_probe.note_redraw(crate::app::PTY_REDRAW_QUIESCENT, reason);
            self.clear_pending();
        } else {
            // When: should_flush_pending_pty_redraw is false, retain the batch for coalescing.
            self.pending = true;
        }
    }

    /// Decide at a quiet wake: pending output flushes once.
    pub(in crate::app) fn on_quiet(
        &mut self,
        handles: &PaneVtHandles,
        now: Instant,
        send: impl FnOnce(),
    ) {
        if let Some((epoch, deadline)) = self.held {
            // When: `self.held` names an open update, a quiet wake releases it only at its deadline.
            if now >= deadline {
                // The update outlived its bound; present what it has.
                self.release_timeout(handles, epoch, send);
            }
            return;
        }
        if self.pending {
            // pending is true at the quiet wake: dispatch the coalesced trailing frame.
            send();
            self.redraw_probe.note_redraw(
                crate::app::PTY_REDRAW_QUIESCENT,
                crate::app::invariants::FlushReason::Interval,
            );
            self.clear_pending();
        }
    }

    /// Decide at disconnect: the shell's final output flushes whatever the mode.
    pub(in crate::app) fn on_disconnect(&mut self, send: impl FnOnce()) {
        if self.pending {
            // pending is true at disconnect: dispatch the shell's final output.
            send();
            self.clear_pending();
        }
    }

    /// Hold for the parser's update when it is open in an epoch after the released one.
    ///
    /// Release identity is the parser's full epoch, which the latch read under the lock; the
    /// published epoch is truncated and is compared only for equality, never for order.
    pub(super) fn refresh_hold(&mut self, handles: &PaneVtHandles) {
        let (set, epoch) = (self.sync_latch.set, self.sync_latch.epoch);
        self.held = (set && epoch > self.released_epoch).then(|| {
            let published = read_published_sync(&handles.sync_word, &handles.sync_deadline_word);
            let deadline = published
                .deadline
                .filter(|_| published.epoch == epoch & SYNC_EPOCH_MASK)
                // This worker published the deadline with its epoch; the origin is the passed
                // fallback should that store ever be missing.
                .unwrap_or_else(super::super::sync_clock::origin);
            (epoch, deadline)
        });
    }

    /// Release update `epoch` at its deadline: flush what it holds, log it, and count it.
    // Ordering: sync_timeouts uses Relaxed; it is a statistic and orders nothing.
    pub(super) fn release_timeout(
        &mut self,
        handles: &PaneVtHandles,
        epoch: u64,
        send: impl FnOnce(),
    ) {
        if self.pending {
            // Output parsed during the update is presented now; later output flushes normally.
            send();
            self.clear_pending();
        }
        self.released_epoch = epoch;
        self.held = None;
        tracing::debug!(
            target: "sonicterm_app::sync_output",
            pane_id = self.pane_id,
            epoch,
            "synchronized output held past 150 ms; released"
        );
        if let Some(counters) = handles.frame_counters.as_ref() {
            counters.vt.sync_timeouts.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Forget the output a flush just covered.
    pub(super) fn clear_pending(&mut self) {
        self.pending = false;
        self.pending_since = None;
        self.pending_bytes = 0;
    }
}

impl PaneVtHandles {
    /// Flush this pane's output through `send`, coalesced on its outstanding token as the worker does.
    pub(in crate::app) fn send_output_with(&self, send: impl FnOnce(WindowId) -> bool) {
        send_output_redraw(
            &self.redraw_target,
            &self.output_outstanding,
            self.frame_counters.as_ref(),
            Instant::now,
            send,
        );
    }
}

/// Send this pane's output event to its current redraw target.
pub(super) fn send_pane_output(
    handles: &PaneVtHandles,
    proxy: &EventLoopProxy<UserEvent>,
    pane_id: u64,
) {
    send_output_redraw(
        &handles.redraw_target,
        &handles.output_outstanding,
        handles.frame_counters.as_ref(),
        Instant::now,
        |window_id| proxy.send_event(UserEvent::PaneOutput { window_id, pane_id }).is_ok(),
    );
}

/// Send at most one outstanding output event to the pane's current redraw target.
///
/// A targeted flush always publishes its timestamp (gate on), then sends only when no event is
/// outstanding; otherwise it is counted as suppressed (gate on). A send the event loop refuses
/// releases the token so the next flush sends again. An untargeted flush leaves the token alone.
/// An armed echo watch records the publication before the token decision and the decision after
/// it, reading `now` only then; it never changes the decision or the send.
// Ordering: output_outstanding swaps AcqRel with the event loop's acknowledgement; a refused send
// stores Release. flushes_suppressed is a Relaxed statistic.
pub(in crate::app) fn send_output_redraw(
    redraw_target: &Mutex<Option<WindowId>>,
    output_outstanding: &AtomicBool,
    counters: Option<&super::super::frame_counters::PaneFrameCounters>,
    mut now: impl FnMut() -> Instant,
    send: impl FnOnce(WindowId) -> bool,
) {
    let mut targeted = false;
    let mut decided = None;
    super::super::redraw_target::dispatch(redraw_target, |target| {
        targeted = true;
        let publication = counters.and_then(|counters| {
            // The timestamp is published before the send, so the event loop never wakes without it.
            let now_ns = super::super::frame_counters::flush_clock_ns();
            let coalesced = super::super::frame_counters::publish_flush(
                &counters.pending_flush,
                now_ns,
                &counters.vt,
            );
            counters.echo.record_publication(Some(target), coalesced, &mut now)
        });
        let outcome = if output_outstanding.swap(true, Ordering::AcqRel) {
            // output_outstanding was already set: that queued event's service reads this batch.
            if let Some(counters) = counters {
                counters.vt.flushes_suppressed.fetch_add(1, Ordering::Relaxed);
            }
            super::super::echo_watch::EchoDeliveryOutcome::Suppressed
        } else if send(target) {
            // When: send(target) queued the event, its token stays set until the event loop services it.
            super::super::echo_watch::EchoDeliveryOutcome::Sent
        } else {
            // When: send refused the event, the loop is gone and nothing is queued; the token must not stay set.
            output_outstanding.store(false, Ordering::Release);
            super::super::echo_watch::EchoDeliveryOutcome::Refused
        };
        decided = publication.map(|publication| (publication, outcome));
    });
    if let (Some((publication, outcome)), Some(counters)) = (decided, counters) {
        // the armed watch recorded this flush's publication, so its token decision is recorded too.
        counters.echo.record_delivery(publication, outcome, now());
    }
    if let (false, Some(counters)) = (targeted, counters) {
        // the pane has no redraw target, no window can consume a timestamp; none is stored.
        super::super::frame_counters::note_untargeted_flush(&counters.vt);
        counters.echo.record_publication(None, false, &mut now);
    }
}

/// Publish the parser's synchronized output to the pane; the caller holds the parser lock.
///
/// A new epoch that is set gets a deadline `SYNC_OUTPUT_TIMEOUT` from `now`; a repeated set keeps it.
// Ordering: sync_resets and sync_deadline_word store Relaxed, then sync_word Release;
// a later epoch's deadline_word is rejected by its epoch tag.
pub(super) fn publish_sync_output(
    handles: &PaneVtHandles,
    state: SyncState,
    now: impl FnOnce() -> Instant,
) {
    handles.sync_resets.store(state.resets, Ordering::Relaxed);
    let published_epoch = (handles.sync_word.load(Ordering::Relaxed) >> 1) & SYNC_EPOCH_MASK;
    if state.set && state.epoch & SYNC_EPOCH_MASK != published_epoch {
        // A newly opened epoch starts its own deadline; a repeated set keeps the old one.
        let deadline = pack_sync_deadline(state.epoch, now() + SYNC_OUTPUT_TIMEOUT);
        handles.sync_deadline_word.store(deadline, Ordering::Relaxed);
    }
    #[cfg(test)]
    PUBLISH_PAUSE.with(|slot| {
        if let Some(pause) = slot.borrow_mut().as_mut() {
            pause();
        }
    });
    handles.sync_word.store(sync_word_of(state), Ordering::Release);
}

#[cfg(test)]
#[path = "spawn_pane_output_tests.rs"]
mod spawn_pane_output_tests;
