//! SonicTerm Terminal — Linux entry point.

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

#[cfg(any(target_os = "linux", test))]
const PACKAGED_FONT_FILES: &[&str] = &[
    "RecMonoSt.Helens-Regular.ttf",
    "RecMonoSt.Helens-Bold.ttf",
    "RecMonoSt.Helens-Italic.ttf",
    "RecMonoSt.Helens-BoldItalic.ttf",
];

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupMode {
    Interactive,
    RuntimeSmoke,
}

#[cfg(any(target_os = "linux", test))]
fn parse_startup_mode<I, S>(args: I) -> std::result::Result<StartupMode, &'static str>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let _program = args.next();
    match (args.next(), args.next()) {
        (None, None) => Ok(StartupMode::Interactive),
        (Some(flag), None) if flag.as_ref() == "--runtime-smoke" => Ok(StartupMode::RuntimeSmoke),
        _ => Err("usage: sonicterm [--runtime-smoke]"),
    }
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

#[cfg(any(target_os = "linux", test))]
fn normalize_linux_config(mut config: Config, warnings: &mut Vec<String>) -> Config {
    if config.appearance.backdrop != BackdropKind::Opaque {
        // When: Linux cannot apply the requested native material, clamp before any window is built.
        warnings.push(format!(
            "Linux does not support the {:?} native backdrop; using opaque",
            config.appearance.backdrop
        ));
        config.appearance.backdrop = BackdropKind::Opaque;
    }
    config
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

#[cfg(target_os = "linux")]
fn load_config(mode: StartupMode, warnings: &mut Vec<String>) -> Config {
    let config = if mode == StartupMode::RuntimeSmoke {
        linux_default_config()
    } else {
        match Config::default_path() {
            Some(path) => {
                if let Err(error) = Config::ensure_user_config_file(&path) {
                    warnings.push(format!(
                        "create default config/examples at {} failed: {error}",
                        path.display()
                    ));
                }
                Config::load_or_default_collecting(&path, warnings)
            }
            None => linux_default_config(),
        }
    };
    normalize_linux_config(config, warnings)
}

#[cfg(target_os = "linux")]
fn load_theme(name: &str) -> Theme {
    Theme::load_name_or_default(name, &asset_dir())
}

#[cfg(target_os = "linux")]
fn load_keymap(name: &str) -> Keymap {
    Keymap::load_name_or_default(name, &asset_dir())
}

#[cfg(target_os = "linux")]
fn preflight_linux_fonts(config: &Config) -> Result<()> {
    let font_dir = asset_dir().join("fonts");
    for face in PACKAGED_FONT_FILES {
        let path = font_dir.join(face);
        if !path.is_file() {
            // When: a packaged Rec Mono face is absent, startup cannot provide the shipped font contract.
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
    let log_dir = runtime_state_dir(mode).unwrap_or_else(sonicterm_logging::log_dir);
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
    let config = load_config(mode, &mut config_warnings);
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
    let theme = load_theme(&config.theme);
    let keymap = load_keymap(&config.keymap);
    let theme_loader: sonicterm_app::ThemeLoader =
        Box::new(|name| Theme::load_name_or_path(name, &asset_dir()));
    let keymap_loader: sonicterm_app::KeymapLoader =
        Box::new(|name| Keymap::load_name_or_path(name, &asset_dir()));
    let machine = sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
    let mut shell = sonicterm_app::shell::LinuxShell::new(machine, theme, config, keymap)
        .with_asset_loaders(theme_loader, keymap_loader);
    if let Some(recorder) = breadcrumb_recorder.clone() {
        // When: breadcrumb startup succeeded, let the app report runtime state without filesystem IO.
        shell = shell.with_breadcrumb_recorder(recorder);
    }
    let (interactive_outcome, smoke_outcome) = match mode {
        StartupMode::Interactive => (Some(shell.run()), None),
        StartupMode::RuntimeSmoke => {
            (None, Some(shell.run_smoke(std::time::Duration::from_secs(30))))
        }
    };
    let clean_shutdown = smoke_outcome.is_some()
        || interactive_outcome.as_ref().is_some_and(std::result::Result::is_ok);
    if clean_shutdown {
        if let Some(recorder) = &breadcrumb_recorder {
            // When: the event loop returned through an orderly interactive or smoke path, record shutdown before flushing.
            let _ = recorder.record(sonicterm_logging::breadcrumbs::BreadcrumbEvent::Lifecycle(
                sonicterm_logging::breadcrumbs::LifecycleEvent::CleanShutdown,
            ));
        }
    }
    if let Some(writer) = breadcrumb_writer {
        let _ = writer.shutdown();
    }
    if clean_shutdown {
        if let Some(session) = session {
            // When: orderly loop return and breadcrumb flush completed, remove the unclean-session marker.
            let _ = session.mark_clean();
        }
    }

    match (interactive_outcome, smoke_outcome) {
        (Some(outcome), None) => {
            outcome?;
            Ok(0)
        }
        (None, Some(outcome)) => {
            if let Err(error) = &outcome {
                tracing::error!(code = error.exit_code(), %error, "Linux runtime smoke failed");
            }
            Ok(runtime_exit_code(&outcome))
        }
        _ => unreachable!("startup mode selects exactly one shell outcome"),
    }
}

fn main() -> Result<std::process::ExitCode> {
    #[cfg(target_os = "linux")]
    {
        let mode = parse_startup_mode(std::env::args()).map_err(anyhow::Error::msg)?;
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
