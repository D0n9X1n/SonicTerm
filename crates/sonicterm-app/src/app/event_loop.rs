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
use sonicterm_gpu::core::GpuRenderer;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{mark_all_panes_dirty, window_dpi, with_integrated_titlebar, App, UserEvent};
use crate::config_watch::ConfigWatcher;
use sonicterm_ui::selection::SelectMode;
use winit::event_loop::ControlFlow;

impl App {
    pub(super) fn expire_notifications(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for ws in self.windows.values_mut() {
            let Some(expires_at) = ws.notification.as_ref().and_then(|bubble| bubble.expires_at) else {
                continue;
            };
            if expires_at <= now {
                ws.notification = None;
                ws.request_redraw();
            } else {
                next = Some(next.map_or(expires_at, |cur| cur.min(expires_at)));
            }
        }
        next
    }

    pub(super) fn do_about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Deferred-exit drain: `run_action` (keymap dispatcher) sets
        // `pending_exit` when the user's Cmd+W chain has just closed
        // the last tab of the last window in
        // `quit_on_last_window_close = true` mode. The dispatcher does
        // not have an `ActiveEventLoop` handle, so honoring it here is
        // the first opportunity to call `el.exit()`.
        if self.pending_exit {
            self.pending_exit = false;
            el.exit();
            return;
        }
        let notification_wake = self.expire_notifications(Instant::now());
        // Schedule the next blink-only redraw via `WaitUntil(..)`
        // rather than `request_redraw()` from inside the render path
        // (which produced the tight redraw loop flagged on PR #81).
        // The renderer hands us the exact instant of the next phase
        // bucket boundary; fall back to `Wait` when blinking is off,
        // the window is unfocused, or no renderer exists. Explicitly
        // resetting to `Wait` (rather than leaving the previous
        // `WaitUntil` in place) is what keeps idle CPU near zero —
        // otherwise an unfocused window would keep waking at 26Hz.
        let mut next: Option<std::time::Instant> = notification_wake;
        // Perf audit #9: if a redraw was deferred for vsync pacing,
        // schedule the next wake at the upcoming frame boundary. This
        // takes priority over (and is bounded by) the blink deadline:
        // typing latency must still feel instant, and a deferred
        // redraw at frame_period in the future is the tightest budget
        // that still preserves vsync alignment.
        if self.pending_redraw {
            if let Some(last_render) = self.main().map(|ws| ws.last_render) {
                // Match the RedrawRequested gate: under IME composition on the
                // software path the cap is lower, so schedule the wake at the
                // same (possibly longer) period — otherwise we'd wake at 33ms,
                // re-defer, and busy-spin (issue #714).
                let composing = self.main().map(|ws| ws.ime.is_composing()).unwrap_or(false);
                let period = crate::app::effective_frame_period(
                    self.software_render_degrade,
                    composing,
                    self.frame_period,
                );
                next = Some(last_render + period);
            }
        }
        // Issue #43: same vsync-pacing schedule for any CHILD window that
        // deferred a redraw (PTY-streaming gate or lock-contention
        // backoff). Each torn-out child keys off its own
        // `WindowState.last_render`, so fold each pending child's next
        // frame boundary into the wake deadline. Stale ids (window
        // reaped) are skipped here and pruned in `new_events`.
        for win_id in &self.pending_redraw_windows {
            if let Some(ws) = self.windows.get(win_id) {
                // Mirror the child gate's composing-aware period (issue #714).
                let period = crate::app::effective_frame_period(
                    self.software_render_degrade,
                    ws.ime.is_composing(),
                    self.frame_period,
                );
                let at = ws.last_render + period;
                next = Some(next.map_or(at, |cur| cur.min(at)));
            }
        }
        if let Some(r) = self.main_renderer() {
            // PR #400: cursor_visible is per-pane — read from the
            // active pane of the active tab so the DECTCEM flag
            // survives tear-out.
            let cursor_visible = self
                .main()
                .and_then(|ws| {
                    let i = ws.tabs.active_index();
                    let active_id = ws.tab_states.get(i).map(|t| t.active_pane)?;
                    ws.panes
                        .get(&active_id)
                        .map(|p| p.cursor_visible.load(std::sync::atomic::Ordering::Relaxed))
                })
                .unwrap_or(true);
            if cursor_visible {
                let blink = r.next_blink_redraw_at();
                next = match (next, blink) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
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
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
            // Issue #43: also re-request the redraw on every CHILD window
            // that deferred one (vsync gate or lock-contention backoff).
            // We do NOT clear an entry on request — exactly like the main
            // window's `pending_redraw`, the marker is cleared when the
            // child actually renders past the gate in
            // `handle_child_window_event`. Take the set out to avoid
            // borrowing `self.windows` and `self.pending_redraw_windows`
            // at once, prune ids whose window was reaped (so the set can't
            // leak / wake the loop forever), then put the survivors back.
            let pending = std::mem::take(&mut self.pending_redraw_windows);
            self.pending_redraw_windows = pending
                .into_iter()
                .filter(|win_id| match self.windows.get(win_id) {
                    Some(ws) => {
                        ws.request_redraw();
                        true
                    }
                    None => false,
                })
                .collect();
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
            UserEvent::DragMoved => {
                let _ = self.handle_os_drag_moved();
            }
            UserEvent::DragEnded => {
                let _ = self.handle_os_drag_ended();
            }
            UserEvent::ClearShapeCache => self.handle_clear_shape_cache(),
            UserEvent::UpdateCheckFinished { level, message } => {
                self.show_notification_for_kind(self.frontmost_kind(), level, message);
            }
        }
        // Any path above that ran an action may have requested a new
        // top-level window; create it now that we have an ActiveEventLoop.
        self.drain_pending_window_creates(el);
        // Issue #462 (speculative defensive fix): drain deferred
        // OS-drag teardown AFTER `drain_pending_window_creates` so any
        // tear-out-spawn from the `DroppedOnEmpty` branch has produced
        // its new window before cross-window drag-residue cleanup
        // runs. Ordering is the entire point — do not move above.
        self.drain_pending_os_teardown();
    }

