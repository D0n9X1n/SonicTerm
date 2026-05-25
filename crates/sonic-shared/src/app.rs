//! App loop. Owns the window, the GPU renderer, the PTY, and the parser.
//!
//! The render path now actually draws characters to the screen.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use sonic_core::{
    config::Config,
    keymap::Keymap,
    pty::PtyHandle,
    theme::Theme,
    vt::{Parser, VtEvent},
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

use crate::{
    render::GpuRenderer,
    tabs::{Tab, TabBar},
};

/// Entry point used by the platform bin crates.
pub fn run(theme: Theme, config: Config, keymap: Keymap) -> Result<()> {
    init_tracing();
    let event_loop = EventLoop::new().context("create event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(theme, config, keymap);
    event_loop.run_app(&mut app).context("run event loop")?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sonic=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

struct App {
    theme: Theme,
    config: Config,
    keymap: Keymap,
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    parser: Option<Arc<Mutex<Parser>>>,
    pty: Option<PtyHandle>,
    tabs: TabBar,
    modifiers: ModifiersState,
    last_render: Instant,
}

impl App {
    fn new(theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self {
            theme,
            config,
            keymap,
            window: None,
            renderer: None,
            parser: None,
            pty: None,
            tabs: TabBar::new(),
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let cols = self.config.window.cols;
        let rows = self.config.window.rows;

        let attrs = Window::default_attributes()
            .with_title(format!("Sonic Terminal — {}", self.theme.name))
            .with_inner_size(winit::dpi::LogicalSize::new(
                f32::from(cols) * 9.0 + self.config.window.padding * 2.0,
                f32::from(rows) * (self.config.font.size * self.config.font.line_height)
                    + self.config.window.padding * 2.0,
            ));
        let window = Arc::new(el.create_window(attrs).expect("create window"));

        let renderer = GpuRenderer::new(
            window.clone(),
            &self.theme,
            &self.config.font.family,
            self.config.font.size,
            self.config.font.line_height,
            self.config.window.padding,
        )
        .expect("init renderer");

        // Recompute cell counts now that we have real font metrics.
        let (real_cols, real_rows) = renderer.cells();
        let grid = sonic_core::grid::Grid::new(real_cols, real_rows);
        let parser = Arc::new(Mutex::new(Parser::new(grid)));

        match PtyHandle::spawn_default_shell(real_cols, real_rows) {
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
                                if let VtEvent::SetTitle(t) = ev {
                                    window_clone.set_title(&format!("Sonic — {t}"));
                                }
                            }
                            drop(p);
                            window_clone.request_redraw();
                        }
                    })
                    .expect("spawn vt loop");
                self.pty = Some(pty);
            }
            Err(e) => tracing::error!("failed to spawn pty: {e}"),
        }

        self.tabs.push(Tab::new("shell"));
        self.window = Some(window.clone());
        self.renderer = Some(renderer);
        self.parser = Some(parser);
        tracing::info!(
            "Sonic ready. theme={} keymap={} bindings={} grid={}x{}",
            self.theme.name,
            self.keymap.meta.name,
            self.keymap.bindings.len(),
            real_cols,
            real_rows,
        );
        window.request_redraw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),

            WindowEvent::RedrawRequested => {
                if let (Some(r), Some(p)) = (self.renderer.as_mut(), self.parser.as_ref()) {
                    let grid = p.lock();
                    if let Err(e) = r.render(grid.grid(), &self.theme) {
                        tracing::warn!("render error: {e}");
                    }
                    self.last_render = Instant::now();
                }
            }

            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                    let (cols, rows) = r.cells();
                    if let Some(p) = self.parser.as_ref() {
                        p.lock().grid_mut().resize(cols, rows);
                    }
                    if let Some(pty) = self.pty.as_ref() {
                        (pty.resize)(cols, rows);
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(bytes) = encode_key(&event, self.modifiers) {
                    if let Some(pty) = self.pty.as_ref() {
                        let _ = pty.in_tx.send(bytes);
                    }
                }
            }

            _ => {}
        }

        // Heartbeat redraw at most ~60 FPS so cursor and output stay live even
        // when the VT thread is bursty.
        if let Some(w) = &self.window {
            if self.last_render.elapsed() > Duration::from_millis(16) {
                w.request_redraw();
            }
        }
    }
}

