use super::*;

#[test]
fn macos_tabbing_uses_shared_process_policy_and_retains_window_callbacks() {
    // Normal and smoke startup keep per-window disallow while the shared winit path owns the class setting.
    let source = include_str!("main.rs");
    assert!(!source.contains("setAllowsAutomaticWindowTabbing"));
    let smoke_start = source.find("fn run_mac_runtime_smoke()").unwrap();
    let smoke_end = source[smoke_start..].find("fn process_privilege_from_euid").unwrap();
    let smoke = &source[smoke_start..smoke_start + smoke_end];
    let normal = &source[source.find("fn main()").unwrap()..];
    for startup in [smoke, normal] {
        assert!(startup.contains("setTabbingMode: 2isize"));
        assert!(startup.contains(".with_on_window_ready(on_window_ready)"));
    }
    assert_eq!(source.matches("setTabbingMode: 2isize").count(), 2);
}

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

#[test]
fn runtime_smoke_uses_scratch_config_and_log_roots_with_posix_expansion() {
    // Protect the packaged smoke from mutating HOME or typing its complete success marker.
    let root = std::path::Path::new("/tmp/native-smoke");
    let spec = runtime_smoke_spec(root, 41).expect("valid macOS smoke spec");
    assert_eq!(spec.shell_program(), "/bin/sh");
    assert_eq!(spec.config_dir(), root.join("config"));
    assert_eq!(spec.log_dir(), root.join("logs"));
    assert_eq!(spec.command(), b"printf '__SONICTERM_SMOKE_%s__\\n' '41'\n");
    assert!(!String::from_utf8_lossy(spec.command()).contains(spec.marker()));
}

#[test]
fn mac_runtime_smoke_exit_codes_include_warm_cleanup() {
    // Protect workflow diagnostics from collapsing warm renderer cleanup into a generic failure.
    use sonicterm_app::app::RuntimeSmokeFailure;
    assert_eq!(runtime_exit_code(&Ok(())), 0);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::WarmLifecycle)), 16);
}

#[test]
fn mac_runtime_smoke_initializes_only_explicit_log_state() {
    // Protect the hidden shipping mode from consulting or overwriting user-home config and logs.
    const MAIN: &str = include_str!("main.rs");
    assert!(MAIN.contains("sonicterm_logging::init_in(&log_cfg, spec.log_dir())"));
    assert!(MAIN.contains("shell.run_smoke(spec"));
    assert!(!MAIN.contains("set_var(\"HOME\""));
    assert!(!MAIN.contains("set_var(\"USERPROFILE\""));
}
