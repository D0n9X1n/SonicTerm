//! SonicTerm Terminal — macOS entry point.

use std::path::PathBuf;

use anyhow::{Context, Result};
use sonicterm_core::{config::Config, keymap::Keymap, theme::Theme};

#[cfg(target_os = "macos")]
use sonicterm_mac::menubar;
#[cfg(target_os = "macos")]
mod os_drag_mac;
#[cfg(target_os = "macos")]
mod tab_drag_os;

fn main() -> Result<()> {
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
    // Install signal + drop-guard exit tracing immediately after the
    // panic hook so EVERY exit path (panic / signal / clean /
    // LoopExiting / exit_with) leaves a marker in sonicterm.log. See
    // `crates/sonicterm-logging/src/exit_trace.rs` for the full matrix.
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
    let theme = load_theme(&config.theme).context("load theme")?;
    let keymap = load_keymap(&config.keymap).context("load keymap")?;
    let theme_loader: sonicterm_app::ThemeLoader = Box::new(|name: &str| load_theme(name));
    let keymap_loader: sonicterm_app::KeymapLoader = Box::new(|name: &str| load_keymap(name));
    #[cfg(target_os = "macos")]
    {
        // The native NSMenu MUST be installed AFTER winit has built
        // the AppKit event loop — installing it before
        // `event_loop.run_app` leaves AppKit with only the default
        // `Apple, sonicterm-mac` menu bar (release-binary smoke caught
        // this on PR #114). The menubar_bridge proxy is installed by
        // `run_with_os_drag_pending_and_hook` BEFORE the hook fires,
        // so NSMenu selectors can wake the loop on first click.
        //
        // Theme list is built once from the bundled `assets/themes/`
        // directory — adding a theme file requires a restart, matching
        // the rest of the bundled-assets contract.
        let themes_dir = asset_dir().join("themes");
        let themes = menubar::scan_themes(&themes_dir);
        let on_resumed: Box<dyn FnOnce() + Send> = Box::new(move || {
            menubar::install(&themes);
        });
        let pending = os_drag_mac::take_pending_payload();
        if let Some(p) = &pending {
            tracing::info!(tab = %p.tab_title, "os_drag_mac: pending payload at startup; will spawn destination tab");
        }
        sonicterm_app::app::run_with_os_drag_pending_and_window_hook(
            theme,
            config,
            keymap,
            os_drag_mac::MacOsDragSink::arc(),
            Some(theme_loader),
            Some(keymap_loader),
            pending,
            Some(on_resumed),
            None,
            Some(tab_drag_os::MacOsTabDragBackend::boxed()),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        // FUTURE: Win32 menu bar — native Windows menus are usually
        // in-window, so wiring them belongs alongside the Win32
        // chrome work in `sonicterm-windows`. The cross-platform
        // `Action` plumbing + `menubar_bridge` queue are already
        // ready when that lands.
        sonicterm_app::run_with(theme, config, keymap, Some(theme_loader), Some(keymap_loader))
    }
}

fn load_config() -> Result<Config> {
    sonicterm_core::config::migrate_legacy_config_if_needed();
    match Config::default_path() {
        Some(path) => Config::load_or_default(&path),
        None => Ok(Config::default()),
    }
}

fn load_theme(name: &str) -> Result<Theme> {
    let path = asset_dir().join("themes").join(format!("{name}.toml"));
    Theme::load(&path)
}

fn load_keymap(name: &str) -> Result<Keymap> {
    let path = asset_dir().join("keymaps").join(format!("{name}.toml"));
    Keymap::load(&path)
}

/// Bundled assets live next to the binary inside the `.app` bundle.
/// In dev (`cargo run`), fall back to the workspace-root `assets/` dir.
fn asset_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        // `.../SonicTerm.app/Contents/MacOS/sonicterm-mac` → `.../Contents/Resources/assets`
        if let Some(macos) = exe.parent() {
            if let Some(contents) = macos.parent() {
                let bundled = contents.join("Resources").join("assets");
                if bundled.exists() {
                    return bundled;
                }
            }
        }
    }
    // dev fallback
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}
