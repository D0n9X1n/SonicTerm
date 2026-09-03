use super::*;
use crate::ProcessPrivilege;

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
