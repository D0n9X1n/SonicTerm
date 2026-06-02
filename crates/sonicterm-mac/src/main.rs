//! SonicTerm Terminal — macOS entry point.

use std::path::PathBuf;

use anyhow::Result;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

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

    let mut cfg_warnings: Vec<String> = Vec::new();
    let config = load_config(&mut cfg_warnings);
    let log_cfg = config.logging.clone();
    let _log_guard = sonicterm_logging::init(&log_cfg).ok();
    sonicterm_logging::cleanup_old_files_async(sonicterm_logging::log_dir(), &log_cfg);
    // Drain any warnings collected during pre-logging Config load so the
    // #522 parse-failure WARN actually reaches sonicterm.log + stderr
    // (Haiku review of PR #534).
    for w in cfg_warnings.drain(..) {
        tracing::warn!(target: "sonicterm-cfg", "{w}");
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "sonic started");
    let theme = load_theme(&config.theme);
    let keymap = load_keymap(&config.keymap);
    // Initial load is infallible (#522 fallback); hot-reload loaders use
    // strict variants so user-visible errors are surfaced after startup.
    let theme_loader: sonicterm_app::ThemeLoader = Box::new(|name: &str| {
        Theme::load_strict(&asset_dir().join("themes").join(format!("{name}.toml")))
    });
    let keymap_loader: sonicterm_app::KeymapLoader = Box::new(|name: &str| {
        Keymap::load_strict(&asset_dir().join("keymaps").join(format!("{name}.toml")))
    });
    #[cfg(target_os = "macos")]
    {
        // The native NSMenu MUST be installed AFTER winit has built
        // the AppKit event loop — installing it before
        // `event_loop.run_app` leaves AppKit with only the default
        // `Apple, sonicterm-mac` menu bar (release-binary smoke caught
        // this on PR #114). The menubar_bridge proxy is installed by
        // `MacShell::run` BEFORE the hook fires, so NSMenu selectors
        // can wake the loop on first click.
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
        // M6b: construct the AppStateMachine in the bin and hand it
        // to the platform shell. State mutation routes through the
        // reducer the shell owns — the bin no longer reaches into
        // the monolithic `App` directly via `run_with_*`.
        let machine =
            sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
        let mut shell = sonicterm_app::shell::MacShell::new(machine, theme, config, keymap)
            .with_asset_loaders(theme_loader, keymap_loader)
            .with_os_drag_sink(os_drag_mac::MacOsDragSink::arc())
            .with_os_drag_backend(tab_drag_os::MacOsTabDragBackend::boxed())
            .with_on_resumed(on_resumed);
        if let Some(p) = pending {
            shell = shell.with_pending_payload(p);
        }
        shell.run()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS targets cannot exercise the macOS shell path
        // (NSMenu, libproc, NSPasteboard). The crate is gated to
        // macOS via Cargo.toml's `[target]` table; this branch only
        // exists so `cargo check --workspace` on non-Mac hosts still
        // type-checks the bin. Unused bindings:
        let _ = (theme, config, keymap, theme_loader, keymap_loader);
        unreachable!("sonicterm-mac binary built for non-macOS target")
    }
}

fn load_config(warnings: &mut Vec<String>) -> Config {
    match Config::default_path() {
        Some(path) => Config::load_or_default_collecting(&path, warnings),
        None => Config::default(),
    }
}

fn load_theme(name: &str) -> Theme {
    let path = asset_dir().join("themes").join(format!("{name}.toml"));
    Theme::load_or_default(&path)
}

fn load_keymap(name: &str) -> Keymap {
    let path = asset_dir().join("keymaps").join(format!("{name}.toml"));
    Keymap::load_or_default(&path)
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
