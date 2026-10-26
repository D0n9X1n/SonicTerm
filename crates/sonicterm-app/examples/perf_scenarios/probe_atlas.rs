//! Atlas retry episode service and bound foreground evidence.

use super::*;

impl Probe {
    /// Start S1/atlas-retry's driver: settle the scene first. It measures only the counters set,
    /// so a run without counters is refused rather than measured without them.
    pub(super) fn start_atlas_retry(&mut self, event_loop: &ActiveEventLoop) -> DriverState {
        if !cfg!(perf_atlas_retry_api) {
            // When: the build lacks `perf_atlas_retry_api`, its App cannot drive the episodes: blocked, not invalid.
            self.finish(event_loop, Status::Blocked, Some(ATLAS_RETRY_UNAVAILABLE.to_owned()));
            return DriverState::None;
        }
        if self.counters_mode != CountersMode::On {
            // When: counters are off or unsupported, the episodes cannot be observed at all.
            self.invalidate(event_loop, "S1/atlas-retry runs only with --counters".to_owned());
            return DriverState::None;
        }
        let now = Instant::now();
        let current_exe = std::env::current_exe().ok();
        let sentinel_at = self.sentinel_seen.first().copied().flatten();
        let final_lookup = final_lookup_for(cfg!(windows), current_exe.as_deref(), sentinel_at);
        let mut retry = AtlasRetryDriver::new(now, final_lookup);
        self.apply_atlas_retry_arm(&mut retry, Arm::Redraw);
        DriverState::AtlasRetry(Box::new(retry))
    }
}

impl Probe {
    /// Apply the renderer side of `arm` for the next frame of `retry`'s machine, at once, before another
    /// dispatch is forwarded; the driver has already recorded the redraw demand.
    pub(super) fn apply_atlas_retry_arm(&mut self, retry: &mut AtlasRetryDriver, arm: Arm) {
        let Some(renderer) = self.app.main_renderer_mut() else {
            // When: there is no main renderer, nothing can be armed or measured.
            retry.failure = Some("no main renderer to arm".to_owned());
            return;
        };
        if arm == Arm::ChangeAtlas {
            // When: frame A is next, its assembly changes the atlas so the frame retries.
            change_atlas_during_next_assembly(renderer);
        }
        renderer.invalidate_retained_frame();
    }
}

impl Probe {
    /// Feed one forwarded dispatch to the episode machine: the counts it moved from `before`, the
    /// scene read before it (`scene_before`) and after it, and `now`, when it completed. The
    /// machine qualifies the scene; the probe only reads it.
    pub(super) fn observe_atlas_retry(
        &mut self,
        before: Option<Counts>,
        scene_before: Option<Scene>,
        cached_before: Option<atlas_retry::AppliedSample>,
        now: Instant,
    ) {
        if !matches!(self.driver, DriverState::AtlasRetry(_)) {
            // When: the driver ended during the dispatch, there is no machine to feed.
            return;
        }
        let after = atlas_retry_counts(&self.app);
        let delta = before.zip(after).map(|(earlier, later)| later.since(earlier));
        // Each side's applied foreground sample is read with that side's scene, at the same boundary.
        let reading = SceneReading {
            before: scene_before,
            after: self.atlas_retry_scene(),
            cached_before,
            cached_after: self.atlas_retry_cached(),
        };
        let DriverState::AtlasRetry(mut retry) =
            std::mem::replace(&mut self.driver, DriverState::None)
        else {
            return;
        };
        let was_settled = retry.machine.scene().is_some();
        if !was_settled {
            // When: still settling, the barrier's inputs are refreshed before the machine sees the dispatch.
            self.resolve_final_process(&mut retry);
            let barrier = self
                .atlas_retry_barrier(retry.final_lookup.expected(), retry.final_lookup.found_at());
            retry.machine.set_barrier(barrier);
        }
        let arm = retry.observe(delta, &reading, now);
        if let (false, Some(settled)) = (was_settled, retry.machine.scene()) {
            // When: this dispatch settled the scene, its identity is logged once, before the episodes.
            let settle_ms = u64::try_from(now.saturating_duration_since(retry.started).as_millis())
                .unwrap_or(u64::MAX);
            let observed = atlas_retry::settle_observation(settled, &reading);
            let started = retry.started;
            self.log_atlas_retry_evidence(
                "settle",
                observed,
                started,
                Some(settle_ms),
                &retry.final_lookup,
            );
        }
        if let Some(arm) = arm {
            // When: the machine armed the next frame, its renderer side is applied before another dispatch.
            self.apply_atlas_retry_arm(&mut retry, arm);
        }
        if let Some(recovery) = retry.recovery.take() {
            // When: the episodes completed and validated, their records become `atlas_recovery`.
            self.atlas_recovery = Some(recovery);
        }
        self.driver = DriverState::AtlasRetry(retry);
    }
}

