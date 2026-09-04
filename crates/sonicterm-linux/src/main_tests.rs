use super::*;

#[test]
fn effective_uid_maps_root_and_non_root_values() {
    // Protect the Linux privilege signal from usernames, titles, or environment variables.
    assert_eq!(process_privilege_from_euid(0), sonicterm_app::ProcessPrivilege::Privileged);
    assert_eq!(process_privilege_from_euid(1), sonicterm_app::ProcessPrivilege::Unprivileged);
    assert_eq!(
        process_privilege_from_euid(u32::MAX),
        sonicterm_app::ProcessPrivilege::Unprivileged
    );
}

#[cfg(target_os = "linux")]
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
    // Protect Linux startup from replacing effective UID with title or environment inference.
    const MAIN: &str = include_str!("main.rs");

    assert!(MAIN.contains("unsafe { libc::geteuid() }"));
    assert!(MAIN.contains("process_privilege_from_euid(euid)"));
    assert!(MAIN.contains(".with_process_privilege(process_privilege)"));
}

const DESKTOP_ENTRY: &str = include_str!("../resources/com.d0n9x1n.SonicTerm.desktop");
const APPSTREAM_METADATA: &str = include_str!("../resources/com.d0n9x1n.SonicTerm.metainfo.xml");

fn desktop_value(key: &str) -> Option<&str> {
    DESKTOP_ENTRY.lines().find_map(|line| {
        line.split_once('=').filter(|(name, _)| *name == key).map(|(_, value)| value)
    })
}

#[test]
fn hidden_runtime_smoke_flag_is_the_only_alternate_startup_mode() {
    // Protect release automation from silently running an ordinary interactive session.
    assert_eq!(parse_startup_mode(["sonicterm"]), Ok(StartupMode::Interactive));
    assert_eq!(parse_startup_mode(["sonicterm", "--runtime-smoke"]), Ok(StartupMode::RuntimeSmoke));
    assert!(parse_startup_mode(["sonicterm", "--runtime-smoke", "extra"]).is_err());
    assert!(parse_startup_mode(["sonicterm", "--unknown"]).is_err());
}

#[test]
fn smoke_failures_retain_their_stable_process_codes() {
    // Protect package jobs from collapsing every runtime boundary into generic exit code one.
    use sonicterm_app::app::RuntimeSmokeFailure;

    assert_eq!(runtime_exit_code(&Ok(())), 0);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::EventLoop)), 10);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::Display)), 11);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::Gpu)), 12);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::Pty)), 13);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::Marker)), 14);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::Present)), 15);
}

#[test]
fn runtime_smoke_isolates_state_without_changing_interactive_startup() {
    // Protect package smokes from touching user state while retaining the ordinary startup path.
    let configured = std::ffi::OsStr::new("/ci/sonicterm-smoke");
    let temp_root = std::path::Path::new("/tmp");

    assert_eq!(
        runtime_state_dir_with(StartupMode::Interactive, Some(configured), temp_root, 41),
        None
    );
    assert_eq!(
        runtime_state_dir_with(StartupMode::RuntimeSmoke, Some(configured), temp_root, 41),
        Some(std::path::PathBuf::from("/ci/sonicterm-smoke"))
    );
    assert_eq!(
        runtime_state_dir_with(StartupMode::RuntimeSmoke, None, temp_root, 41),
        Some(std::path::PathBuf::from("/tmp/sonicterm-runtime-smoke-41"))
    );
}

#[test]
fn desktop_entry_matches_runtime_identity_and_binary() {
    // Protect package discovery, launch, icon, and task grouping as one identity contract.
    assert_eq!(desktop_value("Type"), Some("Application"));
    assert_eq!(desktop_value("Exec"), Some("sonicterm"));
    assert_eq!(desktop_value("Icon"), Some("com.d0n9x1n.SonicTerm"));
    assert_eq!(desktop_value("StartupWMClass"), Some("com.d0n9x1n.SonicTerm"));
    assert_eq!(desktop_value("Terminal"), Some("false"));
    assert!(desktop_value("Categories").is_some_and(|value| value.contains("TerminalEmulator;")));
}

#[test]
fn appstream_metadata_matches_the_desktop_entry() {
    // Protect software-center metadata from drifting away from the installed launcher.
    let document = roxmltree::Document::parse(APPSTREAM_METADATA).expect("valid AppStream XML");
    let component = document.root_element();
    assert_eq!(component.attribute("type"), Some("desktop-application"));
    let text = |name| {
        component.descendants().find(|node| node.has_tag_name(name)).and_then(|node| node.text())
    };
    assert_eq!(text("id"), Some("com.d0n9x1n.SonicTerm"));
    assert_eq!(text("launchable"), Some("com.d0n9x1n.SonicTerm.desktop"));
    assert_eq!(text("binary"), Some("sonicterm"));
    assert_eq!(
        component
            .descendants()
            .find(|node| node.has_tag_name("launchable"))
            .and_then(|node| node.attribute("type")),
        Some("desktop-id")
    );
}

#[test]
fn linux_defaults_use_the_packaged_keymap() {
    // Protect cross-host workspace tests from inheriting the build host's platform keymap.
    assert_eq!(linux_default_config().keymap, "sonicterm-linux");
}

/// Linux normalizes every unsupported native material to opaque with one warning.
#[test]
fn linux_clamps_native_backdrops_to_opaque_with_one_warning() {
    for requested in [BackdropKind::Mica, BackdropKind::Acrylic, BackdropKind::Tabbed] {
        let mut config = linux_default_config();
        config.appearance.backdrop = requested;
        let (normalized, warnings) = normalize_linux_config(config);

        assert_eq!(normalized.appearance.backdrop, BackdropKind::Opaque);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Linux"));
        assert!(warnings[0].contains("opaque"));
    }
}

/// The binary installs Linux policy on the shared shell instead of applying it only at startup.
#[test]
fn linux_shell_installs_the_shared_config_normalizer() {
    const SOURCE: &str = include_str!("main.rs");
    assert!(SOURCE.contains(".with_config_normalizer(Box::new(normalize_linux_config))"));
    assert!(
        !SOURCE.contains("normalize_linux_config(config"),
        "startup-only normalization would let explicit reload bypass Linux policy"
    );
}

/// Opaque input and a second normalization pass are warning-free.
#[test]
fn linux_normalization_is_idempotent_and_silent_after_the_first_pass() {
    let mut requested = linux_default_config();
    requested.appearance.backdrop = BackdropKind::Mica;
    let (normalized, first_warnings) = normalize_linux_config(requested);
    let (second, second_warnings) = normalize_linux_config(normalized.clone());

    assert_eq!(normalized.appearance.backdrop, BackdropKind::Opaque);
    assert_eq!(first_warnings.len(), 1);
    assert_eq!(second, normalized);
    assert!(second_warnings.is_empty());
}

#[test]
fn linux_runtime_assets_include_platform_keymap_and_all_font_faces() {
    // Protect development and packaged layouts through their shared asset resolver contract.
    let assets = asset_dir();
    assert!(assets.join("keymaps/sonicterm-linux.toml").is_file());
    for face in PACKAGED_FONT_FILES {
        assert!(assets.join("fonts").join(face).is_file(), "missing packaged face {face}");
    }
}
