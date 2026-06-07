//! Platform shells that drive the winit event loop on top of
//! [`sonicterm_app_core::AppStateMachine`].
//!
//! M6b lands [`MacShell`]: the macOS bin (`crates/sonicterm-mac`)
//! constructs the state machine itself, then hands it to
//! `MacShell::new(...)` and calls `.run()`. The shell is the only
//! place winit / wgpu / AppKit glue lives — the bin crate no longer
//! touches the legacy monolithic `App` directly for state mutation.
//!
//! M6c lands [`WindowsShell`] along the same lines: the Windows bin
//! (`crates/sonicterm-windows`) constructs the state machine, then
//! hands it to `WindowsShell::new(...)` and calls `.run()`. The
//! Windows variant carries an extra `with_on_window_ready` hook that
//! receives the `raw_window_handle::RawWindowHandle` of the first
//! winit window — used by the bin to install the muda menubar +
//! apply DWM backdrop on the bare HWND, both of which need the
//! handle that only exists after `create_window` succeeds.
//!
//! Today the shell still delegates the actual event loop to the
//! existing [`crate::app::App`] (which itself dispatches Intents
//! through the held machine — wired in M6a-expand-2b/2c). The
//! difference is that the machine is now constructed *by the shell*
//! and passed in via [`crate::app::App::new_with_proxy_and_machine`],
//! so the bin never has to know about `App`'s field layout. Once
//! every per-Intent path is fully reduced through the machine (post
//! M6c/d) the `App` indirection collapses and the shell will drive
//! the loop directly.

use std::sync::Arc;

use anyhow::{Context, Result};
use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::os_drag::OsTabDragBackend;
use crate::app::{App, KeymapLoader, ThemeLoader, UserEvent};
use crate::os_drag::{OsDragSink, TabPayload};
use sonicterm_app_core::AppStateMachine;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
/// macOS platform shell. Owns the externally-built
/// [`AppStateMachine`] and the winit event loop; translates winit
/// events into Intents (via the embedded `App` dispatcher) and
/// consumes the returned `AppEffect` batch through the existing
/// renderer / clipboard / PTY plumbing.
///
/// Constructed by `crates/sonicterm-mac/src/main.rs`. Builder-style:
/// every optional hook has a `with_*` setter; `.run()` consumes the
/// shell and blocks until the event loop exits.
pub struct MacShell {
    machine: AppStateMachine,
    theme: Theme,
    config: Config,
    keymap: Keymap,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
    os_drag_sink: Option<Arc<dyn OsDragSink>>,
    os_drag_backend: Option<Box<dyn OsTabDragBackend>>,
    pending: Option<TabPayload>,
    on_resumed: Option<Box<dyn FnOnce() + Send>>,
    /// #554: one-shot hook fired the instant `create_window` returns
    /// with the raw AppKit window handle. The mac bin uses this slot
    /// to apply AppKit-only per-window setup. Mirrors
    /// [`WindowsShell::with_on_window_ready`].
    on_window_ready: Option<Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>>,
}

impl MacShell {
    /// Build a shell wrapping the caller-constructed state machine.
    /// The bin (`sonicterm-mac::main`) builds the machine via
    /// `AppStateMachine::new(AppState::default())` first, then hands
    /// it here — the shell never silently creates a parallel one.
    #[must_use]
    pub fn new(machine: AppStateMachine, theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self {
            machine,
            theme,
            config,
            keymap,
            theme_loader: None,
            keymap_loader: None,
            os_drag_sink: None,
            os_drag_backend: None,
            pending: None,
            on_resumed: None,
            on_window_ready: None,
        }
    }

    /// Install loaders so live theme/keymap reload works.
    #[must_use]
    pub fn with_asset_loaders(
        mut self,
        theme_loader: ThemeLoader,
        keymap_loader: KeymapLoader,
    ) -> Self {
        self.theme_loader = Some(theme_loader);
        self.keymap_loader = Some(keymap_loader);
        self
    }

