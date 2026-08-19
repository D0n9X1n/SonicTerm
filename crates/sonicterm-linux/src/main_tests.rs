use super::*;

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

#[test]
fn linux_clamps_native_backdrops_to_opaque_with_one_warning() {
    // Protect X11 and Wayland from unsupported compositor-material requests.
    for requested in [BackdropKind::Mica, BackdropKind::Acrylic, BackdropKind::Tabbed] {
        let mut config = linux_default_config();
        config.appearance.backdrop = requested;
        let mut warnings = Vec::new();
        let normalized = normalize_linux_config(config, &mut warnings);

        assert_eq!(normalized.appearance.backdrop, BackdropKind::Opaque);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Linux"));
        assert!(warnings[0].contains("opaque"));
    }
}

#[test]
fn linux_preserves_an_opaque_backdrop_without_warning() {
    // Protect the supported default from producing noisy startup diagnostics.
    let mut warnings = Vec::new();
    let normalized = normalize_linux_config(linux_default_config(), &mut warnings);

    assert_eq!(normalized.appearance.backdrop, BackdropKind::Opaque);
    assert!(warnings.is_empty());
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
