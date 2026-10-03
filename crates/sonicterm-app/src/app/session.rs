//! App session lifecycle: construction and config normalization, tracing setup, exit
//! policy, pane retirement, and session teardown.

use super::*;

/// Preserve every config value for platforms without an additional policy.
pub(crate) fn identity_config_normalizer() -> ConfigNormalizer {
    Box::new(|config| (config, Vec::new()))
}

/// Compatibility no-op; FontStack owns fallback discovery and this helper sends no events.
pub fn build_async_fallback_loader_for_proxy(_proxy: EventLoopProxy<UserEvent>) {}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sonic=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

/// Public wrapper over the crate's `init_tracing` for the platform shell.
///
/// Installs the subscriber idempotently through `try_init`, so a process that
/// already has one keeps it rather than failing or installing a second.
pub fn init_tracing_public() {
    init_tracing();
}

impl App {
    /// Apply native capability policy and emit each resulting diagnostic once.
    pub(super) fn normalize_config(normalizer: &ConfigNormalizer, config: Config) -> Config {
        let (config, warnings) = normalizer(config);
        for warning in warnings {
            tracing::warn!(target: "sonicterm-cfg", "{warning}");
        }
        config
    }

    /// Build an app with no event-loop proxy.
    ///
    /// Without a proxy the app cannot post itself user events, so this suits
    /// callers that drive it directly rather than through a running loop.
    #[doc(hidden)]
    pub fn new(theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self::new_with_proxy(theme, config, keymap, None)
    }

