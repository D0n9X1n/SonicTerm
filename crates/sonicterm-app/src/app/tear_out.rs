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

mod drag_target;
mod os_handoff;

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
    request: super::WindowRequest,
    original_index: usize,
    prior_active_tab_id: Option<sonicterm_ui::tabs::TabId>,
    tab: Tab,
    state: TabState,
    panes: HashMap<u64, super::PaneState>,
}

impl DetachedTab {
    pub(super) fn attach(
        self,
        app: &mut App,
        target: WindowId,
        index: usize,
    ) -> Result<(), super::TransferError> {
        let Self { source, request, original_index, prior_active_tab_id, tab, state, panes } = self;
        let attached = if app.main_window_id == Some(target) {
            app.attach_tab_state(index, tab, state, panes)
        } else {
            // When: target is not main_window_id, use the child admission boundary with the same custody guarantee.
            app.attach_to_child(target, index, tab, state, panes)
        };
        if let Err(failure) = attached {
            // When: attached is Err(failure), restore exact source order/focus without resize or owner effects.
            app.rollback_detached_tab(Self {
                source,
                request,
                original_index,
                prior_active_tab_id,
                tab: failure.tab,
                state: failure.state,
                panes: failure.panes,
            });
            return Err(failure.error);
        }
        Ok(())
    }
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
    /// The destination's GPU device stopped accepting work before anything
    /// was taken for it.
    DeviceStopped,
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
        (_, TearOutStage::DeviceStopped) => DestinationDisposition::Nothing,
        (ChildRendererOrigin::Fresh, TearOutStage::CreateWindow) => DestinationDisposition::Nothing,
        (ChildRendererOrigin::Fresh, _) => DestinationDisposition::DropFresh,
        (ChildRendererOrigin::WarmPool, TearOutStage::RendererConfigure) => {
            DestinationDisposition::RetireWarm
        }
        (ChildRendererOrigin::WarmPool, _) => DestinationDisposition::ReturnWarm,
    }
}

