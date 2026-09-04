//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use sonicterm_ui::ime::ImeState;
use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::{SelectMode, Selection};
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::{Parser, VtEvent};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    window_dpi, with_integrated_titlebar, wrap_paste, App, PaneState, TabState, UserEvent,
    WindowState,
};
use crate::app::window_geom;

/// How a child renderer reached destination preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChildRendererOrigin {
    /// Constructed for this destination.
    Fresh,
    /// Adopted from the hidden warm-window pool.
    WarmPool,
}

/// Window that must receive a detached tab again if destination setup fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TearOutSource {
    /// The distinguished main terminal window.
    Main(WindowId),
    /// A torn-out terminal window.
    Child(WindowId),
}

impl TearOutSource {
    /// Return the concrete source window identity.
    fn window_id(self) -> WindowId {
        match self {
            Self::Main(id) | Self::Child(id) => id,
        }
    }
}

/// Live tab ownership held between source detachment and destination commit.
pub(super) struct DetachedTab {
    source: TearOutSource,
    original_index: usize,
    prior_active_tab_id: Option<sonicterm_ui::tabs::TabId>,
    tab: Tab,
    state: TabState,
    panes: HashMap<u64, super::PaneState>,
}

/// Fallible destination-preparation boundary reached by a tear-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TearOutStage {
    /// Native window creation.
    CreateWindow,
    /// GPU renderer construction.
    RendererInit,
    /// Live renderer adoption and initial sizing.
    RendererConfigure,
}

/// Required disposition of destination artifacts after preparation fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DestinationDisposition {
    /// No destination artifact exists.
    Nothing,
    /// Drop a fresh hidden destination and any renderer it owns.
    DropFresh,
    /// Return an unchanged hidden destination to the warm pool.
    ReturnWarm,
    /// Drop a warm destination whose renderer has already been mutated.
    RetireWarm,
}

/// Select the cleanup owed by an origin and failed preparation stage.
fn destination_disposition(
    origin: ChildRendererOrigin,
    stage: TearOutStage,
) -> DestinationDisposition {
    match (origin, stage) {
        (ChildRendererOrigin::Fresh, TearOutStage::CreateWindow) => DestinationDisposition::Nothing,
        (ChildRendererOrigin::Fresh, _) => DestinationDisposition::DropFresh,
        (ChildRendererOrigin::WarmPool, TearOutStage::RendererConfigure) => {
            DestinationDisposition::RetireWarm
        }
        (ChildRendererOrigin::WarmPool, _) => DestinationDisposition::ReturnWarm,
    }
}

/// Owned one-shot cleanup for a partially prepared destination.
pub(super) struct DestinationUnwind {
    disposition: DestinationDisposition,
    action: Box<dyn FnOnce(&mut App)>,
}

impl DestinationUnwind {
    /// Build cleanup for a failure that created no destination artifact.
    fn nothing() -> Self {
        Self { disposition: DestinationDisposition::Nothing, action: Box::new(|_| {}) }
    }

    /// Own and drop a fresh hidden destination, renderer before window.
    fn drop_fresh(window: Arc<Window>, renderer: Option<GpuRenderer>) -> Self {
        Self {
            disposition: DestinationDisposition::DropFresh,
            action: Box::new(move |_| {
                // The renderer owns another window Arc, so it must release that
                // reference before the final local Arc is dropped.
                drop(renderer);
                drop(window);
            }),
        }
    }

    /// Own a warm destination until it is either returned unchanged or retired.
    fn warm(warm: super::WarmWindow, disposition: DestinationDisposition) -> Self {
        assert!(
            matches!(
                disposition,
                DestinationDisposition::ReturnWarm | DestinationDisposition::RetireWarm
            ),
            "a warm destination supports only return or retirement"
        );
        Self {
            disposition,
            action: Box::new(move |app| match disposition {
                DestinationDisposition::ReturnWarm => {
                    // Adoption has not mutated the hidden renderer, so restore
                    // the unchanged spare to the pool.
                    app.warm_window_pool.push(warm);
                }
                DestinationDisposition::RetireWarm => {
                    // Adoption already changed renderer state, so drop the
                    // hidden entry rather than return a poisoned spare.
                    drop(warm);
                }
                _ => unreachable!("the constructor rejected non-warm dispositions"),
            }),
        }
    }

    /// Run the owned cleanup exactly once and report its disposition.
    fn run(self, app: &mut App) -> DestinationDisposition {
        (self.action)(app);
        self.disposition
    }
}

/// Destination preparation failure with every artifact needed for cleanup.
pub(super) struct DestinationFailure {
    stage: TearOutStage,
    detail: Option<String>,
    unwind: DestinationUnwind,
}

impl DestinationFailure {
    /// Bind a failed stage and diagnostic to its required destination cleanup.
    fn new(
        origin: ChildRendererOrigin,
        stage: TearOutStage,
        detail: String,
        unwind: DestinationUnwind,
    ) -> Self {
        assert_eq!(
            unwind.disposition,
            destination_disposition(origin, stage),
            "destination cleanup must match the failed stage and origin"
        );
        Self { stage, detail: Some(detail), unwind }
    }

