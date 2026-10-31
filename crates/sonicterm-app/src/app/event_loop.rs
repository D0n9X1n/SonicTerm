//! `App::do_resumed` / `do_user_event` / `do_new_events` /
//! `do_about_to_wait` — extracted from the `ApplicationHandler` trait impl
//! from the monolithic app module.
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

#[cfg(target_os = "macos")]
use winit::platform::macos::ActiveEventLoopExtMacOS;

use super::{
    mark_all_panes_dirty, runtime_smoke::RuntimeSmokeFailure, window_dpi, with_integrated_titlebar,
    App, UserEvent,
};
use sonicterm_ui::selection::SelectMode;
use winit::event_loop::ControlFlow;

use super::output_event::OutputEvent;

// Not named `windows`: a module of that name here would collide with the
// `windows` crate's paths on Windows builds.
#[cfg(windows)]
#[path = "event_loop/windows.rs"]
mod windows_os;

/// The earlier of two optional deadlines.
///
/// A deadline is a "wake no later than" bound, so the earliest wins. Folding
/// rather than overwriting is what keeps an earlier contributor from being
/// pushed out past its due instant by a later one.
#[cfg(test)]
fn earliest(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

/// Whether the wake about to be armed exists only to sample memory.
///
/// The distinction decides whether the resulting `ResumeTimeReached` may return
/// before frame and foreground-probe work. A memory-only deadline must not
/// repaint an idle session every retention interval.
///
/// A memory wake is "only" a memory wake when no non-memory contributor is
/// armed, or when the memory deadline lands strictly first. Ties go to the
/// non-memory side so a simultaneous frame or foreground sample is not skipped.
///
/// Pure and free-standing so the decision is testable without a winit event
/// loop, a window, or a GPU.
#[cfg(test)]
fn wake_is_memory_only(non_memory_wake: Option<Instant>, memory_wake: Option<Instant>) -> bool {
    match (non_memory_wake, memory_wake) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(non_memory), Some(memory)) => memory < non_memory,
    }
}

#[cfg(all(windows, test))]
fn wake_is_foreground_probe_only(
    frame_wake: Option<Instant>,
    foreground_wake: Option<Instant>,
    memory_wake: Option<Instant>,
) -> bool {
    foreground_wake.is_some_and(|foreground| {
        frame_wake.is_none_or(|frame| foreground < frame)
            && memory_wake.is_none_or(|memory| foreground <= memory)
    })
}

