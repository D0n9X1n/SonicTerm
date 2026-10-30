//! GPU renderer for the terminal grid using wgpu 29.
//!
//! The legacy `glyphon` chrome path is
//! gone. Every chrome string (tab titles, palette, search, IME,
//! broadcast, drag chip, quick-select hints) flows through
//! [`crate::chrome_text::layout`] → the shared `GlyphAtlas` →
//! [`crate::wezterm_pipeline::WeztermPipeline`]. No second font system,
//! no second atlas, no second render pass.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
#[cfg(test)]
use sonicterm_render_model::boundary::cfg::config::ScrollbarMode;
use sonicterm_render_model::boundary::cfg::config::{
    BackdropKind, SoftwareRenderMode, SubpixelAaMode,
};
use sonicterm_render_model::boundary::cfg::theme::{Color as ThemeColor, Theme};
use sonicterm_render_model::boundary::grid::grid::{
    bounded_grid_size, Cell, CellFlags, Color, Grid, UnderlineStyle,
};
use sonicterm_types::{GlyphRasterVariant, ResourceAmount, ResourceClass};

use crate::field_geometry::{
    clip_glyphs_to_rect, field_caret_rect, field_hit, plan_field, FieldBoundaries, FieldHit,
    FieldHitMode, FieldKind, FieldPlacement, FieldRect, FieldText, PresentedFields,
};
use wgpu::{
    CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor, LoadOp, Operations,
    PresentMode, RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions,
    SurfaceConfiguration, Texture, TextureDescriptor, TextureDimension, TextureFormat,
    TextureUsages, TextureView, TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::chrome_text::{self, ChromeAttrs, ChromeClip};
use crate::color::{
    chrome_color_to_linear_rgba, dim_toward, hex_to_chrome_color, hex_to_premultiplied_rgba,
    hex_to_wgpu_with_alpha, ChromeColor,
};
use crate::cursor::{recolor_cursor_glyphs_in, InactivePaneCursor, RecolorOutcome, RowGlyphSpan};
use crate::device_errors::{
    create_frame_fault_probe, destroy_and_await_loss, install_device_error_handlers,
    run_isolated_validation, DeviceErrorSnapshot, DeviceErrorState, DeviceStateWaker, GpuFaultKind,
};
use sonicterm_render_model::boundary::ui::drag_chip::{DragChipOverlay, DragChipVisual};
use sonicterm_render_model::boundary::ui::status_line::{
    PlacedPart, PlacedSegment, StatusLineView,
};
use sonicterm_render_model::boundary::ui::tab_spans::tab_title_font_size;

const PANE_FOCUS_FLASH_DURATION: Duration = Duration::from_millis(360);
const PANE_FOCUS_FLASH_BUCKET: Duration = Duration::from_millis(16);

#[cfg(test)]
use crate::frame_plan::{
    decide_render_mode, dirty_rows_damage_rect_with_ink_pad, effective_scrollbar_bucket,
    pane_damage_rect_with_ink_pad, pane_scrollbar_identity, RenderSignals,
};
use crate::frame_plan::{
    CopyModeIdentity, CursorCell, FrameFacts, FrameKey, FramePlan, PaneMetadata, PlannedPane,
    RenderMode, WindowIdentity,
};

#[path = "atlas_lifecycle.rs"]
mod atlas_lifecycle;

#[path = "frame_fonts.rs"]
mod frame_fonts;
use crate::frame_stats::CountingRasterizer;
pub use frame_fonts::{FontChange, FrameFonts};
/// The App's wake for a fallback completion, called with the completed notice's id from the
/// fallback worker thread. It must only post an event and never touch renderer state.
pub type FontFallbackWaker = std::sync::Arc<dyn Fn(u64) + Send + Sync>;
#[path = "init_timing.rs"]
mod init_timing;
use init_timing::{InitOutcome, InitTiming};

// The presenters stay a private child module so they keep direct access to renderer fields.
#[path = "present.rs"]
mod present;
#[path = "rebind.rs"]
mod rebind;
#[path = "recovery_context.rs"]
mod recovery_context;
#[cfg(any(target_os = "macos", test))]
pub use present::SurfaceAvailability;
use present::{lap, FrameBatches, FrameLayers};
pub use present::{PresentOutcome, SkipReason, SurfaceRetryReason, SuspendedContext};
pub use rebind::PreparedRebind;
pub use recovery_context::{CandidateSurface, ContextRequest, RecoveredContext, RequestFailure};

#[path = "frame_scratch.rs"]
mod frame_scratch;

#[path = "tab_title_font.rs"]
mod tab_title_font;
use tab_title_font::TabTitleFont;

use crate::{
    atlas_upload::{AtlasBindingKind, AtlasUpload, AtlasUploadStats},
    quad::{
        premultiply, px_to_ndc, scale_premultiplied_alpha, with_premultiplied_alpha, QuadInstance,
    },
    wezterm_pipeline::{ImageInstance, WeztermPipeline},
};
use sonicterm_render_model::boundary::cfg::config::CursorShape;
use sonicterm_render_model::boundary::ui::{
    command_palette::CommandPalette,
    copy_mode::{CopyModeState, QuickSelectHint, QuickSelectState},
    cursor as ui_cursor,
    ime::ImeState,
    overlays::{
        search_bar_label, NotificationBubble, NotificationBubbleLayout, NotificationLevel,
        PaletteLayout, SearchBarLayout, PALETTE_BORDER, PALETTE_PANEL_RADIUS, PALETTE_QUERY_RADIUS,
        PALETTE_ROW_RADIUS, SEARCH_BAR_HEIGHT, SEARCH_BAR_ICON_GAP, SEARCH_BAR_PAD_LEFT,
        SEARCH_BAR_PAD_RIGHT,
    },
    pane::{Rect as PaneRect, SplitAxis, SplitterRect},
    search::SearchState,
    selection::Selection,
    tabbar_view::{
        tab_bar_height, Rect as TabTitleRect, TabBarLayout, ACTIVE_TOP_ACCENT_H,
        ACTIVE_TOP_ACCENT_INSET, TAB_BAR_HEIGHT, TAB_GAP, TAB_VERT_INSET,
    },
    tabs::{ContentWidthRefresh, TabBar, TabContent},
};
use sonicterm_render_model::geometry::PixelRect;
use sonicterm_text::GlyphInstance;
use sonicterm_text::{
    glyph_atlas::GlyphAtlas,
    // The ASCII fast-path predicate is cell-shape based, independent of the FontStack shaper.
    shape::{run_is_ascii_fast, RunStyle},
};

/// Shared ownership of the GPU handles and device-containment state used by sibling renderers.
#[allow(dead_code)]
#[derive(Clone)]
pub struct GpuSharedContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Containment state of `device`, shared by every renderer built from
    /// this context.
    device_errors: Arc<DeviceErrorState>,
}

