//! Sonic Terminal — macOS entry point.

use std::path::PathBuf;

use anyhow::{Context, Result};
use sonic_core::{config::Config, keymap::Keymap, theme::Theme};

fn main() -> Result<()> {
    let config = load_config()?;
    let theme = load_theme(&config.theme).context("load theme")?;
    let keymap = load_keymap(&config.keymap).context("load keymap")?;
    sonic_shared::run(theme, config, keymap)
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
