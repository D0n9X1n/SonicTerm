//! `App::do_resumed` / `do_user_event` / `do_new_events` /
//! `do_about_to_wait` — extracted from the `ApplicationHandler` trait impl
//! in refactor PR 8b.
//!
//! The trait methods in `mod.rs` are 1-line delegators that call into
//! these `impl App` methods. Splitting the bodies out of the trait impl
//! lets us keep the event-loop logic in its own file without breaking
//! the trait-impl rule that all methods must live in one `impl` block.

#![allow(unused_imports)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use sonic_shared::render::GpuRenderer;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{mark_all_panes_dirty, with_integrated_titlebar, App, UserEvent};
use crate::app::integrated_titlebar_inset;
use crate::config_watch::ConfigWatcher;
use winit::event_loop::ControlFlow;

impl App {
    pub(super) fn do_about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Schedule the next blink-only redraw via `WaitUntil(..)`
        // rather than `request_redraw()` from inside the render path
        // (which produced the tight redraw loop flagged on PR #81).
        // The renderer hands us the exact instant of the next phase
        // bucket boundary; fall back to `Wait` when blinking is off,
        // the window is unfocused, or no renderer exists. Explicitly
        // resetting to `Wait` (rather than leaving the previous
        // `WaitUntil` in place) is what keeps idle headless CPU at
        // ~0% — otherwise an unfocused window would keep waking at
        // 26Hz forever (regression: `scripts/bench_headless_gui.sh`
        // reported 17% idle CPU before this gate).
        let mut next: Option<std::time::Instant> = None;
        // Perf audit #9: if a redraw was deferred for vsync pacing,
        // schedule the next wake at the upcoming frame boundary. This
        // takes priority over (and is bounded by) the blink deadline:
        // typing latency must still feel instant, and a deferred
        // redraw at frame_period in the future is the tightest budget
        // that still preserves vsync alignment.
        if self.pending_redraw {
            next = Some(self.last_render + self.frame_period);
        }
        if let Some(r) = self.renderer.as_ref() {
            if self.cursor_visible.load(std::sync::atomic::Ordering::Relaxed) {
                let blink = r.next_blink_redraw_at();
                next = match (next, blink) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
        }
        if self.prefs_toggle_anim_in_flight() {
            let frame = Instant::now() + Duration::from_millis(16);
            next = Some(next.map_or(frame, |at| at.min(frame)));
        }
        match next {
            Some(at) => el.set_control_flow(ControlFlow::WaitUntil(at)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    pub(super) fn do_new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        // When our `WaitUntil(..)` timer expires, winit fires
        // `NewEvents(ResumeTimeReached)` and then nothing else unless
        // we explicitly ask. Request a redraw so the blink animation
        // actually advances to the next phase bucket. PR #81 review.
        // Perf audit #9: this same wakeup also services a vsync-paced
        // redraw deferred by the RedrawRequested handler — we clear
        // `pending_redraw` here so the next render call services it
        // and stale flags can't keep the loop hot.
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            if self.prefs_toggle_anim_in_flight() {
                if let Some(w) = &self.prefs_window {
                    w.request_redraw();
                }
            }
        }
    }

    pub(super) fn do_user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        // Watcher-thread wake. Drain the channel and apply any new
        // config immediately so the reload doesn't sit queued until
        // the next OS event arrives. apply_new_config already
        // request_redraw()s every live window.
        match event {
            UserEvent::ConfigChanged => self.poll_config_reload(),
            UserEvent::MenuAction => self.drain_menubar_actions(el),
            UserEvent::OsDrag => self.drain_os_drag(),
        }
        // Any path above that ran an action may have set
        // `pending_prefs_open` — make sure the prefs window actually
        // materializes regardless of the dispatch source.
        self.drain_pending_window_creates(el);
    }