/// Top-level GPU-backed terminal renderer. Owns the wgpu surface, the
/// text + quad pipelines, the glyph atlas, font/shape caches, and all
/// per-frame layout / cursor / overlay state. One per OS window.
pub struct GpuRenderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    /// True when wgpu selected a CPU/software rasterizer (WARP, llvmpipe,
    /// SwiftShader) — see [`detect_software_rendering`]. The app reads this
    /// via [`GpuRenderer::is_software_rendering`] to degrade the frame cap and
    /// per-frame animation.
    software_rendering: bool,
    /// Resolved no-GPU degrade state: adapter detection combined with
    /// `[appearance].software_render_mode`.
    software_render_degrade: bool,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Containment state of `device`, shared with every renderer on it.
    device_errors: Arc<DeviceErrorState>,
    /// Whether a frame outcome already carried this renderer's one-time stop report.
    device_stop_reported: bool,
    /// Test fault: the next glyph-upload rebuild creates an invalid texture.
    fault_invalid_glyph_upload: bool,
    /// Test fault: every later frame records an invalid clear of this buffer.
    fault_frame_probe: Option<wgpu::Buffer>,
    /// Test seam: the next assembly returns `Err` after its glyph rows were staged.
    fault_assembly_error: bool,
    /// Test inspector: when `Some`, each assembly pass clears it at its start and records, per
    /// drawn pane, every slot at the moment its row is emitted; production leaves it `None`.
    emitted_rows_probe: Option<Vec<(u64, Vec<u16>)>>,
    /// Test seam: the next presentation returns `Err` from the presenter call.
    fault_present_error: bool,
    /// Test seam: return one backend occlusion after the normal frame device gate.
    fault_surface_occluded: bool,
    /// Test seam: replace the glyph atlas identity during the next assembly, as an atlas reset
    /// mid-frame would, so that frame takes the atlas retry.
    fault_atlas_change_during_assembly: bool,
    /// Test seam: stop the device just before the next cached Windows CPU reblit.
    #[cfg(target_os = "windows")]
    fault_stop_before_cached_present: bool,
    /// Frames handed to a native presenter, counted at the present call.
    present_calls: u64,
    surface: wgpu::Surface<'static>,
    config: SurfaceConfiguration,
    hardware_present_mode: PresentMode,
    hardware_alpha_mode: CompositeAlphaMode,
    window: Arc<Window>,

    /// WezTerm-style final presentation pipeline. It consumes every glyph and
    /// geometry primitive for a frame and emits one indexed draw stream.
    present_pipeline: WeztermPipeline,
    frame_texture: Texture,
    frame_view: TextureView,
    frame_blitter: wgpu::util::TextureBlitter,

    // B3 GPU text path for terminal and chrome glyphs.
    glyph_atlas: GlyphAtlas,
    glyph_upload: AtlasUpload,
    /// Monotonic identity of the current glyph-atlas allocation. Unlike the
    /// atlas-local eviction count, this survives replacement and prevents an
    /// old epoch-0 UV cache from matching a new epoch-0 atlas.
    glyph_atlas_generation: u64,
    // Inline media is isolated so large images cannot evict glyph tiles
    // referenced by the row cache.
    image_atlas: GlyphAtlas,
    image_upload: AtlasUpload,
    retained_inline_media_bytes: usize,
    /// Consecutive frames this window has drawn with no renderable inline
    /// media. Promotion of the image atlas is one-way without this: a window
    /// that shows a single image holds the full-size allocation for its whole
    /// life, even after the image scrolls into history. Demoting on the first
    /// idle frame would instead free and reallocate 16 MiB every time an image
    /// scrolled off and back, and re-decode it each time, so the count gates
    /// demotion behind a sustained absence.
    frames_without_inline_media: u32,
    /// Set when assembly resized the CPU image atlas: the GPU mirror is rebuilt after the frame's
    /// source is released, so no device resource is created while parser guards are held.
    image_upload_rebuild_pending: bool,
    /// When the window last assembled a frame without renderable inline media after one with it, or
    /// `None` while media is visible. The interval release counts [`IMAGE_ATLAS_IDLE_INTERVAL`] from it.
    inline_media_absent_since: Option<Instant>,
    /// True after an eviction-triggered compaction. The rebuilt atlas has
    /// eviction disabled until one frame presents successfully, bounding
    /// retries when the visible glyph working set exceeds atlas capacity.
    glyph_atlas_retry_without_eviction: bool,
    /// Growths already counted and the growth episode awaiting its present.
    growth_episodes: crate::frame_stats::GrowthEpisodes,
    /// Test-only: the retained frame texture is built with `COPY_SRC` so a test can read it back.
    /// Only `__enable_retained_frame_readback` sets it; production keeps it false.
    retained_frame_readback: bool,
    /// In-place glyph atlas resets since construction, read only by tests through
    /// [`Self::__test_glyph_atlas_resets`] to prove a growth retry never resets.
    glyph_atlas_resets: u64,
    /// True while a covered-window trim has replaced the frame texture with a 1x1 one; the next
    /// GPU present, a resize, a degrade switch, a readback enablement or a recovery commit clears it.
    frame_texture_trimmed: bool,
    /// Frame textures installed after construction, by a rebuild or a recovery commit; read only
    /// through [`Self::__frame_texture_rebuilds`].
    frame_texture_installs: u64,
    /// Requested bytes of every GPU resource this renderer holds, by class; shared with
    /// `present_pipeline`, which records its own buffer growth.
    graphics_ledger: crate::graphics_ledger::GraphicsLedgerHandle,

    font_family: String,
    font_dirs: Vec<PathBuf>,
    font_size: f32,
    line_height: f32,
    font_weight_scale: f32,
    /// Requested LCD coverage order; effective-mode resolution stays separate.
    subpixel_aa: SubpixelAaMode,
    /// Multiplier applied to the font's natural cell height to derive the
    /// rendered line height (`cell_h = natural_cell_h * line_height_mult`).
    /// Stored so a DPI/scale-factor change can recompute `cell_h` from the
    /// freshly-rasterized natural height — see `rebuild_for_sf`. Without it,
    /// the rebuild had to back this factor out of the stale `line_height`,
    /// which algebraically cancelled and pinned `cell_h` to the old DPI.
    line_height_mult: f32,
    /// DPI multiplier (e.g. 2.0 on Retina). The renderer is raster-px
    /// end-to-end, so draw and hit-test sites no longer multiply or
    /// divide by it; it converts logical sizes into raster pixels, for
    /// example the font size in `raster_px` and the configured padding.
    scale_factor: f32,
    /// Cell width in raster pixels (one terminal column). Sourced from
    /// `FontStack::cell_metrics_raster_px()` so sonicterm-font metrics
    /// drop in without a unit conversion.
    pub cell_w: f32,
    /// Cell height in raster pixels (one terminal row).
    pub cell_h: f32,
    padding_left: f32,
    padding_right: f32,
    padding_top: f32,
    padding_bottom: f32,
    bg: wgpu::Color,
    bg_opacity: f32,
    /// Scrollbar visibility policy from config. Read on
    /// every frame in the per-pane scrollbar emit loop.
    scrollbar_mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    /// Padding between overlay panel chrome and inner content.
    panel_padding: f32,
    fg_default: ChromeColor,
    cursor_color: [f32; 4],
    /// Theme cursor-text color as straight RGBA. Used to recolor glyphs
    /// under block cursors and cursor-like overlay carets without deriving
    /// text color from the background.
    cursor_text_color: [f32; 4],
    /// Theme background as straight RGBA. Used by overlays that intentionally
    /// mask text with the terminal background.
    bg_rgba: [f32; 4],
    /// Visual style of the text cursor (block / bar / underline).
    /// Live-updated from config; see [`Self::set_cursor_shape`].
    cursor_shape: CursorShape,
    /// Whether the text cursor blinks. When `false` the cursor renders
    /// at solid alpha and the FrameKey ignores the phase bucket.
    cursor_blink: bool,
    /// Anchor for the blink phase. Reset on every config change so the
    /// user sees the cursor at full brightness immediately after they
    /// toggle the setting (rather than wherever the cycle happened to
    /// be at the time).
    blink_epoch: Instant,
    /// Whether the OS window currently holds keyboard focus. The text
    /// cursor is hidden while the window is inactive. Defaults to
    /// `true` so a freshly created renderer draws the cursor on the
    /// very first frame, before winit has a chance to deliver
    /// `Focused(true)`.
    window_focused: bool,
    /// Cursor positions inside inactive panes (panes that share the
    /// window with the active pane but don't currently own keyboard
    /// focus). Kept as a compatibility sink for the app-side plumbing;
    /// inactive pane cursors are no longer drawn.
    inactive_pane_cursors: Vec<InactivePaneCursor>,
    /// Short-lived focus confirmation animation for the pane that just
    /// became active. Cleared automatically after
    /// [`PANE_FOCUS_FLASH_DURATION`].
    pane_focus_flash: Option<(u64, Instant)>,
    selection_color: [f32; 4],
    tab_bar_bg: [f32; 4],
    tab_active_bg: [f32; 4],
    tab_inactive_bg: [f32; 4],
    tab_active_fg: ChromeColor,
    tab_inactive_fg: ChromeColor,
    /// Deprecated user override for the removed tab close button. Kept
    /// only so older configs round-trip without changing the renderer
    /// settings surface.
    tab_close_override: Option<[f32; 4]>,
    /// Last reported cursor position in LOGICAL pixels, or `None` when
    /// the cursor is outside the window. Drives tab hover state.
    hover_cursor: Option<(f32, f32)>,
    /// Color for the wezterm-style vertical bar drawn between adjacent
    /// inactive tabs. A dim variant of the inactive-fg works in every
    /// theme; we precompute it here so the per-frame render path stays
    /// allocation-free.
    tab_separator: [f32; 4],
    hyperlink_underline: [f32; 4],
    splitter_color: [f32; 4],
    hyperlink_tint: [f32; 4],
    search_highlight: [f32; 4],
    search_fg: ChromeColor,
    search_bg: [f32; 4],
    // The 11 `*_buffer: legacy chrome buffer`
    // fields that lived here (search, quick_select, palette_{query,rows,
    // footer}, ime, broadcast,
    // drag_chip) are gone. Every chrome string is now shaped on demand
    // inside `render()` via `chrome_text::layout(...)`; the resulting
    // glyph instances feed either `glyph_instances` (pre-overlay
    // chrome — tab titles, search status bar) or
    // `overlay_glyph_instances` (modal chrome — palette,
    // IME preedit, drag-chip title). No per-renderer glyphon buffer
    // state survives.
    /// Last rendered drag-chip rect in raster pixels; absent when no chip was drawn.
    drag_chip_visual: Option<DragChipVisual>,
    /// Last rendered frame key — when the next frame would produce an
    /// identical key, render() short-circuits before any GPU work.
    last_frame_key: Option<FrameKey>,
    /// What the last presented frame's cursor recolors rewrote; written only beside
    /// `last_frame_key` on a presented frame, so it always describes the pixels on screen.
    last_recolor: crate::cursor::RecolorRecord,
    /// Where the last presented frame's tab-title glyphs draw; written only beside
    /// `last_recolor` on a presented frame, so it always describes the title pixels on screen.
    last_tab_ink: crate::cursor::RecolorBounds,
    /// Test seam: a glyph `(x, y, w, h)` surface-pixel rectangle and color appended before the
    /// cursor recolors; only `__inject_test_glyph` sets it, so production keeps `None`.
    injected_test_glyph: Option<((f32, f32, f32, f32), [f32; 4])>,
    /// Test seam: a glyph `(x, y, w, h)` and color attached to the row at `(pane, slot)`, drawn
    /// only when that row is emitted; only `__inject_row_glyph` sets it, so production keeps `None`.
    injected_row_glyph: Option<InjectedRowGlyph>,
    /// Test seam: the reason the next wgpu acquisition reports instead of asking the surface; only
    /// `__fail_next_surface_acquire` sets it, so production keeps `None`.
    fault_surface_acquire: Option<SurfaceRetryReason>,
    /// Where each presented row drew, per `(pane, slot)`; staged during assembly and committed
    /// beside `last_frame_key` only when a frame presents. A partial plan emits by these records.
    row_ink: crate::row_ink::RowInkTable,
    /// The last presented frame's damage, kept beside `last_frame_key` only after
    /// `__enable_presented_damage`; production never enables it.
    presented_damage: PresentedDamageRecorder,
    /// Constant-size geometry of the palette and search query fields as last presented.
    presented_fields: PresentedFields,
    /// Preedit glyphs keyed by text, placement, color, and qualified atlas identity to reject stale UVs.
    preedit_glyph_cache: Option<PreeditGlyphCache>,
    /// Cumulative count of frames skipped via the FrameKey fast-path.
    /// Exposed via tracing::trace for `RUST_LOG=trace` hit-rate dashboards.
    skipped_frames: u64,
    /// Frames that reached a native presentation boundary successfully.
    successful_frame_count: u64,
    /// This renderer's statistics when it counts; set once by its App, before any frame.
    frame_sink: Option<crate::frame_stats::FrameStatsSink>,
    /// Test hook run at the start of each presentation; returning true stops the device there.
    present_hook: Option<Box<dyn FnMut() -> bool + Send>>,
    #[cfg(target_os = "windows")]
    software_frame: Option<crate::software_frame::SoftwareFrame>,
    /// Window label used in renderer-internal timing logs.
    render_timing_label: &'static str,
    /// Whether the tab bar is currently shown. Toggled at runtime by the
    /// View → Toggle Tab Bar menu action; when `false`, [`Self::top_inset`]
    /// returns 0 and the tab bar draw block in [`Self::render`] is skipped.
    tab_bar_visible: bool,
    /// Whether the status line strip is shown.
    status_line_visible: bool,
    /// The window edge the strip sits on: above the panes, or between them and the tab bar.
    status_line_position: sonicterm_render_model::boundary::cfg::status_line::StatusPosition,
    /// `[status_line] font_scale`: the strip text's share of the live terminal font size.
    status_line_font_scale: f32,
    /// Segments the strip may present; replaced by the App before a frame.
    status_line_view: StatusLineView,
    /// Fitted strip keyed by view pixels, width and chrome font, so an
    /// unchanged strip is not reshaped every frame.
    status_line_fit: Option<(u64, crate::status_strip::FittedStrip)>,
    /// The strip font's cap height, keyed by the text size it was measured at (`f32` bits).
    status_line_cap_height: Option<(u32, f32)>,
    /// Hit boxes of the segments the last drawn frame presented.
    status_line_layouts: crate::status_strip::StripLayouts,
    /// The segment keyboard focus is on and whether `strong_focus` doubles its ring; `None` draws
    /// no ring.
    status_line_focus:
        Option<(sonicterm_render_model::boundary::cfg::status_line::StatusSegmentId, bool)>,
    /// Reserved height (logical px) above the tab bar for the OS native
    /// titlebar. Kept at zero while SonicTerm uses the normal OS titlebar with a
    /// bottom-pinned tab bar.
    titlebar_inset: f32,
    /// Characters from the most recent `render()` call that the
    /// rasterizer could not produce a tile for (i.e. would draw as a
    /// tofu outline). Whitespace is excluded. Test-only diagnostic
    /// surfaced through [`Self::last_missing_tofu`]; production code
    /// must not depend on it.
    last_missing_chars: Vec<char>,
    /// Chrome characters (tab titles, palette, search, preedit, footer) the most recent presented
    /// frame drew as tofu or dropped, whitespace excluded. Test-only diagnostic surfaced through
    /// [`Self::last_missing_chrome`]; production code must not depend on it.
    last_missing_chrome_chars: Vec<char>,
    /// Per pane and row slot, the tofu characters of the row presented there.
    presented_missing: std::collections::HashMap<(u64, u16), crate::missing_glyphs::SlotMissing>,
    /// This pass's emitted rows' tofu records, committed into `presented_missing` when it presents.
    staged_missing: Vec<crate::missing_glyphs::StagedMissing>,
    /// The `(notice id, unresolved revision)` the last fallback wake acknowledged.
    acknowledged_unresolved: Option<(u64, u64)>,
    /// The fallback notice's unresolved set as last read, with that notice's id.
    unresolved_snapshot: Option<(u64, sonicterm_engine::UnresolvedSnapshot)>,
    /// What the last presented `Full` frame certified about missing characters, read by
    /// [`Self::completeness_checkpoint`]; only presented frames change it.
    completeness: Option<crate::completeness::Certificate<FrameKey, GlyphContentStamp>>,
    // Row-cache hits skip style-run shaping; misses shape through the font stack before atlas insertion.
    /// Sonicterm-font driven shaper. Owns
    /// the cell metrics (`cell_metrics_raster_px()`), the resolved
    /// font fallback chain, and the `blocking_shape` entry point that
    /// `flush_shape_run` calls through `shape_run_with_wezterm`. The
    /// renderer keeps the `Option<...>` shape so test fixtures (no
    /// bundled fonts on disk) can still construct a `GpuRenderer`
    /// even though the grid path is degraded.
    pub(crate) font_stack: Option<sonicterm_engine::FontStack>,
    /// Tab-title font: its native-size stack (`body + 1`), raster size and width key.
    tab_title_font: TabTitleFont,
    /// Native-size stack for the command-palette footer (`body - 1`).
    palette_footer_font_stack: Option<sonicterm_engine::FontStack>,
    /// Native-size stack for status line text, at `status_strip::status_line_font_size`.
    status_line_font_stack: Option<sonicterm_engine::FontStack>,
    /// Kept tab titles, search-overlay runs and the UI palette; reported as `chrome_cache`.
    chrome_caches: crate::chrome_cache::ChromeCaches,
    /// Whether the title and chrome-run caches keep what they shape; a test turns it off to
    /// compare against cold drawing.
    chrome_reuse: bool,
    /// The per-frame draw vectors, kept between assembled frames; reported as `frame_scratch`.
    frame_scratch: frame_scratch::ScratchHome,
    /// Per-row glyph cache. Stores the shaped
    /// `GlyphInstance`s, underline coalescing, and missing-tofu list
    /// for each visible row, keyed by absolute row index + a content
    /// hash. A row whose contents / style / selection-overlap haven't
    /// changed splices its cached output straight into the frame and
    /// skips the entire `flush_shape_run` walk.
    row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache,
    /// Per-row cache for background/underline/hyperlink-tint quads
    /// Mirrors `row_glyph_cache` but for the
    /// `QuadInstance`s emitted by `emit_cell_bg_quads_clipped` — on a
    /// hit we splice the cached `Vec<QuadInstance>` straight into the
    /// frame's quad vector and skip the per-cell run-length-encode.
    line_quad_cache: crate::row_quad_cache::LineQuadCache,
    /// Test-only `(pane_id, [origin_x_px, origin_y_px])` records for every rendered pane; not a production contract.
    last_emit_origins: Vec<(u64, [f32; 2])>,
    /// Raster-pixel pane layouts for hit-testing with the same snapped column edges; empty before the first layout.
    last_pane_layout: Vec<PaneLayoutSnapshot>,
    /// Monotonic counter bumped on theme / default-fg / default-bg
    /// changes. Folded into every `row_hash` so palette swaps
    /// invalidate cached colours without iterating the cache.
    style_rev: u64,
    /// The `(notice id, generation)` the last frame preparation applied.
    applied_fonts: Option<(u64, u64)>,
    /// A fallback generation apply no render attempt has carried yet; the next attempt takes it.
    unattributed_apply: bool,
    /// The App's wake for fallback completions; attached to every body stack this renderer installs.
    fallback_waker: Option<FontFallbackWaker>,
    /// Active drag-chip overlay: translucent rect drawn at the cursor
    /// while a tab is held. Cleared on release.
    drag_chip: Option<DragChipOverlay>,
    /// Compatibility attachment state, independent of FontStack fallback discovery.
    async_loader: Option<()>,
}

