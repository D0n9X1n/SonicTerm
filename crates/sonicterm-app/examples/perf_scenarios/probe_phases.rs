//! Scenario setup, phase transitions and completion contracts.

use super::*;

impl Probe {
    /// End startup at the warm-pool barrier, record the grid and the display, then focus.
    pub(super) fn end_startup(&mut self, event_loop: &ActiveEventLoop) {
        self.finalize_startup();
        let Some(pane) = self.active_pane() else {
            self.invalidate(event_loop, "no active pane after startup".to_owned());
            return;
        };
        self.grid = self.app.__test_pane_grid_size(pane);
        self.monitor = self.monitor_info();
        // Every host records how it presented, so a macOS row can show it stayed on the hardware path.
        self.presenter = self.presenter_record();
        let blocked = self.presenter.as_ref().and_then(|presenter| {
            presenter_blocked(self.plan.presentation, presenter, scenarios::BUILD_HOST)
        });
        if let Some(reason) = blocked {
            // When: the window did not present through its variant's presenter, the run measures nothing it names.
            self.finish(event_loop, Status::Blocked, Some(reason));
            return;
        }
        if scenarios::BUILD_HOST == Host::Windows {
            // When: only Windows judges S11 by the image atlas, so only it reads the atlas's size here.
            self.image_atlas_start = self.image_atlas_bytes();
        }
        self.role_panes.push(pane);
        if self.recorded_exit_invalidates(event_loop, pane) {
            // When: the first role's program exited before its pane was recorded, the run is already invalid.
            return;
        }
        tracing::info!(target: LOG_TARGET, grid = ?self.grid, "perf_scenarios startup ended at the warm-pool barrier");
        if self.plan.focused {
            self.focus(event_loop, true);
        }
        self.stage = Stage::Setup { next_action: 0 };
    }
}

impl Probe {
    /// The measurement window's display; `None` when winit reports no monitor.
    pub(super) fn monitor_info(&self) -> Option<MonitorInfo> {
        let window = self.app.main_window()?;
        let monitor = window.current_monitor()?;
        Some(MonitorInfo {
            name: monitor.name(),
            refresh_rate_millihertz: monitor.refresh_rate_millihertz(),
            scale_factor: window.scale_factor(),
        })
    }
}

impl Probe {
    /// How the main window presents: the configured mode and the renderer's software flags.
    pub(super) fn presenter_record(&self) -> Option<PresenterRecord> {
        let renderer = self.app.main_renderer()?;
        Some(presenter_record_for(
            scenarios::BUILD_HOST,
            self.software_render_mode,
            renderer.is_software_rendering(),
            renderer.is_software_render_degraded(),
        ))
    }
}

impl Probe {
    /// Move through every stage that can progress now.
    pub(super) fn advance(&mut self, event_loop: &ActiveEventLoop) {
        loop {
            let stage = self.stage;
            match stage {
                Stage::Startup | Stage::Done => return,
                Stage::Setup { next_action } => self.advance_setup(event_loop, next_action),
                Stage::Acks => self.advance_acks(event_loop),
                Stage::Ready => self.advance_ready(event_loop),
                Stage::Steps(index) => self.advance_steps(event_loop, index),
            }
            if self.stage == stage {
                return;
            }
        }
    }
}

impl Probe {
    /// Open roles one at a time; each waits for the previous role's session record, so roles
    /// number in the order their panes open.
    pub(super) fn advance_setup(&mut self, event_loop: &ActiveEventLoop, next_action: usize) {
        let newest = self.role_panes.len().saturating_sub(1);
        if !self.scratch.join(format!("sessions/{newest}.json")).exists() {
            // When: the newest pane's script has not recorded its session, the next pane waits.
            return;
        }
        let plan = Arc::clone(&self.plan);
        let Some(setup) = plan.setup.get(next_action).copied() else {
            if self.role_panes.len() == plan.roles.len() {
                self.stage = Stage::Acks;
            } else {
                let opened = self.role_panes.len();
                self.invalidate(
                    event_loop,
                    format!("setup opened {opened} roles for {} planned", plan.roles.len()),
                );
            }
            return;
        };
        let before = self.active_pane();
        let (action, opens_role) = match setup {
            SetupAction::NewTab => (Action::NewTab, true),
            SetupAction::SplitRight => (Action::SplitRight, true),
            SetupAction::SplitDown => (Action::SplitDown, true),
            SetupAction::ActivateTab(index) => (Action::ActivateTab(index), false),
        };
        if !self.run_action(event_loop, &action) {
            self.invalidate(event_loop, format!("the App refused setup action {setup:?}"));
            return;
        }
        if opens_role {
            match self.active_pane() {
                Some(pane) if Some(pane) != before && !self.role_panes.contains(&pane) => {
                    self.role_panes.push(pane);
                    if self.recorded_exit_invalidates(event_loop, pane) {
                        // When: the new role's program exited before its pane was recorded, the run is invalid.
                        return;
                    }
                }
                _ => {
                    self.invalidate(
                        event_loop,
                        format!("setup action {setup:?} opened no new pane"),
                    );
                    return;
                }
            }
        }
        self.stage = Stage::Setup { next_action: next_action + 1 };
    }
}

