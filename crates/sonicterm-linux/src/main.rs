//! SonicTerm Terminal — Linux entry point.

#![warn(clippy::min_ident_chars)]

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::Result;
#[cfg(any(target_os = "linux", test))]
use sonicterm_cfg::assets::asset_dir;
#[cfg(any(target_os = "linux", test))]
use sonicterm_cfg::config::{BackdropKind, Config};
#[cfg(target_os = "linux")]
use sonicterm_cfg::keymap::Keymap;
#[cfg(target_os = "linux")]
use sonicterm_cfg::theme::Theme;

#[cfg(any(target_os = "linux", all(unix, test)))]
mod default_handlers;
#[cfg(target_os = "linux")]
mod launcher_progress;

/// Fonts the Linux package ships in `assets/fonts`: only the symbols font, since the text face
/// comes from fontconfig's `monospace`.
#[cfg(any(target_os = "linux", test))]
const PACKAGED_FONT_FILES: &[&str] = &["SymbolsNerdFontMono-Regular.ttf"];

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupMode {
    Interactive,
    RuntimeSmoke,
}

#[cfg(any(target_os = "linux", test))]
fn process_privilege_from_euid(euid: u32) -> sonicterm_app::ProcessPrivilege {
    if euid == 0 {
        sonicterm_app::ProcessPrivilege::Privileged
    } else {
        sonicterm_app::ProcessPrivilege::Unprivileged
    }
}

#[cfg(target_os = "linux")]
fn detect_process_privilege() -> sonicterm_app::ProcessPrivilege {
    let euid =
        // SAFETY: `geteuid` reads process credentials and accepts no pointer or owned resource.
        unsafe { libc::geteuid() };
    process_privilege_from_euid(euid)
}

/// What a Linux command line asks the binary to do.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, PartialEq, Eq)]
enum LinuxStartup {
    /// Run the app in `mode`, first queueing `launch` when it is present.
    Run { mode: StartupMode, launch: Option<sonicterm_app::launch_queue::LaunchRequest> },
    /// Print usage to stdout and exit 0.
    Help,
}

/// Turn a parsed command line into a startup decision.
///
/// Every interactive launch queues a terminal request, so the first tab starts
/// in the process cwd (else `home`), as Debian launchers and KIO expect. A
/// relative `--open-script` path resolves against the process cwd.
#[cfg(any(target_os = "linux", test))]
fn linux_startup(
    cli: sonicterm_app::launch_cli::LaunchCli,
    process_cwd: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> Result<LinuxStartup> {
    use sonicterm_app::launch_cli::{terminal_launch, CliDialect, LaunchCli};
    use sonicterm_app::launch_queue::{LaunchRequest, OpenScriptRequest};
    match cli {
        LaunchCli::Help => Ok(LinuxStartup::Help),
        LaunchCli::RuntimeSmoke => {
            Ok(LinuxStartup::Run { mode: StartupMode::RuntimeSmoke, launch: None })
        }
        LaunchCli::Terminal(terminal) => {
            let launch = terminal_launch(terminal, CliDialect::Linux, process_cwd, home);
            Ok(LinuxStartup::Run {
                mode: StartupMode::Interactive,
                launch: Some(LaunchRequest::Terminal(launch)),
            })
        }
        LaunchCli::OpenScript(path) => {
            let request = OpenScriptRequest::resolve_with_cwd_lookup(path, || process_cwd)
                .map_err(|_| {
                    anyhow::anyhow!(
                        "cannot resolve a relative --open-script path: the process cwd is unavailable"
                    )
                })?;
            Ok(LinuxStartup::Run {
                mode: StartupMode::Interactive,
                launch: Some(LaunchRequest::Script(request)),
            })
        }
        LaunchCli::RefreshShellAssociations | LaunchCli::TearOut(_) => {
            // When: a Windows-only mode reached Linux, the shared parser broke its dialect contract.
            anyhow::bail!("the Linux parser accepted a Windows-only option")
        }
    }
}

/// Report a usage error: usage on stderr, then one value-free `warn` in the log.
#[cfg(target_os = "linux")]
fn report_usage_error(error: sonicterm_app::launch_cli::UsageError) {
    use sonicterm_app::launch_cli::{usage, CliDialect};
    eprintln!("sonicterm: {error}\n\n{}", usage(CliDialect::Linux));
    let log_dir = sonicterm_logging::log_dir();
    let _log_guard = sonicterm_logging::init_in(&Config::default().logging, &log_dir).ok();
    error.log();
}

#[cfg(any(target_os = "linux", test))]
fn runtime_exit_code(
    result: &std::result::Result<(), sonicterm_app::app::RuntimeSmokeFailure>,
) -> i32 {
    result.as_ref().map_or_else(|failure| failure.exit_code(), |()| 0)
}

#[cfg(any(target_os = "linux", test))]
fn linux_default_config() -> Config {
    Config { keymap: "sonicterm-linux".to_string(), ..Config::default() }
}

/// Resolve Linux-unsupported native materials before config reaches app state.
#[cfg(any(target_os = "linux", test))]
fn normalize_linux_config(mut config: Config) -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    if config.appearance.backdrop != BackdropKind::Opaque {
        // When: Linux cannot apply the requested native material, clamp before any window is built.
        warnings.push(format!(
            "Linux does not support the {:?} native backdrop; using opaque",
            config.appearance.backdrop
        ));
        config.appearance.backdrop = BackdropKind::Opaque;
    }
    (config, warnings)
}