/// Renderers constructed but not yet dropped, across the whole process.
///
/// A renderer's own `retained_amounts()` cannot answer "did the last one go
/// away": it reports the instance being asked, and every instance reports the
/// same atlas capacity. Reading it in a loop compares a constant to itself and
/// holds whether or not anything leaked. This counter is the quantity a leak
/// actually moves — it rises on construction, falls in `Drop`, and returns to
/// its starting value only if every renderer built was also released.
static LIVE_RENDERERS: AtomicUsize = AtomicUsize::new(0);

/// How many `GpuRenderer`s are alive right now.
///
/// Intended for churn and lifecycle checks: take a reading, create and drop
/// renderers, and compare. A surviving renderer leaves this above where it
/// started.
// Ordering: `LIVE_RENDERERS.load(Ordering::Acquire)`, pairing with the
// `Ordering::AcqRel` RMWs in `new_async` and `Drop`. No payload is published.
pub fn live_renderer_count() -> usize {
    LIVE_RENDERERS.load(Ordering::Acquire)
}

// Lifecycle: `GpuRenderer` releases its `LIVE_RENDERERS` slot here — the sole
// decrement, paired with the increment in `new_async`.
// Lifecycle: teardown counts any glyph atlas growth not yet counted and abandons a pending
// growth timing, since no later frame can present it.
impl Drop for GpuRenderer {
    // Ordering: `LIVE_RENDERERS.fetch_sub(1, Ordering::AcqRel)`, pairing with
    // the `Ordering::AcqRel` increment in `new_async`. Publishes no payload.
    fn drop(&mut self) {
        self.finalize_frame_stats();
        // The renderer's lifetime totals are logged once before its ledger is released, so a
        // dropped window's creations and releases stay in the log after the snapshot loses it.
        let ledger = self.graphics_ledger.snapshot();
        tracing::info!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            graphics_live = %ledger.render_per_class(|tally| (tally.live_bytes, tally.live_resources)),
            graphics_created =
                %ledger.render_per_class(|tally| (tally.created_bytes, tally.created_resources)),
            graphics_released =
                %ledger.render_per_class(|tally| (tally.released_bytes, tally.released_resources)),
            surface_count = ledger.surface_count_kind(),
            "renderer graphics teardown"
        );
        // Every resource this renderer requested is released with it.
        self.graphics_ledger.release_all();
        // Paired with the increment in `new`. Together they make the live
        // count return to its starting value across balanced open/close
        // churn, and stay above it when a renderer survives.
        LIVE_RENDERERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: the next frame-shaping call on this thread fails once.
    static FAIL_NEXT_SHAPE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) use core_tests::warm_and_cold_row_glyphs;