    pub(super) fn do_resumed(&mut self, el: &ActiveEventLoop) {
        // Fire the one-shot post-resume hook before any window work.
        // macOS uses this slot to install the native NSMenu — by now
        // winit has built the AppKit event loop, so `setMainMenu`
        // sticks. Installing it before `run_app` left AppKit with only
        // the default `Apple, sonic-mac` menubar (bug caught by the
        // PR #114 release-binary smoke).
        if let Some(hook) = self.on_resumed.take() {
            hook();
        }

        let cols = self.config.window.cols;
        let rows = self.config.window.rows;

        let attrs = with_integrated_titlebar(
            Window::default_attributes()
                .with_title(format!("Sonic Terminal — {}", self.theme.name))
                .with_inner_size(winit::dpi::LogicalSize::new(
                    f32::from(cols) * 9.0
                        + self.config.window.padding_left
                        + self.config.window.padding_right,
                    f32::from(rows) * (self.config.font.size * self.config.font.line_height)
                        + self.config.window.padding_top
                        + self.config.window.padding_bottom
                        + sonic_ui::tabbar_view::TAB_BAR_HEIGHT,
                )),
        );
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        // Enable IME so CJK input methods (Pinyin, Japanese, Korean…) can
        // deliver preedit + commit events instead of raw keystrokes.
        window.set_ime_allowed(true);
        self.scale_factor = window.scale_factor();

        // Perf audit #9: gate redraws to the monitor's vsync cadence.
        // `refresh_rate_millihertz` returns e.g. 60_000 for 60Hz,
        // 120_000 for 120Hz ProMotion, etc. A zero or absent value
        // means winit could not determine it (headless, virtual
        // display) — fall back to the 60Hz default seeded by `new`.
        if let Some(monitor) = window.current_monitor() {
            if let Some(mhz) = monitor.refresh_rate_millihertz() {
                if mhz > 0 {
                    // period_us = 1_000_000_000 / mhz
                    let period_us = 1_000_000_000u64 / u64::from(mhz);
                    self.frame_period = Duration::from_micros(period_us);
                    tracing::debug!(
                        "vsync pacing: monitor reports {}.{:03} Hz, frame period {:?}",
                        mhz / 1000,
                        mhz % 1000,
                        self.frame_period,
                    );
                }
            }
        }

        let mut renderer = GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            &self.config.font.family,
            self.config.font.size,
            self.config.font.line_height,
            [
                self.config.window.padding_left,
                self.config.window.padding_right,
                self.config.window.padding_top,
                self.config.window.padding_bottom,
            ],
        )
        .expect("init renderer");
        // Seed cursor visuals from config so the very first frame draws
        // the user-selected shape rather than the default. Subsequent
        // edits to sonic.toml take effect through the config-watch hook
        // (see apply_config below).
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);

        self.window = Some(window.clone());
        // Fire the one-shot window-ready hook (Windows uses this slot
        // to install the muda menubar against the HWND). Best-effort:
        // if the platform can't surface a raw handle, skip the hook
        // and log — the rest of the app still runs.
        if let Some(hook) = self.on_window_ready.take() {
            use raw_window_handle::HasWindowHandle;
            match window.window_handle() {
                Ok(h) => hook(h.as_raw()),
                Err(e) => tracing::warn!("on_window_ready: no raw handle: {e}"),
            }
        }
        self.renderer = Some(renderer);
        if let Some(r) = self.renderer.as_mut() {
            r.set_titlebar_inset(integrated_titlebar_inset());
            // Apply the user's `tab_close_button_color` from sonic.toml
            // BEFORE the first frame so a custom always-visible × shows
            // up on the very first paint, not after a config edit.
            r.set_tab_close_override(self.config.tab_close_button_color.as_deref());
        }

        // Seed the first tab + pane now that the window + renderer exist.
        self.new_tab("shell");

        let (rc, rr) = self.renderer.as_ref().map(|r| r.cells()).unwrap_or((0, 0));
        tracing::info!(
            "Sonic ready. theme={} keymap={} bindings={} grid={}x{}",
            self.theme.name,
            self.keymap.meta.name,
            self.keymap.bindings.len(),
            rc,
            rr,
        );
        window.request_redraw();

        // Spawn the sonic.toml live-reload watcher (best-effort; if the
        // user has no config path or the parent dir is unreadable, the
        // app still runs — just without live reload).
        if self.config_watcher.is_none() {
            if let Some(path) = sonic_core::config::Config::default_path() {
                let proxy = self.event_loop_proxy.clone();
                let spawn_result = if let Some(p) = proxy {
                    ConfigWatcher::spawn_with_wake(path.clone(), move || {
                        // Failure here means the event loop has shut
                        // down — nothing to wake, safe to ignore.
                        let _ = p.send_event(UserEvent::ConfigChanged);
                    })
                } else {
                    // No proxy (test harness) — fall back to the
                    // poll-only behavior; the watcher still delivers,
                    // it just won't wake an idle loop.
                    ConfigWatcher::spawn(path.clone())
                };
                match spawn_result {
                    Ok(w) => {
                        tracing::info!("config watcher: watching {path:?}");
                        self.config_watcher = Some(w);
                    }
                    Err(e) => tracing::warn!("config watcher disabled: {e:#}"),
                }
            }
        }
    }
}
