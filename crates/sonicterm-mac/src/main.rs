//! SonicTerm Terminal — macOS entry point.

use anyhow::Result;
use sonicterm_cfg::assets::asset_dir;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

#[cfg(target_os = "macos")]
use sonicterm_mac::menubar;
#[cfg(target_os = "macos")]
mod open_documents;
#[cfg(target_os = "macos")]
mod os_drag_mac;
#[cfg(target_os = "macos")]
mod tab_drag_os;

#[cfg(any(target_os = "macos", test))]
fn process_privilege_from_euid(euid: u32) -> sonicterm_app::ProcessPrivilege {
    if euid == 0 {
        sonicterm_app::ProcessPrivilege::Privileged
    } else {
        sonicterm_app::ProcessPrivilege::Unprivileged
    }
}

#[cfg(target_os = "macos")]
fn detect_process_privilege() -> sonicterm_app::ProcessPrivilege {
    let euid =
        // SAFETY: `geteuid` reads process credentials and accepts no pointer or owned resource.
        unsafe { libc::geteuid() };
    process_privilege_from_euid(euid)
}

fn main() -> Result<()> {
    // Install panic hook BEFORE config load so a panic during load
    // still produces a crash dump. Logger init is deferred until
    // after the user's `[logging]` section has been read so its
    // `level` + retention knobs actually drive the runtime —
    // `tracing_subscriber::try_init` only installs the first subscriber;
    // initializing before config load would silently discard the user's level.
    sonicterm_logging::install_panic_hook(sonicterm_logging::log_dir());
    // Install signal + drop-guard exit tracing immediately after the
    // panic hook so EVERY exit path (panic / signal / clean /
    // LoopExiting / exit_with) leaves a marker in sonicterm.log. See
    // `crates/sonicterm-logging/src/exit_trace.rs` for the full matrix.
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
    let theme = load_theme(&config.theme);
    let keymap = load_keymap(&config.keymap);
    // Initial load is infallible; hot-reload loaders use
    // strict variants so user-visible errors are surfaced after startup.
    let theme_loader: sonicterm_app::ThemeLoader =
        Box::new(|name: &str| Theme::load_name_or_path(name, &asset_dir()));
    let keymap_loader: sonicterm_app::KeymapLoader =
        Box::new(|name: &str| Keymap::load_name_or_path(name, &asset_dir()));
    #[cfg(target_os = "macos")]
    {
        // Disable AppKit's native window tab strip for SonicTerm only.
        // This is a process-local NSWindow class setting, not a system
        // preference change; SonicTerm draws its own tab bar.
        // SAFETY: this class method runs on the AppKit thread before any SonicTerm NSWindow is created.
        unsafe {
            let ns_window = objc2::class!(NSWindow);
            let _: () = objc2::msg_send![ns_window, setAllowsAutomaticWindowTabbing: false];
        }
        // The native NSMenu MUST be installed AFTER winit has built
        // the AppKit event loop — installing it before
        // `event_loop.run_app` leaves AppKit with only the default
        // `Apple, sonicterm-mac` menu bar (release-binary smoke caught
        // this). The menubar_bridge proxy is installed by
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
        // Apply AppKit-only per-window setup as soon as winit creates
        // the NSWindow.
        let on_window_ready: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send> =
            Box::new(|raw| {
                if let raw_window_handle::RawWindowHandle::AppKit(h) = raw {
                    // h.ns_view is `NonNull<c_void>` pointing at an NSView*.
                    let view: *mut objc2::runtime::AnyObject = h.ns_view.as_ptr().cast();
                    // SAFETY: winit supplied a live main-thread NSView; the returned NSWindow is used synchronously and null-checked.
                    unsafe {
                        let window: *mut objc2::runtime::AnyObject = objc2::msg_send![view, window];
                        if !window.is_null() {
                            let _: () = objc2::msg_send![window, setTabbingMode: 2isize];
                        }
                    }
                } else {
                    tracing::warn!("on_window_ready: not an AppKit handle: {raw:?}");
                }
            });
        let pending = os_drag_mac::take_pending_payload();
        if let Some(p) = &pending {
            tracing::info!(tab = %p.tab_title, "os_drag_mac: pending payload at startup; will spawn destination tab");
        }
        // Construct the state machine in the binary and hand it to the
        // platform shell. State mutation routes through the reducer the shell
        // owns, so the binary never reaches into `App`'s field layout.
        open_documents::install();
        let machine =
            sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
        let process_privilege = detect_process_privilege();
        tracing::info!(
            privileged = process_privilege.is_privileged(),
            "process privilege observed"
        );
        let mut shell = sonicterm_app::shell::MacShell::new(machine, theme, config, keymap)
            .with_process_privilege(process_privilege)
            .with_asset_loaders(theme_loader, keymap_loader)
            .with_os_drag_sink(os_drag_mac::MacOsDragSink::arc())
            .with_os_drag_backend(tab_drag_os::MacOsTabDragBackend::boxed())
            .with_on_resumed(on_resumed)
            .with_on_window_ready(on_window_ready);
        if let Some(recorder) = breadcrumb_recorder.clone() {
            shell = shell.with_breadcrumb_recorder(recorder);
        }
        if let Some(p) = pending {
            shell = shell.with_pending_payload(p);
        }
        let outcome = shell.run();
        if outcome.is_ok() {
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
        if outcome.is_ok() {
            if let Some(session) = session {
                let _ = session.mark_clean();
            }
        }
        outcome
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
        Some(path) => {
            if let Err(e) = Config::ensure_user_config_file(&path) {
                warnings.push(format!(
                    "create default config/examples at {} failed: {e}",
                    path.display()
                ));
            }
            Config::load_or_default_collecting(&path, warnings)
        }
        None => Config::default(),
    }
}

fn load_theme(name: &str) -> Theme {
    Theme::load_name_or_default(name, &asset_dir())
}

fn load_keymap(name: &str) -> Keymap {
    Keymap::load_name_or_default(name, &asset_dir())
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