#[cfg(test)]
#[path = "core_tests.rs"]
mod core_tests;
// `hex_to_glyphon` and
// `scale_glyphon_alpha` have moved into `crate::color` under the
// renamed `hex_to_chrome_color` / `scale_chrome_text_alpha` names and
// now consume `ChromeColor` instead of `legacy chrome color`.
// Re-export them at the legacy path so callers that imported
// `sonicterm_gpu::core::scale_glyphon_alpha` can switch to the new
// identifier (see `crates/sonicterm-app/tests/drag_visual_feedback.rs`
// for the port). The legacy names are gone from this file entirely;
// any caller that lingers on them will fail to compile (intentional —
// it's the must-pass #4 grep gate's job to catch survivors).
pub use crate::color::scale_chrome_text_alpha;

#[path = "core_assemble_backgrounds.rs"]
mod assemble_backgrounds;
#[path = "core_assemble_decorations.rs"]
mod assemble_decorations;
#[path = "core_assemble_glyph_rows.rs"]
mod assemble_glyph_rows;
#[path = "core_assemble_inline_images.rs"]
mod assemble_inline_images;
#[path = "core_assemble_messages.rs"]
mod assemble_messages;
#[path = "core_assemble_palette.rs"]
mod assemble_palette;
#[path = "core_assemble_preedit.rs"]
mod assemble_preedit;
#[path = "core_assemble_search.rs"]
mod assemble_search;
#[path = "core_assemble_selection.rs"]
mod assemble_selection;
#[path = "core_assemble_status_line.rs"]
mod assemble_status_line;
#[path = "core_assemble_tabs.rs"]
mod assemble_tabs;
#[path = "core_assemble_transients.rs"]
mod assemble_transients;
#[path = "core_assembly_context.rs"]
mod assembly_context;
use assembly_context::{AssemblyFields, AssemblyGeometry, AssemblyPaneView, AssemblyUi};