impl Probe {
    /// macOS: look the final process up once, throttled, while T is unset. The settle deadline is checked
    /// before the synchronous lookup, which is skipped past it, and again with its completion instant, so a
    /// lookup completing at or past the deadline never sets T. Windows fixed T at the sentinel.
    pub(super) fn resolve_final_process(&self, retry: &mut AtlasRetryDriver) {
        if cfg!(windows) {
            // When: `cfg!(windows)`, the sentinel already fixed T and no lookup is made.
            return;
        }
        let deadline = retry.machine.deadline();
        let started = Instant::now();
        if !retry.final_lookup.begin(started, deadline) {
            // When: found, past the deadline, or within the spacing of the last lookup, none is made now.
            return;
        }
        let Some(identity) = atlas_retry::read_session(&self.scratch.join("sessions/0.json"))
        else {
            // When: the role session is missing or malformed, the leader is unknown and T stays unset.
            return;
        };
        let process = sonicterm_io::proc_info::foreground_process(identity.leader_pid);
        let completed = Instant::now();
        retry.final_lookup.finish(process.as_deref(), started, completed, deadline);
    }
}

impl Probe {
    /// The settle barrier's inputs for the final process `final_name`, observed at `final_at`: the active
    /// tab's expected title, computed as the App derives it (zero-based tab index, the pane's cwd and raw
    /// title), or the fixture problem a custom tab title makes.
    pub(super) fn atlas_retry_barrier(
        &self,
        final_name: &str,
        final_at: Option<Instant>,
    ) -> atlas_retry::Barrier {
        let mut barrier = atlas_retry::Barrier {
            final_at,
            final_name: final_name.to_owned(),
            ..atlas_retry::Barrier::default()
        };
        let Some(main) = self.app.main() else {
            // When: no main window, no title can be expected and the barrier stays unmet.
            return barrier;
        };
        let index = main.tabs.active_index();
        if let Some(custom) = main.tabs.active().and_then(|tab| tab.custom_title.as_deref()) {
            // When: the tab has a custom title, the App would show it instead: a fixture problem.
            barrier.fixture_problem = Some(atlas_retry::custom_title_problem(custom));
            return barrier;
        }
        let Some(state) = self.active_pane().and_then(|pane| main.panes.get(&pane)) else {
            // When: the active pane cannot be found, no title can be expected.
            return barrier;
        };
        let parser = state.parser.lock();
        barrier.expected_title = Some(sonicterm_ui::tab_title::format_tab_title(
            index,
            parser.cwd(),
            Some(final_name),
            parser.title(),
        ));
        barrier
    }
}

impl Probe {
    /// End the run on a failure or an expired step, else forward a pending redraw request.
    pub(super) fn drive_atlas_retry(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
        let DriverState::AtlasRetry(retry) = &mut self.driver else {
            return;
        };
        if let Some(reason) = retry.failure.take().or_else(|| retry.machine.expire(now)) {
            // When: the run failed or a step was not attempted in time, it cannot be read.
            // The failure line comes from the reading the machine kept when it refused it, never a reread.
            let observed = atlas_retry::failure_observation(&retry.machine);
            let started = retry.started;
            let final_lookup = retry.final_lookup.clone();
            self.log_atlas_retry_evidence("failure", observed, started, None, &final_lookup);
            self.invalidate(event_loop, format!("S1/atlas-retry: {reason}"));
            return;
        }
        if !retry.take_request() {
            // When: no redraw is pending, the drive waits for the step's attempt.
            return;
        }
        let Some(window_id) = self.main_id else {
            return;
        };
        let invocation = Invocation::of(InvocationKind::UserEvent, Some(window_id), true);
        self.forward(event_loop, Dispatch::Harness, invocation, |app, active| {
            app.user_event(active, UserEvent::RequestRedraw(window_id))
        });
    }
}

