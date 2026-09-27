use super::*;
use crate::ProcessPrivilege;
use sonicterm_cfg::config::BackdropKind;

fn opaque_normalizer(mut config: Config) -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    if config.appearance.backdrop != BackdropKind::Opaque {
        warnings.push("unsupported backdrop".to_string());
        config.appearance.backdrop = BackdropKind::Opaque;
    }
    (config, warnings)
}

fn machine() -> AppStateMachine {
    AppStateMachine::new(sonicterm_app_core::AppState::default())
}

#[test]
fn every_platform_shell_defaults_to_unprivileged() {
    // Protect callers that do not install a native privilege observation from showing a false warning.
    let mac = MacShell::new(machine(), Theme::default(), Config::default(), Keymap::default());
    let windows =
        WindowsShell::new(machine(), Theme::default(), Config::default(), Keymap::default());
    let linux = LinuxShell::new(machine(), Theme::default(), Config::default(), Keymap::default());

    assert_eq!(mac.runner.process_privilege, ProcessPrivilege::Unprivileged);
    assert_eq!(windows.runner.process_privilege, ProcessPrivilege::Unprivileged);
    assert_eq!(linux.runner.process_privilege, ProcessPrivilege::Unprivileged);
}

#[test]
fn every_platform_shell_accepts_the_same_process_privilege_value() {
    // Protect macOS, Windows, and Linux from diverging at their otherwise-thin startup wrappers.
    let mac = MacShell::new(machine(), Theme::default(), Config::default(), Keymap::default())
        .with_process_privilege(ProcessPrivilege::Privileged);
    let windows =
        WindowsShell::new(machine(), Theme::default(), Config::default(), Keymap::default())
            .with_process_privilege(ProcessPrivilege::Privileged);
    let linux = LinuxShell::new(machine(), Theme::default(), Config::default(), Keymap::default())
        .with_process_privilege(ProcessPrivilege::Privileged);

    assert_eq!(mac.runner.process_privilege, ProcessPrivilege::Privileged);
    assert_eq!(windows.runner.process_privilege, ProcessPrivilege::Privileged);
    assert_eq!(linux.runner.process_privilege, ProcessPrivilege::Privileged);
}

/// Platform shells default to identity normalization until their binary installs policy.
#[test]
fn mac_and_windows_shells_preserve_supported_backdrops() {
    for backdrop in [BackdropKind::Mica, BackdropKind::Acrylic, BackdropKind::Tabbed] {
        let mut config = Config::default();
        config.appearance.backdrop = backdrop;
        let mac = MacShell::new(machine(), Theme::default(), config.clone(), Keymap::default())
            .runner
            .into_app_with_proxy(None);
        let windows = WindowsShell::new(machine(), Theme::default(), config, Keymap::default())
            .runner
            .into_app_with_proxy(None);
        assert_eq!(mac.config.appearance.backdrop, backdrop);
        assert_eq!(windows.config.appearance.backdrop, backdrop);
    }
}

/// The shell-installed normalizer runs before startup config enters App state.
#[test]
fn shell_runner_stores_normalized_startup_config() {
    let mut config = Config::default();
    config.appearance.backdrop = BackdropKind::Mica;
    let app = LinuxShell::new(machine(), Theme::default(), config, Keymap::default())
        .with_config_normalizer(Box::new(opaque_normalizer))
        .runner
        .into_app_with_proxy(None);

    assert_eq!(app.config.appearance.backdrop, BackdropKind::Opaque);
}

#[test]
fn shell_installs_privilege_before_a_pending_startup_tab() {
    // Protect startup payloads and later torn-out windows from seeing different process-level state.
    const SOURCE: &str = include_str!("shell.rs");
    let install = SOURCE
        .find("app.set_process_privilege(self.process_privilege)")
        .expect("ShellRunner must install its process privilege on App");
    let pending = SOURCE
        .find("if let Some(payload) = self.pending")
        .expect("ShellRunner must retain startup payload support");

    assert!(install < pending);
}

#[test]
fn runtime_smoke_spec_keeps_platform_command_and_state_paths_explicit() {
    // Protect platform smokes from substituting a hard-coded Unix shell or user-home state.
    let spec = RuntimeSmokeSpec::new(
        "cmd.exe",
        "__SONICTERM_SMOKE_41__",
        b"set N=41\r\necho __SONICTERM_SMOKE_%N%__\r\n".to_vec(),
        std::path::PathBuf::from("C:/scratch/config"),
        std::path::PathBuf::from("C:/scratch/logs"),
    )
    .expect("valid smoke specification");

    assert_eq!(spec.shell_program(), "cmd.exe");
    assert_eq!(spec.marker(), "__SONICTERM_SMOKE_41__");
    assert_eq!(spec.config_dir(), std::path::Path::new("C:/scratch/config"));
    assert_eq!(spec.log_dir(), std::path::Path::new("C:/scratch/logs"));
    assert!(!String::from_utf8_lossy(spec.command()).contains(spec.marker()));
}