impl Probe {
    /// Run an App action as a forwarded dispatch; whether the App accepted it.
    pub(super) fn run_action(&mut self, event_loop: &ActiveEventLoop, action: &Action) -> bool {
        let mut accepted = false;
        let invocation = Invocation::of(InvocationKind::RunAction, None, true);
        self.forward(event_loop, Dispatch::Harness, invocation, |app, _active| {
            accepted = app.run_action(action)
        });
        accepted
    }
}

impl Probe {
    /// Managed runs wait for the comparison script's acknowledgements; unmanaged runs write them.
    pub(super) fn advance_acks(&mut self, event_loop: &ActiveEventLoop) {
        for role in 0..self.plan.roles.len() {
            let ack = self.scratch.join(format!("acks/{role}"));
            if self.request.managed {
                if !ack.exists() {
                    // When: the comparison script has not validated this role's record, GO waits.
                    return;
                }
            } else if let Err(error) = std::fs::write(&ack, b"") {
                self.invalidate(event_loop, format!("cannot acknowledge role {role}: {error}"));
                return;
            }
        }
        self.stage = Stage::Ready;
    }
}

impl Probe {
    /// GO waits until every role shows READY and no pane outside the plan has claimed a role.
    pub(super) fn advance_ready(&mut self, event_loop: &ActiveEventLoop) {
        let roles = self.plan.roles.len();
        if self.scratch.join(format!("roles/{roles}")).exists() {
            self.invalidate(event_loop, format!("a pane outside the plan claimed role {roles}"));
            return;
        }
        for role in 0..roles {
            let ready = workload::ready_line(role);
            let found = self
                .role_grid(role, |grid| {
                    line_row_near_cursor(grid, &ready, protocol_rows(scenarios::BUILD_HOST))
                })
                .flatten();
            let Some(row) = found else {
                // When: a role's READY is not in its grid yet, setup keeps waiting for it.
                return;
            };
            // The first sighting is kept; a row's lifetime number never changes once printed.
            self.ready_rows[role].get_or_insert(row);
        }
        self.stage = Stage::Steps(0);
    }
}

