//! Platform shells that drive the winit event loop on top of
//! [`sonicterm_app_core::AppStateMachine`].
//!
//! [`MacShell`], [`WindowsShell`], and [`LinuxShell`] receive an externally
//! constructed state machine and delegate shared event-loop setup to one
//! platform-neutral runner. Platform wrappers expose only the hooks supported
//! by their native binary.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use winit::event_loop::{ControlFlow, EventLoop, EventLoopProxy};

use crate::app::os_drag::OsTabDragBackend;
use crate::app::{
    identity_config_normalizer, App, ConfigNormalizer, KeymapLoader, RuntimeSmokeFailure,
    RuntimeSmokeSpec, ThemeLoader, UserEvent,
};
use crate::os_drag::{OsDragSink, TabPayload};
use crate::ProcessPrivilege;
use sonicterm_app_core::AppStateMachine;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

/// Original shell outcome paired with the independently observed native teardown disposition.
#[derive(Debug)]
#[must_use]
pub struct ShellRunResult<E> {
    /// Event-loop or smoke result, preserving the first failure boundary.
    pub result: Result<(), E>,
    /// Whether every owned native teardown settled before the shell returned.
    pub teardown_settled: bool,
}

/// Session-marker policy for an interactive shell or an isolated runtime smoke.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitMode {
    /// Only a successful event loop and settled teardown permit a clean marker.
    Interactive,
    /// A smoke failure still permits a clean marker when native teardown settled.
    RuntimeSmoke,
}

impl<E> ShellRunResult<E> {
    /// Decide whether this outcome permits clean-session evidence without changing its result.
    pub fn is_clean(&self, mode: ExitMode) -> bool {
        self.teardown_settled && (mode == ExitMode::RuntimeSmoke || self.result.is_ok())
    }
}

impl ShellRunResult<RuntimeSmokeFailure> {
    fn smoke(result: Result<(), RuntimeSmokeFailure>, teardown_settled: bool) -> Self {
        let result = result.and({
            if teardown_settled {
                Ok(())
            } else {
                // When: teardown_settled is false, an otherwise successful smoke must report incomplete native cleanup.
                Err(RuntimeSmokeFailure::NativeTeardown)
            }
        });
        Self { result, teardown_settled }
    }
}

/// Flush shutdown diagnostics and mark a session clean only when the shell's exit policy permits it.
pub fn finish_session_diagnostics(
    clean: bool,
    writer: Option<sonicterm_logging::breadcrumbs::BreadcrumbWriter>,
    session: Option<sonicterm_logging::session_state::ArmedSession>,
) {
    if clean {
        // When: clean is true, settlement permits a lifecycle record before the writer's final flush.
        if let Some(writer) = writer.as_ref() {
            // When: writer exists, record clean shutdown best effort without depending on a retained recorder clone.
            let _ = writer.recorder().record(
                sonicterm_logging::breadcrumbs::BreadcrumbEvent::Lifecycle(
                    sonicterm_logging::breadcrumbs::LifecycleEvent::CleanShutdown,
                ),
            );
        }
    }
    if let Some(writer) = writer {
        // Flush accepted diagnostics even when native teardown prevents a clean marker.
        if let Err(error) = writer.shutdown() {
            tracing::warn!(%error, "could not flush shutdown breadcrumbs");
        }
    }
    if clean {
        if let Some(session) = session {
            if let Err(error) = session.mark_clean() {
                tracing::warn!(%error, "could not mark the settled session clean");
            }
        }
    }
}

struct ShellRunner {
    machine: AppStateMachine,
    theme: Theme,
    config: Config,
    keymap: Keymap,
    config_normalizer: ConfigNormalizer,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
    os_drag_sink: Option<Arc<dyn OsDragSink>>,
    os_drag_backend: Option<Box<dyn OsTabDragBackend>>,
    process_privilege: ProcessPrivilege,
    pending: Option<TabPayload>,
    breadcrumb_recorder: Option<sonicterm_logging::breadcrumbs::BreadcrumbRecorder>,
    on_resumed: Option<Box<dyn FnOnce() + Send>>,
    on_window_ready: Option<Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>>,
}

impl ShellRunner {
    fn new(machine: AppStateMachine, theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self {
            machine,
            theme,
            config,
            keymap,
            config_normalizer: identity_config_normalizer(),
            theme_loader: None,
            keymap_loader: None,
            os_drag_sink: None,
            os_drag_backend: None,
            process_privilege: ProcessPrivilege::default(),
            pending: None,
            breadcrumb_recorder: None,
            on_resumed: None,
            on_window_ready: None,
        }
    }

