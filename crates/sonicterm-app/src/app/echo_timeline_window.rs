//! Window echo admission, output service and dispatch observation.

use super::*;

impl WindowTimeline {
    /// The identity of the tick that stored a permit of `generation`, when the last accepted tick
    /// did and its identity is representable.
    pub(super) fn identity_for(&self, generation: u64) -> Option<TickIdentityV1> {
        self.last_tick.filter(|tick| tick.generation == generation).and_then(TickMeta::identity)
    }

    /// The `tick_seq` an admission consuming a permit of `generation` reports.
    pub(in crate::app) fn permit_tick_seq(&self, generation: Option<u64>) -> Option<u32> {
        generation.and_then(|generation| self.identity_for(generation)).map(|tick| tick.tick_seq)
    }

    /// Make `pane_id`'s arming this window's owner and return its slot timeline. While another
    /// pane's owner is active, the arming records nothing and that owner is untouched.
    pub(in crate::app) fn admit(
        &mut self,
        pane_id: u64,
        token: ArmToken,
        watch: &Arc<EchoWatch>,
        chunk_timestamps: bool,
        valid_permit: Option<u64>,
    ) -> Option<Box<SlotTimeline>> {
        if self.owner.as_ref().is_some_and(|owner| owner.pane_id != pane_id) {
            // When: another pane owns the timeline, this arming gets none; the owner keeps its ring.
            return None;
        }
        let initial_permit = match valid_permit {
            None => InitialPermit::Absent,
            Some(generation) => self
                .last_tick
                .filter(|tick| tick.generation == generation)
                .and_then(|tick| {
                    let identity = tick.identity()?;
                    Some(PermitSnapshotV1 { identity, delivered_at: tick.delivered_at })
                })
                .map_or(InitialPermit::Unknown, InitialPermit::Known),
        };
        // A same-pane rearm drops the previous owner, which releases its unfrozen slot timeline.
        self.owner = Some(TimelineOwner {
            pane_id,
            token,
            watch: Arc::downgrade(watch),
            loop_seq: 0,
            dispatch_seq: 0,
            current_dispatch: None,
            flood: FloodRing::new(),
        });
        Some(SlotTimeline::new(chunk_timestamps, initial_permit))
    }

    /// The owner's flood entries, copied, when `pane_id` and `token` name this window's owner.
    pub(in crate::app) fn frozen_flood_for(
        &self,
        pane_id: u64,
        token: ArmToken,
    ) -> Option<FrozenFlood> {
        self.owner
            .as_ref()
            .filter(|owner| owner.pane_id == pane_id && owner.token == token)
            .map(|owner| owner.flood.freeze())
    }

    /// End the owner when it watches pane `pane_id`; its drop releases an unfrozen slot timeline.
    pub(in crate::app) fn end_for_pane(&mut self, pane_id: u64) {
        if self.watched_pane() == Some(pane_id) {
            self.owner = None;
        }
    }

    /// End the owner when it watches `watch`; its drop releases an unfrozen slot timeline.
    pub(in crate::app) fn end_for_watch(&mut self, watch: &Arc<EchoWatch>) {
        if self.owner.as_ref().is_some_and(|owner| owner.watch.as_ptr() == Arc::as_ptr(watch)) {
            self.owner = None;
        }
    }

    /// Keep an accepted tick's metadata and, for an owner, record its `Tick` at the same instant.
    /// The window calls this only while the App's gate is on.
    pub(in crate::app) fn accept_tick(&mut self, generation: u64, delivered_at: Instant) {
        self.tick_seq = self.tick_seq.saturating_add(1);
        let tick = TickMeta { tick_seq: self.tick_seq, generation, delivered_at };
        self.last_tick = Some(tick);
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, only the metadata is kept.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        let identity = tick.identity();
        owner.write_at(delivered_at, |timeline, _, elapsed| {
            timeline.record(
                elapsed,
                loop_seq.map(|loop_seq| EchoTimelineKindV1::Tick { loop_seq, identity }),
            );
        });
    }

    /// Record that a held permit of `generation` was cleared for `cause`.
    pub(in crate::app) fn note_permit_cleared(
        &mut self,
        generation: u64,
        cause: PermitClearCauseV1,
    ) {
        let tick_seq = self.identity_for(generation).map(|identity| identity.tick_seq);
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, no timeline records the clear.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        owner.write(|timeline, _, elapsed| {
            let kind = loop_seq.map(|loop_seq| EchoTimelineKindV1::PermitCleared {
                loop_seq,
                tick_seq,
                cause,
            });
            timeline.record(elapsed, kind);
        });
    }

