//! Phase-meter timing, dispatch attribution and credited input samples.

use super::*;

impl PhaseMeter {
    /// Count one forwarded dispatch that ran from `started` to `ended` and moved the main renderer's presented
    /// count from `frames.0` to `frames.1`: presented frames and intervals for any dispatch, and for a
    /// main-window redraw its duration, span, allocations and presentation trace.
    pub(super) fn observe_dispatch(
        &mut self,
        kind: Dispatch,
        started: Instant,
        ended: Instant,
        frames: (u64, u64),
        allocations: Option<u64>,
    ) {
        let (frames_before, frames_after) = frames;
        let advanced = frames_after > frames_before;
        if advanced {
            self.presented_frames += frames_after - frames_before;
            if let Some(last) = self.last_present {
                self.present_interval_ms.push(ms_between(last, ended));
            }
            self.last_present = Some(ended);
        }
        if kind == Dispatch::Redraw {
            self.redraw_requested += 1;
            let duration_ms = ms_between(started, ended);
            self.dispatch_ms.push(duration_ms);
            // Each redraw is also kept by whether it presented, so a mix shift cannot move them.
            if advanced {
                self.dispatch_presenting_ms.push(duration_ms);
            } else {
                // When: the redraw advanced no presented count, it is a non-presenting dispatch.
                self.dispatch_nonpresenting_ms.push(duration_ms);
            }
            // The span in Unix seconds, from the phase's own start, so a log stamp can be matched to it.
            let start_unix_s =
                self.start_unix_s + started.saturating_duration_since(self.started).as_secs_f64();
            self.slow_dispatches.push(SlowDispatch {
                start_unix_s,
                end_unix_s: start_unix_s + duration_ms / 1000.0,
                duration_ms,
            });
            if let (Some(counts), Some(count)) = (self.allocations.as_mut(), allocations) {
                counts.push(count);
            }
            self.present
                .observe_redraw(ms_between(self.started, ended), advanced.then_some(frames_after));
        }
    }
}

impl PhaseMeter {
    /// The phase's record; its counter delta is `counters_end` minus the totals at its start.
    pub(super) fn finish(
        self,
        counters_end: Option<CounterTotals>,
        completion: crate::transition::Completion,
    ) -> PhaseRecord {
        let (user_s, system_s) = cpu_times();
        let frame_counters = self
            .counters_start
            .as_ref()
            .zip(counters_end.as_ref())
            .map(|(start, end)| end.delta_since(start));
        let (slow_dispatches, dispatch_count) = self.slow_dispatches.finish();
        PhaseRecord {
            name: self.name,
            kind: self.kind.name(),
            endpoint: self.endpoint.map(scenarios::Endpoint::name),
            completion_ms: completion.elapsed_ms,
            completion_missing: completion.missing,
            start_unix_s: self.start_unix_s,
            end_unix_s: unix_now(),
            cpu_user_s: user_s - self.cpu_start.0,
            cpu_system_s: system_s - self.cpu_start.1,
            presented_frames: self.presented_frames,
            redraw_requested: self.redraw_requested,
            first_present_ms: self.present.first.map(|(offset_ms, _)| offset_ms),
            last_present_ms: self.present.last.map(|(offset_ms, _)| offset_ms),
            first_present_seq: self.present.first.map(|(_, count)| count),
            last_present_seq: self.present.last.map(|(_, count)| count),
            present_missing: self.present.missing(),
            go_to_first_output_ms: self.present.output_diagnostics().0,
            first_output_to_present_ms: self.present.output_diagnostics().1,
            nonpresenting_redraws: self.present.nonpresenting,
            dispatch_ms: self.dispatch_ms,
            dispatch_presenting_ms: self.dispatch_presenting_ms,
            dispatch_nonpresenting_ms: self.dispatch_nonpresenting_ms,
            slow_dispatches,
            dispatch_count,
            present_interval_ms: self.present_interval_ms,
            allocations_per_frame: self.allocations,
            frame_counters,
            updates: self.updates,
            s10_attribution: None,
            guard_correlation: None,
            guard_correlation_transport_ns: None,
        }
    }
}

