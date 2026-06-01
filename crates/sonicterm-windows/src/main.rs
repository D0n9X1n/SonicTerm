//! Sonic Terminal — Windows entry point.
//!
//! Hides the console window on release builds so we don't get a stray
//! conhost behind the GPU window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use anyhow::{Context, Result};
use sonicterm_core::{config::Config, keymap::Keymap, theme::Theme};

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
#[cfg(target_os = "windows")]
mod menubar;
#[cfg(target_os = "windows")]
mod os_drag_win;
#[cfg(target_os = "windows")]
mod tab_drag_os;

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
    let tearout_payload = cli::parse_tearout_payload_from_env()?;
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
        let result = sonicterm_app::app::run_with_os_drag_pending_and_window_hook(
            theme,
            config,
            keymap,
            os_drag_win::WinOsDragSink::arc(),
            Some(theme_loader),
            Some(keymap_loader),
            tearout_payload.or_else(os_drag_win::take_pending_payload),
            None,
            Some(on_window_ready),
            Some(tab_drag_os::WinOsTabDragBackend::boxed()),
        );
        os_drag_win::shutdown_ole();
        result
    }
    #[cfg(not(target_os = "windows"))]
    {
        sonicterm_app::run_with(theme, config, keymap, Some(theme_loader), Some(keymap_loader))
    }
}

fn load_config() -> Result<Config> {
    sonicterm_core::config::migrate_legacy_config_if_needed();
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
    Config { keymap: "wezterm-windows".to_string(), ..Config::default() }
}

fn load_theme(name: &str) -> Result<Theme> {
    Theme::load(&asset_dir().join("themes").join(format!("{name}.toml")))
}

fn load_keymap(name: &str) -> Result<Keymap> {
    if name == "user" {
        if let Some(path) = sonicterm_core::keymap::default_user_keymap_path() {
            sonicterm_core::keymap::ensure_user_keymap_file(&path)?;
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
