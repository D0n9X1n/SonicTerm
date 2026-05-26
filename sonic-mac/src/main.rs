//! Sonic Terminal — macOS entry point.

use std::path::PathBuf;

use anyhow::{Context, Result};
use sonic_core::{config::Config, keymap::Keymap, theme::Theme};

#[cfg(target_os = "macos")]
mod os_drag_mac;

fn main() -> Result<()> {
    let config = load_config()?;
    let theme = load_theme(&config.theme).context("load theme")?;
    let keymap = load_keymap(&config.keymap).context("load keymap")?;
    let theme_loader: sonic_shared::ThemeLoader = Box::new(|name: &str| load_theme(name));
    let keymap_loader: sonic_shared::KeymapLoader = Box::new(|name: &str| load_keymap(name));
    #[cfg(target_os = "macos")]
    {
        if let Some(p) = os_drag_mac::take_pending_payload() {
            tracing::info!(tab = %p.tab_title, "os_drag_mac: pending payload at startup");
        }
        sonic_shared::app::run_with_os_drag(
            theme,
            config,
            keymap,
            os_drag_mac::MacOsDragSink::arc(),
            Some(theme_loader),
            Some(keymap_loader),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        sonic_shared::run_with(theme, config, keymap, Some(theme_loader), Some(keymap_loader))
    }
}

fn load_config() -> Result<Config> {
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
        // `.../Sonic.app/Contents/MacOS/sonic` → `.../Contents/Resources/assets`
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets")
}