    fn install_bridges(proxy: &EventLoopProxy<UserEvent>) {
        crate::menubar_bridge::install_proxy(proxy.clone());
        crate::os_drag_bridge::install_proxy(proxy.clone());
        crate::open_script_bridge::install_proxy(proxy.clone());
    }

    fn into_app(self, proxy: EventLoopProxy<UserEvent>) -> App {
        self.into_app_with_proxy(Some(proxy))
    }

    fn into_app_with_proxy(self, proxy: Option<EventLoopProxy<UserEvent>>) -> App {
        let mut app = App::new_with_proxy_machine_and_normalizer(
            self.theme,
            self.config,
            self.keymap,
            proxy,
            self.machine,
            self.config_normalizer,
        );
        app.set_process_privilege(self.process_privilege);
        if let Some(recorder) = self.breadcrumb_recorder {
            app.set_breadcrumb_recorder(recorder);
        }
        app.theme_loader = self.theme_loader;
        app.keymap_loader = self.keymap_loader;
        if let Some(sink) = self.os_drag_sink {
            app.os_drag_sink = Some(sink);
        }
        if let Some(backend) = self.os_drag_backend {
            app.set_os_drag_backend(backend);
        }
        if let Some(hook) = self.on_resumed {
            app.on_resumed = Some(hook);
        }
        if let Some(hook) = self.on_window_ready {
            app.set_on_window_ready(hook);
        }
        if let Some(payload) = self.pending {
            // When: `pending` carries a startup handoff, seed its tab before the event loop starts.
            let _ = app.new_tab_from_payload(&payload);
        }
        app
    }

    fn run(self) -> ShellRunResult<anyhow::Error> {
        crate::app::init_tracing_public();
        let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
            Ok(event_loop) => event_loop,
            Err(error) => {
                // When: build returns Err(error), preserve it; no App or native PTY custody exists to drain.
                return ShellRunResult {
                    result: Err(error).context("create event loop"),
                    teardown_settled: true,
                };
            }
        };
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        Self::install_bridges(&proxy);
        let mut app = self.into_app(proxy);

        let result = event_loop.run_app(&mut app).context("run event loop");
        let teardown_settled = app.finish_session();
        ShellRunResult { result, teardown_settled }
    }

    fn run_smoke(
        mut self,
        spec: RuntimeSmokeSpec,
        timeout: Duration,
    ) -> ShellRunResult<RuntimeSmokeFailure> {
        let scenario = match spec.selected_scenario() {
            Ok(scenario) => scenario,
            Err(failure) => {
                // When: selected_scenario fails, no App or native PTY custody exists to retire.
                return ShellRunResult::smoke(Err(failure), true);
            }
        };
        self.config.terminal.shell = Some(spec.shell_program().to_string());
        crate::app::init_tracing_public();
        let renderer_baseline = sonicterm_gpu::core::live_renderer_count();
        let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
            Ok(event_loop) => event_loop,
            Err(_) => {
                // When: build returns Err(_), preserve the event-loop boundary without claiming uncreated PTYs.
                return ShellRunResult::smoke(Err(RuntimeSmokeFailure::EventLoop), true);
            }
        };
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        Self::install_bridges(&proxy);
        let mut app = self.into_app(proxy.clone());
        app.install_runtime_smoke(&spec, renderer_baseline, scenario);

        let (cancel_tx, cancel_rx) = std::sync::mpsc::sync_channel(1);
        let watchdog = std::thread::Builder::new()
            .name("sonicterm-runtime-smoke-watchdog".to_string())
            .spawn(move || {
                if matches!(
                    cancel_rx.recv_timeout(timeout),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    // When: `matches!(cancel_rx.recv_timeout(timeout), Err(RecvTimeoutError::Timeout))` is true, classify the active boundary.
                    let _ = proxy.send_event(UserEvent::RuntimeSmokeTimeout);
                }
            });

        let result = match watchdog {
            Ok(watchdog) => {
                // When: `watchdog` started, run the event loop and join the bounded monitor before teardown.
                let run_result = event_loop.run_app(&mut app);
                let _ = cancel_tx.send(());
                let _ = watchdog.join();
                if run_result.is_err() {
                    // Classify an event-loop error before the shared teardown and renderer-baseline check.
                    Err(RuntimeSmokeFailure::EventLoop)
                } else {
                    // When: `run_result` succeeded, preserve the app's more specific smoke outcome.
                    app.runtime_smoke_result()
                }
            }
            Err(_) => Err(RuntimeSmokeFailure::EventLoop),
        };
        let teardown_settled = app.finish_session();
        drop(app);
        let result = if sonicterm_gpu::core::live_renderer_count() != renderer_baseline {
            // App drop must restore the renderer baseline without overwriting an earlier smoke failure.
            result.and(Err(RuntimeSmokeFailure::WarmLifecycle))
        } else {
            // When: live_renderer_count matches renderer_baseline, retain the smoke's existing disposition.
            result
        };
        ShellRunResult::smoke(result, teardown_settled)
    }
}

