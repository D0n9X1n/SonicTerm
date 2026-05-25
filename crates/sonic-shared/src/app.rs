//! App loop. Minimal viable: opens a window, spawns a pty, parses output
//! into a grid, and logs activity. GPU rendering is wired but renders a
//! solid theme background — character-rendering hookup is a follow-up PR
//! (the engine produces a `Grid` snapshot every frame ready to be drawn).

use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use sonic_core::{config::Config, keymap::Keymap, pty::PtyHandle, theme::Theme, vt::Parser};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use crate::tabs::{Tab, TabBar};

/// Entry point used by the platform bin crates.
pub fn run(theme: Theme, config: Config, keymap: Keymap) -> Result<()> {
    init_tracing();
    let event_loop = EventLoop::new().context("create event loop")?;
    let mut app = App::new(theme, config, keymap);
    event_loop.run_app(&mut app).context("run event loop")?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sonic=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

struct App {
    theme: Theme,
    config: Config,
    keymap: Keymap,
    window: Option<Arc<Window>>,
    parser: Option<Arc<Mutex<Parser>>>,
    pty: Option<PtyHandle>,
    tabs: TabBar,
}

impl App {
    fn new(theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self {
            theme,
            config,
            keymap,
            window: None,
            parser: None,
            pty: None,
            tabs: TabBar::new(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(format!("Sonic Terminal — {}", self.theme.name))
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.config.window.cols as f32 * 8.0 + self.config.window.padding * 2.0,
                self.config.window.rows as f32 * 17.0 + 40.0 + self.config.window.padding * 2.0,
            ));
        let window = Arc::new(el.create_window(attrs).expect("create window"));

        let grid = sonic_core::grid::Grid::new(self.config.window.cols, self.config.window.rows);
        let parser = Arc::new(Mutex::new(Parser::new(grid)));

        match PtyHandle::spawn_default_shell(self.config.window.cols, self.config.window.rows) {
            Ok(pty) => {
                let parser_clone = parser.clone();
                let out_rx = pty.out_rx.clone();
                let window_clone = window.clone();
                std::thread::Builder::new()
                    .name("sonic-vt-loop".into())
                    .spawn(move || {
                        while let Ok(bytes) = out_rx.recv() {
                            let mut p = parser_clone.lock();
                            for ev in p.advance(&bytes) {
                                if let sonic_core::vt::VtEvent::SetTitle(t) = ev {
                                    window_clone.set_title(&format!("Sonic — {t}"));
                                }
                            }
                            window_clone.request_redraw();
                        }
                    })
                    .expect("spawn vt loop");
                self.pty = Some(pty);
            }
            Err(e) => tracing::error!("failed to spawn pty: {e}"),
        }

        self.tabs.push(Tab::new("shell"));
        self.window = Some(window);
        self.parser = Some(parser);
        tracing::info!(
            "Sonic ready. theme={} keymap={} bindings={}",
            self.theme.name,
            self.keymap.meta.name,
            self.keymap.bindings.len()
        );
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                // GPU draw will land in next PR. For now log a heartbeat.
                tracing::trace!("redraw requested");
            }
            WindowEvent::Resized(_) => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}