#[path = "core_contracts.rs"]
mod contracts;
pub use contracts::acknowledge_receipts;
use contracts::assemble_with_fallback;
use contracts::lend_and_assemble;
use contracts::presented_receipts;
use contracts::push_injected_row_glyph;
pub use contracts::settle_borrowed_frame;
use contracts::settle_retained_frame;
use contracts::settle_without_renderer;
pub(crate) use contracts::upload_staging_amount;
use contracts::Assembled;
use contracts::AssembledLayers;
pub use contracts::FrameOutcome;
pub use contracts::GlyphAtlasFacts;
pub use contracts::InjectedRowGlyph;
use contracts::PassEnd;

#[path = "core_hover.rs"]
mod hover;
use hover::hovered_url_for_pane_row;
use hover::hovered_url_needs_accent;
use hover::hovered_url_row_key_span;
use hover::hovered_url_span_rect;
use hover::pane_focus_flash_sample;

#[path = "core_overlay_geometry.rs"]
mod overlay_geometry;
use overlay_geometry::conservative_badge_text_width;
use overlay_geometry::estimate_badge_text_width;
use overlay_geometry::palette_detail_positions;
pub(crate) use overlay_geometry::palette_footer_font_size;
use overlay_geometry::preedit_bg_rect;
use overlay_geometry::preedit_caret_advance;
use overlay_geometry::preedit_has_visible_ink;
use overlay_geometry::read_only_badge_rect;
use overlay_geometry::scrollbar_tint;
use overlay_geometry::search_badge_content_width;
use overlay_geometry::splitter_color_from_theme;
pub use overlay_geometry::SurfaceAppearance;
use overlay_geometry::NOTIFICATION_CLOSE_ICON;
use overlay_geometry::PALETTE_FOOTER_INSET_X;
use overlay_geometry::READ_ONLY_BADGE_BASELINE_NUDGE_Y;
use overlay_geometry::READ_ONLY_BADGE_ICON;
use overlay_geometry::READ_ONLY_BADGE_LABEL;
use overlay_geometry::READ_ONLY_BADGE_PAD_RIGHT;
use overlay_geometry::READ_ONLY_BADGE_RADIUS;
use overlay_geometry::SEARCH_BADGE_ICON;