impl Probe {
    /// Run plan steps from `index` until one has to wait.
    pub(super) fn advance_steps(&mut self, event_loop: &ActiveEventLoop, mut index: usize) {
        let plan = Arc::clone(&self.plan);
        loop {
            let Some(step) = plan.steps.get(index) else {
                self.finish(event_loop, Status::Valid, None);
                return;
            };
            match step {
                Step::Phase(phase) => {
                    if self.meter.is_none() {
                        self.begin_phase(event_loop, phase);
                    }
                    if self.stage == Stage::Done {
                        return;
                    }
                    self.drive(event_loop, Instant::now());
                    if self.stage == Stage::Done {
                        return;
                    }
                    if self.image.present.take_redraw_request() {
                        self.request_image_redraw(event_loop);
                    }
                    if self.image.role.is_some() {
                        let now = Instant::now();
                        let verdict = waits::image_verdict(
                            scenarios::BUILD_HOST,
                            self.image.present.seen(),
                            self.image_register_deadline(),
                            self.image_atlas_grew(),
                            self.image.present.progress(now),
                            now,
                        );
                        match verdict {
                            ImageVerdict::Blocked(reason) => {
                                // When: on Windows the image never registered, or the atlas never took it.
                                self.finish(event_loop, Status::Blocked, Some(reason));
                                return;
                            }
                            ImageVerdict::Invalid(reason) => {
                                self.invalidate(event_loop, reason);
                                return;
                            }
                            ImageVerdict::Valid | ImageVerdict::Waiting => {}
                        }
                    }
                    if let Some(reason) = self.expired_barrier_reason(phase, Instant::now()) {
                        // When: no qualifying frame presented within the barrier's own bound.
                        self.invalidate(event_loop, reason);
                        return;
                    }
                    if !self.phase_done(phase, Instant::now()) {
                        self.stage = Stage::Steps(index);
                        return;
                    }
                    self.end_phase(event_loop, phase);
                    if self.stage == Stage::Done {
                        // When: the phase's delivery check ended the run, no later step runs.
                        return;
                    }
                }
                Step::Act(act) => {
                    self.perform(event_loop, *act);
                    if self.stage == Stage::Done {
                        return;
                    }
                }
                Step::Checkpoint(label) => {
                    if !self.advance_checkpoint(event_loop, index, label) {
                        if self.stage != Stage::Done {
                            self.stage = Stage::Steps(index);
                        }
                        return;
                    }
                    // The record is complete and its footprint taken: progress.json gets it now,
                    // not when the next phase ends, so a run killed in that phase keeps it.
                    self.record_progress();
                }
            }
            index += 1;
        }
    }
}

impl Probe {
    /// Start a measured phase; the first one writes GO for every role just before it starts.
    pub(super) fn begin_phase(&mut self, event_loop: &ActiveEventLoop, phase: &PhaseSpec) {
        if let Some(updates) = phase.updates {
            self.begin_attribution(event_loop, updates);
            if self.stage == Stage::Done {
                return;
            }
        }
        let releasing = self.go_at.is_none();
        let awaits_output = releasing
            && matches!(phase.kind, scenarios::PhaseKind::Transition)
            && matches!(phase.end, PhaseEnd::Sentinels(_));
        if releasing {
            if let Err(reason) = self.release_roles(awaits_output, Self::release_role) {
                // When: a role's GO could not be written, the run cannot measure what it prints.
                self.invalidate(event_loop, reason);
                return;
            }
            tracing::info!(target: LOG_TARGET, roles = self.plan.roles.len(), "perf_scenarios GO");
        }
        tracing::info!(target: LOG_TARGET, phase = phase.name, "perf_scenarios phase started");
        let counters_start = self.counter_totals();
        let counting = self.allocation_counter.is_some();
        self.meter = Some(PhaseMeter::start(
            phase.name,
            phase.kind,
            phase.endpoint(),
            counting,
            counters_start,
            phase.updates,
        ));
        if awaits_output {
            // When: this phase released the roles to print, it records their first output after GO against
            // the cursors `release_roles` read, as diagnostics; its first present stays the first after GO.
            let go_lead_ms = self.go_at.map_or(0.0, |go_at| {
                self.meter.as_ref().map_or(0.0, |meter| ms_between(go_at, meter.started))
            });
            if let Some(meter) = self.meter.as_mut() {
                meter.present.await_output(go_lead_ms);
            }
        }
        self.sentinel_roles = match &phase.end {
            PhaseEnd::Sentinels(roles) => roles.clone(),
            _ => Vec::new(),
        };
        let image_role = match phase.end {
            PhaseEnd::ImageRegistered(role) => Some(role),
            _ => None,
        };
        self.image =
            ImageState { role: image_role, started: Some(Instant::now()), ..ImageState::default() };
        // A barrier's baseline is the presented-frame count at its own act, just before the act runs.
        self.start_barrier(&phase.end, Instant::now());
        self.scan = ScanThrottle::new(SCAN_INTERVAL);
        for act in &phase.enter {
            self.perform(event_loop, *act);
            if self.stage == Stage::Done {
                return;
            }
        }
        let driver = self.start_driver(event_loop, phase);
        if self.stage != Stage::Done {
            self.driver = driver;
        }
    }
}

