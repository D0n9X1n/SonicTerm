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
    ThemeLoader, UserEvent,
};
use crate::os_drag::{OsDragSink, TabPayload};
use crate::ProcessPrivilege;
use sonicterm_app_core::AppStateMachine;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

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

    fn run(self) -> Result<()> {
        crate::app::init_tracing_public();
        let event_loop =
            EventLoop::<UserEvent>::with_user_event().build().context("create event loop")?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        Self::install_bridges(&proxy);
        let mut app = self.into_app(proxy);

        event_loop.run_app(&mut app).context("run event loop")?;
        Ok(())
    }

    fn run_smoke(mut self, timeout: Duration) -> Result<(), RuntimeSmokeFailure> {
        self.config.terminal.shell = Some("/bin/sh".to_string());
        self.config.window.warm_window_pool = 0;
        crate::app::init_tracing_public();
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .map_err(|_| RuntimeSmokeFailure::EventLoop)?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        Self::install_bridges(&proxy);
        let mut app = self.into_app(proxy.clone());
        app.install_runtime_smoke(std::process::id());

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
            })
            .map_err(|_| RuntimeSmokeFailure::EventLoop)?;

        let run_result = event_loop.run_app(&mut app);
        let _ = cancel_tx.send(());
        let _ = watchdog.join();
        run_result.map_err(|_| RuntimeSmokeFailure::EventLoop)?;
        app.runtime_smoke_result()
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

    /// Run the application until the event loop exits.
    pub fn run(self) -> Result<()> {
        self.runner.run()
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

    /// Run the application until the event loop exits.
    pub fn run(self) -> Result<()> {
        self.runner.run()
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

    /// Run the application until the event loop exits.
    pub fn run(self) -> Result<()> {
        self.runner.run()
    }

    /// Run the bounded Linux package smoke through display, GPU, PTY, grid, and presentation.
    pub fn run_smoke(self, timeout: Duration) -> Result<(), RuntimeSmokeFailure> {
        self.runner.run_smoke(timeout)
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod shell_tests;