    /// Build a test failure whose owned action observes cleanup ordering.
    #[cfg(test)]
    fn probe(
        stage: TearOutStage,
        disposition: DestinationDisposition,
        action: Box<dyn FnOnce(&mut App)>,
    ) -> Self {
        Self { stage, detail: None, unwind: DestinationUnwind { disposition, action } }
    }
}

/// Failure owning both detached source state and partial destination cleanup.
struct TearOutFailure {
    stage: TearOutStage,
    detail: Option<String>,
    transaction: DetachedTab,
    unwind: DestinationUnwind,
}

/// Hidden, fully configured destination ready for an infallible commit.
struct PreparedDestination {
    window: Arc<Window>,
    renderer: GpuRenderer,
    timing: crate::app::TearOutTiming,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LiveFontSettings<'a> {
    family: &'a str,
    size: f32,
    line_height: f32,
    weight_scale: f32,
}

#[derive(Clone, Copy)]
struct LiveRendererSettings<'a> {
    font: Option<LiveFontSettings<'a>>,
    theme: Option<&'a Theme>,
    background: &'a str,
    subpixel_aa: sonicterm_cfg::config::SubpixelAaMode,
    tab_bar_visible: bool,
}

fn live_renderer_settings<'a>(
    config: &'a Config,
    theme: &'a Theme,
    tab_bar_visible: bool,
    origin: ChildRendererOrigin,
) -> LiveRendererSettings<'a> {
    let refresh_cached_state = origin == ChildRendererOrigin::WarmPool;
    LiveRendererSettings {
        font: refresh_cached_state.then_some(LiveFontSettings {
            family: &config.font.family,
            size: config.font.size,
            line_height: config.font.line_height,
            weight_scale: config.font.effective_weight_scale(),
        }),
        theme: refresh_cached_state.then_some(theme),
        background: theme.colors.background.0.as_str(),
        subpixel_aa: config.font.subpixel_aa,
        tab_bar_visible,
    }
}