impl Probe {
    /// End a measured phase: release a held button, close the open sample, record the phase.
    pub(super) fn end_phase(&mut self, event_loop: &ActiveEventLoop, phase: &PhaseSpec) {
        // A trim still waiting for the cover lapses with the phase that asked for it.
        self.service_trim(true);
        if matches!(&self.driver, DriverState::Drag(drag) if drag.pressed) {
            self.button(event_loop, ElementState::Released);
        }
        if let Some(reason) = self.park_problem(phase).or_else(|| self.status_line_problem(phase)) {
            // When: a parked phase measured nothing, or a status-line phase broke the strip's idle contract.
            self.finalize_phase(phase);
            self.invalidate(event_loop, reason);
            return;
        }
        if let Some(reason) = self.finalize_phase(phase) {
            // When: ConPTY did not deliver what the role printed, the phase measured something else.
            self.finish(event_loop, Status::Blocked, Some(reason));
        }
    }
}

impl Probe {
    /// The finalization of startup: its meter, its transport, then progress.
    pub(super) fn finalize_startup(&mut self) {
        self.finish_meter();
        self.transport_guard_correlation();
        self.record_progress();
    }
}

impl Probe {
    /// The finalization of a measured phase after its input is released: close the sample, latch its end and
    /// per-phase results, then transport it and write progress. A delivery problem returns before the
    /// transport, so the caller's finish transports the phase exactly once.
    pub(super) fn finalize_phase(&mut self, phase: &PhaseSpec) -> Option<String> {
        self.close_sample(UnattributedReason::NoCandidate);
        self.driver = DriverState::None;
        self.end_attribution();
        self.finish_meter();
        // A barrier phase ended when its qualifying frame presented, however late this runs.
        let now = self.guard_now();
        let ended_at = self.barrier.and_then(|barrier| barrier.done_at()).unwrap_or(now);
        let ended_unix_s = unix_now() - now.saturating_duration_since(ended_at).as_secs_f64();
        self.phase_ends.push((phase.name, ended_at, ended_unix_s));
        self.barrier = None;
        tracing::info!(target: LOG_TARGET, phase = phase.name, "perf_scenarios phase ended");
        if let (Some(bytes), Some(go_at)) = (phase.throughput_bytes, self.go_at) {
            let seen =
                self.sentinel_roles.iter().filter_map(|role| self.sentinel_seen[*role]).max();
            if let Some(seen) = seen {
                let seconds = seen.saturating_duration_since(go_at).as_secs_f64();
                self.throughput = Some(Throughput { bytes, seconds });
            }
        }
        if let Some(reason) = self.delivery_blocked() {
            // When: delivery_blocked names a reason, the caller finishes the run, which transports this phase.
            return Some(reason);
        }
        self.sentinel_roles.clear();
        self.image = ImageState::default();
        self.scan = ScanThrottle::new(SCAN_INTERVAL);
        // After every endpoint and the per-phase work; its time is reported apart from every measurement.
        self.transport_guard_correlation();
        // Outside every measured window: this phase's meter is finished and the next not started.
        self.record_progress();
        None
    }
}

impl Probe {
    /// Why the phase's delivery invalidates it: ConPTY's on Windows, or a test's forced problem.
    pub(super) fn delivery_blocked(&self) -> Option<String> {
        #[cfg(test)]
        if let Some(reason) = &self.forced_delivery_problem {
            // When: a test forced a delivery problem, it stands in for ConPTY's on any host.
            return Some(reason.clone());
        }
        if scenarios::BUILD_HOST == Host::Windows {
            // When: the host is Windows, ConPTY's delivery is checked against what the roles printed.
            return self.delivery_problem();
        }
        None
    }
}