#[cfg(any(target_os = "linux", test))]
fn runtime_state_dir_with(
    mode: StartupMode,
    configured: Option<&std::ffi::OsStr>,
    temp_root: &std::path::Path,
    process_id: u32,
) -> Option<std::path::PathBuf> {
    if mode != StartupMode::RuntimeSmoke {
        return None;
    }
    configured
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| Some(temp_root.join(format!("sonicterm-runtime-smoke-{process_id}"))))
}

#[cfg(target_os = "linux")]
fn runtime_state_dir(mode: StartupMode) -> Option<std::path::PathBuf> {
    runtime_state_dir_with(
        mode,
        std::env::var_os("SONICTERM_RUNTIME_SMOKE_DIR").as_deref(),
        &std::env::temp_dir(),
        std::process::id(),
    )
}

#[cfg(any(target_os = "linux", test))]
fn runtime_smoke_spec(
    root: &std::path::Path,
    nonce: u32,
) -> std::result::Result<
    sonicterm_app::app::RuntimeSmokeSpec,
    sonicterm_app::app::RuntimeSmokeFailure,
> {
    let marker = format!("__SONICTERM_SMOKE_{nonce}__");
    let command = format!("printf '__SONICTERM_SMOKE_%s__\\n' '{nonce}'\n").into_bytes();
    sonicterm_app::app::RuntimeSmokeSpec::new(
        "/bin/sh",
        marker,
        command,
        root.join("config"),
        root.join("logs"),
    )
}

#[cfg(target_os = "linux")]
fn load_config(mode: StartupMode, warnings: &mut Vec<String>) -> (Config, Option<anyhow::Error>) {
    let config = if mode == StartupMode::RuntimeSmoke {
        (linux_default_config(), None)
    } else {
        match Config::default_path() {
            Some(path) => {
                if let Err(error) = Config::ensure_user_config_file(&path) {
                    warnings.push(format!(
                        "create default config/examples at {} failed: {error}",
                        path.display()
                    ));
                }
                Config::load_or_default_reporting(&path, warnings)
            }
            None => (linux_default_config(), None),
        }
    };
    config
}

#[cfg(target_os = "linux")]
fn load_theme(name: &str) -> (Theme, Option<anyhow::Error>) {
    Theme::load_name_reporting(name, &asset_dir())
}

#[cfg(target_os = "linux")]
fn load_keymap(name: &str) -> (Keymap, Option<anyhow::Error>) {
    Keymap::load_name_reporting(name, &asset_dir())
}