impl Probe {
    /// Log S1/atlas-retry's identity line for `event` from `observed`, the reading the judge saw, with its
    /// titles and the App's applied foreground sample as they were then; nothing of either is reread here.
    /// The line adds the role session (a bounded file read), the cumulative title and foreground counters
    /// read now, and one foreground lookup the harness makes itself, labelled apart with its own time. The
    /// judge's rules are unchanged, but this runs on the event loop: the lookup is a synchronous native
    /// process-table walk with no latency bound, and a failed run makes two (settle and failure).
    pub(super) fn log_atlas_retry_evidence(
        &self,
        event: &'static str,
        observed: atlas_retry::Observed,
        started: Instant,
        settle_ms: Option<u64>,
        final_lookup: &atlas_retry::FinalLookup,
    ) {
        let app_cached = observed.cached.map(|sample| atlas_retry::CachedForeground {
            process: sample.process,
            sampled_rel_ms: atlas_retry::relative_ms(sample.sampled_at, started),
        });
        let session = atlas_retry::read_session(&self.scratch.join("sessions/0.json"));
        let (tab_title_prepares, fg_worker_probes, fg_results_stale) =
            atlas_retry_title_counters(&self.app);
        let harness_lookup = session.map(|identity| {
            let process = sonicterm_io::proc_info::foreground_process(identity.leader_pid);
            atlas_retry::HarnessLookup {
                process,
                observed_rel_ms: atlas_retry::relative_ms(Instant::now(), started),
            }
        });
        let evidence = atlas_retry::Evidence {
            event,
            observation: observed.observation,
            settled_title: observed.settled_title,
            read_title: observed.read_title,
            app_cached,
            session,
            tab_title_prepares,
            fg_worker_probes,
            fg_results_stale,
            settle_ms,
            harness_lookup,
            // Windows makes no lookup; T comes from the sentinel there.
            final_lookups: (!cfg!(windows)).then(|| final_lookup.lookups()),
            final_lookup_us: (!cfg!(windows))
                .then(|| u64::try_from(final_lookup.spent().as_micros()).unwrap_or(u64::MAX)),
            final_at_rel_ms: final_lookup
                .found_at()
                .map(|found_at| atlas_retry::relative_ms(found_at, started)),
        };
        tracing::info!(target: LOG_TARGET, "perf_scenarios atlas-retry evidence {}", evidence.line());
    }
}

impl Probe {
    /// The foreground sample the App has applied for the active pane now, read alongside a scene reading;
    /// `None` when the pane has no cache entry or cannot be found.
    pub(super) fn atlas_retry_cached(&self) -> Option<atlas_retry::AppliedSample> {
        let pane = self.active_pane()?;
        let (sampled_at, process) = self.app.main_panes()?.get(&pane)?.fg_proc_cache.clone()?;
        Some(atlas_retry::AppliedSample { sampled_at, process: process.map(|found| found.name) })
    }
}

impl Probe {
    /// What the main window shows for S1/atlas-retry: the active tab's title, the font fallback
    /// state, the active pane's grid size, cursor and visible rows (trailing blanks trimmed).
    /// `None` when any part cannot be read. The title, notice id and chrome reads exist only in a
    /// tree with `perf_atlas_retry_api`; without it they read `None`, and the driver never starts.
    pub(super) fn atlas_retry_scene(&self) -> Option<Scene> {
        let window_id = self.main_id?;
        let (title, notice_id, missing) = atlas_retry_scene_parts(&self.app, window_id)?;
        let fallback = (notice_id, atlas_retry_fallback_applies(&self.app)?, missing);
        let pane = self.active_pane()?;
        let state = self.app.main_panes()?.get(&pane)?;
        let parser = state.parser.lock();
        let grid = parser.grid();
        let rows = (0..grid.rows)
            .map(|row| {
                let text: String = grid.row(row).iter().map(|cell| cell.ch).collect();
                text.trim_end().to_owned()
            })
            .collect();
        Some(Scene {
            title,
            fallback,
            grid: (grid.cols, grid.rows),
            cursor: (grid.cursor.row, grid.cursor.col),
            rows,
        })
    }
}