    /// Drain a `UserEvent::ClearShapeCache` (Epic #300 P4 follow-up):
    /// an async font fallback family just landed in
    /// [`sonicterm_text::async_fallback::AsyncFallbackLoader`]. Clear every
    /// live renderer's shape / row / line caches (bumping `style_rev`)
    /// and request a redraw on every live window. The next frame
    /// re-walks the fallback chain and the user's tofu cells flip to
    /// real glyphs.
    pub(super) fn handle_clear_shape_cache(&mut self) {
        // PR-B1b: main window lives in `self.windows` with `renderer=Some`,
        // so a single iteration covers main + all torn-out children.
        for child in self.windows.values_mut() {
            if let Some(r) = child.renderer.as_mut() {
                r.clear_shape_cache();
                child.request_redraw();
            }
        }
    }

    pub(super) fn do_resumed(&mut self, el: &ActiveEventLoop) {
        // Fire the one-shot post-resume hook before any window work.
        // macOS uses this slot to install the native NSMenu — by now
        // winit has built the AppKit event loop, so `setMainMenu`
        // sticks. Installing it before `run_app` left AppKit with only
        // the default `Apple, sonicterm-mac` menubar (bug caught by the
        // PR #114 release-binary smoke).
        if let Some(hook) = self.on_resumed.take() {
            hook();
        }

        let cols = self.config.window.cols;
        let rows = self.config.window.rows;

        let attrs = super::with_app_icon(super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(super::NATIVE_WINDOW_TITLE)
                    .with_decorations(true)
                    .with_inner_size(winit::dpi::LogicalSize::new(
                        f32::from(cols) * 9.0
                            + self.config.window.padding_left
                            + self.config.window.padding_right,
                        f32::from(rows) * (self.config.font.size * self.config.font.line_height)
                            + self.config.window.padding_top
                            + self.config.window.padding_bottom
                            + sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT,
                    )),
            ),
            self.config.appearance.backdrop,
        ));
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        // PANIC (above): `create_window` only fails when winit cannot reach
        // the windowing system at all (no display, broken connection). At
        // app startup this is unrecoverable — the user has no terminal to
        // see an error in. Documented per panic audit.
        // Enable IME so CJK input methods (Pinyin, Japanese, Korean…) can
        // deliver preedit + commit events instead of raw keystrokes.
        window.set_ime_allowed(true);
        super::install_native_window_background(&window, self.theme.colors.background.0.as_str());
        let dpi_scale = f64::from(window_dpi(&window));

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
            sonicterm_gpu::core::RendererSettings {
                font_family: &self.config.font.family,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
                padding: [
                    self.config.window.padding_left,
                    self.config.window.padding_right,
                    self.config.window.padding_top,
                    self.config.window.padding_bottom,
                ],
                appearance: sonicterm_gpu::core::SurfaceAppearance {
                    backdrop: self.config.appearance.backdrop,
                    opacity: self.config.appearance.opacity,
                    scrollbar: self.config.appearance.scrollbar,
                    panel_padding: self.config.appearance.panel_padding,
                },
            },
        )
        // PANIC: renderer init failure means wgpu cannot initialize on the
        // user's GPU at all — no recovery path exists in a GPU-accelerated
        // terminal. Same justification as the `create_window` site above.
        .expect("init renderer");
        // Epic #300 P4 follow-up wire: attach the async font fallback
        // loader so frame-time misses on CJK / emoji / nerd-font
        // codepoints trigger a background `request_load` and a
        // `UserEvent::ClearShapeCache` wake-up on completion. Skipped
        // when tests construct the App without a proxy; the existing
        // tofu fallback keeps working in that case.
        if let Some(proxy) = self.event_loop_proxy.clone() {
            renderer.set_async_loader(super::build_async_fallback_loader_for_proxy(proxy));
        }
        // Seed cursor visuals from config so the very first frame draws
        // the user-selected shape rather than the default. Subsequent
        // edits to sonicterm.toml take effect through the config-watch hook
        // (see apply_config below).
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);

        // Issue #713: resolve the no-GPU degrade decision now that the
        // renderer (and its adapter) exists. Combine the config mode with
        // runtime software-rasterizer detection, then clamp the frame period
        // so the CPU isn't asked to rasterize at the monitor's full refresh.
        self.software_render_degrade = crate::app::should_degrade_for_software_render(
            self.config.appearance.software_render_mode,
            renderer.is_software_rendering(),
        );
        if self.software_render_degrade {
            let before = self.frame_period;
            self.frame_period =
                crate::app::software_render_frame_period(true, self.frame_period);
            tracing::info!(
                detected = renderer.is_software_rendering(),
                mode = ?self.config.appearance.software_render_mode,
                frame_period = ?self.frame_period,
                "software-render degrade engaged: frame cap {:?} -> {:?}",
                before,
                self.frame_period,
            );
        }

        // Phase C2 / Haiku #295: register the main window's HWND with
        // the OS-drag backend through the unified entry point so the
        // main and torn-out windows share code paths. No-op on mac.
        let main_id = window.id();
        self.register_window_with_os_drag_backend(main_id, &window);
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
        renderer.set_titlebar_inset(0.0);
        // Apply the user's `tab_close_button_color` from sonicterm.toml
        // BEFORE the first frame so a custom always-visible × shows
        // up on the very first paint, not after a config edit.
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());

        // PR-B1b (#293): renderer is now owned by `WindowState.renderer`.
        // Insert the main entry BEFORE `new_tab` so `spawn_pane` (which
        // reads cell size through `self.main_renderer()`) sees it.
        // PR-B2a (#365): drop any synthetic main entry seeded by tests
        // (`App::__test_synthetic_main`); production `do_resumed` is
        // the authoritative source for `main_window_id`.
        if let Some(prev) = self.main_window_id.take() {
            self.windows.remove(&prev);
        }
        self.main_window_id = Some(main_id);
        let shadow = super::WindowState {
            role: super::WindowRole::Terminal,
            window: Some(window.clone()),
            renderer: Some(renderer),
            tabs: sonicterm_ui::tabs::TabBar::new(),
            tab_states: Vec::new(),
            panes: std::collections::HashMap::new(),
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: std::time::Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale,
            ime: sonicterm_ui::ime::ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: std::collections::HashMap::new(),
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.windows.insert(main_id, shadow);

        // Seed the first tab + pane now that the window + renderer exist.
        self.new_tab("shell");
        self.drain_pending_os_drag_payloads();

        let (rc, rr) = self.main_renderer().map(|r| r.cells()).unwrap_or((0, 0));
        tracing::info!(
            "SonicTerm ready. theme={} keymap={} bindings={} grid={}x{}",
            self.theme.name,
            self.keymap.meta.name,
            self.keymap.bindings.len(),
            rc,
            rr,
        );
        window.request_redraw();

        // Spawn the sonicterm.toml live-reload watcher (best-effort; if the
        // user has no config path or the parent dir is unreadable, the
        // app still runs — just without live reload).
        if self.config_watcher.is_none() {
            if let Some(path) = sonicterm_cfg::config::Config::default_path() {
                let proxy = self.event_loop_proxy.clone();
                let spawn_result = if let Some(p) = proxy {
                    ConfigWatcher::spawn_with_wake(path.clone(), move || {
                        // Failure here means the event loop has shut
                        // down — nothing to wake, safe to ignore.
                        let _ = p.send_event(UserEvent::ConfigChanged);
                    })
                } else {
                    // No proxy in this App instance — fall back to the
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