#[cfg(target_os = "linux")]
fn preflight_linux_fonts(config: &Config) -> Result<()> {
    let font_dir = asset_dir().join("fonts");
    for face in PACKAGED_FONT_FILES {
        let path = font_dir.join(face);
        if !path.is_file() {
            // When: the packaged symbols font is absent, Powerline and Nerd Font icons lose their bundled source.
            anyhow::bail!("packaged font is missing: {}", path.display());
        }
    }
    let stack = sonicterm_engine::FontStack::try_new_full_with_weight_and_font_dirs(
        &config.font.family,
        f64::from(config.font.size),
        72,
        config.font.effective_weight_scale(),
        &[font_dir],
    )
    .context("initialize Linux font discovery")?;
    stack.cell_metrics_raster_px().context("resolve Linux terminal font metrics")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_linux(mode: StartupMode) -> Result<i32> {
    let smoke_spec = runtime_state_dir(mode)
        .map(|root| runtime_smoke_spec(&root, std::process::id()))
        .transpose()?;
    let log_dir = smoke_spec
        .as_ref()
        .map(|spec| spec.log_dir().to_path_buf())
        .unwrap_or_else(sonicterm_logging::log_dir);
    if let Some(spec) = smoke_spec.as_ref() {
        std::fs::create_dir_all(spec.config_dir())?;
        std::fs::create_dir_all(spec.log_dir())?;
    }
    sonicterm_logging::install_panic_hook(log_dir.clone());
    let _exit_guard = sonicterm_logging::install_exit_logging(&log_dir);
    let session = sonicterm_logging::session_state::arm(&log_dir, env!("CARGO_PKG_VERSION")).ok();
    if let Some(session) = session.as_ref() {
        // When: a session marker exists, associate any later crash dump with that exact launch.
        sonicterm_logging::crash::set_session_id(session.id());
    }
    let breadcrumb_writer = session.as_ref().and_then(|session| {
        sonicterm_logging::breadcrumbs::BreadcrumbWriter::start(
            &log_dir,
            session.id(),
            sonicterm_logging::breadcrumbs::BreadcrumbLimits::default(),
        )
        .ok()
    });
    let breadcrumb_recorder = breadcrumb_writer.as_ref().map(|writer| writer.recorder());
    if let Some(recorder) = &breadcrumb_recorder {
        use sonicterm_logging::breadcrumbs::{
            AppVersion, BreadcrumbEvent, LifecycleEvent, Platform,
        };
        if let Ok(version) = env!("CARGO_PKG_VERSION").parse::<AppVersion>() {
            // When: the package version satisfies the bounded breadcrumb grammar, retain it.
            let _ = recorder.record(BreadcrumbEvent::Version(version));
        }
        let _ = recorder.record(BreadcrumbEvent::Platform(Platform::current()));
        let _ = recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started));
    }

    let mut config_warnings = Vec::new();
    let (config, config_error) = load_config(mode, &mut config_warnings);
    let log_config = config.logging.clone();
    sonicterm_logging::cleanup_log_files(&log_dir, &log_config);
    let _log_guard = sonicterm_logging::init_in(&log_config, &log_dir).ok();
    for warning in config_warnings.drain(..) {
        tracing::warn!(target: "sonicterm-cfg", "{warning}");
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "sonic started");
    sonicterm_logging::postmortem::report_prior_sessions(
        &log_dir,
        session.as_ref().map(sonicterm_logging::session_state::ArmedSession::id),
    );
    let artifact_log_dir = log_dir.clone();
    let artifact_log_config = log_config.clone();
    std::thread::Builder::new()
        .name("sonicterm-artifact-cleanup".to_string())
        .spawn(move || {
            sonicterm_logging::cleanup_artifacts(&artifact_log_dir, &artifact_log_config);
        })
        .map(|_| ())
        .unwrap_or_else(|error| tracing::warn!("failed to spawn artifact cleanup: {error}"));

    preflight_linux_fonts(&config)?;
    tracing::info!(
        "Linux native menu, desktop notifications, and cross-process tab drag are unavailable; using in-app fallbacks"
    );
    let (theme, keymap, startup_loads) = if mode == StartupMode::RuntimeSmoke {
        let assets = asset_dir();
        (
            Theme::load_or_default(&assets.join("themes").join(format!("{}.toml", config.theme))),
            Keymap::load_or_default(
                &assets.join("keymaps").join(format!("{}.toml", config.keymap)),
            ),
            None,
        )
    } else {
        let (theme, theme_error) = load_theme(&config.theme);
        let (keymap, keymap_error) = load_keymap(&config.keymap);
        let loads = sonicterm_app::StartupLoads {
            config: config_error,
            theme: theme_error,
            keymap: keymap_error,
        };
        (theme, keymap, Some(loads))
    };
    let machine = sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
    let process_privilege = detect_process_privilege();
    tracing::info!(privileged = process_privilege.is_privileged(), "process privilege observed");
    let mut shell = sonicterm_app::shell::LinuxShell::new(machine, theme, config, keymap)
        .with_process_privilege(process_privilege)
        .with_config_normalizer(Box::new(normalize_linux_config));
    if let Some(loads) = startup_loads {
        // When: an interactive start loaded the files, the strip shows their fallbacks.
        shell = shell.with_startup_loads(loads);
    }
    if mode == StartupMode::Interactive {
        let theme_loader: sonicterm_app::ThemeLoader =
            Box::new(|name| Theme::load_name_or_path(name, &asset_dir()));
        let keymap_loader: sonicterm_app::KeymapLoader = Box::new(Keymap::load_strict);
        // Only the palette command reaches this backend; installing it changes nothing.
        shell = shell
            .with_asset_loaders(theme_loader, keymap_loader)
            .with_default_handlers(Box::new(
                default_handlers::LinuxDefaultHandlers::from_process_env(),
            ))
            // The launcher entry is signalled only after a program reports `OSC 9;4` progress.
            .with_taskbar_progress(Box::new(launcher_progress::LauncherEntry::default()));
    }
    if let Some(recorder) = breadcrumb_recorder.clone() {
        // When: breadcrumb startup succeeded, let the app report runtime state without filesystem IO.
        shell = shell.with_breadcrumb_recorder(recorder);
    }
    let (interactive_outcome, smoke_outcome) = match mode {
        StartupMode::Interactive => (Some(shell.run()), None),
        StartupMode::RuntimeSmoke => {
            let spec = smoke_spec.expect("runtime mode constructs a smoke specification");
            (None, Some(shell.run_smoke(spec, std::time::Duration::from_secs(30))))
        }
    };
    let clean_shutdown = smoke_outcome
        .as_ref()
        .is_some_and(|outcome| outcome.is_clean(sonicterm_app::shell::ExitMode::RuntimeSmoke))
        || interactive_outcome
            .as_ref()
            .is_some_and(|outcome| outcome.is_clean(sonicterm_app::shell::ExitMode::Interactive));
    sonicterm_app::shell::finish_session_diagnostics(clean_shutdown, breadcrumb_writer, session);

    match (interactive_outcome, smoke_outcome) {
        (Some(outcome), None) => {
            outcome.result?;
            Ok(0)
        }
        (None, Some(outcome)) => {
            if let Err(error) = &outcome.result {
                tracing::error!(code = error.exit_code(), %error, "Linux runtime smoke failed");
            }
            Ok(runtime_exit_code(&outcome.result))
        }
        _ => unreachable!("startup mode selects exactly one shell outcome"),
    }
}