impl App {
    fn tear_out_renderer_settings(
        &self,
        role: &'static str,
    ) -> sonicterm_gpu::core::RendererSettings<'_> {
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
            role,
        }
    }

    pub(super) fn configure_child_renderer(
        &self,
        renderer: &mut GpuRenderer,
        window: &Window,
        origin: ChildRendererOrigin,
    ) -> bool {
        if let Some(proxy) = self.event_loop_proxy.clone() {
            super::build_async_fallback_loader_for_proxy(proxy);
            renderer.set_async_loader(());
        }
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);
        renderer.set_software_render_degrade(crate::app::should_degrade_for_software_render(
            self.config.appearance.software_render_mode,
            renderer.is_software_rendering(),
        ));
        renderer.set_titlebar_inset(0.0);
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());
        let live = live_renderer_settings(&self.config, &self.theme, self.tab_bar_visible, origin);
        // RendererSettings does not carry tab-bar visibility, so every fresh or
        // pooled child receives the current app value here.
        renderer.set_subpixel_aa_mode(live.subpixel_aa);
        renderer.set_tab_bar_visible(live.tab_bar_visible);
        super::install_native_window_background(window, live.background);
        if let Some(font) = live.font {
            // Runtime font-weight and theme actions walk visible windows only.
            // A pooled renderer captured both values when it was hidden, so
            // adoption must resynchronize it before its first visible frame.
            // Fresh renderers already received them from their constructors and
            // `live.font` is None, skipping the expensive atlas/font rebuild.
            renderer.set_font(font.family, font.size, font.line_height, font.weight_scale);
        }
        if let Some(theme) = live.theme {
            renderer.set_theme(theme);
        }
        let real_sf = window_dpi(window);
        renderer.force_rebuild_for_scale(real_sf);
        let target = super::apply_terminal_window_minimum(window, renderer);
        renderer.try_resize(target.width.max(1), target.height.max(1))
    }

    pub(super) fn warm_window_pool_maintain(&mut self, el: &ActiveEventLoop) {
        let Some(software_rendering) = self.main_renderer().map(|renderer| {
            renderer.is_software_rendering() || renderer.is_software_render_degraded()
        }) else {
            // When: `main_renderer` is absent, so the software-rendering state that
            // sizes the pool cannot be read; skip maintenance rather than size it wrong.
            return;
        };
        let configured = self.config.window.warm_window_pool;
        let target = super::warm_window_pool_target(configured, software_rendering);
        let count_before = self.warm_window_pool.len();
        if self.warm_window_pool.len() > target {
            self.warm_window_pool.truncate(target);
        }
        if super::warm_window_pool_should_spawn(
            self.warm_window_pool.len(),
            configured,
            software_rendering,
        ) {
            if let Some(warm) = self.create_warm_window(el) {
                self.warm_window_pool.push(warm);
            }
        }
        if self.warm_window_pool.len() != count_before {
            tracing::debug!(
                target: "memory",
                configured,
                target,
                software_rendering,
                warm_renderer_count = self.warm_window_pool.len(),
                "warm renderer pool maintained"
            );
        }
    }

    fn create_warm_window(&mut self, el: &ActiveEventLoop) -> Option<super::WarmWindow> {
        let attrs = super::with_app_icon(super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(super::NATIVE_WINDOW_TITLE)
                    .with_decorations(true)
                    .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0))
                    .with_visible(false),
            ),
            self.config.appearance.backdrop,
            self.config.appearance.software_render_mode,
        ));
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                // When: `create_window` failed, so there is no window to pool; the pool
                // only pre-warms, so a short pool costs tear-out latency, not correctness.
                tracing::warn!("warm-window-pool: create_window failed: {e}");
                return None;
            }
        };
        window.set_ime_allowed(true);
        let settings = self.tear_out_renderer_settings("warm");
        let shared_gpu = self.main_renderer().map(GpuRenderer::shared_context);
        let mut renderer = match shared_gpu.map_or_else(
            || GpuRenderer::new(window.clone(), el, &self.theme, settings),
            |ctx| {
                GpuRenderer::new_with_shared_context(window.clone(), el, &self.theme, settings, ctx)
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                // When: `GpuRenderer` construction failed, so the created window has no
                // renderer to pool; drop it rather than pool a window that cannot draw.
                tracing::warn!("warm-window-pool: renderer init failed: {e}");
                return None;
            }
        };
        if !self.configure_child_renderer(&mut renderer, &window, ChildRendererOrigin::Fresh) {
            // When: `configure_child_renderer` rejected the initial size, so this window
            // cannot render; drop it rather than leave an unusable entry in the pool.
            tracing::error!("warm-window-pool: renderer rejected unsafe initial size");
            return None;
        }
        Some(super::WarmWindow { window, renderer, created_at: Instant::now() })
    }

    fn take_warm_window(&mut self) -> Option<super::WarmWindow> {
        self.warm_window_pool.pop()
    }

    pub(super) fn is_warm_window_id(&self, win_id: WindowId) -> bool {
        self.warm_window_pool.iter().any(|warm| warm.window.id() == win_id)
    }

    /// The id of the tab currently at `idx` in `window`.
    ///
    /// Recorded when a tear-out is queued so the request names a tab rather
    /// than a slot.
    pub(super) fn tab_id_at(
        &self,
        window: WindowId,
        idx: usize,
    ) -> Option<sonicterm_ui::tabs::TabId> {
        self.windows.get(&window)?.tabs.tabs().get(idx).map(|tab| tab.id)
    }

    /// Where the tab `id` currently sits in `window`, or `None` if it is gone.
    ///
    /// The counterpart to [`Self::tab_id_at`]: a queued request re-resolves
    /// through this at the moment it is applied, so a tab that moved is still
    /// found and a tab that closed fails the operation instead of silently
    /// promoting whichever tab inherited its index.
    pub(super) fn tab_index_of_id(
        &self,
        window: WindowId,
        id: sonicterm_ui::tabs::TabId,
    ) -> Option<usize> {
        self.windows.get(&window)?.tabs.tabs().iter().position(|tab| tab.id == id)
    }

    /// Detach one tab into a transaction that can be committed or restored.
    pub(super) fn detach_for_tear_out(
        &mut self,
        source: TearOutSource,
        index: usize,
    ) -> Option<DetachedTab> {
        let source_window = source.window_id();
        let prior_active_tab_id = {
            let window = self.windows.get(&source_window)?;
            let source_matches = match source {
                TearOutSource::Main(id) => self.main_window_id == Some(id),
                TearOutSource::Child(id) => self.main_window_id != Some(id),
            };
            if !source_matches || index >= window.tabs.len() || index >= window.tab_states.len() {
                // When: the source role or either parallel tab vector rejects the
                // index, no state may be detached from a different slot or window.
                return None;
            }
            window.tabs.active().map(|tab| tab.id)
        };
        let (tab, state, panes) = match source {
            TearOutSource::Main(_) => self.detach_tab_state(index),
            TearOutSource::Child(id) => self.detach_from_child(id, index),
        }?;
        Some(DetachedTab { source, original_index: index, prior_active_tab_id, tab, state, panes })
    }

    /// Restore a detached transaction without applying transfer side effects.
    fn rollback_detached_tab(&mut self, transaction: DetachedTab) {
        let DetachedTab { source, original_index, prior_active_tab_id, tab, state, panes } =
            transaction;
        let window = self.windows.get_mut(&source.window_id()).expect(
            "a tear-out source cannot disappear during synchronous destination preparation",
        );
        let index = original_index.min(window.tabs.len());
        for (pane_id, pane) in panes {
            let displaced = window.panes.insert(pane_id, pane);
            assert!(displaced.is_none(), "a detached pane id must remain absent until rollback");
        }
        window.tabs.insert(index, tab);
        window.tab_states.insert(index, state);
        let active_index = prior_active_tab_id
            .and_then(|id| window.tabs.tabs().iter().position(|tab| tab.id == id))
            .unwrap_or(index);
        window.tabs.activate(active_index);
        // Rollback deliberately bypasses the normal attach helpers: resize,
        // redraw-target replacement, and owner reattribution are transfer effects.
    }

    /// Prepare and either commit a destination or unwind it before source rollback.
    fn tear_out_with_destination<F>(
        &mut self,
        transaction: DetachedTab,
        prepare: F,
    ) -> Option<WindowId>
    where
        F: FnOnce(&mut Self) -> Result<PreparedDestination, DestinationFailure>,
    {
        match prepare(self) {
            Ok(destination) => Some(self.commit_torn_out_window(transaction, destination)),
            Err(DestinationFailure { stage, detail, unwind }) => {
                // The failure owns both halves so partial native state cannot
                // outlive source recovery.
                self.unwind_tear_out_failure(TearOutFailure { stage, detail, transaction, unwind });
                None
            }
        }
    }

    /// Dispose of partial destination state before restoring its detached source.
    fn unwind_tear_out_failure(&mut self, failure: TearOutFailure) -> DestinationDisposition {
        let source = failure.transaction.source;
        let disposition = failure.unwind.run(self);
        self.rollback_detached_tab(failure.transaction);
        tracing::warn!(
            ?source,
            stage = ?failure.stage,
            detail = failure.detail.as_deref().unwrap_or("unavailable"),
            ?disposition,
            "tear-out destination failed; source transaction restored"
        );
        disposition
    }

    pub(super) fn queue_active_tab_tear_out(&mut self, source_window: WindowId) -> bool {
        if self.pending_tear_out.is_some() {
            // When: a tear-out request is already queued; a second would overwrite the
            // first and strand its tab, so refuse until `pending_tear_out` is drained.
            return false;
        }
        let source_tab_idx = if Some(source_window) == self.main_window_id {
            // When: `source_window` is the main window, whose tabs live in `main_tabs`,
            // not in the `windows` map; read the active index from there.

            // When: `main_tabs` is missing or empty, so there is no active tab to tear
            // out; refuse rather than record an index that names nothing.
            let tabs = match self.main_tabs() {
                Some(tabs) if !tabs.is_empty() => tabs,
                _ => return false,
            };
            tabs.active_index()
        } else {
            // When: `source_window` is a child window, so its tabs live in the
            // `windows` map, not `main_tabs`; resolve the index from that entry.

            // When: no child window is registered under `source_window`, or it holds no
            // tabs; refuse rather than name an index no window can supply.
            let child = match self.windows.get(&source_window) {
                Some(child) if !child.tabs.is_empty() => child,
                _ => return false,
            };
            child.tabs.active_index()
        };
        let source_tab_id = self.tab_id_at(source_window, source_tab_idx);
        self.pending_tear_out = Some(super::PendingTearOut {
            source_window,
            source_tab_idx,
            source_tab_id,
            drop_screen_pos: None,
        });
        true
    }

    /// Tear a main-window tab into a new native window.
    pub(super) fn tear_out_tab(&mut self, el: &ActiveEventLoop, index: usize) -> bool {
        self.tear_out_tab_with_installer(index, |app, transaction, screen_pos, source| {
            app.install_torn_out_window(el, transaction, screen_pos, source)
        })
    }

    /// Run the main tear-out route with only destination installation injected.
    fn tear_out_tab_with_installer<I>(&mut self, index: usize, install: I) -> bool
    where
        I: FnOnce(&mut App, DetachedTab, Option<(i32, i32)>, &'static str) -> Option<WindowId>,
    {
        if self.try_cross_window_merge(index) {
            // When: `try_cross_window_merge` returns true, account for the tab
            // leaving only after that route reports completion.
            self.dispatch_tear_out_intent(index);
            return true;
        }
        // OS handoff must run before local detachment because an acknowledged
        // receiver becomes the new owner, while an unacknowledged publication
        // leaves this live session available for in-process tear-out.
        if self.try_os_drag_handoff(index) {
            // When: `try_os_drag_handoff` returns true, account for departure
            // only after the handoff reports that the local route must stop.
            self.dispatch_tear_out_intent(index);
            return true;
        }
        let Some(main_id) = self.main_window_id else {
            // When: `main_window_id` is `None`, no main tab can be detached or accounted.
            return true;
        };
        let Some(transaction) = self.detach_for_tear_out(TearOutSource::Main(main_id), index)
        else {
            // When: `detach_for_tear_out` returns `None`, end the gesture without
            // changing either live topology or its reducer mirror.
            return true;
        };
        if install(self, transaction, None, "main").is_none() {
            // When: `install` returns `None`, its owning failure already restored
            // the source; success-only activation and hiding must not run.
            return true;
        }
        self.dispatch_tear_out_intent(index);
        self.tear_out_apply_source_side(index);
        tracing::info!("tab torn out as new window; windows={}", self.windows.len());
        true
    }

    /// Record reducer state only after a main tab has left its source strip.
    fn dispatch_tear_out_intent(&mut self, index: usize) {
        let pending_new_window = self.pending_new_window;
        self.dispatch_intent(sonicterm_app_core::AppIntent::TearOutTab {
            src_window: sonicterm_types::WindowKey::new(0),
            src_tab: index,
        });
        // The route already created or selected its destination. Preserve the
        // reducer's WindowOpen observability without spawning a duplicate window.
        self.pending_new_window = pending_new_window;
    }

    /// Install a detached transaction through the native preparation boundary.
    pub(super) fn install_torn_out_window(
        &mut self,
        el: &ActiveEventLoop,
        transaction: DetachedTab,
        screen_pos: Option<(i32, i32)>,
        source: &'static str,
    ) -> Option<WindowId> {
        self.tear_out_with_destination(transaction, |app| {
            app.prepare_tear_out_destination(el, screen_pos, source)
        })
    }

    /// Build and configure a hidden destination without mutating pane state.
    fn prepare_tear_out_destination(
        &mut self,
        el: &ActiveEventLoop,
        screen_pos: Option<(i32, i32)>,
        source: &'static str,
    ) -> Result<PreparedDestination, DestinationFailure> {
        let tear_start = Instant::now();
        let (window, renderer, create_window_ms, renderer_init_ms, resize_ms) =
            match self.take_warm_window() {
                Some(mut warm) => {
                    // When: `take_warm_window` returns `Some`, adopt that hidden
                    // renderer rather than constructing another destination.
                    if let Some((sx, sy)) = screen_pos {
                        warm.window.set_outer_position(winit::dpi::PhysicalPosition::new(sx, sy));
                    }
                    let resize_start = Instant::now();
                    if !self.configure_child_renderer(
                        &mut warm.renderer,
                        &warm.window,
                        ChildRendererOrigin::WarmPool,
                    ) {
                        // When: pooled adoption mutated the renderer but rejected its
                        // size, retire that hidden entry rather than returning it poisoned.
                        return Err(DestinationFailure::new(
                            ChildRendererOrigin::WarmPool,
                            TearOutStage::RendererConfigure,
                            "renderer rejected unsafe child size".to_owned(),
                            DestinationUnwind::warm(warm, DestinationDisposition::RetireWarm),
                        ));
                    }
                    let resize_ms = resize_start.elapsed().as_secs_f32() * 1000.0;
                    (warm.window, warm.renderer, 0.0, 0.0, resize_ms)
                }
                None => {
                    // When: `take_warm_window` returns `None`, construct a fresh
                    // destination hidden so every failure remains invisible.
                    let mut attrs = super::with_app_icon(super::with_backdrop_transparency(
                        with_integrated_titlebar(
                            Window::default_attributes()
                                .with_title(super::NATIVE_WINDOW_TITLE)
                                .with_decorations(true)
                                .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0))
                                .with_visible(false),
                        ),
                        self.config.appearance.backdrop,
                        self.config.appearance.software_render_mode,
                    ));
                    if let Some((sx, sy)) = screen_pos {
                        attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(sx, sy));
                    }
                    let create_start = Instant::now();
                    let window = el.create_window(attrs).map(Arc::new).map_err(|error| {
                        DestinationFailure::new(
                            ChildRendererOrigin::Fresh,
                            TearOutStage::CreateWindow,
                            error.to_string(),
                            DestinationUnwind::nothing(),
                        )
                    })?;
                    let create_window_ms = create_start.elapsed().as_secs_f32() * 1000.0;
                    window.set_ime_allowed(true);
                    let shared_gpu = self.main_renderer().map(GpuRenderer::shared_context);
                    let renderer_settings = self.tear_out_renderer_settings("child");
                    let renderer_start = Instant::now();
                    let mut renderer = shared_gpu
                        .map_or_else(
                            || GpuRenderer::new(window.clone(), el, &self.theme, renderer_settings),
                            |ctx| {
                                GpuRenderer::new_with_shared_context(
                                    window.clone(),
                                    el,
                                    &self.theme,
                                    renderer_settings,
                                    ctx,
                                )
                            },
                        )
                        .map_err(|error| {
                            DestinationFailure::new(
                                ChildRendererOrigin::Fresh,
                                TearOutStage::RendererInit,
                                error.to_string(),
                                DestinationUnwind::drop_fresh(window.clone(), None),
                            )
                        })?;
                    let renderer_init_ms = renderer_start.elapsed().as_secs_f32() * 1000.0;
                    let resize_start = Instant::now();
                    if !self.configure_child_renderer(
                        &mut renderer,
                        &window,
                        ChildRendererOrigin::Fresh,
                    ) {
                        // When: fresh configuration rejects the hidden destination,
                        // its renderer and window remain owned by the failure.
                        return Err(DestinationFailure::new(
                            ChildRendererOrigin::Fresh,
                            TearOutStage::RendererConfigure,
                            "renderer rejected unsafe child size".to_owned(),
                            DestinationUnwind::drop_fresh(window, Some(renderer)),
                        ));
                    }
                    let resize_ms = resize_start.elapsed().as_secs_f32() * 1000.0;
                    (window, renderer, create_window_ms, renderer_init_ms, resize_ms)
                }
            };
        let mut timing = crate::app::TearOutTiming::new(source, tear_start);
        timing.create_window_ms = create_window_ms;
        timing.renderer_init_ms = renderer_init_ms;
        timing.resize_ms = resize_ms;
        Ok(PreparedDestination { window, renderer, timing })
    }

    /// Install a fully prepared destination and then reveal it exactly once.
    fn commit_torn_out_window(
        &mut self,
        transaction: DetachedTab,
        mut destination: PreparedDestination,
    ) -> WindowId {
        let install_start = Instant::now();
        destination.renderer.set_render_timing_label("child");
        let win_id = destination.window.id();
        // Each independent redraw-target guard is released before the pane map
        // moves into the destination window; no two pane locks are nested.
        for pane in transaction.panes.values() {
            *pane.redraw_target.lock() = Some(win_id);
        }

        let mut child_tabs = TabBar::new();
        let active_pane = transaction.state.active_pane;
        child_tabs.push(transaction.tab);
        let child = WindowState {
            // Registered when the window is inserted; construction has no
            // governor in scope.
            owner: None,
            role: crate::app::WindowRole::Terminal,
            window: Some(destination.window.clone()),
            renderer: Some(destination.renderer),
            tabs: child_tabs,
            tab_states: vec![TabState {
                tree: transaction.state.tree,
                active_pane,
                search: transaction.state.search,
                command: transaction.state.command,
            }],
            panes: transaction.panes,
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
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            path_probe: crate::app::path_target::PathProbeState::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: std::collections::HashMap::new(),
            pending_tear_out_timing: Some(destination.timing),
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.insert_window_registered(win_id, child);
        // Now that the child WindowState exists, size each migrated pane to
        // its OWN split sub-rect (via compute_pane_rects_for) instead of the
        // full child grid. Sizing every pane to the whole `(cols, rows)` is
        // what makes a torn-out SPLIT overlap — the left pane stays
        // full-window wide and wraps/paints across the divider into the right
        // pane. For a single-pane tab this is equivalent to a full-grid resize.
        if let Some(child) = self.windows.get_mut(&win_id) {
            super::child_window::resize_visible_panes_in_child(child);
        }
        // Register the new window's HWND with
        // the OS-drag backend so drops on this child window reach
        // IDropTarget::Drop. No-op on mac (pasteboard model).
        self.register_window_with_os_drag_backend(win_id, &destination.window);
        if let Some(child) = self.windows.get_mut(&win_id) {
            if let Some(timing) = child.pending_tear_out_timing.as_mut() {
                timing.install_ms = install_start.elapsed().as_secs_f32() * 1000.0;
                tracing::warn!(
                    target: "tear_out_timing",
                    source = timing.source,
                    create_window_ms = timing.create_window_ms,
                    renderer_init_ms = timing.renderer_init_ms,
                    resize_ms = timing.resize_ms,
                    install_ms = timing.install_ms,
                    "tear-out install latency breakdown"
                );
            }
        }
        // Both origins were hidden throughout preparation. The destination is
        // now registered and sized, so its first visible frame is complete.
        destination.window.set_visible(true);
        destination.window.request_redraw();
        // A consumed pooled window is not replaced here; the pool refills on
        // the next idle tick rather than on this path.
        self.frontmost_window = Some(win_id);
        win_id
    }

    /// source-side post-tear-out cleanup, factored
    /// out so unit tests can drive it without an `ActiveEventLoop`.
    ///
    /// * If main is now empty, hide it (existing drained-main path).
    /// * Else activate `max(0, removed_idx - 1)` (the left neighbor).
    ///
    /// `detach_tab_state` already adjusts the active index via
    /// `TabBar::close`, but its rule ("stay at the same numeric
    /// index, clamp on overflow") shifts focus RIGHT when the active
    /// tab was removed. Overridden to consistently pick the
    /// LEFT neighbor, matching common terminal-emulator UX.
    pub fn tear_out_apply_source_side(&mut self, removed_idx: usize) {
        let is_empty = self.main_tabs().map(|t| t.is_empty()).unwrap_or(true);
        if is_empty {
            // When: `is_empty` reports main drained by the tear-out; hide main only if a
            // child window survives, so the user is never left with no visible window.
            if self.child_window_count() > 0 {
                self.hide_main_window();
            }
            return;
        }
        if let Some(t) = self.main_tabs_mut() {
            let target = removed_idx.saturating_sub(1).min(t.len().saturating_sub(1));
            t.activate(target);
        }
    }
}