#[path = "core_settings.rs"]
mod settings;
pub use settings::start_dim;
pub use settings::GlyphAtlasStart;
pub use settings::RendererSettings;

#[path = "core_theme.rs"]
mod theme;
use theme::active_cursor_color;
use theme::cursor_color_from_theme;
use theme::cursor_text_color_from_theme;
use theme::effective_font_weight_scale;
pub(crate) use theme::glyph_flags;

#[path = "core_font.rs"]
mod font;
pub(crate) use font::renderer_font_stacks;
// Preserve the existing crate-visible path after moving its implementation.
#[allow(unused_imports)]
pub(crate) use font::renderer_font_views;
pub(crate) use font::RendererFontStacks;

#[path = "core_row_geometry.rs"]
mod row_geometry;
pub use row_geometry::build_snapped_cell_x;
pub(crate) use row_geometry::fill_snapped_cell_x;
#[cfg(test)]
pub(crate) use row_geometry::fit_single_cell_status_marker;
pub(crate) use row_geometry::fit_status_marker_rect;
pub use row_geometry::pixel_to_local_col;
use row_geometry::positioned_shaped_glyph_rect;
pub use row_geometry::quick_select_hint_display_row;
pub use row_geometry::quick_select_viewport_row;
use row_geometry::shaped_cluster_x_offset;
pub use row_geometry::snapped_col_at;
use row_geometry::software_block_glyph_target_rect;
pub(crate) use row_geometry::status_marker_fit_eligible;
use row_geometry::terminal_vertical_ink_pad;

