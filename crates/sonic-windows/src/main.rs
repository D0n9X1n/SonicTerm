//! Sonic Terminal — Windows entry point.
//!
//! Hides the console window on release builds so we don't get a stray
//! conhost behind the GPU window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

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