impl Probe {
    /// Whether `phase` has reached its end.
    pub(super) fn phase_done(&self, phase: &PhaseSpec, now: Instant) -> bool {
        match &phase.end {
            PhaseEnd::Hold(_) | PhaseEnd::AfterGo(_) => {
                self.phase_deadline(phase).is_some_and(|deadline| now >= deadline)
            }
            PhaseEnd::Sentinels(roles) => {
                roles.iter().all(|role| self.sentinel_seen[*role].is_some())
            }
            // Only a frame known to follow the registration ends it; the phase's step invalidates
            // the run when none presents in time.
            PhaseEnd::ImageRegistered(_) => {
                self.image.present.progress(now) == ImageProgress::Presented
            }
            PhaseEnd::MediaFree | PhaseEnd::Reshow => {
                self.barrier.is_some_and(|barrier| barrier.progress(now) == BarrierProgress::Done)
            }
            PhaseEnd::HoldFrom { .. } => {
                self.phase_deadline(phase).is_some_and(|deadline| now >= deadline)
            }
            PhaseEnd::DriverDone => match &self.driver {
                DriverState::Typing(typing) => {
                    typing.typed >= typing.chars
                        && typing.done_at.is_some_and(|at| now >= at + typing.settle)
                }
                // One interval after the last tick, so its frame can present inside the phase.
                DriverState::Wheel(wheel) => {
                    wheel.sent >= wheel.total && now >= wheel.started + wheel.interval * wheel.total
                }
                DriverState::AtlasRetry(retry) => {
                    retry.machine.is_done() && retry.failure.is_none()
                }
                DriverState::Commit(commit) => {
                    commit.sent >= commit.steps.len() && now >= commit.settle_end()
                }
                _ => true,
            },
        }
    }
}

impl Probe {
    /// When `phase` ends by the clock, if it does.
    pub(super) fn phase_deadline(&self, phase: &PhaseSpec) -> Option<Instant> {
        match &phase.end {
            PhaseEnd::Hold(hold_ms) => {
                self.meter.as_ref().map(|meter| meter.started + Duration::from_millis(*hold_ms))
            }
            PhaseEnd::AfterGo(after_go_ms) => {
                self.go_at.map(|go_at| go_at + Duration::from_millis(*after_go_ms))
            }
            // Windows also wakes at the registration bound, since an image that never registers
            // leaves nothing else to wake the loop before the run's deadline.
            PhaseEnd::ImageRegistered(_) => self.image.present.deadline().or_else(|| {
                (scenarios::BUILD_HOST == Host::Windows).then(|| self.image_register_deadline())
            }),
            PhaseEnd::MediaFree | PhaseEnd::Reshow | PhaseEnd::HoldFrom { .. } => {
                let started = self.meter.as_ref().map_or_else(Instant::now, |meter| meter.started);
                let anchor = match &phase.end {
                    PhaseEnd::HoldFrom { anchor, .. } => self.phase_end_instant(anchor),
                    _ => None,
                };
                barrier_phase_deadline(&phase.end, self.barrier.as_ref(), started, anchor)
            }
            PhaseEnd::Sentinels(_) | PhaseEnd::DriverDone => None,
        }
    }
}

impl Probe {
    /// When the most recent phase named `name` ended, if one has.
    pub(super) fn phase_end_instant(&self, name: &str) -> Option<Instant> {
        self.phase_ends.iter().rev().find(|(ended, ..)| *ended == name).map(|(_, at, _)| *at)
    }
}

impl Probe {
    /// Items in the main renderer's image atlas, for the reshow barrier; capacity is never read.
    pub(super) fn image_atlas_items(&self) -> usize {
        self.image_atlas_amount().items
    }
}

impl Probe {
    /// The main renderer's image atlas reading: its capacity in bytes and the images it holds.
    pub(super) fn image_atlas_amount(&self) -> ResourceAmount {
        #[cfg(test)]
        if let Some((_, amount)) = self.test_readings {
            // When: a test stands in for the renderer, its atlas reading is the reading.
            return amount;
        }
        self.app.main_renderer().map_or_else(ResourceAmount::default, |renderer| {
            renderer.retained_amounts().image_atlas
        })
    }
}

impl Probe {
    /// Start `end`'s frame barrier at its act, `act_at`, with the renderer's presented frames as its
    /// baseline; a phase without one clears the barrier.
    pub(super) fn start_barrier(&mut self, end: &PhaseEnd, act_at: Instant) {
        let barrier_wait = match end {
            PhaseEnd::MediaFree => Some((waits::MEDIA_FREE_WAIT, false)),
            PhaseEnd::Reshow => Some((waits::RESHOW_WAIT, true)),
            _ => None,
        };
        self.barrier = barrier_wait.map(|(wait, needs_image_item)| {
            FrameBarrier::new(act_at, self.frame_count(), wait, needs_image_item)
        });
    }
}