impl Drag {
    /// The step due at `now`: press, move across the grid, release; idle ticks skip to the next
    /// cycle's press. Past `quiet_from` only a held press is released, then the drag finishes.
    pub(super) fn next_motion(&mut self, now: Instant) -> Option<DragMotion> {
        if self.finished {
            return None;
        }
        if self.quiet_from.is_some_and(|quiet_from| now >= quiet_from) {
            // When: the quiet tail began, the selection must stop changing before the phase ends.
            self.finished = true;
            return std::mem::take(&mut self.pressed).then_some(DragMotion::Release);
        }
        let step = self.ticks % self.cycle_ticks;
        let (motion, advance) = if step == 0 {
            let release_at = self.started + self.interval * (self.ticks + self.move_ticks + 1);
            if release_at >= self.deadline {
                // When: a whole press, move and release no longer fits in the phase, the drag stops.
                self.finished = true;
                return None;
            }
            self.pressed = true;
            (DragMotion::Press(self.start), 1)
        } else if step <= self.move_ticks {
            let fraction = f64::from(step) / f64::from(self.move_ticks);
            (DragMotion::Move(lerp(self.start, self.end, fraction)), 1)
        } else {
            self.pressed = false;
            (DragMotion::Release, self.cycle_ticks - step)
        };
        self.ticks += advance;
        Some(motion)
    }
}

impl Typing {
    /// The next injection, or the end of the settle once every character is typed.
    pub(super) fn next_due(&self) -> Option<Instant> {
        let first_at = self.first_at?;
        if self.typed < self.chars {
            Some(first_at + self.interval * self.typed)
        } else {
            self.done_at.map(|at| at + self.settle)
        }
    }
}

impl Sweep {
    /// A triangle wave across the window every 2 s, moving to the next lane every quarter second.
    pub(super) fn position(&self) -> (f64, f64) {
        let period = u64::from(self.hertz.max(1)) * 2;
        let half = period / 2;
        let tick = u64::from(self.ticks);
        let along = tick % period;
        let rising = if along < half { along } else { period - along };
        let fraction = rising as f64 / half as f64;
        let lane = (tick / (u64::from(self.hertz) / 4).max(1)) as usize % self.lanes.len();
        (self.left + (self.right - self.left) * fraction, self.lanes[lane])
    }
}

pub(super) fn lerp(from: (f64, f64), to: (f64, f64), fraction: f64) -> (f64, f64) {
    (from.0 + (to.0 - from.0) * fraction, from.1 + (to.1 - from.1) * fraction)
}

impl Probe {
    /// A probe around `app` for `plan`, before startup: no window, phase or reading yet.
    ///
    /// The run's sentinels, allocation counter, startup meter, render-mode text and foreground note are
    /// set by the caller, which has them; tests build a headless probe through this same constructor.
    pub(super) fn new(
        app: App,
        plan: Plan,
        request: RunArgs,
        scratch: PathBuf,
        run_deadline: Instant,
    ) -> Self {
        let roles = plan.roles.len();
        let counters_mode = CountersMode::for_run(request.counters);
        Self {
            app,
            plan: Arc::new(plan),
            request,
            scratch,
            sentinels: Vec::new(),
            allocation_counter: None,
            run_deadline,
            stage: Stage::Startup,
            main_id: None,
            cover: None,
            occlusion: OcclusionState::default(),
            frames: 0,
            first_present: false,
            startup_endpoint: None,
            meter: None,
            phases: Vec::new(),
            guard_window: None,
            #[cfg(test)]
            guard_transport_override: None,
            #[cfg(test)]
            guard_clock_override: None,
            #[cfg(test)]
            forced_delivery_problem: None,
            attribution: None,
            role_panes: Vec::new(),
            park_entry: None,
            exited_panes: Vec::new(),
            go_at: None,
            go_cursors: Vec::new(),
            #[cfg(unix)]
            go_signals: Vec::new(),
            sentinel_roles: Vec::new(),
            sentinel_seen: vec![None; roles],
            ready_rows: vec![None; roles],
            sentinel_rows: vec![None; roles],
            image_atlas_start: None,
            image: ImageState::default(),
            barrier: None,
            phase_ends: Vec::new(),
            scan: ScanThrottle::new(SCAN_INTERVAL),
            driver: DriverState::None,
            open_sample: None,
            samples: Vec::new(),
            checkpoints: Vec::new(),
            checkpoint_pending: None,
            grid: None,
            monitor: None,
            throughput: None,
            uncover_ms: None,
            scrollback_rows_retained: None,
            native_focus_dropped: 0,
            native_pointer: waits::NativePointer::default(),
            synthetic_occlusion: false,
            trim_hook: TrimHookOutcome::NotReached,
            trim_pending: false,
            trim_seq_after_hook: None,
            atlas_recovery: None,
            timeline_storage: TimelineStorage::default(),
            running_outer: None,
            first_present_bound: FirstPresentBound::default(),
            presenter: None,
            software_render_mode: "",
            foreground_lock_failure: None,
            outcome: None,
            counters_mode,
            counter_error: None,
            #[cfg(test)]
            test_readings: None,
            #[cfg(test)]
            test_take: None,
        }
    }
}