    /// Record one completed `PaneOutput` service of `serviced_pane`: its flood entry, and the
    /// watched pane's check at `watched_generation` with the Output request's `outcome`.
    pub(in crate::app) fn note_output_service(
        &mut self,
        serviced_pane: u64,
        observation: Option<WatchedObservation>,
        outcome: OutputCheckOutcomeV1,
    ) {
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, nothing is recorded and no lock is taken.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        if let Some(loop_seq) = loop_seq {
            owner.flood.push(loop_seq, serviced_pane);
        }
        if let Some(observation) = observation.filter(|seen| seen.token == owner.token) {
            // The check is written at its observation's instant, not after the request work that followed.
            owner.write_at(observation.observed_at, |timeline, appearance, elapsed| {
                let generation = observation.generation;
                timeline.note_check(elapsed, loop_seq, generation, outcome, appearance);
            });
        }
        // A lost flood entry sets overflow even when its check was compressed or recording had stopped.
        owner.mark_lost(loop_seq);
    }

    /// Record one admission decision of the watched dispatch, the streaming check that preceded it
    /// when it ran (`check`, outcome `None`), and a held permit it discarded.
    pub(in crate::app) fn note_admission(
        &mut self,
        decision: AdmissionDecisionV1,
        discarded: Option<u64>,
        check: Option<WatchedObservation>,
    ) {
        let discard_tick =
            discarded.map(|generation| self.identity_for(generation).map(|tick| tick.tick_seq));
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, nothing is recorded and no lock is taken.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        let dispatch = owner.dispatch();
        if let Some(observation) = check.filter(|seen| seen.token == owner.token) {
            // The streaming check is written at its observation's instant, before the decision.
            owner.write_at(observation.observed_at, |timeline, appearance, elapsed| {
                let (generation, outcome) = (observation.generation, OutputCheckOutcomeV1::None);
                timeline.note_check(elapsed, loop_seq, generation, outcome, appearance);
            });
        }
        owner.mark_lost(loop_seq);
        owner.write(|timeline, _, elapsed| {
            if let Some(dispatch_seq) = dispatch {
                // Inside a watched dispatch: an unrepresentable sequence drops the decision.
                let kind = loop_seq.zip(dispatch_seq).map(|(loop_seq, dispatch_seq)| {
                    EchoTimelineKindV1::Admission { loop_seq, dispatch_seq, decision }
                });
                timeline.record(elapsed, kind);
            }
            if let Some(tick_seq) = discard_tick {
                let cause = PermitClearCauseV1::AdmissionDiscard;
                let kind = loop_seq.map(|loop_seq| EchoTimelineKindV1::PermitCleared {
                    loop_seq,
                    tick_seq,
                    cause,
                });
                timeline.record(elapsed, kind);
            }
        });
    }

    /// Start one watched `RedrawRequested` dispatch and record its entry.
    pub(in crate::app) fn note_dispatch_entry(&mut self) {
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, the dispatch is not watched.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        owner.dispatch_seq = owner.dispatch_seq.saturating_add(1);
        owner.current_dispatch = Some(owner.dispatch_seq);
        let dispatch = owner.dispatch().flatten();
        owner.write(|timeline, _, elapsed| {
            let kind = loop_seq.zip(dispatch).map(|(loop_seq, dispatch_seq)| {
                EchoTimelineKindV1::DispatchEntry { loop_seq, dispatch_seq }
            });
            timeline.record(elapsed, kind);
        });
    }

    /// End the watched dispatch in progress, if any, and record its return.
    pub(in crate::app) fn note_dispatch_return(&mut self) {
        let Some(owner) = self.owner.as_mut() else {
            // When: `owner` is None, no watched dispatch is in progress.
            return;
        };
        let dispatch = owner.dispatch();
        owner.current_dispatch = None;
        let Some(dispatch_seq) = dispatch else {
            // When: `dispatch` is None, the owner armed during this dispatch; it has no entry.
            return;
        };
        let loop_seq = owner.next_loop_seq();
        owner.write(|timeline, _, elapsed| {
            let kind = loop_seq.zip(dispatch_seq).map(|(loop_seq, dispatch_seq)| {
                EchoTimelineKindV1::DispatchReturn { loop_seq, dispatch_seq }
            });
            timeline.record(elapsed, kind);
        });
    }

    /// The marker the renderer call of the watched dispatch in progress records through.
    pub(in crate::app) fn render_marker(&self) -> Option<RenderMarker> {
        let owner = self.owner.as_ref()?;
        let dispatch_seq = owner.dispatch()?;
        Some(RenderMarker { watch: owner.watch.upgrade()?, token: owner.token, dispatch_seq })
    }
}

#[cfg(test)]
#[path = "echo_timeline_window_tests.rs"]
mod echo_timeline_window_tests;
