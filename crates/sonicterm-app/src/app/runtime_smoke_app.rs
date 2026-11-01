//! App-side native smoke setup, marker checks and GPU fault progression.

use super::*;

impl super::super::App {
    pub(crate) fn install_runtime_smoke(
        &mut self,
        spec: &RuntimeSmokeSpec,
        renderer_baseline: usize,
        scenario: RuntimeSmokeScenario,
    ) {
        self.runtime_config_path = Some(spec.config_dir().join("sonicterm.toml"));
        let mut smoke = RuntimeSmokeState::from_spec(spec, renderer_baseline);
        smoke.verify_fresh_drop_target = self.owns_native_drop_target();
        smoke.scenario = scenario;
        self.runtime_smoke = Some(smoke);
    }

    pub(in crate::app) fn smoke_check_native_title(
        &mut self,
        window: &winit::window::Window,
        expected: &str,
    ) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let observed = window.title();
            let matches = observed == expected;
            tracing::warn!(?observed, ?expected, matches, "runtime smoke native title readback");
            if !matches {
                // Native readback disproves the title write even if the model is correct.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(RuntimeSmokeFailure::Display);
                }
            }
            matches
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = (window, expected);
            // X11 winit title() is unimplemented; Wayland caches locally, so neither proves compositor state.
            tracing::warn!(
                "runtime smoke native title readback unsupported; desktop evidence required"
            );
            true
        }
    }

    pub(in crate::app) fn smoke_exercise_window_name(
        &mut self,
        id: winit::window::WindowId,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(native) = self.windows.get(&id).and_then(|state| state.window.clone()) else {
            // When: id lacks a native handle, model-only assertions cannot establish this smoke contract.
            return false;
        };
        for name in ["Work 工作", ""] {
            self.start_rename_window(id);
            self.command_palette.set_query(name);
            self.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
            // A name shows as `#N name`; a cleared one gives the title back to the active tab.
            let number = self.window_keys.get(id).unwrap().raw();
            let body = self
                .windows
                .get(&id)
                .and_then(|state| state.tabs.active())
                .map(|tab| super::super::native_title::tab_display_body(tab).to_owned());
            let expected =
                super::super::native_title::window_title_text(number, name, body.as_deref());
            if !self.smoke_check_native_title(&native, &expected) {
                // When: smoke_check_native_title fails after a real editor commit, stop before claiming presentation success.
                return false;
            }
        }
        true
    }

    pub(crate) fn runtime_smoke_result(&self) -> Result<(), RuntimeSmokeFailure> {
        let Some(smoke) = self.runtime_smoke.as_ref() else {
            // When: no smoke was installed, a caller cannot claim runtime-smoke completion.
            return Err(RuntimeSmokeFailure::EventLoop);
        };
        smoke.outcome().unwrap_or_else(|| Err(smoke.timeout_failure()))
    }

    /// Only an installed fault smoke contributes periodic wakeups; normal sessions stay idle.
    pub(in crate::app) fn gpu_fault_smoke_deadline(&self) -> Option<Instant> {
        self.runtime_smoke
            .as_ref()
            .filter(|smoke| smoke.fault_pending())
            .map(|smoke| smoke.next_probe_at.unwrap_or_else(Instant::now))
    }

    /// Advance fault injection on the event-loop thread, outside every parser/render borrow.
    pub(in crate::app) fn drive_gpu_fault_smoke(&mut self, now: Instant) -> bool {
        if !self.runtime_smoke.as_ref().is_some_and(|smoke| smoke.fault_pending()) {
            // When: runtime_smoke is absent or not in a fault phase, no fault or polling is enabled.
            return false;
        }
        if self
            .runtime_smoke
            .as_ref()
            .and_then(|smoke| smoke.next_probe_at)
            .is_some_and(|due| now < due)
        {
            // When: next_probe_at is still ahead of now, avoid a redraw busy-loop during observation.
            return false;
        }
        let Some(mut smoke) = self.runtime_smoke.take() else {
            // When: runtime_smoke disappeared, no smoke-owned action may run.
            return false;
        };
        if let Err(failure) = self.tick_gpu_fault_smoke(&mut smoke, now) {
            smoke.fail(failure);
        }
        smoke.next_probe_at = Some(Instant::now() + FAULT_POLL_INTERVAL);
        let finished = smoke.outcome().is_some();
        if finished {
            tracing::warn!(outcome = ?smoke.outcome(), "runtime smoke GPU fault verdict");
        }
        self.runtime_smoke = Some(smoke);
        finished
    }

    /// Queue the smoke marker in the original main shell without embedding it literally in input.
    pub(super) fn queue_gpu_fault_smoke_marker(
        &self,
        smoke: &RuntimeSmokeState,
        failure: RuntimeSmokeFailure,
    ) -> Result<(), RuntimeSmokeFailure> {
        let pane = self
            .main_active_pane_id()
            .and_then(|id| self.main().and_then(|window| window.panes.get(&id)))
            .ok_or(failure)?;
        pane.pty
            .as_ref()
            .ok_or(failure)?
            .send_input_nonblocking(smoke.command().to_vec())
            .map_err(|_| failure)
    }

    pub(super) fn tick_gpu_fault_smoke(
        &mut self,
        smoke: &mut RuntimeSmokeState,
        now: Instant,
    ) -> Result<(), RuntimeSmokeFailure> {
        let failure = smoke.timeout_failure();
        if smoke
            .fault_started
            .is_some_and(|started| now.saturating_duration_since(started) >= FAULT_DEADLINE)
        {
            // When: fault_started exceeds FAULT_DEADLINE, even parser contention must fail closed.
            return Err(failure);
        }
        let snapshot = self.main_renderer().ok_or(failure)?.device_error_snapshot();
        let shared = self.windows.values().all(|window| {
            window
                .renderer
                .as_ref()
                .is_some_and(|renderer| renderer.device_generation() == snapshot.generation)
        }) && self
            .warm_window_pool
            .iter()
            .all(|warm| warm.renderer.device_generation() == snapshot.generation);
        if !shared {
            // When: shared is false, a fault cannot establish process-wide containment on this topology.
            return Err(RuntimeSmokeFailure::GpuFaultContainment);
        }
        if matches!(
            smoke.phase,
            RuntimeSmokePhase::FaultPrecondition | RuntimeSmokePhase::FrameValidationReady
        ) {
            // When: matches! selects a starting phase, freeze renderer population before sampling totals.
            self.config.window.warm_window_pool = 0;
            self.warm_window_pool.clear();
            if matches!(smoke.phase, RuntimeSmokePhase::FaultPrecondition)
                && sonicterm_gpu::core::live_renderer_count() != smoke.renderer_baseline + 1
            {
                // When: live_renderer_count exceeds the main renderer, warm release was not complete.
                return Err(RuntimeSmokeFailure::WarmLifecycle);
            }
        }
        let frames = self.windows.values().filter_map(|window| window.renderer.as_ref()).fold(
            SmokeFrameCounts::default(),
            |mut total, renderer| {
                total.presents = total.presents.saturating_add(renderer.present_call_count());
                total.successful =
                    total.successful.saturating_add(renderer.successful_frame_count());
                total
            },
        );
        let pane = self
            .main_active_pane_id()
            .and_then(|id| self.main().and_then(|window| window.panes.get(&id)))
            .ok_or(failure)?;
        let marker_rows = {
            let Some(parser) = pane.parser.try_lock() else {
                // When: parser is busy, defer observation rather than blocking the event loop.
                return Ok(());
            };
            grid_marker_rows(parser.grid(), smoke.marker())
        };
        if let Some(kind) = smoke.next_fault() {
            // When: next_fault supplies kind, freeze evidence before the one injection for that phase.
            if matches!(kind, GpuFaultKind::IsolatedOperation | GpuFaultKind::FrameValidation)
                && (snapshot.state != DeviceState::Usable || snapshot.destroy_requested)
            {
                // When: snapshot is already stopped, a fresh fault cannot prove the intended transition.
                return Err(RuntimeSmokeFailure::GpuFaultContainment);
            }
            let baseline = smoke.capture_fault_baseline(
                self.main_window_id.ok_or(failure)?,
                kind,
                &snapshot,
                frames,
                marker_rows,
            );
            smoke.begin_fault(kind, baseline, now);
            tracing::warn!(
                ?kind,
                generation = snapshot.generation,
                ?frames,
                marker_rows,
                "runtime smoke inject GPU fault"
            );
            let renderer = self.main_renderer_mut().ok_or(failure)?;
            renderer.__inject_gpu_fault(kind);
            if kind == GpuFaultKind::RetainedResourceCreation {
                renderer.force_rebuild_for_scale(renderer.scale_factor());
            }
            if !matches!(kind, GpuFaultKind::IsolatedOperation | GpuFaultKind::FrameValidation) {
                self.queue_gpu_fault_smoke_marker(smoke, failure)?;
            }
            if kind != GpuFaultKind::FrameValidation {
                // Preserve retained/loss timing: the bounded destroy poll does not consume the marker deadline.
                smoke.fault_started = Some(Instant::now());
            }
            // FrameValidation retains begin_fault's five-second arming deadline, even if render is delayed.
            self.mark_all_window_inputs();
            self.request_redraw_all_terminal_windows();
        } else {
            // When: next_fault is absent, sample the already-injected fault until proof or its deadline.
            smoke.observe_fault(&snapshot, frames, marker_rows, now);
            if smoke.frame_marker_pending() {
                // frame_marker_pending confirms Unusable; resend after the fresh baseline before starting quiet time.
                tracing::warn!(
                    generation = snapshot.generation,
                    marker_rows,
                    ?frames,
                    "runtime smoke frame device stopped; resend shell marker"
                );
                self.queue_gpu_fault_smoke_marker(smoke, failure)?;
                smoke.begin_frame_marker_wait(Instant::now());
            }
            // Exercise stopped redraw refusal throughout the bounded interval, not merely an idle device snapshot.
            self.mark_all_window_inputs();
            self.request_redraw_all_terminal_windows();
        }
        Ok(())
    }
}
