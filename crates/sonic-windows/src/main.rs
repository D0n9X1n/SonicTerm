//! Sonic Terminal — Windows entry point.
//!
//! Hides the console window on release builds so we don't get a stray
//! conhost behind the GPU window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use anyhow::{Context, Result};
use sonic_core::{config::Config, keymap::Keymap, theme::Theme};

#[cfg(target_os = "windows")]
mod backdrop;
#[cfg(target_os = "windows")]
mod chrome;
#[cfg(target_os = "windows")]
mod menubar;
#[cfg(target_os = "windows")]
mod os_drag_win;

fn main() -> Result<()> {
    // Install panic hook BEFORE config load so a panic during load
    // still produces a crash dump. Logger init is deferred until
    // after the user's `[logging]` section has been read so its
    // `level` + retention knobs actually drive the runtime —
    // `tracing_subscriber::try_init` only ever installs the first
    // subscriber, so the previous "bootstrap-then-reinit" dance
    // silently dropped the user-configured level (Haiku review of
    // PR #222).
    sonic_logging::install_panic_hook(sonic_logging::log_dir());
    let bootstrap_cfg = sonic_logging::LoggingConfig::default();
    sonic_logging::cleanup_old_files_async(sonic_logging::log_dir(), &bootstrap_cfg);

    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sonic: config load failed: {e:?}");
            return Err(e);
        }
    };
    let log_cfg = config.logging.clone();
    let _log_guard = sonic_logging::init(&log_cfg).ok();
    sonic_logging::cleanup_old_files_async(sonic_logging::log_dir(), &log_cfg);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "sonic started");
    let theme = load_theme(&config.theme).context("load theme")?;
    let keymap = load_keymap(&config.keymap).context("load keymap")?;
    let theme_loader: sonic_app::ThemeLoader = Box::new(|name: &str| load_theme(name));
    let keymap_loader: sonic_app::KeymapLoader = Box::new(|name: &str| load_keymap(name));
    #[cfg(target_os = "windows")]
    {
        use sonic_app::menu::{PlatformMenu, Sender};
        // Initialize OLE once on the main thread so RegisterDragDrop /
        // DoDragDrop are usable from the same thread that owns the
        // winit HWND.
        os_drag_win::init_ole();
        // Install the muda menubar the instant winit hands us an HWND.
        // muda's `init_for_hwnd` requires the window to exist; the
        // `on_window_ready` hook fires exactly once, right after
        // `el.create_window(...)` succeeds in `App::resumed`.
        let on_window_ready: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send> =
            Box::new(|raw| {
                if let raw_window_handle::RawWindowHandle::Win32(h) = raw {
                    let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
                    chrome::install_subclass(hwnd);
                    backdrop::apply_backdrop(hwnd);
                    let mac = menubar::WinMenu::new(hwnd);
                    if let Err(e) = mac.install(Sender::new()) {
                        tracing::error!("WinMenu install failed: {e}");
                    }
                    // SAFETY: HWND is alive (winit just created it)
                    // and OLE was initialized above on this same
                    // thread.
                    unsafe { os_drag_win::register_for_window(hwnd) };
                } else {
                    tracing::warn!("on_window_ready: not a Win32 handle: {raw:?}");
                }
            });
        let result = sonic_app::app::run_with_os_drag_pending_and_window_hook(
            theme,
            config,
            keymap,
            os_drag_win::WinOsDragSink::arc(),
            Some(theme_loader),
            Some(keymap_loader),
            os_drag_win::take_pending_payload(),
            None,
            Some(on_window_ready),
        );
        os_drag_win::shutdown_ole();
        result
    }
    #[cfg(not(target_os = "windows"))]
    {
        sonic_app::run_with(theme, config, keymap, Some(theme_loader), Some(keymap_loader))
    }
}

fn load_config() -> Result<Config> {
    match Config::default_path() {
        Some(path) => Config::load_or_default(&path),
        None => Ok(Config::default()),
    }
}

fn load_theme(name: &str) -> Result<Theme> {
    Theme::load(&asset_dir().join("themes").join(format!("{name}.toml")))
}

fn load_keymap(name: &str) -> Result<Keymap> {
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