#[path = "core_tab_geometry.rs"]
mod tab_geometry;
use tab_geometry::emit_privilege_badge_quads;
pub use tab_geometry::emit_tab_bar_quads;
pub(crate) use tab_geometry::hovered_tab_at;
use tab_geometry::privilege_marker_reserve_px;
use tab_geometry::tab_bar_hash;
use tab_geometry::tab_content_width_px;
use tab_geometry::tab_font_key;
use tab_geometry::tab_title_block_placement;
use tab_geometry::tab_title_color;
pub(crate) use tab_geometry::TabBarHoverGeometry;
pub use tab_geometry::TabBarQuadParams;
pub use tab_geometry::ACTIVE_PANEL_MARKER_ALPHA_FOCUSED;
pub use tab_geometry::ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED;

#[path = "core_frame_identity.rs"]
mod frame_identity;
use frame_identity::drawn_cursor_cell;
use frame_identity::invalidate_planned_quad_rows;
use frame_identity::terminal_cursor_columns;
use frame_identity::terminal_cursor_drawn_at_view;

#[path = "core_device_policy.rs"]
mod device_policy;
pub use device_policy::allocator_snapshot_from;
use device_policy::allocator_snapshot_from_report;
pub use device_policy::backend_display_name;
pub use device_policy::detect_software_rendering;
pub use device_policy::device_descriptor_for;
pub use device_policy::device_memory_policy_from;
pub use device_policy::effective_subpixel_aa_mode;
pub use device_policy::selected_optional_device_features;
use device_policy::software_render_degrade_from;
pub use device_policy::AllocatorSnapshot;
pub use device_policy::DeviceMemoryPolicy;

#[path = "core_decorations.rs"]
mod decorations;
pub use decorations::clip_rect_to_pane;
pub use decorations::collect_hyperlink_runs;
pub use decorations::command_status_hash;
use decorations::emit_broadcast_borders;
pub use decorations::emit_pane_scrollbar;
use decorations::fold_u64_to_u32;
use decorations::push_underline_quads;
pub use decorations::selection_quad_rects;
use decorations::splitter_rects_from_panes;
use decorations::underline_key;

#[path = "core_atlas_policy.rs"]
mod atlas_policy;
use atlas_policy::atlas_changed_during_frame;
use atlas_policy::atlas_payload_bytes;
use atlas_policy::atlas_texture_rebuild_required;
use atlas_policy::desired_gpu_atlas_dimensions;
use atlas_policy::growth_only_change;
use atlas_policy::image_atlas_demotion_ready;
use atlas_policy::image_atlas_promoted;
use atlas_policy::image_atlas_promotion_required;
use atlas_policy::image_atlas_release_deadline_for;
use atlas_policy::image_atlas_release_due;
use atlas_policy::image_atlas_reset_warranted;
use atlas_policy::next_inline_media_absent_since;
use atlas_policy::row_cache_atlas_identity;
use atlas_policy::scale_factor_rebuild_required;
use atlas_policy::GlyphContentStamp;
use atlas_policy::IMAGE_ATLAS_IDLE_FRAMES;
use atlas_policy::IMAGE_ATLAS_IDLE_INTERVAL;
use atlas_policy::PLACEHOLDER_ATLAS_DIM;

