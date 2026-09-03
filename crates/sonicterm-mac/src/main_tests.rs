use super::*;

#[test]
fn effective_uid_maps_root_and_non_root_values() {
    // Protect the macOS privilege signal from usernames, titles, or environment variables.
    assert_eq!(process_privilege_from_euid(0), sonicterm_app::ProcessPrivilege::Privileged);
    assert_eq!(process_privilege_from_euid(1), sonicterm_app::ProcessPrivilege::Unprivileged);
    assert_eq!(
        process_privilege_from_euid(u32::MAX),
        sonicterm_app::ProcessPrivilege::Unprivileged
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_effective_uid_probe_matches_libc() {
    // Exercise the real process boundary without assuming the runner is root or non-root.
    let euid =
        // SAFETY: `geteuid` reads process credentials and accepts no pointer or owned resource.
        unsafe { libc::geteuid() };
    assert_eq!(detect_process_privilege(), process_privilege_from_euid(euid));
}

#[test]
fn startup_passes_the_effective_uid_observation_to_the_shell() {
    // Protect macOS startup from replacing effective UID with title or environment inference.
    const MAIN: &str = include_str!("main.rs");

    assert!(MAIN.contains("unsafe { libc::geteuid() }"));
    assert!(MAIN.contains("process_privilege_from_euid(euid)"));
    assert!(MAIN.contains(".with_process_privilege(process_privilege)"));
}