    /// Build an app that posts user events through `event_loop_proxy`.
    ///
    /// The state machine is built here rather than supplied, so callers that
    /// already own one should hand it in instead.
    #[doc(hidden)]
    pub fn new_with_proxy(
        theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    ) -> Self {
        Self::new_with_proxy_and_machine(
            theme,
            config,
            keymap,
            event_loop_proxy,
            sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default()),
        )
    }

    /// Build an app around an externally-built
    /// [`sonicterm_app_core::AppStateMachine`].
    ///
    /// The platform shell constructs the machine first and hands it in, so all
    /// state mutation routes through the reducer the shell already owns rather
    /// than a second machine built here.
    pub fn new_with_proxy_and_machine(
        theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
        machine: sonicterm_app_core::AppStateMachine,
    ) -> Self {
        Self::new_with_proxy_machine_and_normalizer(
            theme,
            config,
            keymap,
            event_loop_proxy,
            machine,
            identity_config_normalizer(),
        )
    }

    /// Build an app after applying the supplied native capability policy.
    pub(crate) fn new_with_proxy_machine_and_normalizer(
        mut theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
        machine: sonicterm_app_core::AppStateMachine,
        config_normalizer: ConfigNormalizer,
    ) -> Self {
        let config = Self::normalize_config(&config_normalizer, config);
        theme.apply_accessibility(&config.accessibility);
        // Seed the process-global tab width limits from config before any tab
        // bar is laid out, so configured values take effect on the very first
        // frame (hot-reload updates them later via apply_new_config).
        sonicterm_ui::tabbar_view::set_min_tab_width(config.tab_min_width);
        sonicterm_ui::tabbar_view::set_max_tab_width(config.tab_max_width);
        let i18n = sonicterm_ui::i18n::I18n::new(if config.locale.is_empty() {
            None
        } else {
            // When: `config` names a locale, so that tag selects the translation
            // set instead of leaving the OS default to choose it.
            Some(config.locale.as_str())
        });
        let mut command_palette = CommandPalette::new();
        command_palette.set_keymap(&keymap, &i18n);
        let configured_font_size = config.font.size;
        let configured_weight_scale = config.font.effective_weight_scale();
        let font_dirs = vec![sonicterm_cfg::assets::asset_dir().join("fonts")];
        let path_workers = event_loop_proxy.as_ref().and_then(|proxy| {
            match path_target::PathWorkers::start(proxy.clone()) {
                Ok(workers) => Some(workers),
                Err(error) => {
                    tracing::warn!(target: "sonicterm_app::app", %error, "path workers unavailable");
                    None
                }
            }
        });
        let home_dir = path_target::native_home_dir();
        let local_hostname = gethostname::gethostname().to_string_lossy().into_owned();
        let governor = ResourceGovernor::new(
            ProcessKind::Gui,
            GovernorLimits {
                // Per-seam caps enforce storage limits; the process ledger tracks their ownership.
                process_bytes: usize::MAX,
                class_bytes: enum_map::enum_map! { _ => usize::MAX },
                class_items: enum_map::enum_map! { _ => None },
            },
        )
        .expect("an unlimited governor cannot fail to construct");
        let pty_reaper = reaper_driver::ReaperDriver::new(governor.clone())
            .expect("native PTY teardown driver could not start");
        Self {
            theme,
            inline_media_pool: media::InlineMediaPool::process_default(),
            capture_staging_pool: CaptureStagingPool::process_default(),
            process_privilege: crate::ProcessPrivilege::default(),
            #[cfg(windows)]
            foreground_probe_wake: None,
            config,
            config_normalizer,
            font_dirs,
            configured_font_size,
            configured_weight_scale,
            keymap,
            clipboard: Clipboard::new().ok(),
            #[cfg(target_os = "windows")]
            pending_osc52_reassert: None,
            test_clipboard_text: None,
            test_clipboard_write_failure: false,
            test_pty_writes: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            test_pane_launches: std::cell::RefCell::new(Vec::new()),
            // No event-loop proxy ⇒ headless/test construction ⇒ record PTY
            // writes for assertions. Production always passes `Some(proxy)`,
            // so the ledger stays disabled and adds no per-write cost.
            pty_write_log_enabled: event_loop_proxy.is_none(),
            pending_new_window: None,
            pending_tear_out: None,
            pending_os_teardown: false,
            test_post_snapshot_hook: None,
            pending_exit: false,
            last_retention_sample: None,
            last_memory_totals: None,
            breadcrumb_recorder: None,
            command_palette,
            palette_attached_window: None,
            window_rename_target: None,
            tab_edit_target: None,
            palette_pointer_capture: None,
            field_pointer_capture: None,
            field_owed_releases: Vec::new(),
            os_drag_handoff_started: false,
            governor,
            windows: HashMap::new(),
            pty_reaper,
            session_finished: None,
            frame_counters: super::frame_counters::AppFrameCounters::from_tracing(),
            frame_counters_sealed: std::cell::Cell::new(false),
            main_window_id: None,
            frontmost_window: None,
            pending_os_drag_payloads: Vec::new(),
            pending_winit_file_drops: HashMap::new(),
            theme_loader: None,
            keymap_loader: None,
            event_loop_proxy,
            gpu_recovery: None,
            path_workers,
            home_dir,
            runtime_config_path: None,
            local_hostname,
            runtime_smoke: None,
            // Default to 60 Hz until `resumed` probes the actual
            // monitor refresh rate. ~16.667 ms = 1/60 s.
            frame_period: Duration::from_micros(16_667),
            monitor_frame_period: Duration::from_micros(16_667),
            // Resolved after the renderer is created in `do_resumed`.
            software_render_degrade: false,
            pending_redraw: false,
            pending_redraw_windows: HashSet::new(),
            redraw_due: Vec::new(),
            warm_window_pool: Vec::new(),
            i18n,
            os_drag_sink: None,
            os_drag_backend: None,
            os_drag_pending: Arc::new(os_drag::PendingDragOutcome::default()),
            os_drag_bars: Arc::new(os_drag::TabBarRegistry::default()),
            os_drag_source: None,
            tab_bar_visible: true,
            broadcast: BroadcastState::Off,
            quit_hold: quit_hold::QuitHold::new(),
            on_resumed: None,
            on_window_ready: None,
            redraw_request_count: std::sync::atomic::AtomicUsize::new(0),
            reap_call_count: std::sync::atomic::AtomicUsize::new(0),
            test_viewport_override: None,
            machine,
            window_keys: crate::window_key_boundary::WindowKeyRegistry::new(),
        }
    }

    /// Retire every pane, including hidden windows, and return the cached bounded native settlement result.
    pub fn finish_session(&mut self) -> bool {
        if let Some(settled) = self.session_finished {
            // When: session_finished is cached, no pane or driver may be shut down a second time.
            return settled;
        }
        let panes: Vec<_> = self
            .windows
            .values_mut()
            .flat_map(|window| std::mem::take(&mut window.panes).into_values())
            .collect();
        for pane in panes {
            self.retire_pane(pane);
        }
        let settled = self.pty_reaper.finish();
        self.session_finished = Some(settled);
        settled
    }

    pub(super) fn retire_previous_main(&mut self) {
        if let Some(previous_id) = self.main_window_id.take() {
            self.cancel_window_rename(previous_id);
            self.cancel_tab_edit(previous_id);
            if let Some(mut previous) = self.windows.remove(&previous_id) {
                self.retire_window_counters(previous_id, &mut previous);
                for pane in std::mem::take(&mut previous.panes).into_values() {
                    self.retire_pane(pane);
                }
                self.release_owners_of(&mut previous);
            }
            self.window_keys.remove(previous_id);
        }
    }

    pub(super) fn reserve_pane_teardown(&self, pane: &mut PaneState) {
        if pane.reap_slot.is_some() {
            // When: reap_slot already follows this pane, a transfer must not reserve native custody twice.
            return;
        }
        if let Some(pty) = pane.pty.as_mut() {
            match self.pty_reaper.reserve(pty) {
                Ok(slot) => pane.reap_slot = Some(slot),
                Err(reason) => {
                    tracing::debug!(
                        target: "sonicterm_app::app",
                        ?reason,
                        "PTY teardown reservation refused; retirement will retry once"
                    );
                }
            }
        }
    }

    pub(super) fn retire_pane(&mut self, mut pane: PaneState) {
        *pane.redraw_target.lock() = None;
        if let Some(pty) = pane.pty.take() {
            self.pty_reaper.retire(pty, pane.reap_slot.take());
        }
        pane.charges.clear();
        drop(pane.owner.take());
    }

    pub(crate) fn set_breadcrumb_recorder(
        &mut self,
        recorder: sonicterm_logging::breadcrumbs::BreadcrumbRecorder,
    ) {
        self.breadcrumb_recorder = Some(recorder);
    }

    /// Reports [`Config::quit_on_last_window_close`] on macOS and `true`
    /// elsewhere. No exit path consults it: SonicTerm exits when its last
    /// window closes on every platform. It remains for source compatibility.
    #[doc(hidden)]
    pub fn should_exit_on_last_window_close(config: &Config) -> bool {
        #[cfg(target_os = "macos")]
        {
            config.quit_on_last_window_close
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = config;
            true
        }
    }

    /// Install a one-shot callback fired at the top of the first
    /// `ApplicationHandler::resumed` tick. macOS uses this to install
    /// the native NSMenu after winit has built the AppKit event loop —
    /// installing earlier leaves AppKit with only the default
    /// `Apple, sonicterm-mac` menu bar.
    pub fn set_on_resumed<F: FnOnce() + Send + 'static>(&mut self, hook: F) {
        self.on_resumed = Some(Box::new(hook));
    }

    /// Set the one-shot hook fired right after window creation, with
    /// the window's raw handle. See the field docs for the use-case
    /// (Windows muda menubar install).
    pub fn set_on_window_ready<F>(&mut self, hook: F)
    where
        F: FnOnce(raw_window_handle::RawWindowHandle) + Send + 'static,
    {
        self.on_window_ready = Some(Box::new(hook));
    }

    /// Decide whether the event loop should exit. The app should keep
    /// running as long as ANY active terminal window owns at least one tab:
    /// a visible main window with tabs, or any torn-out child window. A
    /// hidden/drained main window is intentionally NOT active for process
    /// lifecycle purposes; once the final child is gone there is no window
    /// the user can interact with, so requires quitting instead of
    /// leaving a dock-alive/headless process around.
    #[doc(hidden)]
    pub fn should_exit(&self) -> bool {
        Self::should_exit_pure(
            self.main_tabs().map(|tabs| tabs.len()).unwrap_or(0),
            self.main_is_hidden(),
            self.child_window_count(),
        )
    }

    /// Test-only: pure policy fn mirroring `should_exit` so integration
    /// tests can exercise the rule without constructing a real
    /// `WindowState` (which requires a live winit Window + GpuRenderer).
    #[doc(hidden)]
    pub fn should_exit_pure(main_tabs: usize, main_hidden: bool, child_count: usize) -> bool {
        let main_alive = !main_hidden && main_tabs > 0;
        !main_alive && child_count == 0
    }

    /// Mark a deferred process exit when no active terminal windows remain.
    /// This is the `ActiveEventLoop`-free counterpart to `event_loop.exit()`
    /// for keymap/tab-close paths; `do_about_to_wait` drains the flag. OS
    /// window close handlers with an event-loop handle may still call
    /// `event_loop.exit()` directly after this predicate becomes true.
    pub(super) fn request_exit_if_no_active_windows(&mut self) {
        if self.should_exit() {
            self.pending_exit = true;
        }
    }

    /// Unified "did this close just empty the affected window?" check
    /// for the keymap path. Mirrors what the mouse-click close-button
    /// path in `window_event.rs` and the OS `CloseRequested` arm do —
    /// hide the main window (or exit, on the last window) when its
    /// tabs vec is empty, and reap child windows the same way the drag-
    /// merge path does. The flag set here is drained in
    /// `do_about_to_wait`.
    pub(super) fn reap_empty_main_window_after_close(&mut self) {
        if !self.main_tabs().map(|tabs| tabs.is_empty()).unwrap_or(true) {
            // When: `main_tabs` still holds a tab, so the window is in use and
            // the drained-window teardown below would close live work.
            return;
        }
        if self.child_window_count() == 0 {
            self.hide_main_window();
            self.request_exit_if_no_active_windows();
        } else {
            // When: `child_window_count` is nonzero, so tabs survive elsewhere;
            // hide main and leave the exit decision to the last child closing.
            self.hide_main_window();
        }
    }

    /// Charge every pane this app creates to `pool` instead of the
    /// process-default pool, so a test measuring media budgets or totals
    /// observes only its own panes. Call before any pane exists: panes already
    /// created keep the pool they were built with.
    #[cfg(test)]
    pub(crate) fn with_inline_media_pool(mut self, pool: Arc<media::InlineMediaPool>) -> Self {
        debug_assert!(
            self.windows.values().all(|window| window.panes.is_empty()),
            "inject the inline-media pool before any pane exists"
        );
        self.inline_media_pool = pool;
        self
    }

    /// Stage every media capture this app's panes open in `pool` instead of the
    /// process-default pool, so a test that needs a capture admitted depends
    /// only on its own captures. Call before any pane exists: panes already
    /// created keep the pool they were built with.
    #[cfg(test)]
    pub(crate) fn with_capture_staging_pool(mut self, pool: Arc<CaptureStagingPool>) -> Self {
        debug_assert!(
            self.windows.values().all(|window| window.panes.is_empty()),
            "inject the capture staging pool before any pane exists"
        );
        self.capture_staging_pool = pool;
        self
    }
}