/// macOS shell around the shared application runner.
pub struct MacShell {
    runner: ShellRunner,
}

impl MacShell {
    /// Build a shell around the caller-constructed state machine.
    #[must_use]
    pub fn new(machine: AppStateMachine, theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self { runner: ShellRunner::new(machine, theme, config, keymap) }
    }

    /// Install the process privilege observed by the macOS startup boundary.
    #[must_use]
    pub fn with_process_privilege(mut self, privilege: ProcessPrivilege) -> Self {
        self.runner.process_privilege = privilege;
        self
    }

    /// Install loaders used by live theme and keymap reload.
    #[must_use]
    pub fn with_asset_loaders(
        mut self,
        theme_loader: ThemeLoader,
        keymap_loader: KeymapLoader,
    ) -> Self {
        self.runner.theme_loader = Some(theme_loader);
        self.runner.keymap_loader = Some(keymap_loader);
        self
    }

    /// Install the macOS pasteboard drag sink.
    #[must_use]
    pub fn with_os_drag_sink(mut self, sink: Arc<dyn OsDragSink>) -> Self {
        self.runner.os_drag_sink = Some(sink);
        self
    }

    /// Install the macOS drag-session backend.
    #[must_use]
    pub fn with_os_drag_backend(mut self, backend: Box<dyn OsTabDragBackend>) -> Self {
        self.runner.os_drag_backend = Some(backend);
        self
    }

    /// Seed a tab payload received before startup.
    #[must_use]
    pub fn with_pending_payload(mut self, pending: TabPayload) -> Self {
        self.runner.pending = Some(pending);
        self
    }

    /// Install the nonblocking postmortem breadcrumb recorder.
    #[must_use]
    pub fn with_breadcrumb_recorder(
        mut self,
        recorder: sonicterm_logging::breadcrumbs::BreadcrumbRecorder,
    ) -> Self {
        self.runner.breadcrumb_recorder = Some(recorder);
        self
    }

    /// Install the one-shot hook run on the first resumed event.
    #[must_use]
    pub fn with_on_resumed(mut self, hook: Box<dyn FnOnce() + Send>) -> Self {
        self.runner.on_resumed = Some(hook);
        self
    }

    /// Install the one-shot hook run after the first native window is created.
    #[must_use]
    pub fn with_on_window_ready(
        mut self,
        hook: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>,
    ) -> Self {
        self.runner.on_window_ready = Some(hook);
        self
    }

    /// Run the event loop and report its result separately from native teardown settlement.
    pub fn run(self) -> ShellRunResult<anyhow::Error> {
        self.runner.run()
    }

    /// Run the bounded macOS smoke through window, renderer, PTY, presentation, and warm lifecycle.
    pub fn run_smoke(
        self,
        spec: RuntimeSmokeSpec,
        timeout: Duration,
    ) -> ShellRunResult<RuntimeSmokeFailure> {
        self.runner.run_smoke(spec, timeout)
    }
}

/// Windows shell around the shared application runner.
pub struct WindowsShell {
    runner: ShellRunner,
}

impl WindowsShell {
    /// Build a shell around the caller-constructed state machine.
    #[must_use]
    pub fn new(machine: AppStateMachine, theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self { runner: ShellRunner::new(machine, theme, config, keymap) }
    }

    /// Install the process privilege observed by the Windows startup boundary.
    #[must_use]
    pub fn with_process_privilege(mut self, privilege: ProcessPrivilege) -> Self {
        self.runner.process_privilege = privilege;
        self
    }