impl Probe {
    /// Forward one dispatch to the App and account for it: frames, `RedrawRequested` time and
    /// allocations, present intervals, the uncover time and, while a sample is open, attribution.
    pub(super) fn forward<EventLoopRef: ?Sized>(
        &mut self,
        event_loop: &EventLoopRef,
        kind: Dispatch,
        invocation: Invocation,
        dispatch: impl FnOnce(&mut App, &EventLoopRef),
    ) {
        let frames_before = self.frame_count();
        // S1/atlas-retry reads the main renderer's counts and the scene around every forwarded dispatch.
        let retry_before = matches!(self.driver, DriverState::AtlasRetry(_)).then(|| {
            (atlas_retry_counts(&self.app), self.atlas_retry_scene(), self.atlas_retry_cached())
        });
        // The snapshot is released before the dispatch; whether it advanced is known only after.
        let before = self.open_sample.is_some().then(|| self.echo_snapshot());
        let allocations_before = match kind {
            Dispatch::Redraw => self.allocation_counter.map(|counter| counter()),
            Dispatch::Native | Dispatch::Harness => None,
        };
        let started = Instant::now();
        if let Some(sample) = self.open_sample.as_mut() {
            // When: a sample is open, its timeline records this invocation from the forward's own readings.
            sample.timeline.enter_invocation(invocation, started);
        }
        dispatch(&mut self.app, event_loop);
        let allocations = allocations_before
            .zip(self.allocation_counter)
            .map(|(before_count, counter)| counter().saturating_sub(before_count));
        let ended = Instant::now();
        if let Some(sample) = self.open_sample.as_mut() {
            // When: a sample is open, the invocation's return is recorded before anything can close it.
            sample.timeline.exit_invocation(ended);
        }
        let frames_after = self.frame_count();
        if let Some((counts, scene, cached)) = retry_before {
            // When: the atlas-retry driver runs, this dispatch's delta and scene feed its episode machine.
            self.observe_atlas_retry(counts, scene, cached, ended);
        }
        let advanced = frames_after > frames_before;
        self.frames = frames_after;
        if advanced {
            self.first_present = true;
            if let Some(from) = self.occlusion.uncover_from.take() {
                self.uncover_ms = Some(ms_between(from, ended));
            }
            self.image.present.observe_frames(frames_after, ended);
            self.observe_barrier_frame(ended);
        }
        if let Some(meter) = self.meter.as_mut() {
            // The adapter's own readings feed the phase's counts and presentation trace.
            meter.observe_dispatch(
                kind,
                started,
                ended,
                (frames_before, frames_after),
                allocations,
            );
        }
        if let Some(before) = before {
            if self.open_sample.is_some() {
                let after = self.echo_snapshot();
                self.attribute(&DispatchObservation { before, after, advanced }, started, ended);
            }
        }
        if matches!(self.stage, Stage::Steps(_)) {
            self.maybe_scan(ended, kind.scan_trigger());
        }
    }
}