#[test]
fn every_platform_shell_exposes_the_same_bounded_smoke_api() {
    // Every platform must return both its smoke outcome and the native teardown disposition.
    type RunSmoke = fn(MacShell, RuntimeSmokeSpec, Duration) -> ShellRunResult<RuntimeSmokeFailure>;
    let mac: RunSmoke = MacShell::run_smoke;
    let _ = mac;

    let windows: fn(
        WindowsShell,
        RuntimeSmokeSpec,
        Duration,
    ) -> ShellRunResult<RuntimeSmokeFailure> = WindowsShell::run_smoke;
    let linux: fn(LinuxShell, RuntimeSmokeSpec, Duration) -> ShellRunResult<RuntimeSmokeFailure> =
        LinuxShell::run_smoke;
    let _ = (windows, linux);
}

#[test]
fn runtime_smoke_uses_clean_shell_startup_without_replacing_home() {
    // Protect main and adopted child PTYs from profile hooks while preserving their real user home.
    const MAIN: &str = include_str!("app/spawn_pane.rs");
    const CHILD: &str = include_str!("app/child_window.rs");
    assert!(MAIN.contains("shell_opts.clean_e2e = self.runtime_smoke.is_some()"));
    assert!(CHILD.contains("clean_e2e: self.runtime_smoke.is_some()"));
    assert!(!MAIN.contains("set_var(\"HOME\""));
    assert!(!CHILD.contains("set_var(\"HOME\""));
}

#[test]
fn runtime_smoke_checks_cleanup_after_every_post_app_failure() {
    // Watchdog and loop failures still settle PTYs before App drop and the renderer-baseline check.
    const SOURCE: &str = include_str!("shell.rs");
    let start = SOURCE.find("fn run_smoke(").expect("shared smoke runner");
    let body = &SOURCE[start..SOURCE.find("/// macOS shell").expect("runner impl end")];
    let app = body.find("let mut app =").expect("App construction");
    let settle = body.find("app.finish_session()").expect("common PTY settlement");
    let drop_app = body.find("drop(app)").expect("common App drop");
    let baseline = body[drop_app..]
        .find("live_renderer_count() != renderer_baseline")
        .expect("post-drop baseline check");
    assert!(!body[app..drop_app].contains('?'));
    assert!(app < settle && settle < drop_app);
    assert!(baseline > 0);
}

#[test]
fn interactive_loop_errors_still_reach_native_settlement() {
    // The interactive runner must preserve an event-loop error without returning before PTY settlement.
    let source = include_str!("shell.rs");
    let start = source.find("    fn run(self)").expect("shared interactive runner");
    let end = source[start..].find("    fn run_smoke(").expect("next runner method") + start;
    let body = &source[start..end];
    let run = body.find("event_loop.run_app(&mut app)").expect("event-loop call");
    let settle = body.find("app.finish_session()").expect("native settlement call");
    assert!(run < settle);
    assert!(!body[run..settle].contains('?'));
}

struct ExitEvidence {
    root: std::path::PathBuf,
    marker: std::path::PathBuf,
    breadcrumbs: std::path::PathBuf,
    session: Option<sonicterm_logging::session_state::ArmedSession>,
    writer: Option<sonicterm_logging::breadcrumbs::BreadcrumbWriter>,
}

impl ExitEvidence {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join(format!("sonicterm-shell-exit-{}-{serial}", std::process::id()));
        std::fs::create_dir(&root).expect("create unique exit evidence directory");
        let session = sonicterm_logging::session_state::arm(&root, "1.2.3").expect("arm session");
        let marker = session.path().to_path_buf();
        let breadcrumbs = sonicterm_logging::breadcrumbs::breadcrumb_path(&root, session.id())
            .expect("breadcrumb path");
        let writer = sonicterm_logging::breadcrumbs::BreadcrumbWriter::start(
            &root,
            session.id(),
            sonicterm_logging::breadcrumbs::BreadcrumbLimits::default(),
        )
        .expect("start breadcrumb writer");
        Self { root, marker, breadcrumbs, session: Some(session), writer: Some(writer) }
    }

    fn finish(&mut self, clean: bool) {
        finish_session_diagnostics(clean, self.writer.take(), self.session.take());
    }

    fn assert_clean(&self, expected: bool) {
        assert_eq!(!self.marker.exists(), expected, "session marker disposition");
        let text = std::fs::read_to_string(&self.breadcrumbs).expect("flushed breadcrumbs");
        assert_eq!(text.contains("lifecycle=clean_shutdown"), expected, "{text}");
    }
}