impl App {
    pub(super) fn compute_child_drag_target(
        &self,
        src_id: WindowId,
        local_in_src: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let src_child = self.windows.get(&src_id)?;
        let src_origin = src_child
            .window
            .as_ref()?
            .inner_position()
            .map(|p| (p.x, p.y))
            .unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(src_origin, local_in_src);
        let mut candidates: Vec<(WindowId, crate::tab_drag::WindowGeom, Option<TabBarLayout>)> =
            Vec::new();
        if let Some(main) = self.main_window() {
            let geom = window_geom(main);
            let width = self.main_renderer().map(|r| r.width() as f32).unwrap_or(0.0);
            let inset = self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0);
            let bar_h = self
                .main_renderer()
                .map(|r| r.tab_bar_logical_height())
                .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT);
            candidates.push((
                main.id(),
                geom,
                self.main_tabs().map(|t| {
                    TabBarLayout::compute_with_height(t, width, bar_h)
                        .with_top_offset(inset)
                        .with_visible(self.tab_bar_visible)
                }),
            ));
        }
        for (id, c) in &self.windows {
            if *id == src_id || Some(*id) == self.main_window_id {
                // When: `id` is the drag source or the window named by `main_window_id`,
                // which was already pushed above; skip so no window is a candidate twice.
                continue;
            }
            let Some(r) = c.renderer.as_ref() else {
                // When: the child has no `renderer`, so its width and tab-bar height are
                // unknown and no drop rect can be built; leave it out of `candidates`.
                continue;
            };
            let Some(cw) = c.window.as_ref() else {
                // When: the child holds no `window`, so `window_geom` has nothing to read
                // its screen rect from; skip it rather than hit-test a placeless entry.
                continue;
            };
            let geom = window_geom(cw);
            let bar_width = r.width() as f32;
            let layout =
                TabBarLayout::compute_with_height(&c.tabs, bar_width, r.tab_bar_logical_height())
                    .with_top_offset(r.tab_bar_y_offset())
                    .with_visible(r.tab_bar_visible());
            candidates.push((*id, geom, Some(layout)));
        }
        crate::tab_drag::find_drop_target_skipping_unrendered(global, candidates)
    }
    pub(super) fn compute_main_drag_target(
        &self,
        local_in_main: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let main_window = self.main_window()?;
        let main_origin =
            main_window.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(main_origin, local_in_main);
        let candidates = self.windows.iter().filter_map(|(id, c)| {
            if Some(*id) == self.main_window_id {
                // When: `id` is `main_window_id`, the window this drag started in; a tab
                // cannot drop onto its own source, so keep it out of `candidates`.
                return None;
            }
            let r = c.renderer.as_ref()?;
            let cw = c.window.as_ref()?;
            let geom = window_geom(cw);
            let bar_width = r.width() as f32;
            let layout =
                TabBarLayout::compute_with_height(&c.tabs, bar_width, r.tab_bar_logical_height())
                    .with_top_offset(r.tab_bar_y_offset())
                    .with_visible(r.tab_bar_visible());
            Some((*id, geom, Some(layout)))
        });
        crate::tab_drag::find_drop_target_skipping_unrendered(global, candidates)
    }
    pub(super) fn try_os_drag_handoff(&mut self, index: usize) -> bool {
        let Some(sink) = self.os_drag_sink.clone() else {
            // When: no `os_drag_sink` is installed, so there is no cross-process route
            // for the payload; return false to fall back to the in-process tear-out.
            return false;
        };
        if self.cursor_inside_any_window() {
            // When: `cursor_inside_any_window` is true, so the drop is still over a
            // SonicTerm window; keep the gesture in-process instead of publishing to OS.
            return false;
        }
        let Some(payload) = self.build_payload_for_tab(index) else {
            // When: `build_payload_for_tab` found no tab at `index`, so there is nothing
            // to publish to the OS; return false and let the in-process path decide.
            return false;
        };

        // Hand the gesture to the installed OsTabDragBackend first. The backend is
        // responsible for OS cursor capture + pasteboard / OLE handoff. If
        // `handles_full_gesture()` returns true (Windows: DoDragDrop ran end-to-end
        // inside the backend) we MUST NOT also invoke `sink.begin_drag` — that would
        // re-enter DoDragDrop with no live gesture, immediately returning NONE and
        // falsely triggering `spawn_tearout_child`. The backend's DragOutcome routes
        // through `handle_os_drag_ended` (transfer_tab / cancel_drag_session); when the
        // backend owns the gesture we return true here without detaching — the
        // dispatcher will handle source-side removal via transfer_tab.
        //
        // On Mac (handles_full_gesture == false) the backend only writes the pasteboard
        // (winit intercepts mouse events, so NSDraggingSession proper isn't reachable) —
        // we still fall through to the sink, which also writes the pasteboard and
        // returns NotAcknowledged.
        if self.os_drag_backend.is_some() {
            // When: an `os_drag_backend` is installed, so it owns cursor capture and the
            // pasteboard/OLE handoff; run it before the sink to avoid a second DoDragDrop.
            let payload_json = payload.to_json().unwrap_or_default();
            let source_window = self.main_window().map(|w| w.id());
            if let Some(src_id) = source_window {
                // When: `source_window` resolves to a real id, which `begin_os_tab_drag`
                // needs to anchor the session and record the drag source.

                // Render a small PNG thumbnail for backends that support a
                // native preview. Windows OLE uses it; the current macOS
                // pasteboard-only backend records but cannot display it.
                // See `crates/sonicterm-app/src/tab_thumbnail.rs` for the
                // rationale behind the CPU-side renderer.
                let thumb_inputs =
                    crate::tab_thumbnail::tab_thumbnail_inputs_from_payload(&payload.tab_title);
                let drag_image_png = crate::tab_thumbnail::render_tab_thumbnail_png(&thumb_inputs);
                let started = self.begin_os_tab_drag(src_id, index, payload_json, drag_image_png);
                if started && self.os_drag_backend_handles_full_gesture() {
                    // When: `started` and the backend owns the whole gesture, so it already
                    // ran DoDragDrop; a second, gestureless call would return NONE.
                    tracing::info!(
                        tab = %payload.tab_title,
                        "backend owns gesture end-to-end; legacy sink skipped"
                    );
                    return true;
                }
            }
        }

        let ack = sink.begin_drag(&payload);
        match ack {
            crate::os_drag::DragAck::Accepted => {
                // When: the sink reports `Accepted`, so a destination adopted the payload;
                // the moved panes are dropped here, ending the local shell via PtyHandle.
                let _ = self.detach_tab_state(index);
                tracing::info!(
                    tab = %payload.tab_title,
                    "OS drag: destination acknowledged; local tab dropped"
                );
                true
            }
            crate::os_drag::DragAck::NotAcknowledged => {
                // No destination confirmed adoption. Leave the source tab
                // alive and fall back to the in-process tear-out path so
                // the user does not lose a live shell.
                tracing::warn!(
                    tab = %payload.tab_title,
                    "OS drag: sink NotAcknowledged; keeping source tab, falling back to in-process tear-out"
                );
                false
            }
        }
    }
    pub(super) fn build_payload_for_tab(&self, index: usize) -> Option<crate::os_drag::TabPayload> {
        let tab = self.main_tabs()?.tabs().get(index)?.clone();
        // Scrollback is not carried in the payload: Grid exposes no full
        // visible+scrollback text accessor, so the buffer ships empty and
        // the destination shell starts at a fresh prompt.
        let scrollback_bytes: Vec<u8> = Vec::new();
        Some(crate::os_drag::TabPayload {
            pty_pid: 0,
            tab_title: tab.title,
            scrollback_b64: crate::os_drag::TabPayload::encode_scrollback(&scrollback_bytes),
            cwd: String::new(),
            cmd: self.config.terminal.shell.clone().unwrap_or_default(),
            env: Vec::new(),
        })
    }
    pub(super) fn cursor_inside_any_window(&self) -> bool {
        let Some(main) = self.main_window() else {
            // When: no `main_window` exists, so the cursor has no origin to be made
            // global against; report it as outside rather than guess a screen point.
            return false;
        };
        let main_origin = main.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
        let global = crate::tab_drag::local_to_global(main_origin, cursor_pos);
        if crate::tab_drag::global_to_local(window_geom(main), global).is_some() {
            // When: `global_to_local` places the cursor inside main's rect, so the drop
            // is still over SonicTerm; stop before walking the child windows.
            return true;
        }
        for c in self.windows.values() {
            let Some(cw) = c.window.as_ref() else {
                // When: this child holds no `window`, so it has no screen rect to test
                // the cursor against; skip it rather than treat it as a hit.
                continue;
            };
            if crate::tab_drag::global_to_local(window_geom(cw), global).is_some() {
                // When: `global_to_local` places the cursor inside this child's rect, so
                // the drop is over SonicTerm; stop the walk at the first hit.
                return true;
            }
        }
        false
    }
    pub fn try_cross_window_merge(&mut self, index: usize) -> bool {
        let main_id = self.main_window().map(|w| w.id());
        let Some(target) =
            self.main().and_then(|ws| ws.drag_target).filter(|t| Some(t.window) != main_id)
        else {
            // When: no `drag_target` names a window other than `main_id`, so there is no
            // destination to merge into; return false so the caller tears out instead.
            return false;
        };
        if let Some(ws) = self.main_mut() {
            ws.drag_target = None;
            ws.pressed_tab = None;
            ws.mouse_down = false;
        }
        self.merge_main_into_child(index, target);
        true
    }
    pub fn tear_out_would_be_noop(&self) -> bool {
        // Tear-out is always productive — a single-tab tear creates a new
        // window with that tab and hides the now-empty main. Nothing in the
        // workspace calls this, so it stands as a `pub` `false` constant and
        // no gesture consults it before tearing out.
        false
    }

    /// Tear a child-window tab into a new native window.
    pub(super) fn tear_out_from_child(
        &mut self,
        el: &ActiveEventLoop,
        src_id: WindowId,
        index: usize,
    ) -> bool {
        self.tear_out_from_child_with_installer(
            src_id,
            index,
            |app, transaction, screen_pos, source| {
                app.install_torn_out_window(el, transaction, screen_pos, source)
            },
        )
    }

    /// Run the child tear-out route with only destination installation injected.
    pub(super) fn tear_out_from_child_with_installer<I>(
        &mut self,
        src_id: WindowId,
        index: usize,
        install: I,
    ) -> bool
    where
        I: FnOnce(&mut App, DetachedTab, Option<(i32, i32)>, &'static str) -> Option<WindowId>,
    {
        let Some(transaction) = self.detach_for_tear_out(TearOutSource::Child(src_id), index)
        else {
            // When: `detach_for_tear_out` returns `None`, no child gesture state was consumed.
            return false;
        };
        let Some(win_id) = install(self, transaction, None, "child") else {
            // When: `install` returns `None`, rollback already restored the source;
            // reaping or neighbour activation would mutate that restored window.
            return true;
        };
        self.frontmost_window = Some(win_id);
        self.tear_out_apply_child_source_side(src_id, index);
        tracing::info!(
            "tab torn out of child {:?} as new window; windows={}",
            src_id,
            self.windows.len()
        );
        true
    }

    /// child-side post-tear-out cleanup. Mirrors
    /// [`Self::tear_out_apply_source_side`] for a torn-from-child
    /// origin. Removes the source child window from
    /// `self.windows` if it became empty; else activates the
    /// LEFT neighbor of the removed slot.
    pub fn tear_out_apply_child_source_side(&mut self, src_id: WindowId, removed_idx: usize) {
        let src_empty = self.windows.get(&src_id).map(|c| c.tabs.is_empty()).unwrap_or(false);
        if src_empty {
            // When: `src_empty` reports the source child drained by the tear-out; reap it
            // so no empty window is left on screen once its last tab has moved out.
            self.reap_empty_child(src_id);
            return;
        }
        if let Some(c) = self.windows.get_mut(&src_id) {
            let target = removed_idx.saturating_sub(1).min(c.tabs.len().saturating_sub(1));
            c.tabs.activate(target);
            c.request_redraw();
        }
    }
}

#[cfg(test)]
#[path = "tear_out_tests.rs"]
mod tear_out_tests;