impl Probe {
    /// Settle the open sample from one observed dispatch, the forward that ran `started..ended`.
    pub(super) fn attribute(
        &mut self,
        observation: &DispatchObservation,
        started: Instant,
        ended: Instant,
    ) {
        match attribute_dispatch(observation) {
            Attribution::Pending => {}
            Attribution::Credited => {
                if let Some(sample) = self.open_sample.take() {
                    // The takes run after both snapshots have released the parser lock.
                    let main = self.measurement_window();
                    let mut timeline = unarmed_timeline(sample.arm);
                    let outcome = echo_outcome(sample.arm, |token| {
                        let (outcome, drained) =
                            echo_api::take(&mut self.app, sample.pane, token, main);
                        timeline = Some(drained);
                        outcome
                    });
                    let (outcome, timeline) = self.credited_take(outcome, timeline);
                    // The credited close takes the sample's dispatch timeline too, after the echo take; neither
                    // reads a clock the other's evidence uses, and both keep this forward's `started`/`ended`.
                    let dispatch_timeline =
                        sample.timeline.close(&mut self.app, &mut self.timeline_storage);
                    let changed = snapshot_identity_changed(observation, sample.identity);
                    let credited = LatencySample::credited(
                        sample.inject_unix_s,
                        sample.injected,
                        ended,
                        &outcome,
                        changed,
                    )
                    .with_dispatch_timeline(dispatch_timeline.as_str());
                    // The credited forward bounds the dispatch pair the timeline binds.
                    let context = Credited {
                        injected: sample.injected,
                        forward_started: started,
                        ended,
                        split: credited.split,
                        split_reason: credited.split_reason,
                        token: armed_token(sample.arm),
                        window: main.map(u64::from),
                        pane: sample.pane,
                    };
                    let echo_timeline = timeline.map(|take| analyze(&take, &context));
                    self.samples.push(credited.with_timeline(echo_timeline));
                }
            }
            Attribution::Unattributed(reason) => self.close_sample(reason),
        }
    }
}

impl Probe {
    /// The credited sample's takes: the App's, or in a test the controlled pair it set.
    pub(super) fn credited_take(
        &mut self,
        outcome: EchoOutcome,
        timeline: Option<TimelineTake>,
    ) -> (EchoOutcome, Option<TimelineTake>) {
        #[cfg(test)]
        if let Some((outcome, timeline)) = self.test_take.take() {
            // When: `test_take` is set, a test supplies this sample's evidence through the production handoff.
            return (outcome, Some(timeline));
        }
        (outcome, timeline)
    }
}

impl Probe {
    /// Close the open sample, if any, as unattributed; an armed watch is disarmed and its record
    /// discarded, so a closed sample costs the worker nothing more.
    pub(super) fn close_sample(&mut self, reason: UnattributedReason) {
        if let Some(sample) = self.open_sample.take() {
            if let ArmState::Armed(token) = sample.arm {
                echo_api::discard(&mut self.app, sample.pane, token);
            }
            // Every close takes the sample's dispatch timeline and records what it came to.
            let timeline = sample.timeline.close(&mut self.app, &mut self.timeline_storage);
            self.samples.push(
                LatencySample::uncredited(sample.inject_unix_s, reason.as_str())
                    .with_dispatch_timeline(timeline.as_str()),
            );
        }
    }
}

impl Probe {
    /// The window the measurement presents in: the real main window, else the App's main entry.
    pub(super) fn measurement_window(&self) -> Option<WindowId> {
        self.main_id
            .or_else(|| self.app.main().map(|_| sonicterm_app::app::synthetic_main_window_id()))
    }
}

impl Probe {
    /// The finalization clock: now, or a test's injected clock.
    pub(super) fn guard_now(&mut self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = self.guard_clock_override.as_mut() {
            // When: a test injected a clock, every finalization read comes from it, in call order.
            return clock();
        }
        Instant::now()
    }
}