#[path = "core_surface.rs"]
mod surface;
use surface::build_frame_texture;
// Preserve the existing crate-visible path after moving its implementation.
#[allow(unused_imports)]
pub(crate) use surface::classify_resize;
use surface::draw_retained_frame;
use surface::frame_texture_extent;
use surface::frame_texture_payload_bytes;
pub(crate) use surface::validated_surface_size;
pub use surface::ResizeOutcome;
// Preserve the existing crate-visible path after moving its implementation.
#[allow(unused_imports)]
pub(crate) use surface::ValidatedSurfaceSize;
use surface::MAX_SURFACE_BYTES;
pub(crate) use surface::MAX_SURFACE_DIMENSION;

#[path = "core_inspection_types.rs"]
mod inspection_types;
pub use inspection_types::padded_readback_row_bytes;
pub use inspection_types::unpad_readback_rows;
pub use inspection_types::PresentedDamage;
pub use inspection_types::PresentedDamageRecorder;
pub use inspection_types::RetainedFrameReadback;

#[path = "core_init.rs"]
mod init;
use init::new_instance;

#[path = "core_preedit_cache.rs"]
mod preedit_cache;
use preedit_cache::PreeditGlyphCache;

#[path = "core_chrome_glyphs.rs"]
mod chrome_glyphs;
pub use chrome_glyphs::emit_overlay_text_glyphs;
pub use chrome_glyphs::emit_tab_title_glyphs;
pub use chrome_glyphs::OverlayTextGlyphDebug;
pub use chrome_glyphs::PaneLayoutSnapshot;
pub use chrome_glyphs::TabTitleGlyphDebug;

#[path = "core_retention.rs"]
mod retention;
pub use retention::RendererRetention;
pub use retention::TrimReport;

#[path = "core_layout.rs"]
mod layout;

#[path = "core_status_strip.rs"]
mod status_strip;

#[path = "core_inspect.rs"]
mod inspect;

#[path = "core_copy_mode.rs"]
mod copy_mode;

#[path = "core_fields.rs"]
mod fields;

#[path = "core_tabs.rs"]
mod tabs;

#[path = "core_render.rs"]
mod render;

#[path = "core_assemble.rs"]
mod assemble;

#[path = "core_row_shaping.rs"]
mod row_shaping;
#[cfg(test)]
use row_shaping::cell_fg;
use row_shaping::color_to_chrome;
#[path = "core_inline_images.rs"]
mod inline_images;
#[path = "core_kitty_placeholders.rs"]
mod kitty_placeholders;
use inline_images::emit_inline_image_instances;
use inline_images::report_inline_image_pressure;
use inline_images::InlineImagePlacement;

#[path = "core_backgrounds.rs"]
mod backgrounds;
pub use backgrounds::cell_bg_rgba;
pub use backgrounds::emit_cell_bg_quads;
pub use backgrounds::emit_cell_bg_quads_clipped;
pub use backgrounds::emit_cell_bg_quads_for_row;
pub(crate) use backgrounds::emit_row_background;
pub(crate) use backgrounds::focus_flash_quad;
use backgrounds::indexed;
pub(crate) use backgrounds::push_selection_quads;
pub(crate) use backgrounds::RowBackgroundGeometry;
pub(crate) use backgrounds::RowBackgroundRow;
pub(crate) use backgrounds::SelectionGeometry;

#[path = "core_row_projection.rs"]
mod row_projection;
#[cfg(test)]
pub(crate) use row_projection::fail_next_shape_for_test;
#[cfg(test)]
use row_projection::inject_shape_failure;
#[cfg(not(test))]
use row_projection::inject_shape_failure;
pub(crate) use row_projection::project_cached_row;
// Preserve the existing crate-visible path after moving its implementation.
#[allow(unused_imports)]
pub(crate) use row_projection::project_row_glyph;
use row_projection::run_cell_at;
use row_projection::set_shaped_bits;
pub(crate) use row_projection::software_blocks_fit;
use row_projection::tile_record;
pub(crate) use row_projection::RowPlacement;

#[path = "core_row_glyphs.rs"]
mod row_glyphs;
pub(crate) use row_glyphs::assemble_pane_glyph_rows;
pub(crate) use row_glyphs::begin_glyph_pass;
// Preserve the existing crate-visible path after moving its implementation.
#[allow(unused_imports)]
pub(crate) use row_glyphs::emit_row_glyphs;
// Preserve the existing crate-visible path after moving its implementation.
#[allow(unused_imports)]
pub(crate) use row_glyphs::emitted_row_key;
pub(crate) use row_glyphs::GlyphFrame;
// Preserve the existing crate-visible path after moving its implementation.
#[allow(unused_imports)]
pub(crate) use row_glyphs::GlyphRow;
pub(crate) use row_glyphs::GlyphShaping;
pub(crate) use row_glyphs::PaneGlyphRows;
pub(crate) use row_glyphs::PaneGlyphSinks;

#[cfg(test)]
#[path = "core_source.rs"]
mod source;
#[cfg(test)]
pub(crate) use source::SOURCE as CORE_SOURCE;
