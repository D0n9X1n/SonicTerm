//! SonicTerm Terminal — Windows entry point.
//!
//! Hides the console window on release builds so we don't get a stray
//! conhost behind the GPU window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use anyhow::{Context, Result};
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
mod backdrop;
mod cli;
#[cfg(all(target_os = "windows", feature = "harness"))]
mod harness_pipe;
#[cfg(target_os = "windows")]
mod menubar;
#[cfg(target_os = "windows")]
mod os_drag_win;
#[cfg(target_os = "windows")]
mod tab_drag_os;
#[cfg(all(target_os = "windows", feature = "harness"))]
mod win_sid;

fn main() -> Result<()> {
    set_process_dpi_awareness();
    // Install panic hook BEFORE config load so a panic during load
    // still produces a crash dump. Logger init is deferred until
    // after the user's `[logging]` section has been read so its
    // `level` + retention knobs actually drive the runtime —
    // `tracing_subscriber::try_init` only ever installs the first
    // subscriber, so the previous "bootstrap-then-reinit" dance
    // silently dropped the user-configured level (Haiku review of
    // PR #222).
    sonicterm_logging::install_panic_hook(sonicterm_logging::log_dir());
    let bootstrap_cfg = sonicterm_logging::LoggingConfig::default();
    sonicterm_logging::cleanup_old_files_async(sonicterm_logging::log_dir(), &bootstrap_cfg);
    // Exit-path tracing — drop guard + (Unix only) signal handlers.
    // See `crates/sonicterm-logging/src/exit_trace.rs`.
    let _exit_guard = sonicterm_logging::install_exit_logging(&sonicterm_logging::log_dir());

    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sonic: config load failed: {e:?}");
            return Err(e);
        }
    };
    let log_cfg = config.logging.clone();
    let _log_guard = sonicterm_logging::init(&log_cfg).ok();
    sonicterm_logging::cleanup_old_files_async(sonicterm_logging::log_dir(), &log_cfg);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "sonic started");
    #[cfg(target_os = "windows")]
    let parsed_cli = cli::parse_cli_from_env()?;
    #[cfg(target_os = "windows")]
    let tearout_payload = parsed_cli.tearout;
    #[cfg(all(target_os = "windows", feature = "harness"))]
    let harness_request = parsed_cli.harness_input_pipe;
    let theme = load_theme(&config.theme).context("load theme")?;
    let keymap = load_keymap(&config.keymap).context("load keymap")?;
    let theme_loader: sonicterm_app::ThemeLoader = Box::new(|name: &str| load_theme(name));
    let keymap_loader: sonicterm_app::KeymapLoader = Box::new(|name: &str| load_keymap(name));
    #[cfg(target_os = "windows")]
    {
        use sonicterm_app::menu::{PlatformMenu, Sender};
        // Initialize OLE once on the main thread so RegisterDragDrop /
        // DoDragDrop are usable from the same thread that owns the
        // winit HWND.
        os_drag_win::init_ole();
        // Install the muda menubar the instant winit hands us an HWND.
        // muda's `init_for_hwnd` requires the window to exist; the
        // `on_window_ready` hook fires exactly once, right after
        // `el.create_window(...)` succeeds in `App::resumed`.
        let backdrop_kind = config.appearance.backdrop;
        let on_window_ready: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send> =
            Box::new(move |raw| {
                if let raw_window_handle::RawWindowHandle::Win32(h) = raw {
                    let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
                    backdrop::apply_backdrop(hwnd, backdrop_kind);
                    let mac = menubar::WinMenu::new(hwnd);
                    if let Err(e) = mac.install(Sender::new()) {
                        tracing::error!("WinMenu install failed: {e}");
                    }
                    // RegisterDragDrop is now handled via the unified
                    // OsTabDragBackend::register_window entry point in
                    // App::resumed — Haiku #295 fix to ensure torn-out
                    // child windows go through the same code path.
                } else {
                    tracing::warn!("on_window_ready: not a Win32 handle: {raw:?}");
                }
            });
        #[cfg(feature = "harness")]
        let harness_sink_for_app = if let Some(req) = harness_request.as_deref() {
            // #508: construct the shared slot via the App-side helper so
            // both halves agree on the type alias. The Arc is cloned —
            // one clone stays with the pipe-reader thread, the other is
            // installed on the App via `WindowsShell::with_harness_sink`
            // so every active-pane-change publishes the current
            // `PtyHandle::in_tx` into the slot.
            let sink = sonicterm_app::harness::new_sink();
            match harness_pipe::spawn(req, sink.clone()) {
                Ok(name) => tracing::info!(pipe = %name, "harness pipe thread spawned"),
                Err(e) => tracing::error!(error = ?e, "failed to spawn harness pipe"),
            }
            Some(sink)
        } else {
            None
        };
        let result = {
            // M6c: construct the AppStateMachine in the bin and hand
            // it to the platform shell. State mutation routes through
            // the reducer the shell owns — the bin no longer reaches
            // into the monolithic `App` directly via `run_with_*`.
            let machine =
                sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
            let mut shell = sonicterm_app::shell::WindowsShell::new(machine, theme, config, keymap)
                .with_asset_loaders(theme_loader, keymap_loader)
                .with_os_drag_sink(os_drag_win::WinOsDragSink::arc())
                .with_os_drag_backend(tab_drag_os::WinOsTabDragBackend::boxed())
                .with_on_window_ready(on_window_ready);
            #[cfg(feature = "harness")]
            if let Some(sink) = harness_sink_for_app {
                shell = shell.with_harness_sink(sink);
            }
            if let Some(p) = tearout_payload.or_else(os_drag_win::take_pending_payload) {
                shell = shell.with_pending_payload(p);
            }
            shell.run()
        };
        os_drag_win::shutdown_ole();
        result
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Non-Windows targets cannot exercise the Windows shell path
        // (ConPTY, muda, OLE drag, Mica). Keep this branch only so
        // `cargo check --workspace` on non-Win hosts type-checks the
        // bin. Unused bindings:
        let _ = (theme, config, keymap, theme_loader, keymap_loader);
        unreachable!("sonicterm-windows binary built for non-Windows target")
    }
}

fn load_config() -> Result<Config> {
    match Config::default_path() {
        Some(path) => {
            if path.exists() {
                Config::load_or_default(&path)
            } else {
                Ok(windows_default_config())
            }
        }
        None => Ok(windows_default_config()),
    }
}

pub fn windows_default_config() -> Config {
    Config { keymap: "sonicterm-windows".to_string(), ..Config::default() }
}

fn load_theme(name: &str) -> Result<Theme> {
    Theme::load(&asset_dir().join("themes").join(format!("{name}.toml")))
}

fn load_keymap(name: &str) -> Result<Keymap> {
    if name == "user" {
        if let Some(path) = sonicterm_cfg::keymap::default_user_keymap_path() {
            sonicterm_cfg::keymap::ensure_user_keymap_file(&path)?;
            return Keymap::load(&path);
        }
    }
    Keymap::load(&asset_dir().join("keymaps").join(format!("{name}.toml")))
}

/// Installer copies assets next to the .exe; in dev, fall back to workspace.
fn asset_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join("assets");
            if bundled.exists() {
                return bundled;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}