/// Translate a winit key event into raw bytes to send to the pty.
fn encode_key(event: &KeyEvent, mods: ModifiersState) -> Option<Vec<u8>> {
    encode_logical(&event.logical_key, mods)
}

/// Pure function — easy to test without constructing a platform `KeyEvent`.
fn encode_logical(key: &Key, mods: ModifiersState) -> Option<Vec<u8>> {
    let ctrl = mods.control_key();
    match key {
        Key::Named(n) => Some(match n {
            NamedKey::Enter => b"\r".to_vec(),
            NamedKey::Backspace => b"\x7f".to_vec(),
            NamedKey::Tab => b"\t".to_vec(),
            NamedKey::Escape => b"\x1b".to_vec(),
            NamedKey::Space => b" ".to_vec(),
            NamedKey::ArrowUp => b"\x1b[A".to_vec(),
            NamedKey::ArrowDown => b"\x1b[B".to_vec(),
            NamedKey::ArrowRight => b"\x1b[C".to_vec(),
            NamedKey::ArrowLeft => b"\x1b[D".to_vec(),
            NamedKey::Home => b"\x1b[H".to_vec(),
            NamedKey::End => b"\x1b[F".to_vec(),
            NamedKey::PageUp => b"\x1b[5~".to_vec(),
            NamedKey::PageDown => b"\x1b[6~".to_vec(),
            NamedKey::Delete => b"\x1b[3~".to_vec(),
            NamedKey::F1 => b"\x1bOP".to_vec(),
            NamedKey::F2 => b"\x1bOQ".to_vec(),
            NamedKey::F3 => b"\x1bOR".to_vec(),
            NamedKey::F4 => b"\x1bOS".to_vec(),
            _ => return None,
        }),
        Key::Character(s) => {
            if ctrl {
                let mut bytes = Vec::with_capacity(1);
                for ch in s.chars() {
                    let lower = ch.to_ascii_lowercase();
                    if lower.is_ascii_lowercase() {
                        bytes.push((lower as u8) - b'a' + 1);
                    } else {
                        bytes.extend(ch.to_string().as_bytes());
                    }
                }
                Some(bytes)
            } else {
                Some(s.as_bytes().to_vec())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use winit::keyboard::SmolStr;

    use super::*;

    #[test]
    fn arrow_keys_emit_csi() {
        assert_eq!(
            encode_logical(&Key::Named(NamedKey::ArrowUp), ModifiersState::empty()).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode_logical(&Key::Named(NamedKey::ArrowLeft), ModifiersState::empty()).unwrap(),
            b"\x1b[D"
        );
    }

    #[test]
    fn enter_emits_cr() {
        assert_eq!(
            encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty()).unwrap(),
            b"\r"
        );
    }

    #[test]
    fn backspace_emits_del() {
        assert_eq!(
            encode_logical(&Key::Named(NamedKey::Backspace), ModifiersState::empty()).unwrap(),
            b"\x7f"
        );
    }

    #[test]
    fn ctrl_c_maps_to_0x03() {
        assert_eq!(
            encode_logical(&Key::Character(SmolStr::new("c")), ModifiersState::CONTROL).unwrap(),
            vec![0x03_u8]
        );
    }

    #[test]
    fn ctrl_letter_range_covers_a_and_z() {
        for (ch, byte) in [('a', 0x01_u8), ('z', 0x1a)] {
            let bytes = encode_logical(
                &Key::Character(SmolStr::new(ch.to_string())),
                ModifiersState::CONTROL,
            )
            .unwrap();
            assert_eq!(bytes, vec![byte]);
        }
    }

    #[test]
    fn plain_letter_passes_through() {
        assert_eq!(
            encode_logical(&Key::Character(SmolStr::new("h")), ModifiersState::empty()).unwrap(),
            b"h"
        );
    }

    #[test]
    fn unknown_named_returns_none() {
        assert!(encode_logical(&Key::Named(NamedKey::Insert), ModifiersState::empty()).is_none());
    }
}