impl Probe {
    /// Report a presenting dispatch that ended at `ended` to the running barrier, with the renderer's
    /// frame count and image atlas items.
    pub(super) fn observe_barrier_frame(&mut self, ended: Instant) {
        if self.barrier.is_none() {
            // When: no barrier phase runs, the atlas is not read.
            return;
        }
        let frames = self.frame_count();
        let image_items = self.image_atlas_items();
        if let Some(barrier) = self.barrier.as_mut() {
            barrier.observe(ended, frames, image_items);
        }
    }
}

impl Probe {
    /// The invalid reason when `phase`'s barrier has expired at `now`, else `None`.
    pub(super) fn expired_barrier_reason(&self, phase: &PhaseSpec, now: Instant) -> Option<String> {
        let barrier = self.barrier?;
        (barrier.progress(now) == BarrierProgress::Expired)
            .then(|| waits::barrier_expired_reason(phase.name, barrier.wait()))
    }
}

impl Probe {
    /// The main renderer's frame texture extent, for S11's `end` checkpoint.
    #[cfg(feature = "perf-frame-texture")]
    pub(super) fn frame_texture_extent(&self) -> Option<(u32, u32)> {
        if self.plan.scenario != "S11" {
            // When: another scenario runs, its `end` reports no frame texture.
            return None;
        }
        self.app.main_renderer().map(GpuRenderer::frame_texture_extent)
    }
}

impl Probe {
    /// Without the feature the tree's renderer may have no extent reading, so none is taken.
    #[cfg(not(feature = "perf-frame-texture"))]
    pub(super) fn frame_texture_extent(&self) -> Option<(u32, u32)> {
        None
    }
}

impl Probe {
    /// The phase being measured now, if a phase step is running.
    pub(super) fn current_phase(&self) -> Option<&PhaseSpec> {
        let Stage::Steps(index) = self.stage else {
            return None;
        };
        self.meter.as_ref()?;
        match self.plan.steps.get(index)? {
            Step::Phase(phase) => Some(phase),
            Step::Act(_) | Step::Checkpoint(_) => None,
        }
    }
}

impl Probe {
    /// Perform one synthetic action.
    pub(super) fn perform(&mut self, event_loop: &ActiveEventLoop, act: Act) {
        match act {
            Act::Unfocus => self.focus(event_loop, false),
            Act::OpenSearch => {
                let accepted = self.run_action(event_loop, &Action::OpenSearch);
                let opened = self.app.main().is_some_and(|main| {
                    main.tab_states
                        .get(main.tabs.active_index())
                        .is_some_and(|tab| tab.search.is_some())
                });
                if !accepted || !opened {
                    // When: no search field opened, no public path enters a query, so the
                    // scenario is blocked rather than measured without its search.
                    let reason =
                        "Action::OpenSearch opened no search field, so no query can be entered";
                    self.finish(event_loop, Status::Blocked, Some(reason.to_owned()));
                }
            }
            Act::Commit(text) => {
                self.window_input(event_loop, WindowEvent::Ime(Ime::Commit(text.to_owned())));
            }
            Act::ActivateTab(index) => {
                if !self.run_action(event_loop, &Action::ActivateTab(index)) {
                    self.invalidate(event_loop, format!("the App refused ActivateTab({index})"));
                }
            }
            Act::Cover => self.cover(event_loop),
            Act::Uncover => {
                self.uncover();
                if !waits::occlusion_wait_applies(scenarios::BUILD_HOST)
                    && self.occlusion.delivered == Some(true)
                {
                    // When: this host reports no occlusion and the trim delivered `Occluded(true)`, the
                    // probe reveals the window to the App itself, which starts the uncover time.
                    self.deliver_occlusion(event_loop, false);
                }
            }
            Act::TrimCovered => self.request_trim(event_loop),
        }
    }
}

impl Probe {
    /// Deliver a synthetic `Focused` to the measurement window.
    pub(super) fn focus(&mut self, event_loop: &ActiveEventLoop, focused: bool) {
        self.window_input(event_loop, WindowEvent::Focused(focused));
    }
}

impl Probe {
    /// End the run with `status`; the partial phase and an open sample are kept for the result.
    pub(super) fn finish(
        &mut self,
        event_loop: &ActiveEventLoop,
        status: Status,
        reason: Option<String>,
    ) {
        if self.finalize_run(status, reason) {
            event_loop.exit();
        }
    }
}

#[cfg(test)]
#[path = "probe_phases_tests.rs"]
mod probe_phases_tests;
