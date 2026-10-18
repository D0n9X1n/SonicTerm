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

/// Parse `args` (binary name prepended) and decide startup with a fixed cwd and home.
fn startup(args: &[&str]) -> std::result::Result<LinuxStartup, String> {
    use sonicterm_app::launch_cli::{parse_launch_args, CliDialect};
    let mut all = vec!["sonicterm"];
    all.extend_from_slice(args);
    let cwd = std::env::temp_dir();
    let cli =
        parse_launch_args(all, CliDialect::Linux, Some(&cwd)).map_err(|error| error.to_string())?;
    linux_startup(cli, Some(cwd), None).map_err(|error| error.to_string())
}

/// Release automation's hidden smoke flag stays exclusive and queues no launch.
#[test]
fn hidden_runtime_smoke_flag_stays_exclusive() {
    assert_eq!(
        startup(&["--runtime-smoke"]),
        Ok(LinuxStartup::Run { mode: StartupMode::RuntimeSmoke, launch: None })
    );
    assert!(startup(&["--runtime-smoke", "extra"]).is_err());
    assert!(startup(&["--unknown"]).is_err());
}

/// A plain launch queues a terminal request whose first tab starts in the process cwd.
#[test]
fn plain_launch_starts_in_the_process_cwd() {
    use sonicterm_app::launch_queue::LaunchRequest;
    let Ok(LinuxStartup::Run { mode, launch: Some(LaunchRequest::Terminal(launch)) }) =
        startup(&[])
    else {
        panic!("a plain launch queues a terminal request");
    };
    assert_eq!(mode, StartupMode::Interactive);
    assert_eq!(launch.cwd, Some(std::env::temp_dir()));
    assert_eq!(launch.cwd_source.map(|source| source.as_str()), Some("process"));
    assert!(launch.command.is_none());
}

/// `-e` passes the program and every later argument through unchanged, with the title and hold.
#[test]
fn command_launch_keeps_exact_argv() {
    use sonicterm_app::launch_queue::LaunchRequest;
    let Ok(LinuxStartup::Run { launch: Some(LaunchRequest::Terminal(launch)), .. }) =
        startup(&["-T", "Logs", "--hold", "-e", "printf", "%s\\n", "a b", "-T"])
    else {
        panic!("a command launch queues a terminal request");
    };
    let command = launch.command.expect("command");
    assert_eq!(command.program, "printf");
    assert_eq!(command.args, ["%s\\n", "a b", "-T"]);
    assert_eq!(launch.title.as_deref(), Some("Logs"));
    assert!(launch.hold);
}

/// `--open-script` queues a draft resolved against the process cwd, never a command.
#[test]
fn open_script_queues_a_draft() {
    use sonicterm_app::launch_queue::LaunchRequest;
    let Ok(LinuxStartup::Run { launch: Some(LaunchRequest::Script(request)), .. }) =
        startup(&["--open-script", "x.sh"])
    else {
        panic!("--open-script queues a script draft");
    };
    assert_eq!(request.launch_path, std::env::temp_dir().join("x.sh"));
}

/// `--help` prints usage instead of starting, and usage errors use exit code 2.
#[test]
fn help_and_usage_exit_codes() {
    assert_eq!(startup(&["--help"]), Ok(LinuxStartup::Help));
    assert_eq!(sonicterm_app::launch_cli::USAGE_EXIT_CODE, 2);
    assert!(startup(&["file.sh"]).is_err());
}

/// `main` reads arguments losslessly and maps usage errors to exit 2, never panicking on non-UTF-8.
#[test]
fn main_parses_os_arguments_and_exits_two() {
    const MAIN: &str = include_str!("main.rs");
    assert!(MAIN.contains("std::env::args_os()"));
    assert!(!MAIN.contains("std::env::args()"));
    assert!(MAIN.contains("ExitCode::from(USAGE_EXIT_CODE)"));
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
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::WarmLifecycle)), 16);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::GpuFaultContainment)), 17);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::GpuDeviceLoss)), 18);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::GpuDeviceRecovery)), 19);
    assert_eq!(runtime_exit_code(&Err(RuntimeSmokeFailure::NativeTeardown)), 20);
}