/// The stage that refuses a tear-out before it takes the pooled spare.
///
/// `spare_accepts_gpu_work` is `None` when the pool is empty. A spare whose
/// device stopped would adopt the tab into a window that never presents, so it
/// is refused before it is taken, configured, or committed, as renderer
/// construction refuses a fresh destination on a stopped device.
fn warm_destination_refusal(spare_accepts_gpu_work: Option<bool>) -> Option<TearOutStage> {
    (spare_accepts_gpu_work == Some(false)).then_some(TearOutStage::DeviceStopped)
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

/// Hidden, fully configured destination awaiting accounting admission and live installation.
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
    /// Settings for a warm-pool renderer: a hidden window that may never draw, so its glyph
    /// atlas starts at the 256 floor and grows after adoption like any other.
    fn warm_renderer_settings(&self) -> sonicterm_gpu::core::RendererSettings<'_> {
        sonicterm_gpu::core::RendererSettings {
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Minimum,
            ..self.tear_out_renderer_settings("warm")
        }
    }

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
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        }
    }

    pub(super) fn configure_child_renderer(
        &self,
        renderer: &mut GpuRenderer,
        window: &Window,
        origin: ChildRendererOrigin,
    ) -> bool {
        if let Some(proxy) = self.event_loop_proxy.clone() {
            // Preserve generation-tagged recovery notifications through the available event-loop proxy.
            renderer.set_device_state_waker(super::gpu_recovery::generation_waker(
                proxy.clone(),
                renderer.device_generation(),
            ));
            // A fallback face published for this window's fonts wakes this window.
            renderer.set_font_fallback_waker(super::font_fallback_waker(proxy, window.id()));
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

    pub(super) fn warm_window_pool_maintain(&mut self, event_loop: &ActiveEventLoop) {
        let Some(software_rendering) = self.main_renderer().map(|renderer| {
            renderer.is_software_rendering() || renderer.is_software_render_degraded()
        }) else {
            // When: `main_renderer` is absent, so the software-rendering state that
            // sizes the pool cannot be read; skip maintenance rather than size it wrong.
            return;
        };
        // A stopped device refuses every renderer, so prewarming would churn hidden windows.
        let device_accepts_gpu_work =
            self.main_renderer().is_some_and(GpuRenderer::device_accepts_gpu_work);
        let configured = self.config.window.warm_window_pool;
        let target = super::warm_window_pool_target(configured, software_rendering);
        let count_before = self.warm_window_pool.len();
        if self.warm_window_pool.len() > target {
            self.warm_window_pool.truncate(target);
        }
        if super::warm_window_pool_may_spawn(
            device_accepts_gpu_work,
            self.warm_window_pool.len(),
            configured,
            software_rendering,
        ) {
            if let Some(warm) = self.create_warm_window(event_loop) {
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

    fn create_warm_window(&mut self, event_loop: &ActiveEventLoop) -> Option<super::WarmWindow> {
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
        let attrs = self.native_drop_attributes(attrs);
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                // When: `create_window` failed, so there is no window to pool; the pool
                // only pre-warms, so a short pool costs tear-out latency, not correctness.
                tracing::warn!("warm-window-pool: create_window failed: {error}");
                return None;
            }
        };
        window.set_ime_allowed(true);
        let settings = self.warm_renderer_settings();
        let shared_gpu = self.shared_gpu_context();
        let mut renderer = match shared_gpu.map_or_else(
            || GpuRenderer::new(window.clone(), event_loop, &self.theme, settings),
            |ctx| {
                GpuRenderer::new_with_shared_context(
                    window.clone(),
                    event_loop,
                    &self.theme,
                    settings,
                    ctx,
                )
            },
        ) {
            Ok(renderer) => renderer,
            Err(error) => {
                // When: `GpuRenderer` construction failed, so the created window has no
                // renderer to pool; drop it rather than pool a window that cannot draw.
                tracing::warn!("warm-window-pool: renderer init failed: {error}");
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

    /// Test hook: build one warm-pool window exactly as pool maintenance does, pool it, and return
    /// its id; `None` when its window or renderer could not be built.
    #[doc(hidden)]
    pub fn __test_prewarm_window(&mut self, event_loop: &ActiveEventLoop) -> Option<WindowId> {
        let warm = self.create_warm_window(event_loop)?;
        let id = warm.window.id();
        self.warm_window_pool.push(warm);
        Some(id)
    }

    /// Test hook: the glyph atlas dimension of pooled warm window `id`, `None` once it left the pool.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_warm_glyph_atlas_dim(&self, id: WindowId) -> Option<u32> {
        self.warm_window_pool
            .iter()
            .find(|warm| warm.window.id() == id)
            .map(|warm| warm.renderer.glyph_atlas_facts().dim)
    }

    /// Test hook: tear the main window's tab at `index` out through the production route, which
    /// adopts a pooled warm window when one is ready.
    #[doc(hidden)]
    pub fn __test_tear_out_tab(&mut self, event_loop: &ActiveEventLoop, index: usize) -> bool {
        self.tear_out_tab(event_loop, index)
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
            let tab = &window.tab_states[index];
            if !tab.tree.leaves().contains(&tab.active_pane)
                || tab.tree.zoomed_pane_id().is_some_and(|id| id != tab.active_pane)
            {
                // When: the source tab's active/visible identity is invalid, refuse before any custody is detached.
                return None;
            }
            window.tabs.active().map(|tab| tab.id)
        };
        let request = self.window_request(Some(source_window));
        let (tab, state, panes) = match source {
            TearOutSource::Main(_) => self.detach_tab_state(index),
            TearOutSource::Child(id) => self.detach_from_child(id, index),
        }?;
        Some(DetachedTab {
            source,
            request,
            original_index: index,
            prior_active_tab_id,
            tab,
            state,
            panes,
        })
    }

    /// Restore a detached transaction without applying transfer side effects.
    pub(super) fn rollback_detached_tab(&mut self, transaction: DetachedTab) {
        let DetachedTab {
            source,
            request: _,
            original_index,
            prior_active_tab_id,
            tab,
            state,
            panes,
        } = transaction;
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
            Ok(destination) => self.commit_torn_out_window(transaction, destination),
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
    pub(super) fn tear_out_tab(&mut self, event_loop: &ActiveEventLoop, index: usize) -> bool {
        self.tear_out_tab_with_installer(index, |app, transaction, screen_pos, source| {
            app.install_torn_out_window(event_loop, transaction, screen_pos, source)
        })
    }

    /// Run the main tear-out route with only destination installation injected.
    fn tear_out_tab_with_installer<I>(&mut self, index: usize, install: I) -> bool
    where
        I: FnOnce(&mut App, DetachedTab, Option<(i32, i32)>, &'static str) -> Option<WindowId>,
    {
        let has_target = self
            .main()
            .and_then(|window| window.drag_target)
            .is_some_and(|target| Some(target.window) != self.main_window_id);
        if has_target {
            // When: has_target selects a merge, refusal restores source custody instead of falling through to another move.
            if self.try_cross_window_merge(index) {
                self.dispatch_tear_out_intent(index);
            }
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
        if let Some(src_window) = self.main_window_id.and_then(|id| self.window_key(id)) {
            self.observe_intent(sonicterm_app_core::AppIntent::TearOutTab {
                src_window,
                src_tab: index,
            });
        }
    }

    /// Install a detached transaction through the native preparation boundary.
    pub(super) fn install_torn_out_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        transaction: DetachedTab,
        screen_pos: Option<(i32, i32)>,
        source: &'static str,
    ) -> Option<WindowId> {
        let request = transaction.request;
        self.tear_out_with_destination(transaction, |app| {
            app.prepare_tear_out_destination(event_loop, screen_pos, source, request)
        })
    }

    /// Build and configure a hidden destination without mutating pane state.
    fn prepare_tear_out_destination(
        &mut self,
        event_loop: &ActiveEventLoop,
        screen_pos: Option<(i32, i32)>,
        source: &'static str,
        request: super::WindowRequest,
    ) -> Result<PreparedDestination, DestinationFailure> {
        let tear_start = Instant::now();
        let spare_accepts_gpu_work =
            self.warm_window_pool.last().map(|warm| warm.renderer.device_accepts_gpu_work());
        if let Some(stage) = warm_destination_refusal(spare_accepts_gpu_work) {
            // When: `warm_destination_refusal` returns a stage, the stopped spare stays pooled.
            return Err(DestinationFailure::new(
                ChildRendererOrigin::WarmPool,
                stage,
                "warm destination's GPU device stopped accepting work".to_owned(),
                DestinationUnwind::nothing(),
            ));
        }
        let (window, renderer, create_window_ms, renderer_init_ms, resize_ms) = match self
            .take_warm_window()
        {
            Some(mut warm) => {
                // When: `take_warm_window` returns `Some`, adopt that hidden
                // renderer rather than constructing another destination.
                if let Some((screen_x, screen_y)) = screen_pos {
                    warm.window
                        .set_outer_position(winit::dpi::PhysicalPosition::new(screen_x, screen_y));
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
                if !super::apply_window_request(&warm.window, &mut warm.renderer, request) {
                    // When: inherited sizing exceeds renderer bounds, retire the mutated hidden window and restore its source.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::WarmPool,
                        TearOutStage::RendererConfigure,
                        "renderer rejected inherited child size".to_owned(),
                        DestinationUnwind::warm(warm, DestinationDisposition::RetireWarm),
                    ));
                }
                if !warm.renderer.device_accepts_gpu_work() {
                    // When: `device_accepts_gpu_work` fails, retire the adopted spare.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::WarmPool,
                        TearOutStage::RendererConfigure,
                        "warm destination's GPU device stopped during adoption".to_owned(),
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
                            .with_inner_size(request.inner_size)
                            .with_visible(false),
                    ),
                    self.config.appearance.backdrop,
                    self.config.appearance.software_render_mode,
                ));
                if let Some((screen_x, screen_y)) = screen_pos {
                    attrs =
                        attrs.with_position(winit::dpi::PhysicalPosition::new(screen_x, screen_y));
                }
                let create_start = Instant::now();
                let attrs = self.native_drop_attributes(attrs);
                let window = event_loop.create_window(attrs).map(Arc::new).map_err(|error| {
                    DestinationFailure::new(
                        ChildRendererOrigin::Fresh,
                        TearOutStage::CreateWindow,
                        error.to_string(),
                        DestinationUnwind::nothing(),
                    )
                })?;
                let create_window_ms = create_start.elapsed().as_secs_f32() * 1000.0;
                window.set_ime_allowed(true);
                let shared_gpu = self.shared_gpu_context();
                let renderer_settings = self.tear_out_renderer_settings("child");
                let renderer_start = Instant::now();
                let mut renderer = shared_gpu
                    .map_or_else(
                        || {
                            GpuRenderer::new(
                                window.clone(),
                                event_loop,
                                &self.theme,
                                renderer_settings,
                            )
                        },
                        |ctx| {
                            GpuRenderer::new_with_shared_context(
                                window.clone(),
                                event_loop,
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
                if !renderer.device_accepts_gpu_work() {
                    // When: `device_accepts_gpu_work` fails, drop the fresh destination.
                    return Err(DestinationFailure::new(
                        ChildRendererOrigin::Fresh,
                        TearOutStage::RendererConfigure,
                        "fresh destination's GPU device stopped during sizing".to_owned(),
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
        mut transaction: DetachedTab,
        mut destination: PreparedDestination,
    ) -> Option<WindowId> {
        let install_start = Instant::now();
        destination.renderer.set_render_timing_label("child");
        let win_id = destination.window.id();
        if let Err(error) = self.register_window_with_os_drag_backend(win_id, &destination.window) {
            // When: native registration refuses the destination, preserve the detached source before any ownership transfer.
            drop(destination);
            self.rollback_detached_tab(transaction);
            tracing::error!(?win_id, %error, "tear-out native drop-target registration failed; source restored");
            return None;
        }
        if !destination.renderer.device_accepts_gpu_work() {
            // When: device_accepts_gpu_work fails after native registration, revoke custody before restoring the source.
            self.release_child_window_registries(win_id);
            drop(destination);
            self.rollback_detached_tab(transaction);
            tracing::warn!(
                ?win_id,
                "tear-out GPU device stopped during registration; source restored"
            );
            return None;
        }
        let owner = self
            .governor
            .create_child(
                self.governor.root_owner(),
                super::OwnerKind::Window,
                super::tracking_only_owner_limits(),
            )
            .map(|id| super::OwnerGuard::new(self.governor.clone(), id))
            .ok();
        let mut child_tabs = TabBar::new();
        child_tabs.push(transaction.tab.clone());
        let mut tab_states = Vec::with_capacity(1);
        self.windows.reserve(1);
        if let Err(error) = self
            .transfer_pane_owners(&mut transaction.panes, owner.as_ref().map(super::OwnerGuard::id))
        {
            // When: transfer_pane_owners refuses custody, release native registration before retiring hidden artifacts.
            self.release_child_window_registries(win_id);
            drop(destination);
            drop(owner);
            self.rollback_detached_tab(transaction);
            tracing::warn!(?error, "tear-out accounting refused; source transaction restored");
            return None;
        }
        tab_states.push(transaction.state);
        // Each independent redraw-target guard is released before the pane map
        // moves into the destination window; no two pane locks are nested.
        for pane in transaction.panes.values() {
            *pane.redraw_target.lock() = Some(win_id);
        }

        // Both pacing clocks start at one instant, so neither paces a new window differently.
        let created_at = Instant::now();
        let child = WindowState {
            owner,
            pending_receipts: Vec::new(),
            role: crate::app::WindowRole::Terminal,
            custom_window_name: String::new(),
            window: Some(destination.window.clone()),
            renderer: Some(destination.renderer),
            tabs: child_tabs,
            tab_states,
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
            pty_pressed_keys: std::collections::HashMap::new(),
            last_render: created_at,
            stream_clock: created_at,
            retry_not_before: None,
            visible_frame_invalid: false,
            redraw: Default::default(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            link_preview: None,
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
            #[cfg(test)]
            test_image_atlas_release: None,
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
        crate::app::frame_counters::request_native_redraw(&destination.window);
        // A consumed pooled window is not replaced here; the pool refills on
        // the next idle tick rather than on this path.
        self.frontmost_window = Some(win_id);
        Some(win_id)
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
        let is_empty = self.main_tabs().map(|tabs| tabs.is_empty()).unwrap_or(true);
        if is_empty {
            // When: `is_empty` reports main drained by the tear-out; hide main only if a
            // child window survives, so the user is never left with no visible window.
            if self.child_window_count() > 0 {
                self.hide_main_window();
            }
            return;
        }
        if let Some(tabs) = self.main_tabs_mut() {
            let target = removed_idx.saturating_sub(1).min(tabs.len().saturating_sub(1));
            tabs.activate(target);
        }
        self.resize_visible_panes();
    }
}

impl App {
    /// Report whether a tear-out would leave the window layout unchanged; it never does.
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
        event_loop: &ActiveEventLoop,
        src_id: WindowId,
        index: usize,
    ) -> bool {
        self.tear_out_from_child_with_installer(
            src_id,
            index,
            |app, transaction, screen_pos, source| {
                app.install_torn_out_window(event_loop, transaction, screen_pos, source)
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
        let src_empty =
            self.windows.get(&src_id).map(|child| child.tabs.is_empty()).unwrap_or(false);
        if src_empty {
            // When: `src_empty` reports the source child drained by the tear-out; reap it
            // so no empty window is left on screen once its last tab has moved out.
            self.reap_empty_child(src_id);
            return;
        }
        if let Some(child) = self.windows.get_mut(&src_id) {
            let target = removed_idx.saturating_sub(1).min(child.tabs.len().saturating_sub(1));
            child.tabs.activate(target);
            super::child_window::resize_visible_panes_in_child(child);
        }
    }
}

#[derive(Debug, Clone)]
pub struct TearOutTiming {
    pub source: &'static str,
    pub start: Instant,
    pub create_window_ms: f32,
    pub renderer_init_ms: f32,
    pub resize_ms: f32,
    pub install_ms: f32,
}

impl TearOutTiming {
    /// Start a timing record for one tear-out, with every phase still unmeasured.
    ///
    /// `source` names the gesture that began the tear-out, so timings from
    /// different entry points stay distinguishable in the logs.
    #[must_use]
    pub fn new(source: &'static str, start: Instant) -> Self {
        Self {
            source,
            start,
            create_window_ms: 0.0,
            renderer_init_ms: 0.0,
            resize_ms: 0.0,
            install_ms: 0.0,
        }
    }

    /// Milliseconds from the tear-out gesture to the child window's first frame.
    ///
    /// This is the user-visible latency of the whole tear-out, so it spans every
    /// phase rather than any single one. A first render recorded before the
    /// start instant saturates to zero instead of wrapping.
    #[must_use]
    pub fn total_until_first_render_ms(&self, first_render_at: Instant) -> f32 {
        first_render_at.saturating_duration_since(self.start).as_secs_f32() * 1000.0
    }
}

/// Deferred in-process tab tear-out request. Drag tear-out records a screen
/// position; command-palette/keymap tear-out leaves it unset so the window
/// manager chooses the destination position.
#[derive(Debug, Clone)]
pub struct PendingTearOut {
    pub source_window: WindowId,
    pub source_tab_idx: usize,
    /// The tab this request names, independent of where it currently sits.
    ///
    /// An index is a position, and positions move: a tab closing at a lower
    /// index leaves the recorded one in range but naming a different tab, so a
    /// bounds check passes and the wrong tab is torn out. That became reachable
    /// once a shell exiting could close a tab on its own, with no user action
    /// to serialise against the drag.
    ///
    /// `None` only for requests built before an id was available, which fall
    /// back to the index.
    pub source_tab_id: Option<sonicterm_ui::tabs::TabId>,
    pub drop_screen_pos: Option<(i32, i32)>,
}

#[cfg(test)]
#[path = "tear_out_tests.rs"]
mod tear_out_tests;

#[cfg(test)]
#[path = "tear_out_timing_tests.rs"]
mod tear_out_timing_tests;