    /// Install loaders used by live theme and keymap reload.
    #[must_use]
    pub fn with_asset_loaders(
        mut self,
        theme_loader: ThemeLoader,
        keymap_loader: KeymapLoader,
    ) -> Self {
        self.runner.theme_loader = Some(theme_loader);
        self.runner.keymap_loader = Some(keymap_loader);
        self
    }

    /// Install the Windows OLE drag sink.
    #[must_use]
    pub fn with_os_drag_sink(mut self, sink: Arc<dyn OsDragSink>) -> Self {
        self.runner.os_drag_sink = Some(sink);
        self
    }

    /// Install the Windows OLE drag-session backend.
    #[must_use]
    pub fn with_os_drag_backend(mut self, backend: Box<dyn OsTabDragBackend>) -> Self {
        self.runner.os_drag_backend = Some(backend);
        self
    }

    /// Seed a tab payload received before startup.
    #[must_use]
    pub fn with_pending_payload(mut self, pending: TabPayload) -> Self {
        self.runner.pending = Some(pending);
        self
    }

    /// Install the nonblocking postmortem breadcrumb recorder.
    #[must_use]
    pub fn with_breadcrumb_recorder(
        mut self,
        recorder: sonicterm_logging::breadcrumbs::BreadcrumbRecorder,
    ) -> Self {
        self.runner.breadcrumb_recorder = Some(recorder);
        self
    }

    /// Install the one-shot hook run after the first native window is created.
    #[must_use]
    pub fn with_on_window_ready(
        mut self,
        hook: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>,
    ) -> Self {
        self.runner.on_window_ready = Some(hook);
        self
    }

    /// Run the event loop and report its result separately from native teardown settlement.
    pub fn run(self) -> ShellRunResult<anyhow::Error> {
        self.runner.run()
    }

    /// Run the bounded Windows smoke through window, renderer, PTY, presentation, and warm lifecycle.
    pub fn run_smoke(
        self,
        spec: RuntimeSmokeSpec,
        timeout: Duration,
    ) -> ShellRunResult<RuntimeSmokeFailure> {
        self.runner.run_smoke(spec, timeout)
    }
}

/// Linux shell around the shared application runner.
pub struct LinuxShell {
    runner: ShellRunner,
}

impl LinuxShell {
    /// Build a shell around the caller-constructed state machine.
    #[must_use]
    pub fn new(machine: AppStateMachine, theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self { runner: ShellRunner::new(machine, theme, config, keymap) }
    }

    /// Install the process privilege observed by the Linux startup boundary.
    #[must_use]
    pub fn with_process_privilege(mut self, privilege: ProcessPrivilege) -> Self {
        self.runner.process_privilege = privilege;
        self
    }

    /// Install loaders used by live theme and keymap reload.
    #[must_use]
    pub fn with_asset_loaders(
        mut self,
        theme_loader: ThemeLoader,
        keymap_loader: KeymapLoader,
    ) -> Self {
        self.runner.theme_loader = Some(theme_loader);
        self.runner.keymap_loader = Some(keymap_loader);
        self
    }

    /// Install the Linux capability policy used by startup and reload.
    #[must_use]
    pub fn with_config_normalizer(mut self, normalizer: ConfigNormalizer) -> Self {
        self.runner.config_normalizer = normalizer;
        self
    }

    /// Build a headless app for startup-policy regression tests.
    #[cfg(test)]
    pub(crate) fn into_app_for_test(self) -> App {
        self.runner.into_app_with_proxy(None)
    }

    /// Install the nonblocking postmortem breadcrumb recorder.
    #[must_use]
    pub fn with_breadcrumb_recorder(
        mut self,
        recorder: sonicterm_logging::breadcrumbs::BreadcrumbRecorder,
    ) -> Self {
        self.runner.breadcrumb_recorder = Some(recorder);
        self
    }

    /// Install the one-shot hook run after the first native window is created.
    #[must_use]
    pub fn with_on_window_ready(
        mut self,
        hook: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>,
    ) -> Self {
        self.runner.on_window_ready = Some(hook);
        self
    }

    /// Run the event loop and report its result separately from native teardown settlement.
    pub fn run(self) -> ShellRunResult<anyhow::Error> {
        self.runner.run()
    }

    /// Run the bounded Linux package smoke through window, renderer, PTY, presentation, and warm lifecycle.
    pub fn run_smoke(
        self,
        spec: RuntimeSmokeSpec,
        timeout: Duration,
    ) -> ShellRunResult<RuntimeSmokeFailure> {
        self.runner.run_smoke(spec, timeout)
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod shell_tests;