impl App {
    pub(super) fn do_user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::MenuAction => self.drain_menubar_actions(event_loop),
            UserEvent::OpenScripts => {
                self.drain_launch_requests();
            }
            UserEvent::OsDrag => self.drain_os_drag(),
            UserEvent::DragMoved => {
                // When: DragMoved arrives; handle_os_drag_moved only drains the
                // cursor mailbox, and the drag chip renders from tab_drag state.
                let _ = self.handle_os_drag_moved();
            }
            UserEvent::DragEnded => {
                // When: DragEnded arrives; handle_os_drag_ended routes the drop
                // internally and returns the outcome only for tests to assert on.
                let _ = self.handle_os_drag_ended();
            }
            UserEvent::Accessibility { window_id, request } => {
                self.handle_accessibility_request(window_id, request);
            }
            UserEvent::RequestRedraw(window_id) => {
                self.service_output_event(OutputEvent::Explicit(window_id), Instant::now());
            }
            UserEvent::DisplayLinkTick { window_id, generation, target } => {
                self.handle_display_link_tick(window_id, generation, target);
            }
            UserEvent::PaneOutput { window_id, pane_id } => {
                self.service_output_event(OutputEvent::Pane { window_id, pane_id }, Instant::now());
            }
            UserEvent::ClearShapeCache => self.handle_clear_shape_cache(),
            UserEvent::FontFallbackReady { window_id, notice_id } => {
                self.handle_font_fallback_ready(window_id, notice_id);
            }
            UserEvent::ForegroundProbeReady => self.drain_foreground_probe_results(Instant::now()),
            UserEvent::FontCatalogReady { families } => self.adopt_font_catalog(families),
            UserEvent::GpuDeviceStateChanged => {
                self.request_device_state_redraws();
                self.service_gpu_recovery(event_loop, Instant::now());
            }
            UserEvent::GpuDeviceGenerationChanged { generation } => {
                self.gpu_generation_changed(event_loop, generation);
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.observe_recovery_device_event(generation);
                }
            }
            UserEvent::GpuRecoveryReady { ticket } => {
                self.gpu_recovery_ready(event_loop, ticket);
            }
            UserEvent::MemoryReportSampled { generation, sample } => {
                self.handle_memory_report_sampled(generation, *sample);
            }
            UserEvent::DefaultHandlerResult { request, index, outcome } => {
                self.handle_default_handler_result(request, index, outcome);
            }
            UserEvent::DefaultHandlerDeadline { request } => {
                self.handle_default_handler_deadline(request);
            }
            UserEvent::UpdateCheckFinished { level, message } => {
                self.show_notification_for_kind(self.frontmost_kind(), level, message);
            }
            UserEvent::PaneProcessExited { pane_id, status, .. } => {
                self.handle_pane_process_exited(pane_id, status);
            }
            UserEvent::LaunchFailed { message } | UserEvent::ScriptDraftRejected { message } => {
                self.handle_script_draft_rejected(message);
            }
            UserEvent::ClipboardWrite { pane_id, text } => {
                let bytes = text.len();
                let written = self.handle_clipboard_write(text);
                self.note_clip_write(pane_id, bytes, written);
            }
            UserEvent::ClipboardAttempts { pane_id, attempts } => {
                self.note_clip_attempts(pane_id, attempts, Instant::now());
            }
            event @ (UserEvent::ProgramProgress { .. } | UserEvent::ProgramNotify { .. }) => {
                self.handle_program_event(event);
            }
            UserEvent::PathProbeFinished(result) => {
                self.handle_path_probe_finished(*result);
            }
            UserEvent::PathOpenFailed { window_id, pane_id, reason, target } => {
                self.report_failed_target(window_id, pane_id, &reason, &target);
            }
            UserEvent::PtyInputRejected {
                pane_id,
                source,
                rejected_bytes,
                reason,
                diagnostics,
            } => {
                self.handle_pty_input_rejected(
                    pane_id,
                    source,
                    rejected_bytes,
                    reason,
                    diagnostics,
                );
            }
            UserEvent::RuntimeSmokeTimeout => {
                // When: `event` is `RuntimeSmokeTimeout`, classify the boundary before exiting.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    // When: `self.runtime_smoke.as_mut()` yields `smoke`, preserve its active boundary.
                    smoke.fail_from_watchdog(Instant::now());
                    event_loop.exit();
                    return;
                }
            }
        }
        // Any path above that ran an action may have requested a new
        // top-level window; create it now that we have an ActiveEventLoop.
        self.drain_pending_window_creates(event_loop);
        // Drain deferred OS-drag teardown AFTER `drain_pending_window_creates`
        // so any tear-out-spawn from the `DroppedOnEmpty` branch has produced
        // its new window before cross-window drag-residue cleanup
        // runs. Ordering is the entire point — do not move above.
        self.drain_pending_os_teardown();
    }

    /// Log payload-free rejection evidence and notify only the pane's current live window.
    pub(super) fn handle_pty_input_rejected(
        &mut self,
        pane_id: u64,
        source: super::PtyInputSource,
        rejected_bytes: usize,
        reason: String,
        diagnostics: sonicterm_io::pty::PtyInputDiagnostics,
    ) {
        let window_id = self
            .windows
            .iter()
            .find_map(|(id, window)| window.panes.contains_key(&pane_id).then_some(*id));
        tracing::warn!(
            pane_id,
            ?window_id,
            ?source,
            rejected_bytes,
            %reason,
            observation = "concurrent",
            queued_messages = diagnostics.queued_messages,
            queued_bytes = diagnostics.queued_bytes,
            queue_capacity = diagnostics.queue_capacity,
            writer_phase = ?diagnostics.writer_phase,
            in_flight_bytes = diagnostics.in_flight_bytes,
            in_flight_millis = ?diagnostics.in_flight_millis,
            completed_messages = diagnostics.completed_messages,
            "terminal input was not queued because the PTY writer is unavailable or saturated"
        );
        let Some(window_id) = window_id else {
            // When: `window_id` is absent, retain the diagnostic without alarming an unrelated frontmost terminal.
            return;
        };
        let not_sent = super::status_line::not_sent_reason(&reason);
        let now = self.dispatch_now();
        self.note_input(window_id, sonicterm_ui::status_line::InputState::NotSent(not_sent), now);
        let kind = if Some(window_id) == self.main_window_id {
            super::FrontmostKind::Main
        } else {
            // When: `window_id` differs from `main_window_id`, route the notification to that child rather than the frontmost window.
            super::FrontmostKind::Child(window_id)
        };
        self.show_notification_for_kind(
            kind,
            sonicterm_ui::overlays::NotificationLevel::Error,
            format!("Terminal input was not sent (pane {pane_id}; {source:?}; {reason}; {rejected_bytes} bytes)."),
        );
    }

    /// Apply one accepted OSC 52 write; returns whether the native clipboard took it.
    pub(super) fn handle_clipboard_write(&mut self, text: String) -> bool {
        #[cfg(target_os = "windows")]
        let (previous_text, reassert_text) = (self.clipboard_text_for_reassert(), text.clone());
        let written = self.set_clipboard_text(text);
        // Windows reasserts a successful write once; a failed one has no value to reassert.
        #[cfg(target_os = "windows")]
        if written {
            self.pending_osc52_reassert = Some(super::PendingOsc52Reassert {
                text: reassert_text,
                previous_text,
                due: Instant::now() + super::OSC52_CLIPBOARD_REASSERT_DELAY,
            });
        }
        written
    }

    pub(super) fn handle_script_draft_rejected(&mut self, message: String) {
        self.show_notification_for_kind(
            self.frontmost_kind(),
            sonicterm_ui::overlays::NotificationLevel::Warning,
            message,
        );
    }

    /// Drain a `UserEvent::ClearShapeCache`:
    /// an async font fallback family just landed in
    /// [`sonicterm_text::async_fallback::AsyncFallbackLoader`]. Clear every
    /// live renderer's shape / row / line caches (bumping `style_rev`)
    /// and request a redraw on every live window. The next frame
    /// re-walks the fallback chain and the user's tofu cells flip to
    /// real glyphs.
    /// Handle a fallback wake: the window's renderer acknowledges its own current notice and
    /// says whether that generation still needs applying. A closed window, a replaced notice or an
    /// applied generation does nothing.
    /// A face publication requests the `Chrome` frame that applies it. An unresolved-only wake
    /// requests nothing here: the status-line sync that follows this event recounts `glyphs` and
    /// requests one `StatusLine` frame only when the shown value changed.
    pub(super) fn handle_font_fallback_ready(&mut self, window_id: WindowId, notice_id: u64) {
        let wake = self
            .windows
            .get_mut(&window_id)
            .and_then(|window| window.renderer.as_mut())
            .map(|renderer| renderer.acknowledge_font_fallback(notice_id))
            .unwrap_or_default();
        self.request_font_fallback_frame(window_id, wake.faces_due);
    }

    /// Request the frame that applies a published fallback generation when `due`. A hidden,
    /// occluded or parked window only records the cause, and its next prepared frame applies it.
    pub(super) fn request_font_fallback_frame(&mut self, window_id: WindowId, due: bool) {
        if due {
            // The window's renderer has not applied this generation, so one frame must.
            self.request_owner_redraw(window_id, super::redraw::RedrawCause::Chrome);
        }
    }

    pub(super) fn handle_clear_shape_cache(&mut self) {
        // main window lives in `self.windows` with `renderer=Some`,
        // so a single iteration covers main + all torn-out children.
        for child in self.windows.values_mut() {
            if let Some(renderer) = child.renderer.as_mut() {
                renderer.clear_shape_cache();
                child.request_window_redraw();
            }
        }
    }

    pub(super) fn drain_launch_requests(&mut self) -> usize {
        self.drain_launch_requests_with_fallback(false)
    }

    /// Open a tab for each queued launch and show each queued notice; returns the tabs opened.
    ///
    /// With `fallback_shell`, a drain that opened no tab opens the plain shell tab first, so a
    /// notice on a cold start (for example a rejected link) lands in a window with a terminal.
    fn drain_launch_requests_with_fallback(&mut self, fallback_shell: bool) -> usize {
        if self.main().is_none() {
            // When: no main window exists yet to host a tab; returning before
            // drain() leaves the requests queued for a later pass.
            return 0;
        }
        let requests = crate::launch_queue::drain();
        let mut count = 0;
        let mut notices = Vec::new();
        for request in requests {
            match request {
                sonicterm_types::LaunchRequest::Script(request) => {
                    let title = request
                        .launch_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("script")
                        .to_string();
                    let launch = super::pane_launch::PaneLaunch::for_script(request);
                    self.new_tab_with_launch(title, launch);
                    count += 1;
                }
                sonicterm_types::LaunchRequest::Terminal(launch) => {
                    let tab_title =
                        sonicterm_types::replace_control_characters(&launch.tab_title());
                    if let Some(title) = launch.title.as_deref() {
                        // A launcher's title names the native window as well as the first tab.
                        self.set_main_window_name(&sonicterm_types::replace_control_characters(
                            title,
                        ));
                    }
                    let launch = super::pane_launch::PaneLaunch::for_terminal(launch);
                    self.new_tab_with_launch(tab_title, launch);
                    count += 1;
                }
                sonicterm_types::LaunchRequest::Notice(message) => notices.push(message),
            }
        }
        if fallback_shell && count == 0 {
            // A cold start that opened no tab still gets its plain shell before any notice shows.
            self.new_tab("shell");
        }
        if (count > 0 || !notices.is_empty()) && self.main_is_hidden() {
            self.show_main_window();
        }
        for message in notices {
            self.handle_script_draft_rejected(sonicterm_types::replace_control_characters(
                &message,
            ));
        }
        count
    }

    /// Name the main window, as the rename command does, and update its native title.
    fn set_main_window_name(&mut self, name: &str) {
        let Some(id) = self.main_window_id else {
            // When: `main_window_id` is unset there is no window to name.
            return;
        };
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `windows` no longer holds the main id, the title has no target.
            return;
        };
        window.custom_window_name = name.to_string();
        let _ = self.sync_native_title(id, Instant::now(), true);
    }

    pub(super) fn seed_initial_tabs(&mut self) {
        self.drain_launch_requests_with_fallback(true);
    }

    pub(super) fn do_resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "macos")]
        {
            // SonicTerm owns tab grouping before hooks or native windows can create AppKit tabs.
            event_loop.set_allows_automatic_window_tabbing(false);
            if let Some(smoke) = self.runtime_smoke.as_mut() {
                // When: smoke is active, verify the process property rather than infer visible tab-strip behavior.
                if event_loop.allows_automatic_window_tabbing() {
                    // When: event_loop still allows automatic tabbing, stop before creating a window under the wrong policy.
                    tracing::error!("runtime smoke process automatic tabbing remains enabled");
                    smoke.fail(RuntimeSmokeFailure::Display);
                    event_loop.exit();
                    return;
                }
            }
        }
        // Fire the one-shot post-resume hook before any window work.
        // macOS uses this slot to install the native NSMenu — by now
        // winit has built the AppKit event loop, so `setMainMenu`
        // sticks. Installing it before `run_app` left AppKit with only
        // the default `Apple, sonicterm-mac` menubar (bug caught by the
        // release-binary smoke).
        if let Some(hook) = self.on_resumed.take() {
            hook();
        }

        let attrs = super::with_app_icon(super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(super::NATIVE_WINDOW_TITLE)
                    .with_visible(false)
                    .with_decorations(true)
                    .with_inner_size(super::configured_window_size(
                        &self.config,
                        self.tab_bar_visible,
                    )),
            ),
            self.config.appearance.backdrop,
            self.config.appearance.software_render_mode,
        ));
        let attrs = self.native_drop_attributes(attrs);
        let window_started = std::time::Instant::now();
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                // When: `event_loop.create_window(attrs)` returns `Err(error)`, smoke exits while normal startup panics.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    // When: `self.runtime_smoke.as_mut()` yields `smoke`, retain the display failure.
                    tracing::error!(%error, "runtime smoke could not create a window");
                    smoke.fail(RuntimeSmokeFailure::Display);
                    event_loop.exit();
                    return;
                }
                panic!("create window: {error}");
            }
        };
        if let Some(smoke) = self.runtime_smoke.as_mut() {
            smoke.begin_gpu();
        }
        // PANIC (above): `create_window` only fails when winit cannot reach
        // the windowing system at all (no display, broken connection). At
        // app startup this is unrecoverable — the user has no terminal to
        // see an error in. Documented per panic audit.
        // Enable IME so CJK input methods (Pinyin, Japanese, Korean…) can
        // deliver preedit + commit events instead of raw keystrokes.
        window.set_ime_allowed(true);
        // A dormant adapter must exist before the window is first shown.
        self.note_startup(window_started, |spans| &mut spans.window_ns);
        let install_started = std::time::Instant::now();
        self.install_accessibility(event_loop, &window);
        self.note_startup(install_started, |spans| &mut spans.adapter_install_ns);
        super::install_native_window_background(&window, self.theme.colors.background.0.as_str());
        let dpi_scale = f64::from(window_dpi(&window));

        // Gate redraws to the monitor's vsync cadence.
        // `refresh_rate_millihertz` returns e.g. 60_000 for 60Hz,
        // 120_000 for 120Hz ProMotion, etc. A zero or absent value
        // means winit could not determine it (headless, virtual
        // display) — fall back to the 60Hz default seeded by `new`.
        if let Some(monitor) = window.current_monitor() {
            if let Some(mhz) = monitor.refresh_rate_millihertz() {
                if mhz > 0 {
                    // period_us = 1_000_000_000 / mhz
                    let period_us = 1_000_000_000u64 / u64::from(mhz);
                    self.monitor_frame_period = Duration::from_micros(period_us);
                    self.frame_period = self.monitor_frame_period;
                    tracing::debug!(
                        "vsync pacing: monitor reports {}.{:03} Hz, frame period {:?}",
                        mhz / 1000,
                        mhz % 1000,
                        self.frame_period,
                    );
                }
            }
        }

        let renderer_started = std::time::Instant::now();
        let renderer_result = GpuRenderer::new(
            window.clone(),
            event_loop,
            &self.theme,
            sonicterm_gpu::core::RendererSettings {
                font_family: &self.config.font.family,
                font_dirs: &self.font_dirs,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
                font_weight_scale: self.config.font.effective_weight_scale(),
                subpixel_aa: self.config.font.subpixel_aa,
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
                    software_render_mode: self.config.appearance.software_render_mode,
                },
                role: "main",
                glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
            },
        );
        let mut renderer = match renderer_result {
            Ok(renderer) => renderer,
            Err(error) => {
                // When: `renderer_result` is `Err(error)`, smoke exits while normal startup panics.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    // When: `self.runtime_smoke.as_mut()` yields `smoke`, retain the GPU failure.
                    tracing::error!(%error, "runtime smoke could not initialize the renderer");
                    smoke.fail(RuntimeSmokeFailure::Gpu);
                    event_loop.exit();
                    return;
                }
                panic!("init renderer: {error}");
            }
        };
        self.initialize_gpu_recovery(&renderer);
        if let Some(proxy) = self.event_loop_proxy.clone() {
            // Device-stop notifications wake the application through the available event-loop proxy.
            renderer.set_device_state_waker(super::gpu_device_state_waker(proxy.clone()));
            // A fallback face published for the main window's fonts wakes the main window.
            renderer.set_font_fallback_waker(super::font_fallback_waker(proxy, window.id()));
        }
        // Seed cursor visuals from config so the very first frame draws
        // the user-selected shape rather than the default. Later edits to
        // sonicterm.toml reach the renderer only when the user asks for a
        // reload; nothing watches the file.
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);

        // resolve the no-GPU degrade decision now that the
        // renderer (and its adapter) exists. Combine the config mode with
        // runtime software-rasterizer detection, then clamp the frame period
        // so the CPU isn't asked to rasterize at the monitor's full refresh.
        let degrade = crate::app::should_degrade_for_software_render(
            self.config.appearance.software_render_mode,
            renderer.is_software_rendering(),
        );
        self.set_software_render_degrade(degrade);
        renderer.set_software_render_degrade(self.software_render_degrade);
        if let Some(recorder) = &self.breadcrumb_recorder {
            // When: a breadcrumb_recorder is installed; the adapter class is only
            // knowable once the renderer exists, so it is recorded here.
            use sonicterm_logging::breadcrumbs::{
                AdapterClass, BreadcrumbEvent, RendererIdentity, RendererMode,
            };
            let software_adapter = renderer.is_software_rendering();
            let mode = if software_adapter { RendererMode::Software } else { RendererMode::Gpu };
            let adapter =
                if software_adapter { AdapterClass::Software } else { AdapterClass::Hardware };
            let identity = if cfg!(target_os = "windows") && renderer.is_software_render_degraded()
            {
                // When: target_os is windows and the renderer degraded; that pairing
                // gets its own identity so a later report separates it from wgpu.
                RendererIdentity::Software
            } else {
                // When: target_os is not windows, or is_software_render_degraded is
                // false; Wgpu covers hardware and every non-degraded adapter.
                RendererIdentity::Wgpu
            };
            let _ = recorder.record(BreadcrumbEvent::Renderer { identity, mode, adapter });
        }
        if self.software_render_degrade {
            let before = self.frame_period;
            // Resolved from the monitor's own period, not from `frame_period`:
            // that field is the resolved value and would already hold the cap
            // on a re-resolution, making the decision one-way.
            self.frame_period =
                crate::app::software_render_frame_period(true, self.monitor_frame_period);
            tracing::info!(
                detected = renderer.is_software_rendering(),
                mode = ?self.config.appearance.software_render_mode,
                frame_period = ?self.frame_period,
                "software-render degrade engaged: frame cap {:?} -> {:?}",
                before,
                self.frame_period,
            );
        }

        // Register the main window's HWND with
        // the OS-drag backend through the unified entry point so the
        // main and torn-out windows share code paths. No-op on mac.
        let main_id = window.id();
        if let Err(error) = self.register_window_with_os_drag_backend(main_id, &window) {
            // When: native registration fails, stop before a visible window or shell can depend on its drop ownership.
            tracing::error!(?main_id, %error, "main native drop-target registration failed");
            if let Some(smoke) = self.runtime_smoke.as_mut() {
                smoke.fail(RuntimeSmokeFailure::Display);
            }
            event_loop.exit();
            return;
        }
        // Fire the one-shot window-ready hook (Windows uses this slot
        // to install the muda menubar against the HWND). Best-effort:
        // if the platform can't surface a raw handle, skip the hook
        // and log — the rest of the app still runs.
        if let Some(hook) = self.on_window_ready.take() {
            use raw_window_handle::HasWindowHandle;
            match window.window_handle() {
                Ok(window_handle) => hook(window_handle.as_raw()),
                Err(error) => tracing::warn!("on_window_ready: no raw handle: {error}"),
            }
        }
        self.note_startup(renderer_started, |spans| &mut spans.renderer_ns);
        renderer.set_titlebar_inset(0.0);
        let _ = super::apply_terminal_window_minimum(&window, &mut renderer);
        // Apply the user's `tab_close_button_color` from sonicterm.toml
        // BEFORE the first frame so a custom always-visible × shows
        // up on the very first paint, not after a config edit.
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());

        // renderer is now owned by `WindowState.renderer`.
        // Insert the main entry BEFORE `new_tab` so `spawn_pane` (which
        // reads cell size through `self.main_renderer()`) sees it.
        // drop any synthetic main entry seeded by tests
        // (`App::__test_synthetic_main`); production `do_resumed` is
        // the authoritative source for `main_window_id`.
        self.retire_previous_main();
        self.main_window_id = Some(main_id);
        let checks_started = std::time::Instant::now();
        self.record_startup_load_errors();
        self.note_startup(checks_started, |spans| &mut spans.load_checks_ns);
        // Both pacing clocks start at one instant, so neither paces a new window differently.
        let created_at = std::time::Instant::now();
        let shadow = super::WindowState {
            // Registered when the window is inserted; construction has no
            // governor in scope.
            owner: None,
            pending_receipts: Vec::new(),
            role: super::WindowRole::Terminal,
            custom_window_name: String::new(),
            native_title: Default::default(),
            window: Some(window.clone()),
            renderer: Some(renderer),
            tabs: sonicterm_ui::tabs::TabBar::new(),
            tab_states: Vec::new(),
            panes: std::collections::HashMap::new(),
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            pointer_gesture: None,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            pty_pressed_keys: std::collections::HashMap::new(),
            last_render: created_at,
            stream_clock: created_at,
            retry_not_before: None,
            visible_frame_invalid: false,
            redraw: Default::default(),
            display_link: Default::default(),
            hover_link: false,
            hover_unresolved: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale,
            ime: sonicterm_ui::ime::ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            link_preview: None,
            path_probe: super::path_target::PathProbeState::default(),
            status_strip: None,
            sel_status: Default::default(),
            ime_discarded_at: None,
            alert_settings: None,
            hover_detection: super::path_target::HoverDetectionCache::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: std::collections::HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
            #[cfg(test)]
            test_image_atlas_release: None,
            #[cfg(test)]
            test_strip_settings: None,
        };
        self.insert_window_registered(main_id, shadow);

        // Seed script-file tabs when launch events arrived before the window;
        // otherwise preserve the normal one-shell startup.
        if let Some(smoke) = self.runtime_smoke.as_mut() {
            smoke.begin_pty();
        }
        let spawn_started = std::time::Instant::now();
        self.seed_initial_tabs();
        self.note_startup(spawn_started, |spans| &mut spans.pane_spawn_ns);
        // The first tab now names the window, before it is shown or a smoke reads it back.
        let _ = self.sync_native_title(main_id, Instant::now(), true);
        if self.runtime_smoke.is_some()
            && (!self.smoke_check_native_title(&window, "#1 shell")
                || !self.smoke_exercise_window_name(main_id))
        {
            // When: runtime_smoke cannot verify admission, rename, and reset before reveal, fail the display boundary.
            event_loop.exit();
            return;
        }
        window.set_visible(true);
        if self.runtime_smoke.is_some() {
            // When: `self.runtime_smoke.is_some()` is true, verify and exercise the smoke PTY.
            let command = self.runtime_smoke.as_ref().map(|smoke| smoke.command().to_vec());
            let active_pane = self
                .main_active_pane_id()
                .and_then(|pane_id| self.main().and_then(|window| window.panes.get(&pane_id)));
            let expected_shell = self.config.terminal.shell.as_deref();
            let smoke_failure = match (active_pane.and_then(|pane| pane.pty.as_ref()), command) {
                (Some(pty), Some(command)) if expected_shell == Some(pty.shell_program_path()) => {
                    pty.send_input_nonblocking(command).err().map(|error| {
                        tracing::error!(%error, "runtime smoke could not queue its shell marker");
                        RuntimeSmokeFailure::Marker
                    })
                }
                _ => Some(RuntimeSmokeFailure::Pty),
            };
            if let Some(failure) = smoke_failure {
                // When: `smoke_failure` contains `failure`, retain that PTY/marker boundary.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(failure);
                }
                event_loop.exit();
                return;
            }
            if let Some(smoke) = self.runtime_smoke.as_mut() {
                smoke.begin_marker_wait();
            }
        }
        self.drain_pending_os_drag_payloads();

        if let Some(recorder) = &self.breadcrumb_recorder {
            // When: a breadcrumb_recorder is installed; Ready marks the end of
            // startup so a later hang is placed before or after the first frame.
            use sonicterm_logging::breadcrumbs::{BreadcrumbEvent, LifecycleEvent};
            let windows = u32::try_from(self.windows.len()).unwrap_or(u32::MAX);
            let panes = u32::try_from(
                self.windows
                    .values()
                    .fold(0usize, |total, window| total.saturating_add(window.panes.len())),
            )
            .unwrap_or(u32::MAX);
            let _ = recorder.record(BreadcrumbEvent::Counts { windows, panes });
            let _ = recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready));
        }

        // The face the main renderer resolved is the session's family when the config names none.
        self.adopt_resolved_font_family();
        let (cols, rows) = self.main_renderer().map(|renderer| renderer.cells()).unwrap_or((0, 0));
        tracing::info!(
            "SonicTerm ready. theme={} keymap={} bindings={} grid={}x{}",
            self.theme.name,
            self.keymap.meta.name,
            self.keymap.bindings.len(),
            cols,
            rows,
        );
        crate::app::frame_counters::request_native_redraw(&window);
    }
}

#[cfg(test)]
#[path = "event_loop_tests.rs"]
mod event_loop_tests;

#[path = "event_loop_wait.rs"]
mod wait;
