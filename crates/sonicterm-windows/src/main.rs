//! SonicTerm Terminal — Windows entry point.
//!
//! Hides the console window on release builds so we don't get a stray
//! conhost behind the GPU window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anyhow::Result;
use sonicterm_cfg::assets::asset_dir;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

#[cfg(target_os = "windows")]
fn set_process_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    // SAFETY: process-wide opt-in before winit creates any HWND. Failure is
    // non-fatal (Windows may reject it if a manifest already set awareness),
    // but calling here avoids blurry/scaled glyphs on mixed-DPI monitors.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(target_os = "windows"))]
fn set_process_dpi_awareness() {}

#[cfg(target_os = "windows")]
fn refresh_shell_associations() {
    use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

    // SAFETY: this process-wide shell notification passes no item-list pointers or owned handles.
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

#[cfg(target_os = "windows")]
mod backdrop;
mod cli;
#[cfg(target_os = "windows")]
mod menubar;
#[cfg(target_os = "windows")]
mod os_drag_win;
#[cfg(test)]
mod packaging_manifest;
mod startup;
// Windows-only: it resolves `software_render_mode` for the Win32 backdrop
// decision, and nothing off-Windows consumes it.
#[cfg(target_os = "windows")]
mod software_presenter;
#[cfg(target_os = "windows")]
mod tab_drag_os;

fn main() -> Result<()> {
    set_process_dpi_awareness();
    #[cfg(target_os = "windows")]
    let parsed_cli = cli::parse_cli_from_env()?;
    #[cfg(target_os = "windows")]
    if parsed_cli.refresh_shell_associations {
        refresh_shell_associations();
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    startup::queue_startup_open_script(&parsed_cli, || std::env::current_dir().ok())?;
    // Install panic hook BEFORE config load so a panic during load
    // still produces a crash dump. Logger init is deferred until
    // after the user's `[logging]` section has been read so its
    // `level` + retention knobs actually drive the runtime —
    // `tracing_subscriber::try_init` only installs the first subscriber;
    // initializing before config load would silently discard the user's level.
    sonicterm_logging::install_panic_hook(sonicterm_logging::log_dir());
    // Exit-path tracing — drop guard + (Unix only) signal handlers.
    // See `crates/sonicterm-logging/src/exit_trace.rs`.
    let _exit_guard = sonicterm_logging::install_exit_logging(&sonicterm_logging::log_dir());
    // Arm the session marker before any work that could fail. A kill after
    // this point leaves the marker behind, which is what lets the next launch
    // tell that this session never reached its shutdown path. Armed before the
    // logger exists on purpose: a session killed during config load is exactly
    // the one with no other evidence.
    let session = sonicterm_logging::session_state::arm(
        &sonicterm_logging::log_dir(),
        env!("CARGO_PKG_VERSION"),
    )
    .ok();
    // Tag any dump this process writes with the session that wrote it, so the
    // next launch can attach the artifact to the right session rather than
    // guessing.
    if let Some(session) = session.as_ref() {
        sonicterm_logging::crash::set_session_id(session.id());
    }
    let breadcrumb_writer = session.as_ref().and_then(|session| {
        sonicterm_logging::breadcrumbs::BreadcrumbWriter::start(
            &sonicterm_logging::log_dir(),
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
            let _ = recorder.record(BreadcrumbEvent::Version(version));
        }
        let _ = recorder.record(BreadcrumbEvent::Platform(Platform::current()));
        let _ = recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Started));
    }

    let mut cfg_warnings: Vec<String> = Vec::new();
    let config = load_config(&mut cfg_warnings);
    let log_cfg = config.logging.clone();
    sonicterm_logging::cleanup_log_files(&sonicterm_logging::log_dir(), &log_cfg);
    let _log_guard = sonicterm_logging::init(&log_cfg).ok();
    // Drain any warnings collected during pre-logging Config load so the
    // parse-failure WARN actually reaches sonicterm.log + stderr.
    for w in cfg_warnings.drain(..) {
        tracing::warn!(target: "sonicterm-cfg", "{w}");
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "sonic started");
    // Report what the previous session left behind, now that the logger is up.
    sonicterm_logging::postmortem::report_prior_sessions(
        &sonicterm_logging::log_dir(),
        session.as_ref().map(sonicterm_logging::session_state::ArmedSession::id),
    );
    let artifact_log_dir = sonicterm_logging::log_dir();
    let artifact_log_cfg = log_cfg.clone();
    std::thread::Builder::new()
        .name("sonicterm-artifact-cleanup".to_string())
        .spawn(move || {
            sonicterm_logging::cleanup_artifacts(&artifact_log_dir, &artifact_log_cfg);
        })
        .map(|_| ())
        .unwrap_or_else(|error| tracing::warn!("failed to spawn artifact cleanup: {error}"));
    #[cfg(target_os = "windows")]
    let tearout_payload = parsed_cli.tearout;
    let theme = load_theme(&config.theme);
    let keymap = load_keymap(&config.keymap);
    // Initial load is infallible; hot-reload loaders use
    // strict variants so user-visible errors are surfaced after startup.
    let theme_loader: sonicterm_app::ThemeLoader =
        Box::new(|name: &str| Theme::load_name_or_path(name, &asset_dir()));
    let keymap_loader: sonicterm_app::KeymapLoader =
        Box::new(|name: &str| Keymap::load_name_or_path(name, &asset_dir()));
    #[cfg(target_os = "windows")]
    {
        use sonicterm_app::menu::{PlatformMenu, Sender};
        // Initialize OLE once on the main thread so RegisterDragDrop /
        // DoDragDrop are usable from the same thread that owns the
        // winit HWND.
        let ole_guard = os_drag_win::init_ole();
        let ole_available = ole_guard.is_some();
        // Install the muda menubar the instant winit hands us an HWND.
        // muda's `init_for_hwnd` requires the window to exist; the
        // `on_window_ready` hook fires exactly once, right after
        // `el.create_window(...)` succeeds in `App::resumed`.
        let software_presenter_pref =
            software_presenter::WindowsSoftwarePresenterPreference::from_config(
                config.appearance.software_render_mode,
            );
        // No log line here. Whether the software path applies depends on
        // adapter detection, which comes from the renderer — built inside
        // `WindowsShell` below, after this point. Asking the question here
        // meant passing a hardcoded `false`, so under `Auto` (the default) the
        // answer was always "no" regardless of the host's adapter.
        //
        // `app/event_loop.rs` already logs `software-render degrade engaged`
        // with both `detected` and `mode`, at the moment those are real.
        //
        // `forces_opaque_window` below needs no detection: only `Force`
        // overrides the backdrop, and that is a pure config question.
        let backdrop_kind = if software_presenter_pref.forces_opaque_window() {
            // Say so. `software_render_mode = "force"` discards whatever
            // backdrop the user configured, and silently ignoring a setting
            // leaves them reading a config that is not in effect — there is
            // nothing on screen to distinguish "mica was applied" from "mica
            // was overridden". Warn rather than info: the app is declining to
            // honour an explicit choice.
            if config.appearance.backdrop != sonicterm_cfg::config::BackdropKind::Opaque {
                tracing::warn!(
                    configured = ?config.appearance.backdrop,
                    applied = ?sonicterm_cfg::config::BackdropKind::Opaque,
                    "software_render_mode = force overrides the configured backdrop; \
                     the software presenter cannot composite transparency"
                );
            }
            sonicterm_cfg::config::BackdropKind::Opaque
        } else {
            config.appearance.backdrop
        };
        let on_window_ready: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send> =
            Box::new(move |raw| {
                if let raw_window_handle::RawWindowHandle::Win32(h) = raw {
                    let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
                    // SAFETY: winit just supplied this live HWND and the callback
                    // uses it synchronously before the Window is released.
                    unsafe { backdrop::apply_backdrop(hwnd, backdrop_kind) };
                    let mac =
                        // SAFETY: the same winit Window owns `hwnd` through menu installation.
                        unsafe { menubar::WinMenu::new(hwnd) };
                    if let Err(e) = mac.install(Sender::new()) {
                        tracing::error!("WinMenu install failed: {e}");
                    }
                    // RegisterDragDrop is now handled via the unified
                    // OsTabDragBackend::register_window entry point in
                    // App::resumed — to ensure torn-out
                    // child windows go through the same code path.
                } else {
                    tracing::warn!("on_window_ready: not a Win32 handle: {raw:?}");
                }
            });
        let result = {
            // Construct the state machine in the binary and hand it to the
            // platform shell. State mutation routes through the reducer the shell
            // owns, so the binary never reaches into `App`'s field layout.
            let machine =
                sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
            let mut shell = sonicterm_app::shell::WindowsShell::new(machine, theme, config, keymap)
                .with_asset_loaders(theme_loader, keymap_loader)
                .with_on_window_ready(on_window_ready);
            if ole_available {
                let drag_sink =
                    // SAFETY: `ole_guard` proves successful OLE initialization on
                    // this UI thread and remains live until after `shell.run()`.
                    unsafe { os_drag_win::WinOsDragSink::arc() };
                let drag_backend =
                    // SAFETY: the same live `ole_guard` keeps the backend on an
                    // initialized UI thread through `shell.run()`.
                    unsafe { tab_drag_os::WinOsTabDragBackend::boxed() };
                shell = shell.with_os_drag_sink(drag_sink).with_os_drag_backend(drag_backend);
            } else {
                tracing::warn!("OLE unavailable; native tab drag/drop is disabled");
            }
            if let Some(recorder) = breadcrumb_recorder.clone() {
                shell = shell.with_breadcrumb_recorder(recorder);
            }
            if let Some(p) = tearout_payload.or_else(os_drag_win::take_pending_payload) {
                shell = shell.with_pending_payload(p);
            }
            shell.run()
        };
        if result.is_ok() {
            if let Some(recorder) = &breadcrumb_recorder {
                let _ =
                    recorder.record(sonicterm_logging::breadcrumbs::BreadcrumbEvent::Lifecycle(
                        sonicterm_logging::breadcrumbs::LifecycleEvent::CleanShutdown,
                    ));
            }
        }
        if let Some(writer) = breadcrumb_writer {
            let _ = writer.shutdown();
        }
        // Mark clean only after a successful event-loop return and after the
        // breadcrumb worker flushed the clean-shutdown event.
        if result.is_ok() {
            if let Some(session) = session {
                let _ = session.mark_clean();
            }
        }
        result
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Non-Windows targets cannot exercise the Windows shell path
        // (ConPTY, muda, OLE drag, Mica). Keep this branch only so
        // `cargo check --workspace` on non-Win hosts type-checks the
        // bin. Unused bindings:
        let _ = (theme, config, keymap, theme_loader, keymap_loader, session);
        unreachable!("sonicterm-windows binary built for non-Windows target")
    }
}

fn load_config(warnings: &mut Vec<String>) -> Config {
    match Config::default_path() {
        Some(path) => {
            if let Err(e) = Config::ensure_user_config_file(&path) {
                warnings.push(format!(
                    "create default config/examples at {} failed: {e}",
                    path.display()
                ));
            }
            Config::load_or_default_collecting(&path, warnings)
        }
        None => windows_default_config(),
    }
}

pub fn windows_default_config() -> Config {
    Config { keymap: "sonicterm-windows".to_string(), ..Config::default() }
}

fn load_theme(name: &str) -> Theme {
    Theme::load_name_or_default(name, &asset_dir())
}

fn load_keymap(name: &str) -> Keymap {
    if name == "user" {
        if let Some(path) = sonicterm_cfg::keymap::default_user_keymap_path() {
            if sonicterm_cfg::keymap::ensure_user_keymap_file(&path).is_ok() {
                return Keymap::load_or_default(&path);
            }
        }
    }
    Keymap::load_name_or_default(name, &asset_dir())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