    /// Install the platform OS-drag sink (NSPasteboard on mac).
    #[must_use]
    pub fn with_os_drag_sink(mut self, sink: Arc<dyn OsDragSink>) -> Self {
        self.os_drag_sink = Some(sink);
        self
    }

    /// Install the OS-level drag-session backend (NSDraggingSession on mac).
    #[must_use]
    pub fn with_os_drag_backend(mut self, backend: Box<dyn OsTabDragBackend>) -> Self {
        self.os_drag_backend = Some(backend);
        self
    }

    /// Seed an already-received tab payload (a tear-out from another
    /// SonicTerm process found on the pasteboard at startup).
    #[must_use]
    pub fn with_pending_payload(mut self, pending: TabPayload) -> Self {
        self.pending = Some(pending);
        self
    }

    /// One-shot hook fired at the top of the first `resumed` tick —
    /// the mac bin uses it to install the native NSMenu once winit
    /// has built the AppKit event loop.
    #[must_use]
    pub fn with_on_resumed(mut self, hook: Box<dyn FnOnce() + Send>) -> Self {
        self.on_resumed = Some(hook);
        self
    }

    /// #554: one-shot hook fired the instant `create_window` returns,
    /// with the raw AppKit window handle. Mirrors
    /// [`WindowsShell::with_on_window_ready`] — same signature, same
    /// plumbing into the cross-platform firing site in
    /// `app/event_loop.rs`.
    #[must_use]
    pub fn with_on_window_ready(
        mut self,
        hook: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>,
    ) -> Self {
        self.on_window_ready = Some(hook);
        self
    }

    /// Consume the shell, build the winit event loop, install the
    /// menubar / OS-drag bridges, and run until the loop exits.
    pub fn run(self) -> Result<()> {
        let MacShell {
            machine,
            theme,
            config,
            keymap,
            theme_loader,
            keymap_loader,
            os_drag_sink,
            os_drag_backend,
            pending,
            on_resumed,
            on_window_ready,
        } = self;

        crate::app::init_tracing_public();
        let event_loop =
            EventLoop::<UserEvent>::with_user_event().build().context("create event loop")?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        crate::menubar_bridge::install_proxy(proxy.clone());
        crate::os_drag_bridge::install_proxy(proxy.clone());

        let mut app = App::new_with_proxy_and_machine(theme, config, keymap, Some(proxy), machine);
        app.theme_loader = theme_loader;
        app.keymap_loader = keymap_loader;
        if let Some(sink) = os_drag_sink {
            app.os_drag_sink = Some(sink);
        }
        if let Some(b) = os_drag_backend {
            app.set_os_drag_backend(b);
        }
        if let Some(hook) = on_resumed {
            app.on_resumed = Some(hook);
        }
        if let Some(hook) = on_window_ready {
            app.set_on_window_ready(hook);
        }
        if let Some(p) = pending {
            let _ = app.new_tab_from_payload(&p);
        }

        event_loop.run_app(&mut app).context("run event loop")?;
        Ok(())
    }
}

/// Windows platform shell. Symmetric peer of [`MacShell`] — owns the
/// caller-built [`AppStateMachine`] and the winit event loop, and
/// drives the existing renderer / PTY plumbing through the embedded
/// `App` dispatcher. Adds [`WindowsShell::with_on_window_ready`] for
/// the muda menubar + DWM backdrop install which need the bare
/// HWND that only exists after `create_window` succeeds.
///
/// Constructed by `crates/sonicterm-windows/src/main.rs`. Builder
/// style — every optional hook has a `with_*` setter; `.run()`
/// consumes the shell and blocks until the event loop exits.
pub struct WindowsShell {
    machine: AppStateMachine,
    theme: Theme,
    config: Config,
    keymap: Keymap,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
    os_drag_sink: Option<Arc<dyn OsDragSink>>,
    os_drag_backend: Option<Box<dyn OsTabDragBackend>>,
    pending: Option<TabPayload>,
    on_window_ready: Option<Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>>,
}