impl Probe {
    /// Transport the guard-correlation records of the phase `finish_meter` just pushed, after every endpoint
    /// of its route is read; a route that finalized no phase transports nothing.
    pub(super) fn transport_guard_correlation(&mut self) {
        let Some((started, counters_end_at)) = self.guard_window.take() else {
            // When: guard_window is None, no phase was finalized since the last transport.
            return;
        };
        let Some(phase_index) = self.phases.len().checked_sub(1) else {
            // When: phases is empty, there is no record to carry the field.
            return;
        };
        let transport_started = self.guard_now();
        #[cfg(test)]
        if let Some(record) = self.guard_transport_override.as_mut() {
            // When: a test injected the transport, it records this call in place of the take and the write.
            record(GuardTransportCall { phase_index, started, counters_end_at, transport_started });
            return;
        }
        let outcome = crate::guard_transport::api::take(&mut self.app);
        let window = crate::guard_transport::Window {
            start_ns: crate::guard_transport::api::epoch_ns(started),
            end_ns: crate::guard_transport::api::epoch_ns(counters_end_at),
        };
        let binding = crate::guard_transport::Binding {
            run_nonce: crate::guard_transport::run_nonce(),
            harness_hash: self.request.harness_hash.as_deref(),
            pid: std::process::id(),
            phase_index,
            phase_name: self.phases[phase_index].name,
            window,
        };
        let field =
            crate::guard_transport::transport(&self.scratch, &binding, &outcome, |from, to| {
                std::fs::rename(from, to)
            });
        let transport_ns =
            u64::try_from(transport_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let record = &mut self.phases[phase_index];
        record.guard_correlation = Some(field);
        record.guard_correlation_transport_ns = Some(transport_ns);
    }
}

impl Probe {
    /// A transition meter's completion: its endpoint's latched instant, eligible only before the
    /// run's deadline, or why it has none. A hold or sustained meter has none.
    pub(super) fn completion_of(&self, meter: &PhaseMeter) -> crate::transition::Completion {
        let Some(endpoint) = meter.endpoint else {
            // When: `endpoint` is None, the phase is a hold or sustained one and has no completion.
            return crate::transition::Completion::default();
        };
        let reached = match endpoint {
            scenarios::Endpoint::SentinelParsed => {
                crate::transition::latest_sentinel(&self.sentinel_seen, &self.sentinel_roles)
            }
            scenarios::Endpoint::ImageRegisteredThenPresented => self.image.present.presented_at(),
            scenarios::Endpoint::FirstPresentAfterEntry
            | scenarios::Endpoint::FirstPresentWithImage => {
                self.barrier.as_ref().and_then(FrameBarrier::done_at)
            }
            scenarios::Endpoint::FirstPresentThenAboutToWait => self.startup_endpoint,
        };
        crate::transition::completion(
            meter.started,
            reached,
            self.run_deadline,
            Instant::now(),
            crate::transition::INCOMPLETE,
        )
    }
}

impl Probe {
    /// Before GO of a phase that plays `updates` counted updates: read role 0's pane state, then arm
    /// the App's watch from a closed baseline. An update already open before GO voids the run.
    pub(super) fn begin_attribution(&mut self, event_loop: &ActiveEventLoop, updates: u32) {
        let pane = self.role_panes.first().copied();
        let counting = self.counters_mode == CountersMode::On;
        // The pane's state is read only once `start` has checked the API and the counters gate.
        let started = attribution::start(counting, attribution::API_ENABLED, || {
            pane.and_then(|pane_id| attribution::api::read_sync(&self.app, pane_id))
        });
        match (pane, started) {
            (_, Start::Skip(reason)) => {
                self.attribution = Some(attribution::Attribution::Unavailable { reason });
            }
            (_, Start::Refuse(reason)) => self.invalidate(event_loop, reason),
            (Some(pane_id), Start::Arm(reading)) => {
                let sentinel = self.sentinels.first().cloned().unwrap_or_default();
                let outcome = attribution::api::arm(
                    &mut self.app,
                    pane_id,
                    &sentinel,
                    workload::PROMPT,
                    updates,
                );
                let seq_start = self.frame_count();
                self.attribution =
                    Some(attribution::armed(outcome, pane_id, updates, reading, seq_start));
            }
            (None, Start::Arm(_)) => {
                // When: `pane` is None, no role pane was read, so `start` cannot have armed; kept total.
                self.attribution =
                    Some(attribution::Attribution::Unavailable { reason: "no-baseline" });
            }
        }
    }
}

impl Probe {
    /// At the end of the counted phase: record the presented count and the pane's state, then disarm.
    pub(super) fn end_attribution(&mut self) {
        let Some(attribution::Attribution::Armed(record)) = self.attribution.as_ref() else {
            // When: `attribution` holds no armed record, nothing was armed and nothing is read.
            return;
        };
        let pane_id = record.pane;
        let seq_end = self.frame_count();
        let end = attribution::api::read_sync(&self.app, pane_id);
        attribution::api::disarm(&mut self.app, pane_id);
        if let Some(attribution::Attribution::Armed(record)) = self.attribution.as_mut() {
            record.seq_end = Some(seq_end);
            record.end = end;
        }
    }
}

#[cfg(test)]
#[path = "probe_sampling_tests.rs"]
mod probe_sampling_tests;