// Lifecycle: ExitEvidence stops its owned writer before removing only its unique scratch directory.
impl Drop for ExitEvidence {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            let _ = writer.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn unsettled_interactive_exit_preserves_result_and_unclean_evidence() {
    // Native teardown failure must not turn a successful loop result into a clean-session claim.
    let outcome = ShellRunResult::<anyhow::Error> { result: Ok(()), teardown_settled: false };
    let mut evidence = ExitEvidence::new();
    evidence.finish(outcome.is_clean(ExitMode::Interactive));
    assert!(outcome.result.is_ok());
    evidence.assert_clean(false);
}

#[test]
fn interactive_error_keeps_unclean_evidence_after_settled_teardown() {
    // Settled native resources do not erase the original interactive event-loop error.
    let outcome = ShellRunResult {
        result: Err(anyhow::anyhow!("event loop failed")),
        teardown_settled: true,
    };
    let mut evidence = ExitEvidence::new();
    evidence.finish(outcome.is_clean(ExitMode::Interactive));
    assert_eq!(outcome.result.unwrap_err().to_string(), "event loop failed");
    evidence.assert_clean(false);
}

#[test]
fn settled_interactive_exit_flushes_clean_evidence() {
    // A successful loop with settled native resources records and flushes clean shutdown.
    let outcome = ShellRunResult::<anyhow::Error> { result: Ok(()), teardown_settled: true };
    let mut evidence = ExitEvidence::new();
    evidence.finish(outcome.is_clean(ExitMode::Interactive));
    evidence.assert_clean(true);
}

#[test]
fn unsettled_smoke_reports_native_teardown_without_marking_clean() {
    // Teardown alone fails the smoke with its dedicated code and leaves the armed marker intact.
    let outcome = ShellRunResult::smoke(Ok(()), false);
    let mut evidence = ExitEvidence::new();
    evidence.finish(outcome.is_clean(ExitMode::RuntimeSmoke));
    assert_eq!(outcome.result, Err(RuntimeSmokeFailure::NativeTeardown));
    assert_eq!(outcome.result.unwrap_err().exit_code(), 20);
    evidence.assert_clean(false);
}

#[test]
fn earlier_smoke_failure_survives_unsettled_teardown() {
    // Native cleanup cannot replace the first observed smoke boundary.
    for failure in [
        RuntimeSmokeFailure::EventLoop,
        RuntimeSmokeFailure::Display,
        RuntimeSmokeFailure::Gpu,
        RuntimeSmokeFailure::Pty,
        RuntimeSmokeFailure::Marker,
        RuntimeSmokeFailure::Present,
        RuntimeSmokeFailure::WarmLifecycle,
        RuntimeSmokeFailure::GpuFaultContainment,
        RuntimeSmokeFailure::GpuDeviceLoss,
    ] {
        let outcome = ShellRunResult::smoke(Err(failure), false);
        assert_eq!(outcome.result, Err(failure));
        assert!(!outcome.is_clean(ExitMode::RuntimeSmoke));
    }
}

#[test]
fn settled_smoke_flushes_clean_evidence_for_either_result() {
    // Smoke failure classification is independent of whether all native teardown settled.
    for result in [
        Ok(()),
        Err(RuntimeSmokeFailure::Marker),
        Err(RuntimeSmokeFailure::GpuFaultContainment),
        Err(RuntimeSmokeFailure::GpuDeviceLoss),
    ] {
        let outcome = ShellRunResult::smoke(result, true);
        let mut evidence = ExitEvidence::new();
        evidence.finish(outcome.is_clean(ExitMode::RuntimeSmoke));
        assert_eq!(outcome.result, result);
        evidence.assert_clean(true);
    }
}

#[test]
fn platform_entries_share_the_settled_exit_policy() {
    // Platform-gated entry points must not recreate a clean marker independently of PTY settlement.
    for source in [
        include_str!("../../sonicterm-mac/src/main.rs"),
        include_str!("../../sonicterm-windows/src/main.rs"),
        include_str!("../../sonicterm-linux/src/main.rs"),
    ] {
        assert!(source.contains("sonicterm_app::shell::finish_session_diagnostics("));
        assert!(source.contains("ExitMode::Interactive"));
        assert!(source.contains("ExitMode::RuntimeSmoke"));
        assert!(!source.contains("session.mark_clean()"));
        assert!(!source.contains("LifecycleEvent::CleanShutdown"));
    }
}

#[test]
fn runtime_smoke_spec_rejects_echoable_markers_and_ambiguous_paths() {
    // Protect marker proof from terminal echo and state isolation from one shared directory.
    assert!(RuntimeSmokeSpec::new(
        "/bin/sh",
        "complete-marker",
        b"printf complete-marker".to_vec(),
        std::path::PathBuf::from("/scratch/config"),
        std::path::PathBuf::from("/scratch/logs"),
    )
    .is_err());
    assert!(RuntimeSmokeSpec::new(
        "/bin/sh",
        "complete-marker",
        b"printf %s marker".to_vec(),
        std::path::PathBuf::from("/scratch/state"),
        std::path::PathBuf::from("/scratch/state"),
    )
    .is_err());
}