impl WindowsShell {
    /// Build a shell wrapping the caller-constructed state machine.
    /// The bin (`sonicterm-windows::main`) builds the machine via
    /// `AppStateMachine::new(AppState::default())` first, then hands
    /// it here — the shell never silently creates a parallel one.
    #[must_use]
    pub fn new(machine: AppStateMachine, theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self {
            machine,
            theme,
            config,
            keymap,
            theme_loader: None,
            keymap_loader: None,
            os_drag_sink: None,
            os_drag_backend: None,
            pending: None,
            on_window_ready: None,
        }
    }

    /// Install loaders so live theme/keymap reload works.
    #[must_use]
    pub fn with_asset_loaders(
        mut self,
        theme_loader: ThemeLoader,
        keymap_loader: KeymapLoader,
    ) -> Self {
        self.theme_loader = Some(theme_loader);
        self.keymap_loader = Some(keymap_loader);
        self
    }

    /// Install the platform OS-drag sink (OLE drop target on Windows).
    #[must_use]
    pub fn with_os_drag_sink(mut self, sink: Arc<dyn OsDragSink>) -> Self {
        self.os_drag_sink = Some(sink);
        self
    }

    /// Install the OS-level drag-session backend (OLE DoDragDrop on Windows).
    #[must_use]
    pub fn with_os_drag_backend(mut self, backend: Box<dyn OsTabDragBackend>) -> Self {
        self.os_drag_backend = Some(backend);
        self
    }

    /// Seed an already-received tab payload (a tear-out from another
    /// SonicTerm process received via env / pasteboard at startup).
    #[must_use]
    pub fn with_pending_payload(mut self, pending: TabPayload) -> Self {
        self.pending = Some(pending);
        self
    }

    /// One-shot hook fired the instant `create_window` returns, with
    /// the raw `HWND` handle. The Windows bin uses this slot to
    /// install the muda menubar + apply DWM backdrop — both require
    /// the HWND that only exists after winit has built the window.
    #[must_use]
    pub fn with_on_window_ready(
        mut self,
        hook: Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>,
    ) -> Self {
        self.on_window_ready = Some(hook);
        self
    }

    /// Consume the shell, build the winit event loop, install the
    /// OS-drag + window-ready bridges, and run until the loop exits.
    pub fn run(self) -> Result<()> {
        let WindowsShell {
            machine,
            theme,
            config,
            keymap,
            theme_loader,
            keymap_loader,
            os_drag_sink,
            os_drag_backend,
            pending,
            on_window_ready,
        } = self;

        crate::app::init_tracing_public();
        let event_loop =
            EventLoop::<UserEvent>::with_user_event().build().context("create event loop")?;
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        // Same bridges as MacShell: cheap + safe on Windows — the
        // menubar bridge proxy is harmless if NSMenu never fires
        // (it won't on Win32), and the OS-drag bridge proxy is
        // required so OLE drop callbacks can wake the loop.
        crate::menubar_bridge::install_proxy(proxy.clone());
        crate::os_drag_bridge::install_proxy(proxy.clone());

        let mut app = App::new_with_proxy_and_machine(theme, config, keymap, Some(proxy), machine);
        app.theme_loader = theme_loader;
        app.keymap_loader = keymap_loader;
        if let Some(sink) = os_drag_sink {
            app.os_drag_sink = Some(sink);
        }
        if let Some(b) = os_drag_backend {
            app.set_os_drag_backend(b);
        }
        if let Some(hook) = on_window_ready {
            app.set_on_window_ready(hook);
        }
        if let Some(p) = pending {
            let _ = app.new_tab_from_payload(&p);
        }

        event_loop.run_app(&mut app).context("run event loop")?;
        Ok(())
    }
}
