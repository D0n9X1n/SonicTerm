//! Event-loop ownership of the shared device's recovery and native rebind.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sonicterm_gpu::core::{
    ContextRequest, GpuRenderer, GpuSharedContext, PreparedRebind, PresentOutcome,
    RecoveredContext, RequestFailure,
};
use sonicterm_gpu::device_errors::{DeviceGate, DeviceStateWaker};
use sonicterm_gpu::recovery::{
    CommitDecision, CommitReport, Next, PollDecision, RecoveryCoordinator, RecoveryPhase,
    RecoveryPolicy, RequestDecision, RequestOutcome, StopDecision,
};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::WindowId;

use super::gpu_recovery_worker::{RequestError, RequestWorker};
use super::{App, UserEvent};

const RESULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const EXHAUSTED_POLL_INTERVAL: Duration = Duration::from_secs(1);

type DeviceRequestResult = Result<RecoveredContext, RequestFailure>;

/// Keep one committed context and one persistent request worker across window changes.
pub(super) struct GpuRecovery {
    context: GpuSharedContext,
    coordinator: RecoveryCoordinator,
    worker: RequestWorker<ContextRequest, DeviceRequestResult>,
    requested_window: Option<(u64, WindowId)>,
    result_poll_at: Option<Instant>,
    observed_gate: Option<DeviceGate>,
    disconnected: bool,
}

impl GpuRecovery {
    fn new(context: GpuSharedContext, proxy: Option<EventLoopProxy<UserEvent>>) -> Self {
        let notify = proxy.clone();
        let worker = RequestWorker::new(
            move |request: ContextRequest| {
                request.run(|generation| match proxy.clone() {
                    Some(proxy) => generation_waker(proxy, generation),
                    None => Arc::new(|| {}),
                })
            },
            move |ticket| {
                if let Some(proxy) = &notify {
                    // When: `notify` holds a proxy, post only a hint; the result stays owned by the channel.
                    let _ = proxy.send_event(UserEvent::GpuRecoveryReady { ticket });
                }
            },
        );
        Self {
            coordinator: RecoveryCoordinator::new(RecoveryPolicy::proposed(), context.generation()),
            context,
            worker,
            requested_window: None,
            result_poll_at: None,
            observed_gate: None,
            disconnected: false,
        }
    }

    /// The authoritative context also survives a requesting window's closure.
    pub(super) fn context(&self) -> GpuSharedContext {
        self.context.clone()
    }

    /// Start stability only after an acknowledged frame on the committed generation.
    pub(super) fn observe_frame(
        &mut self,
        generation: u64,
        outcome: &PresentOutcome,
        now: Instant,
    ) {
        observe_frame(&mut self.coordinator, generation, outcome, now);
    }

    fn arm_result_poll(&mut self, now: Instant) {
        self.result_poll_at = result_poll_deadline(
            self.result_poll_at,
            self.worker.in_flight().is_some() && !self.disconnected,
            self.coordinator.phase() == RecoveryPhase::Exhausted,
            now,
        );
    }
}

/// A late device callback names its original generation, never the current window.
pub(super) fn generation_waker(
    proxy: EventLoopProxy<UserEvent>,
    generation: u64,
) -> DeviceStateWaker {
    let proxy = std::sync::Mutex::new(proxy);
    Arc::new(move || {
        let Ok(proxy) = proxy.try_lock() else {
            // When: `try_lock` refuses, another callback already posts this generation's wake.
            return;
        };
        // A closed event loop has no recovery owner left to notify.
        let _ = proxy.send_event(UserEvent::GpuDeviceGenerationChanged { generation });
    })
}

fn result_poll_deadline(
    previous: Option<Instant>,
    in_flight: bool,
    exhausted: bool,
    now: Instant,
) -> Option<Instant> {
    if !in_flight {
        // When: `in_flight` is false, no native result remains to dispose.
        return None;
    }
    let interval = if exhausted { EXHAUSTED_POLL_INTERVAL } else { RESULT_POLL_INTERVAL };
    Some(previous.filter(|due| *due > now).unwrap_or(now + interval))
}

