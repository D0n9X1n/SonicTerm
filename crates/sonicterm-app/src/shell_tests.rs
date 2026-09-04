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
