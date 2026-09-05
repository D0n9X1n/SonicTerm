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
    // Protect macOS and Windows from silently losing the shared native-runtime gate.
    type RunSmoke =
        fn(MacShell, RuntimeSmokeSpec, Duration) -> std::result::Result<(), RuntimeSmokeFailure>;
    let mac: RunSmoke = MacShell::run_smoke;
    let _ = mac;

    let windows: fn(
        WindowsShell,
        RuntimeSmokeSpec,
        Duration,
    ) -> std::result::Result<(), RuntimeSmokeFailure> = WindowsShell::run_smoke;
    let linux: fn(
        LinuxShell,
        RuntimeSmokeSpec,
        Duration,
    ) -> std::result::Result<(), RuntimeSmokeFailure> = LinuxShell::run_smoke;
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
    // Protect watchdog and event-loop errors from bypassing App drop and renderer-baseline verification.
    const SOURCE: &str = include_str!("shell.rs");
    let start = SOURCE.find("fn run_smoke(").expect("shared smoke runner");
    let body = &SOURCE[start..SOURCE.find("/// macOS shell").expect("runner impl end")];
    let app = body.find("let mut app =").expect("App construction");
    let drop_app = body.find("drop(app)").expect("common App drop");
    let baseline = body[drop_app..]
        .find("live_renderer_count() != renderer_baseline")
        .expect("post-drop baseline check");
    assert!(!body[app..drop_app].contains('?'));
    assert!(baseline > 0);
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