fn observe_frame(
    coordinator: &mut RecoveryCoordinator,
    generation: u64,
    outcome: &PresentOutcome,
    now: Instant,
) {
    if matches!(outcome, PresentOutcome::Presented) {
        coordinator.observe_presented(generation, now);
    }
}

fn deadline(phase: RecoveryPhase, result_poll: Option<Instant>) -> Option<Instant> {
    let recovery = match phase {
        RecoveryPhase::Scheduled { due, .. } => Some(due),
        RecoveryPhase::Requesting { deadline, .. } => Some(deadline),
        RecoveryPhase::Active | RecoveryPhase::Rebinding { .. } | RecoveryPhase::Exhausted => None,
    };
    match (recovery, result_poll) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

fn log_next(generation: u64, next: Next, now: Instant) {
    match next {
        Next::Retry { due, attempt } => {
            tracing::warn!(
                target: "sonic::gpu::recovery",
                generation,
                attempt,
                delay_ms = due.saturating_duration_since(now).as_millis(),
                "shared GPU recovery scheduled"
            );
        }
        Next::Exhausted => {
            tracing::error!(
                target: "sonic::gpu::recovery",
                generation,
                "shared GPU recovery exhausted; terminal sessions remain running"
            );
        }
    }
}

#[derive(Clone, Copy)]
enum RendererOwner {
    Window(WindowId),
    Warm(usize),
}

impl App {
    /// Retain one process context before later windows can install compatibility wakers.
    pub(super) fn initialize_gpu_recovery(&mut self, renderer: &GpuRenderer) {
        if self.gpu_recovery.is_none() {
            self.gpu_recovery =
                Some(GpuRecovery::new(renderer.shared_context(), self.event_loop_proxy.clone()));
        }
        if let Some(proxy) = self.event_loop_proxy.clone() {
            renderer.set_device_state_waker(generation_waker(proxy, renderer.device_generation()));
        }
    }

    /// Reject stale callback identities before inspecting or scheduling current work.
    pub(super) fn gpu_generation_changed(&mut self, el: &ActiveEventLoop, generation: u64) {
        if self
            .gpu_recovery
            .as_ref()
            .is_some_and(|recovery| recovery.coordinator.committed() == generation)
        {
            self.request_redraw_all_terminal_windows();
            self.service_gpu_recovery(el, Instant::now());
        }
    }

    /// A completion event is a hint; only the worker channel transfers its result.
    pub(super) fn gpu_recovery_ready(&mut self, el: &ActiveEventLoop, ticket: u64) {
        if self
            .gpu_recovery
            .as_ref()
            .is_some_and(|recovery| recovery.worker.in_flight() == Some(ticket))
        {
            self.service_gpu_recovery(el, Instant::now());
        }
    }

    /// Capture coordinator identity and counters without retaining native objects.
    pub(super) fn gpu_recovery_snapshot(
        &self,
    ) -> Option<sonicterm_gpu::recovery::RecoverySnapshot> {
        self.gpu_recovery.as_ref().map(|recovery| recovery.coordinator.snapshot())
    }

    /// Recovery timers never create a frame; even exhausted requests retain a disposal wake.
    pub(super) fn gpu_recovery_deadline(&self) -> Option<Instant> {
        if self.runtime_smoke.as_ref().is_some_and(|smoke| !smoke.recovery_enabled()) {
            // When: `runtime_smoke` owns containment evidence, its deliberate loss must remain stopped.
            return None;
        }
        let recovery = self.gpu_recovery.as_ref()?;
        deadline(recovery.coordinator.phase(), recovery.result_poll_at)
    }

    /// Drain actual completions and commit every renderer before returning to event dispatch.
    pub(super) fn service_gpu_recovery(&mut self, el: &ActiveEventLoop, now: Instant) {
        if self.runtime_smoke.as_ref().is_some_and(|smoke| !smoke.recovery_enabled()) {
            // When: `runtime_smoke` owns containment evidence, recovery would invalidate its stopped-device oracle.
            return;
        }
        let Some(mut recovery) = self.gpu_recovery.take() else {
            // When: `gpu_recovery` is absent, no renderer has established the process context yet.
            return;
        };
        let gate = recovery.context.gate();
        if recovery.observed_gate != Some(gate) {
            recovery.observed_gate = Some(gate);
            match recovery.coordinator.observe_stop(recovery.context.generation(), gate, now) {
                StopDecision::Lost(next) => log_next(recovery.context.generation(), next, now),
                StopDecision::DeferredUnusable => tracing::error!(
                    target: "sonic::gpu::recovery",
                    generation = recovery.context.generation(),
                    "GPU device unusable without loss; recovery is not applicable"
                ),
                StopDecision::NotStopped
                | StopDecision::Stale
                | StopDecision::Duplicate
                | StopDecision::Exhausted => {
                    // When: `observe_stop` schedules nothing, preserve its current recovery phase.
                }
            }
        }
        if !recovery.disconnected && recovery.worker.in_flight().is_some() {
            match recovery.worker.try_result() {
                Ok(Some((ticket, result))) => {
                    let requested_window = recovery
                        .requested_window
                        .take()
                        .and_then(|(pending, id)| (pending == ticket).then_some(id));
                    self.finish_gpu_request(
                        &mut recovery,
                        ticket,
                        requested_window,
                        result,
                        Instant::now(),
                    );
                }
                Ok(None) => {
                    // When: `try_result` is empty, keep the worker's admission and disposal timer intact.
                }
                Err(_) => {
                    // A disconnected persistent consumer is never replaced.
                    recovery.disconnected = true;
                    recovery.requested_window = None;
                    if let Some(ticket) = recovery.worker.in_flight() {
                        let decision = recovery.coordinator.request_finished(
                            ticket,
                            RequestOutcome::Failed,
                            now,
                        );
                        report_request_failure(recovery.context.generation(), decision, now);
                    }
                    tracing::error!(target: "sonic::gpu::recovery", "GPU recovery worker disconnected");
                }
            }
        }
        match recovery.coordinator.poll(Instant::now()) {
            PollDecision::StartRequest { ticket, attempt, .. } => {
                self.start_gpu_request(&mut recovery, el, ticket, attempt);
            }
            PollDecision::TimedOut { ticket, next } => {
                tracing::warn!(target: "sonic::gpu::recovery", ticket, "GPU recovery request timed out; worker retained");
                log_next(recovery.context.generation(), next, Instant::now());
            }
            PollDecision::WorkerBusy { overdue, next } => {
                tracing::warn!(target: "sonic::gpu::recovery", overdue, "GPU recovery retry refused while an earlier request runs");
                log_next(recovery.context.generation(), next, Instant::now());
            }
            PollDecision::Idle
            | PollDecision::WaitUntil(_)
            | PollDecision::AwaitingRebind { .. }
            | PollDecision::Exhausted => {
                // When: `poll` starts no work, preserve the coordinator and its remaining disposal deadline.
            }
        }
        recovery.arm_result_poll(Instant::now());
        self.gpu_recovery = Some(recovery);
    }

    fn start_gpu_request(
        &mut self,
        recovery: &mut GpuRecovery,
        el: &ActiveEventLoop,
        ticket: u64,
        attempt: usize,
    ) {
        let request = if recovery.disconnected {
            Err(anyhow::anyhow!("GPU recovery worker is unavailable"))
        } else {
            // When: `disconnected` is false, choose a surviving native surface without opening a per-window device.
            self.main_renderer()
                .or_else(|| {
                    self.windows
                        .iter()
                        .filter_map(|(id, window)| {
                            window.renderer.as_ref().map(|renderer| (id, renderer))
                        })
                        .min_by_key(|(id, _)| *id)
                        .map(|(_, renderer)| renderer)
                })
                .or_else(|| self.warm_window_pool.first().map(|warm| &warm.renderer))
                .ok_or_else(|| anyhow::anyhow!("no window remains for GPU recovery"))
                .and_then(|renderer| renderer.recovery_request(el))
        };
        let admitted = request.and_then(|request| {
            let id = request.window().id();
            match recovery.worker.try_request(ticket, request) {
                Ok(()) => {
                    recovery.requested_window = Some((ticket, id));
                    Ok(())
                }
                Err(RequestError::Busy { ticket, request }) => {
                    drop(request);
                    Err(anyhow::anyhow!("GPU recovery worker refused busy request {ticket}"))
                }
                Err(RequestError::Disconnected { ticket, request }) => {
                    recovery.disconnected = true;
                    drop(request);
                    Err(anyhow::anyhow!("GPU recovery worker disconnected before request {ticket}"))
                }
            }
        });
        if let Err(error) = admitted {
            // Refused request surfaces remain on the event-loop thread for disposal.
            tracing::warn!(target: "sonic::gpu::recovery", ticket, attempt, %error, "GPU recovery request refused");
            let now = Instant::now();
            let decision =
                recovery.coordinator.request_finished(ticket, RequestOutcome::Failed, now);
            report_request_failure(recovery.context.generation(), decision, now);
        } else {
            // When: `admitted` succeeded, the worker exclusively owns the request until its result returns.
            tracing::warn!(target: "sonic::gpu::recovery", ticket, attempt, "GPU recovery request started");
        }
    }

    fn finish_gpu_request(
        &mut self,
        recovery: &mut GpuRecovery,
        ticket: u64,
        requested_window: Option<WindowId>,
        result: DeviceRequestResult,
        now: Instant,
    ) {
        let candidate = match result {
            Ok(candidate) => Some(candidate),
            Err(failure) => {
                tracing::warn!(target: "sonic::gpu::recovery", ticket, error = %failure.error(), "GPU recovery negotiation failed");
                drop(failure);
                None
            }
        };
        let outcome = candidate.as_ref().map_or(RequestOutcome::Failed, |candidate| {
            RequestOutcome::Created { generation: candidate.generation() }
        });
        let decision = recovery.coordinator.request_finished(ticket, outcome, now);
        tracing::warn!(target: "sonic::gpu::recovery", ticket, ?outcome, ?decision, "GPU recovery request completed");
        match decision {
            RequestDecision::Rebind { .. } => {
                self.rebind_gpu_renderers(
                    recovery,
                    candidate.expect("a created context is required for rebind"),
                    requested_window,
                );
            }
            RequestDecision::Failed(next) => log_next(recovery.context.generation(), next, now),
            RequestDecision::TimedOut { retired, next } => {
                if let Some(retired) = retired {
                    candidate
                        .as_ref()
                        .expect("retired context exists")
                        .context()
                        .destroy_retired(retired)
                        .expect("retirement names the returned candidate");
                }
                log_next(recovery.context.generation(), next, now);
            }
            RequestDecision::Discard(retired) => {
                if let Some(retired) = retired {
                    candidate
                        .as_ref()
                        .expect("retired context exists")
                        .context()
                        .destroy_retired(retired)
                        .expect("retirement names the returned candidate");
                }
            }
        }
    }

    fn rebind_gpu_renderers(
        &mut self,
        recovery: &mut GpuRecovery,
        candidate: RecoveredContext,
        requested_window: Option<WindowId>,
    ) {
        let (context, surface) = candidate.into_parts();
        let mut surface = Some(surface);
        let mut windows: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(id, window)| window.renderer.as_ref().map(|_| *id))
            .collect();
        windows.sort_unstable();
        let live = windows.len() + self.warm_window_pool.len();
        let prepared = (|| -> anyhow::Result<Vec<(RendererOwner, PreparedRebind)>> {
            let mut prepared = Vec::with_capacity(live);
            for id in windows {
                let renderer = self.windows[&id]
                    .renderer
                    .as_ref()
                    .expect("renderer population is stable inside the callback");
                let owned_surface =
                    (requested_window == Some(id)).then(|| surface.take()).flatten();
                prepared.push((
                    RendererOwner::Window(id),
                    renderer.prepare_rebind(
                        &context,
                        owned_surface,
                        self.config.appearance.software_render_mode,
                    )?,
                ));
            }
            for (index, warm) in self.warm_window_pool.iter().enumerate() {
                let owned_surface =
                    (requested_window == Some(warm.window.id())).then(|| surface.take()).flatten();
                prepared.push((
                    RendererOwner::Warm(index),
                    warm.renderer.prepare_rebind(
                        &context,
                        owned_surface,
                        self.config.appearance.software_render_mode,
                    )?,
                ));
            }
            Ok(prepared)
        })();
        // A closed requesting window's unused surface is disposed on the event loop.
        drop(surface);
        let mut rebound = 0;
        match prepared {
            Ok(prepared) => {
                // When: `prepared` contains every renderer, commit them without returning to event dispatch.
                for (owner, prepared) in prepared {
                    let renderer = match owner {
                        RendererOwner::Window(id) => self
                            .windows
                            .get_mut(&id)
                            .and_then(|window| window.renderer.as_mut())
                            .expect("prepared renderer is still owned"),
                        RendererOwner::Warm(index) => &mut self.warm_window_pool[index].renderer,
                    };
                    match renderer.commit_rebind(prepared) {
                        Ok(gate) => {
                            // When: `commit_rebind` returned a gate, count this renderer before checking native configure errors.
                            rebound += 1;
                            if !gate.accepts_gpu_work() {
                                // When: `gate` closes during configure, stop committing and retire the entire candidate below.
                                break;
                            }
                        }
                        Err(error) => {
                            // When: `commit_rebind` refuses this renderer, retire the candidate before dispatch can resume.
                            tracing::warn!(target: "sonic::gpu::recovery", %error, "GPU recovery renderer commit failed");
                            break;
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(target: "sonic::gpu::recovery", %error, "GPU recovery renderer preparation failed")
            }
        }
        let report =
            CommitReport { candidate: context.generation(), rebound, live, gate: context.gate() };
        match recovery.coordinator.finish_rebind(report, Instant::now()) {
            CommitDecision::Committed { generation, retired } => {
                let old = std::mem::replace(&mut recovery.context, context);
                recovery.observed_gate = None;
                old.destroy_retired(retired)
                    .expect("retirement names the previous committed context");
                if let Some(renderer) = self
                    .main_renderer()
                    .or_else(|| self.windows.values().find_map(|window| window.renderer.as_ref()))
                    .or_else(|| self.warm_window_pool.first().map(|warm| &warm.renderer))
                {
                    self.software_render_degrade = super::should_degrade_for_software_render(
                        self.config.appearance.software_render_mode,
                        renderer.is_software_rendering(),
                    );
                    self.frame_period = super::software_render_frame_period(
                        self.software_render_degrade,
                        self.monitor_frame_period,
                    );
                }
                self.input_dirty = true;
                for window in self.windows.values_mut().filter(|window| window.renderer.is_some()) {
                    window.retry_not_before = None;
                    window.request_redraw();
                }
                tracing::warn!(target: "sonic::gpu::recovery", generation, rebound, "shared GPU recovery committed");
            }
            CommitDecision::Discarded { retired, next } => {
                // A partial rebind cannot escape this callback with its candidate gate open.
                context.destroy_retired(retired).expect("retirement names the rejected context");
                log_next(recovery.context.generation(), next, Instant::now());
            }
            CommitDecision::Unexpected => unreachable!("only the pending candidate reaches rebind"),
        }
    }
}

fn report_request_failure(generation: u64, decision: RequestDecision, now: Instant) {
    match decision {
        RequestDecision::Failed(next) | RequestDecision::TimedOut { next, .. } => {
            log_next(generation, next, now);
        }
        RequestDecision::Discard(None) => {
            // When: `decision` is `Discard(None)`, no native candidate needs retirement.
        }
        RequestDecision::Rebind { .. } | RequestDecision::Discard(Some(_)) => {
            unreachable!("a failed request creates no candidate")
        }
    }
}

#[cfg(test)]
#[path = "gpu_recovery_tests.rs"]
mod gpu_recovery_tests;