fn main() -> Result<std::process::ExitCode> {
    #[cfg(target_os = "linux")]
    {
        use sonicterm_app::launch_cli::{
            home_dir, parse_launch_args, usage, CliDialect, USAGE_EXIT_CODE,
        };
        let process_cwd = std::env::current_dir().ok();
        let cli =
            match parse_launch_args(std::env::args_os(), CliDialect::Linux, process_cwd.as_deref())
            {
                Ok(cli) => cli,
                Err(error) => {
                    // When: the command line breaks the grammar, exit 2 with usage and a value-free log line.
                    report_usage_error(error);
                    return Ok(std::process::ExitCode::from(USAGE_EXIT_CODE));
                }
            };
        let (mode, launch) = match linux_startup(cli, process_cwd, home_dir())? {
            LinuxStartup::Help => {
                print!("{}", usage(CliDialect::Linux));
                return Ok(std::process::ExitCode::SUCCESS);
            }
            LinuxStartup::Run { mode, launch } => (mode, launch),
        };
        if let Some(launch) = launch {
            // When: an interactive `launch` exists, queue it before the shell installs its proxy and drains.
            let _ = sonicterm_app::launch_queue::push_requests(vec![launch]);
        }
        let code = u8::try_from(run_linux(mode)?).context("Linux exit code fits in one byte")?;
        Ok(std::process::ExitCode::from(code))
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("the sonicterm binary is supported only on Linux")
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