#[test]
fn linux_runtime_smoke_uses_separate_scratch_config_and_logs() {
    // Protect the packaged smoke from replacing HOME while keeping all writable state isolated.
    let root = std::path::Path::new("/tmp/native-smoke");
    let spec = runtime_smoke_spec(root, 41).expect("valid Linux smoke spec");
    assert_eq!(spec.shell_program(), "/bin/sh");
    assert_eq!(spec.config_dir(), root.join("config"));
    assert_eq!(spec.log_dir(), root.join("logs"));
    assert!(!String::from_utf8_lossy(spec.command()).contains(spec.marker()));
    const MAIN: &str = include_str!("main.rs");
    assert!(!MAIN.contains("set_var(\"HOME\""));
    assert!(!MAIN.contains("set_var(\"USERPROFILE\""));
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
    assert_eq!(desktop_value("Exec"), Some("sonicterm"), "KDE appends -e to the raw Exec");
    assert_eq!(desktop_value("Icon"), Some("com.d0n9x1n.SonicTerm"));
    assert_eq!(desktop_value("StartupWMClass"), Some("com.d0n9x1n.SonicTerm"));
    assert_eq!(desktop_value("Terminal"), Some("false"));
    assert!(desktop_value("Categories").is_some_and(|value| value.contains("TerminalEmulator;")));
}

/// The terminal entry advertises the launcher options xdg-terminal-exec reads.
#[test]
fn desktop_entry_declares_terminal_launcher_arguments() {
    assert_eq!(desktop_value("X-TerminalArgExec"), Some("-e"));
    assert_eq!(desktop_value("X-ExecArg"), Some("-e"));
    assert_eq!(desktop_value("X-TerminalArgTitle"), Some("-T"));
    assert_eq!(desktop_value("X-TerminalArgDir"), Some("--dir="));
    assert_eq!(desktop_value("X-TerminalArgHold"), Some("--hold"));
    assert_eq!(desktop_value("MimeType"), None, "the terminal entry handles no file types");
}

const OPEN_SCRIPT_ENTRY: &str =
    include_str!("../resources/com.d0n9x1n.SonicTerm.open-script.desktop");

/// The value of `key` in the hidden open-script entry.
fn open_script_value(key: &str) -> Option<&str> {
    OPEN_SCRIPT_ENTRY.lines().find_map(|line| {
        line.split_once('=').filter(|(name, _)| *name == key).map(|(_, value)| value)
    })
}

/// The hidden script entry opens scripts as drafts, never as commands, and stays out of menus.
#[test]
fn open_script_entry_types_drafts_and_is_hidden() {
    assert_eq!(open_script_value("Type"), Some("Application"));
    assert_eq!(open_script_value("Name"), Some("SonicTerm"));
    assert_eq!(open_script_value("Icon"), Some("com.d0n9x1n.SonicTerm"));
    assert_eq!(open_script_value("StartupWMClass"), Some("com.d0n9x1n.SonicTerm"));
    assert_eq!(open_script_value("NoDisplay"), Some("true"));
    assert_eq!(open_script_value("Exec"), Some("sonicterm --open-script %f"));
    assert_eq!(
        open_script_value("MimeType"),
        Some("application/x-shellscript;text/x-shellscript;")
    );
    assert_eq!(open_script_value("Terminal"), Some("false"));
    assert!(!OPEN_SCRIPT_ENTRY.contains(" -e"), "a file association must never execute");
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

/// A relative `--open-script` with no process cwd is an error, never a guess at a directory.
#[test]
fn relative_open_script_without_a_cwd_is_refused() {
    use sonicterm_app::launch_cli::LaunchCli;
    let error = linux_startup(LaunchCli::OpenScript("x.sh".into()), None, None)
        .expect_err("a relative script needs a process cwd");
    assert!(error.to_string().contains("process cwd is unavailable"), "{error}");
}

/// Windows-only modes that reach the Linux startup are refused, not run as a terminal.
#[test]
fn windows_only_modes_are_refused_on_linux() {
    use sonicterm_app::launch_cli::LaunchCli;
    for cli in [LaunchCli::RefreshShellAssociations, LaunchCli::TearOut("{}".into())] {
        let error = linux_startup(cli, None, None).expect_err("Windows-only mode");
        assert!(error.to_string().contains("Windows-only"), "{error}");
    }
}

/// On a non-Linux host the Linux binary refuses to start instead of opening a window.
#[cfg(not(target_os = "linux"))]
#[test]
fn main_refuses_non_linux_hosts() {
    let error = main().expect_err("the Linux binary must not start elsewhere");
    assert!(error.to_string().contains("supported only on Linux"), "{error}");
}
