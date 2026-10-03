//! App loop. Owns the window, the GPU renderer, all tab/pane state, the
//! per-pane PTYs and parsers, selection state, and clipboard. Drives keymap
//! dispatch.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use arboard::Clipboard;
use parking_lot::Mutex;
use sonicterm_cfg::{
    config::{BackdropKind, Config, SoftwareRenderMode},
    keymap::{Action, BroadcastScope, Keymap},
    theme::Theme,
};
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_resource::ResourceGovernor;
use sonicterm_types::{
    GovernorLimits, OwnerKind, OwnerLimits, ProcessKind, ResourceClass, ResourceOwnerId,
};
use sonicterm_vt::vt::{CaptureStagingPool, CommandEvent, MouseTracking, Parser};
use winit::{
    application::ApplicationHandler,
    event::{InnerSizeWriter, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{ModifiersState, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

use sonicterm_gpu::core::GpuRenderer;
use sonicterm_ui::{
    broadcast::BroadcastState,
    command_palette::CommandPalette,
    copy_mode::CopyModeState,
    ime::ImeState,
    overlays::{NotificationBubble, NotificationLevel},
    pane::PaneTree,
    selection::{SelectMode, Selection},
    tabs::{CommandStatus, Tab, TabBar},
};

/// Default native title; terminal output never supplies OS titles.
pub const NATIVE_WINDOW_TITLE: &str = "SonicTerm";

/// Linux desktop entry, AppStream component, and Wayland application ID.
pub const LINUX_DESKTOP_ID: &str = "com.d0n9x1n.SonicTerm";

/// Linux X11 `WM_CLASS` instance paired with [`LINUX_DESKTOP_ID`].
pub const LINUX_INSTANCE_NAME: &str = "sonicterm";

/// Maximum gap (ms) between consecutive left-presses on the same cell for
/// them to count as a double/triple click. Beyond this the streak resets
/// to a single click.
pub const MULTI_CLICK_MS: u128 = 400;

/// Hard minimum terminal content width in cells for every native window.
pub const MIN_WINDOW_COLS: u16 = 30;
/// Hard minimum terminal content height in cells for every native window.
pub const MIN_WINDOW_ROWS: u16 = 10;

pub const PTY_REDRAW_QUIESCENT: Duration = Duration::from_millis(3);
pub const PTY_REDRAW_MAX_LATENCY: Duration = Duration::from_millis(8);
pub const PTY_REDRAW_FLUSH_BYTES: usize = 128 * 1024;
pub const MAX_PANE_COMMAND_EVENTS: usize = 1024;

/// Sum of every exact production pane-seam cap contribution.
///
/// This is derived only from [`pane_seam_cap_terms`]. PTY input is one class in
/// that inventory, so production and tests cannot add its queue bound through
/// separate arithmetic.
pub const PANE_SEAM_CAP_SUM_BYTES: usize = pane_seam_cap_sum_bytes();

/// Headroom multiplier between the seam caps and the governor's backstop.
///
/// The backstop exists to catch a seam that has *stopped* bounding, so it must
/// sit far enough above correct operation that it never fires there. Two times
/// the sum leaves room for allocator slack, capacity overshoot, and the
/// deliberate residual where a pane keeps one oversized image rather than
/// rendering nothing — while still being a small multiple rather than an
/// unbounded curve.
const BACKSTOP_HEADROOM: usize = 2;

/// The committed budget an `AppPane` owner is held to.
///
/// **A tripwire, not a second enforcement point.** That distinction is what
/// makes it safe, and it is the whole design:
///
/// The objection to a governor limit was that two limits which must agree and
/// are maintained separately will drift, and the one that stops agreeing keeps
/// reporting itself as enforced. That objection holds for a limit that shares
/// the enforcement job. It does not hold for one derived from the other limits
/// and set above all of them: this cannot disagree with the seam caps, because
/// it is computed from them, and it cannot silently stop enforcing, because it
/// was never the thing enforcing.
///
/// What it catches is the cross-seam failure each local cap cannot: retained
/// bytes that outgrow the cap inventory, whether one seam under-reports them or
/// its charging path stops running. Local allocation checks cannot expose that
/// disagreement because each seam can still look correct in isolation.
pub const PANE_COMMITTED_BUDGET_BYTES: usize = PANE_SEAM_CAP_SUM_BYTES * BACKSTOP_HEADROOM;

/// Frame period cap applied when rendering on a CPU/software rasterizer
/// (~40 fps). On a real GPU the monitor's refresh period is used as-is.
///
/// The DirectWrite + full-screen software path looks smooth at this cadence
/// without asking the CPU rasterizer to track the monitor's full refresh rate.
pub const SOFTWARE_RENDER_FRAME_PERIOD: Duration = Duration::from_micros(25_000);

/// Frame period cap while an IME composition is in flight on the software
/// rasterizer (~12 fps). Each preedit keystroke forces a full-surface raster
/// composing is interactive but doesn't need a high rate, so we
/// cap it lower to roughly halve the whole-surface presents while the user
/// types a long pinyin run. Only applied when BOTH software-render and
/// composing.
pub const SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD: Duration = Duration::from_micros(83_333);

pub const WARM_WINDOW_POOL_MAX: usize = 5;

/// Quote a single path or word for POSIX-shell paste. Re-exported from the
/// shared `sonicterm-types` implementation so file drops on macOS and Windows
/// paste the same bytes. Kept at this path so existing `super::shell_quote_posix`
/// imports continue to resolve.
pub use sonicterm_types::shell_quote_posix;

const FOREGROUND_PROCESS_TTL: std::time::Duration = std::time::Duration::from_millis(500);

/// Loader callback type used by the platform shell to reload a theme by name.
pub type ThemeLoader = Box<dyn Fn(&str) -> Result<Theme> + Send + 'static>;
/// Loader callback type used by the platform shell to reload a resolved keymap path.
pub type KeymapLoader = Box<dyn Fn(&Path) -> Result<Keymap> + Send + 'static>;
/// Platform policy that resolves unsupported config values and returns diagnostics.
///
/// Implementations must be idempotent: normalizing an already-normalized config
/// returns it unchanged with no diagnostics. App construction and explicit reload
/// are the only invocation points.
pub type ConfigNormalizer = Box<dyn Fn(Config) -> (Config, Vec<String>) + Send + 'static>;

/// Custom user events delivered through [`EventLoopProxy`].
///
/// Config is read at startup and thereafter only when the user explicitly
/// asks for it via `Action::ReloadConfig`, so there is no watcher thread and
/// no event for "the config file changed on disk".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    /// A pending action arrived from the macOS native menubar. The
    /// payload itself is queued in the static
    /// [`crate::menubar_bridge`] buffer; this variant is just the
    /// wake-up signal so the loop drains it.
    MenuAction,
    /// Script-file open requests were queued by a platform boundary.
    OpenScripts,
    /// A platform OS-drag drop landed and stashed payloads in
    /// [`crate::os_drag_bridge`]. The variant is just the wake-up
    /// signal so the loop drains the queues — separate from
    /// [`Self::MenuAction`] so a noisy drag stream does not flood the
    /// menubar drain path.
    OsDrag,
    /// A platform drag backend reported a cursor move. Windows OLE can produce
    /// these during a native session; the current macOS pasteboard backend does
    /// not. The
    /// actual position is in the [`os_drag::PendingDragOutcome`]
    /// mailbox shared with the backend.
    DragMoved,
    /// A platform drag backend terminated (drop or cancel). The outcome
    /// (drop target, tear-out, or cancel) is in
    /// the [`os_drag::PendingDragOutcome`] mailbox; the dispatcher
    /// inspects it and routes to `App::transfer_tab` or
    /// `App::cancel_drag_session` accordingly.
    DragEnded,
    /// A VT worker coalesced terminal output for this window. The event-loop
    /// thread resolves the live window and requests its redraw; VT workers never
    /// call native window APIs directly.
    RequestRedraw(WindowId),
    /// A previously-deferred font fallback family finished loading in the
    /// `sonicterm_text::async_fallback` background thread. The handler walks
    /// every live window's `GpuRenderer`, calls `clear_shape_cache()` (which
    /// bumps `style_rev` and drops the shape / row / line caches), and issues
    /// `window.request_redraw()` so the next frame re-shapes through the newly
    /// available face and the user's tofu cells get replaced by real glyphs.
    ClearShapeCache,
    /// The foreground-probe worker stored results, or stopped; drain its result map.
    ForegroundProbeReady,
    /// Background update check finished; show a reusable notification bubble.
    UpdateCheckFinished { level: NotificationLevel, message: String },
    /// A pane's child process ended, and its output channel closed with it.
    ///
    /// Raised once by that pane's VT worker, which classifies the exit before
    /// posting: the child becoming reapable and its pty reaching EOF are
    /// unordered, so the answer needs a bounded wait that must not happen on
    /// the event-loop thread.
    PaneProcessExited {
        /// The pane whose child ended.
        pane_id: u64,
        /// Whether that child exited cleanly, or `None` if it could not be
        /// determined. `None` is not a crash — it holds the pane open, the
        /// same as an unclean exit.
        was_clean: Option<bool>,
    },
    /// A script path could not be represented safely for the active shell.
    ScriptDraftRejected {
        /// User-facing explanation of why no draft was inserted.
        message: String,
    },
    /// A validated OSC 52 clipboard write reached the event-loop thread.
    ///
    /// The VT worker decodes and bounds the payload before constructing this
    /// event; native clipboard access remains confined to the app thread.
    ClipboardWrite {
        /// UTF-8 text requested by the terminal application.
        text: String,
    },
    /// A local-target openability probe completed off the event-loop thread.
    PathProbeFinished(Box<path_target::PathProbeResult>),
    /// Native local-target failure returned to the window that initiated it.
    PathOpenFailed {
        /// Original window, never replaced by the currently focused window.
        window_id: WindowId,
        /// Original pane; closed panes discard late failures.
        pane_id: u64,
        /// Native failure explanation shown as escaped text.
        reason: String,
        /// Attempted local path copied only when its failed-click response is delivered.
        target: String,
    },
    /// Payload-free metadata for terminal input that could not be queued.
    PtyInputRejected {
        /// Stable identity of the pane whose input was rejected.
        pane_id: u64,
        /// Producer-assigned input category.
        source: PtyInputSource,
        /// Length of the rejected input; its bytes are not retained.
        rejected_bytes: usize,
        /// Human-readable rejection reason without payload contents.
        reason: String,
        /// Concurrent queue and writer observations, not a transactional snapshot.
        diagnostics: sonicterm_io::pty::PtyInputDiagnostics,
    },
    /// The bounded Linux package-smoke watchdog expired.
    RuntimeSmokeTimeout,
    /// A GPU device stopped accepting work or was lost.
    ///
    /// Posted by that device's error handler, at most once per transition. The
    /// event loop redraws every window so each renderer observes the stop once.
    GpuDeviceStateChanged,
    /// A device callback tagged with the generation that installed it.
    GpuDeviceGenerationChanged {
        /// Process-unique identity of the device that changed state.
        generation: u64,
    },
    /// A worker has queued a result; the channel retains ownership until consumed.
    GpuRecoveryReady {
        /// Identity of the admitted recovery request.
        ticket: u64,
    },
}

mod broadcast;
mod child_tabs;
mod child_window;
pub use child_window::{
    apply_dpi_to_renderer_if_present, child_window_dpi_changed_handles_no_renderer,
    child_window_resized_handles_no_renderer, resize_renderer_and_panes_if_present,
};
mod child_window_pointer;
mod child_window_redraw;
mod command_events;
use command_events::{append_bounded_command_events, notify_command_done};
pub use command_events::{poll_command_events_for_child_window, poll_command_events_for_tab_state};
mod config_apply;
mod effects;
use effects::close_owner;
mod event_loop;
mod fg_probe;
mod field_input;
mod field_pointer;
mod frame_counters;
pub use frame_counters::{
    CounterRecord, FrameCountersSnapshot, FrameCountersTooLate, HistogramBuckets,
};
mod frame_pacing;
pub use frame_pacing::{
    effective_frame_period, should_defer_streaming_redraw, should_degrade_for_software_render,
    should_flush_pending_pty_redraw, software_render_frame_period,
};
mod gpu_recovery;
mod gpu_recovery_worker;
pub mod hovered_url;
mod input_dispatch;
use input_dispatch::PendingPointerMotion;
pub use input_dispatch::{pick_prompt_target, wrap_paste, PtyInputSource};
pub mod invariants;
mod key_encoding;
mod keyboard_protocol;
pub use keyboard_protocol::HeldKey;
mod keymap_dispatch;
mod locale;
mod media;
pub mod memory_snapshot;
mod misc;
pub mod os_drag;
mod overlays;
mod owners;
#[cfg(test)]
use owners::install_transferred_pane_owner;
pub(crate) use owners::OwnerGuard;
use owners::{pane_owner_limits, pane_seam_cap_sum_bytes, tracking_only_owner_limits};
pub use owners::{pane_seam_cap_terms, PaneSeamCapTerm};
mod pane_exit;
mod pane_launch;
mod pane_refresh;
use pane_refresh::update_terminal_ime_cursor_area;
pub use pane_refresh::{
    invalidate_selection_for_content, mark_all_panes_dirty, resize_all_panes,
    resize_panes_to_rects, seed_parser_theme_colors,
};
mod pane_state;
use pane_state::pane_id_at_point;
pub use pane_state::{next_pane_id, PaneCommandEvent, PaneState};
mod path_target;
mod privilege;
use privilege::refresh_window_tab_privileges;
mod quit_hold;
mod reaper_driver;
mod redraw_target;
mod runtime_smoke;
pub use runtime_smoke::{RuntimeSmokeFailure, RuntimeSmokeScenario, RuntimeSmokeSpec};
mod redraw;
mod render_timing;
pub mod renderer_retention;
pub mod retention;
mod scroll;
pub mod scrollbar_input;
pub mod scrollbar_visibility;
mod search_handle;
mod selection_gesture;
pub use selection_gesture::next_click_count;
use selection_gesture::{PointerCell, PointerGesture, PointerGestureOwner};
mod session;
pub(crate) use session::identity_config_normalizer;
pub use session::{build_async_fallback_loader_for_proxy, init_tracing_public};
mod shared_gpu;
pub(crate) use shared_gpu::gpu_device_state_waker;
mod spawn_pane;
mod splitter_input;
mod tab_gesture;
mod tab_state;
pub use tab_state::{refresh_active_tab_title, TabState};
pub mod tab_transfer;
pub use tab_transfer::TransferError;
mod tab_widths;
mod tear_out;
pub use tear_out::{PendingTearOut, TearOutTiming};
mod test_hooks_media;
mod test_hooks_overlays;
mod test_hooks_owners;
mod test_hooks_panes;
mod test_hooks_windows;
mod text_edit;
#[doc(hidden)]
pub mod update_check;
mod viewport_anchor;
mod visible_frame;
mod warm_window_pool;
pub use warm_window_pool::{
    warm_window_pool_may_spawn, warm_window_pool_should_spawn, warm_window_pool_target, WarmWindow,
};
mod window_event;
mod window_keyboard;
mod window_pointer;
mod window_registry;
use window_registry::{next_synthetic_child_window_id, window_dpi, window_geom};
pub use window_registry::{synthetic_main_window_id, FrontmostKind};
mod window_setup;
mod window_state;
pub use config_apply::{
    config_diff_needs_font_apply, renderer_scrollbar_mode_differs,
    renderer_subpixel_aa_mode_differs,
};
pub use key_encoding::{
    encode_logical, encode_logical_with_modes, key_name, key_to_string, key_to_strings, KeyName,
};
pub use window_setup::{
    apply_terminal_window_minimum, install_native_window_background, minimum_terminal_inner_size,
    with_app_icon, with_backdrop_transparency, with_integrated_titlebar,
};
use window_setup::{
    apply_window_dpi_transition, apply_window_request, apply_window_state_minimum,
    compose_window_title, configured_window_size, inherited_window_size, WindowRequest,
};
#[cfg(test)]
use window_setup::{dpi_transition_inner_size, dpi_transition_size_scale};
use window_state::TopologyChange;
pub use window_state::{SplitterDragState, WindowRole, WindowState};

/// Delay allowing a failing Windows clipboard helper to release its open handle.
#[cfg(target_os = "windows")]
const OSC52_CLIPBOARD_REASSERT_DELAY: Duration = Duration::from_millis(150);

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub(super) struct PendingOsc52Reassert {
    /// Clipboard text to restore after the helper releases the clipboard.
    text: String,
    /// Clipboard value observed before the OSC write.
    previous_text: Option<String>,
    /// Event-loop deadline for the one permitted reassertion.
    due: Instant,
}

#[doc(hidden)]
pub struct App {
    pub(super) theme: Theme,
    /// Pool each pane charges its inline media to: the process-default pool in
    /// production, or a private pool a test injects with
    /// `App::with_inline_media_pool`.
    pub(super) inline_media_pool: Arc<media::InlineMediaPool>,
    /// Pool each pane's parser stages media captures in: the process-default
    /// pool in production, or a private pool a test injects with
    /// `App::with_capture_staging_pool`.
    pub(super) capture_staging_pool: Arc<CaptureStagingPool>,
    /// Process privilege observed once by the native binary before window creation.
    pub(super) process_privilege: crate::ProcessPrivilege,
    /// Foreground-process probes: demand, the worker and its latest-value result map.
    pub(super) fg_probes: Arc<fg_probe::ForegroundProbes>,
    /// Activity and warning wakes for the foreground-process schedule; only Windows arms them.
    pub(super) foreground_schedule: fg_probe::ForegroundSchedule,
    pub(super) config: Config,
    /// Native-platform capability policy applied before config affects app state.
    pub(crate) config_normalizer: ConfigNormalizer,
    /// Packaged font directories retained for every renderer and live font rebuild.
    pub(super) font_dirs: Vec<PathBuf>,
    /// Font size the loaded config asked for, in logical px. `ResetFontSize`
    /// returns here rather than to the compile-time default, so Cmd+0 restores
    /// the user's configured size instead of a value they never chose.
    ///
    /// Tracks the config this session has *loaded*, not the file on disk.
    /// Editing `sonicterm.toml` does not move it — the background watcher may
    /// apply other settings from that edit, but the reset target stays where
    /// the session started. Only an explicit `ReloadConfig` moves it.
    pub(super) configured_font_size: f32,
    /// Monochrome-text `weight_scale` the loaded config asked for. Follows the
    /// same rule as [`Self::configured_font_size`]: `ResetFontWeight` returns
    /// here, and only an explicit reload moves it.
    pub(super) configured_weight_scale: f32,
    pub(super) keymap: Keymap,
    // The main window holds no state of its own here. Its renderer, tabs,
    // tab states, panes, selection, modifiers, copy mode, last render, cursor
    // visibility, and hover link all live in `self.windows[main_window_id]`,
    // the same place a torn-out child's do — so one set of code paths serves
    // both. Reach them through `main_renderer()`, `main_tabs()`,
    // `main_panes()`, `main_selection()`, and their `_mut` counterparts.
    //
    // Callers needing several at once should go through `main_mut()` and
    // split-borrow the fields disjointly; taking two `main_*_mut()` accessors
    // together is a double borrow of the same map entry.
    pub(super) clipboard: Option<Clipboard>,
    #[cfg(target_os = "windows")]
    /// One delayed OSC 52 write that survives a failing clipboard helper's cleanup.
    pub(super) pending_osc52_reassert: Option<PendingOsc52Reassert>,
    /// Test-only in-memory clipboard override for integration tests that need
    /// to observe copy/paste routing without depending on a desktop clipboard
    /// service. `None` means production arboard behavior; `Some(_)` means reads
    /// and writes use this buffer instead.
    #[doc(hidden)]
    pub(super) test_clipboard_text: Option<String>,
    /// Test-only clipboard write rejection injected at the production write
    /// boundary. Disabled in every constructor so ordinary runs retain the real
    /// clipboard behavior and the in-memory success seam remains opt-in.
    #[doc(hidden)]
    pub(super) test_clipboard_write_failure: bool,
    /// Test-only PTY write ledger. `write_to_pane` records every boundary write
    /// here before resolving the pane to a real PTY, so headless tests can assert
    /// which pane an action targeted without constructing a process-backed PTY.
    #[doc(hidden)]
    pub(super) test_pty_writes: Arc<Mutex<Vec<(u64, Vec<u8>)>>>,
    /// Unit-test observation of actual PTY spawn inputs; absent from production builds.
    #[cfg(test)]
    test_pane_launches: std::cell::RefCell<Vec<(u64, pane_launch::PaneLaunch)>>,
    /// Whether the PTY write ledger above is actually recorded. `false` in
    /// production so `dispatch_pty_write_effect` does no lock/clone/push per
    /// write (the ledger would otherwise grow unbounded for the whole
    /// session —). Set `true` when the app is built without an
    /// event-loop proxy (headless/test construction) so existing tests keep
    /// capturing writes with no per-test opt-in.
    #[doc(hidden)]
    pub(super) pty_write_log_enabled: bool,
    // `App`-level DPI and hovered_url fields deleted — both
    // now live exclusively on `WindowState`. Readers go through
    // `self.main()?.dpi_scale` / `self.main()?.hovered_url`
    // (with safe-default fallbacks at call sites). The shadow-sync
    // path was deleted as the last of the per-window migration.
    /// Requested logical dimensions survive focus changes until native window creation can run.
    pub(super) pending_new_window: Option<WindowRequest>,
    /// Deferred in-process tab tear-out request from either drag/drop or the
    /// Move Tab to New Window action. Drained only while an ActiveEventLoop is
    /// available so every path uses the same native-window constructor.
    pub(super) pending_tear_out: Option<PendingTearOut>,
    /// Deferred `cancel_drag_session` request. Set by `handle_os_drag_ended`
    /// on the `DroppedOnEmpty` branch instead of cancelling inline,
    /// so any tear-out-spawn produced by the existing
    /// `pending_new_window` drain runs to completion BEFORE
    /// cross-window drag-residue cleanup mutates `self.windows`.
    /// Drained by `App::drain_pending_os_teardown` AFTER
    /// `App::drain_pending_window_creates` at the natural event-loop
    /// boundary in `event_loop.rs::do_user_event`. The
    /// `cancel_drag_session` all-windows loop runs **unconditionally**
    /// when drained — this flag controls only WHEN it runs, not
    /// WHETHER, so the cleanup stays idempotent.
    pub(super) pending_os_teardown: bool,
    /// Test-only callback fired
    /// inside [`Self::cancel_drag_session`] AFTER the `self.windows.keys()`
    /// snapshot is collected but BEFORE the per-id iteration body runs.
    /// Lets a regression test
    /// mutate `self.windows` in the exact race window that the
    /// `get_mut(&id).else { continue }` arm is designed to tolerate.
    /// Consumed (`take()`-d) at the call site so the closure is invoked
    /// at most once per `cancel_drag_session` run and the mutable
    /// borrow on `self.windows` is not held while it runs. Production
    /// cost is one extra `Option::take()` per `cancel_drag_session`
    /// invocation (always `None` outside tests) — gated by
    /// `#[doc(hidden)]` rather than `#[cfg(test)]` so an integration test,
    /// which compiles the crate without `cfg(test)`, can install it through
    /// `App::__test_set_post_snapshot_hook`.
    #[doc(hidden)]
    pub(super) test_post_snapshot_hook: Option<Box<dyn FnOnce(&mut App) + Send>>,
    /// Deferred app-exit request, set by a quit action or by a close that
    /// leaves no active terminal window. `do_about_to_wait` drains it by
    /// calling `event_loop.exit()`; the flag exists because those paths have
    /// no `ActiveEventLoop` handle.
    pub(super) pending_exit: bool,
    /// When pane retention was last sampled for the memory log.
    ///
    /// `None` until the first sample. Gating on elapsed time rather than
    /// sampling every idle turn keeps a measurement that walks every pane off
    /// the path that governs idle CPU.
    pub(super) last_retention_sample: Option<std::time::Instant>,
    /// The preceding cycle's totals, so a snapshot can report movement.
    ///
    /// `None` until the first sample has been taken, which is what makes the
    /// first snapshot's deltas report `unavailable` rather than `+0` — the
    /// latter claims the process did not move, which is a measurement nobody
    /// made.
    ///
    /// Only the totals are retained rather than the whole snapshot: the
    /// per-renderer breakdown is a string per renderer, and holding it between
    /// samples would keep it alive for the life of the session to serve a
    /// report that never reads it.
    pub(super) last_memory_totals: Option<memory_snapshot::MemoryTotals>,
    /// Nonblocking recorder for bounded postmortem breadcrumbs.
    ///
    /// The platform binary owns the writer thread; the app only holds this cheap
    /// sender and never performs filesystem IO on the event-loop path.
    pub(super) breadcrumb_recorder: Option<sonicterm_logging::breadcrumbs::BreadcrumbRecorder>,
    pub(super) command_palette: CommandPalette,
    /// Which window the (single, modal) command palette is attached to.
    /// `None` means it is closed OR attached to the main window; `Some(id)`
    /// means that child window. Both the main and child render paths consult
    /// it so the palette paints only on the window it was opened from —
    /// without it, Cmd+Shift+P typed in a torn-out child opened the palette
    /// on the original main window.
    pub(super) palette_attached_window: Option<WindowId>,
    /// Stable editor target, including main; an absent key never falls back to another window.
    pub(super) window_rename_target: Option<sonicterm_types::WindowKey>,
    /// Tab captured when a rename or color editor opened; submit edits only that live tab.
    tab_edit_target: Option<overlays::TabEditTarget>,
    /// One modal press retains its source and target until release or an intervening input change.
    palette_pointer_capture: Option<overlays::PalettePointerCapture>,
    /// Left-button gesture inside a palette or search query, bound to its source window and field.
    field_pointer_capture: Option<field_pointer::FieldPointerCapture>,
    /// Windows whose cancelled field gesture still owes a left release after another window
    /// started a field gesture; bounded by live windows and cleared on release or close.
    field_owed_releases: Vec<winit::window::WindowId>,
    /// Set the moment a held-tab drag
    /// crosses [`os_drag::OS_DRAG_THRESHOLD_PX`] from its press point,
    /// before the user releases the button. Guards
    /// [`Self::try_os_drag_handoff`] in the `CursorMoved` path so the
    /// OS-level drag session starts mid-gesture (cursor still down)
    /// rather than waiting until mouse-up — which was too late for
    /// `DoDragDrop` to capture the cursor across windows. Cleared on
    /// `cancel_drag_session` and at every fresh mouse-down so a new
    /// gesture re-arms cleanly.
    pub(super) os_drag_handoff_started: bool,
    /// Windows spawned by tearing tabs out of the parent bar. Keyed by
    /// winit WindowId so events route back to the right child.
    /// Process-wide resource governor and its owner hierarchy.
    ///
    /// Holds the `Process` root; every window registers a `Window` owner below
    /// it and every pane an `AppPane` owner below its window. That hierarchy
    /// is what makes a window's total derivable from its panes — the question
    /// per-pane accounting cannot answer and the one a user asks when they
    /// close a window to reclaim memory.
    ///
    /// Registered here rather than accounted here: this change establishes
    /// ownership only. Charging producers through it is the larger job that
    /// changes when allocation happens, not merely where the number lives.
    pub(super) governor: ResourceGovernor,
    pub(super) windows: HashMap<WindowId, WindowState>,
    /// One bounded driver owns every retired native transport after its pane leaves the UI.
    pty_reaper: reaper_driver::ReaperDriver,
    /// Cached terminal disposition prevents a repeated finish from retiring panes or draining twice.
    session_finished: Option<bool>,
    /// This App's frame and lock counters; `Some` only when its gate is on, fixed for its lifetime.
    pub(super) frame_counters: Option<frame_counters::AppFrameCounters>,
    /// Set by the first window or pane; the gate can no longer be forced on after it.
    pub(super) frame_counters_sealed: std::cell::Cell<bool>,
    /// Id of the main window. Set in `do_resumed` once the main `Window` is
    /// created and its [`WindowState`] is inserted into [`Self::windows`].
    ///
    /// `None` before that point, which is why every `main_*()` accessor
    /// returns an `Option` rather than assuming a main window exists.
    pub(super) main_window_id: Option<WindowId>,
    /// Most-recently-OS-frontmost window id, INCLUDING the main window.
    /// Tracks *every* sonic-owned terminal window with a single
    /// non-`Option` discriminant once the first focus arrives:
    ///
    ///   * `Some(main_window_id)`  → main window is OS-frontmost
    ///   * `Some(child_window_id)` → that child window is OS-frontmost
    ///   * `None`                  → no sonic window has been focused yet,
    ///     OR focus has moved out of every sonic window to another app.
    ///
    /// Subsumes a separate "which child has focus" field: main-vs-child is
    /// discriminated by `frontmost_kind()`, so one id answers both questions
    /// and the two cannot disagree.
    ///
    /// Keyboard / menubar / accelerator actions (Cmd+T, Cmd+W, Cmd+\\, …)
    /// route through this id so a chord typed in window B never mutates
    /// window A's tab vec. Set in both the main and child `Focused(true)`
    /// arms; on `Focused(false)` we only clear when the dropped window was
    /// the current frontmost — focus moving to a *different* sonic window
    /// arrives as that other window's `Focused(true)` and overwrites
    /// frontmost in the right order.
    ///
    /// Without it, Cmd+T after a tear-out opened a tab in the wrong window,
    /// and Cmd+W in a new window closed the old window's tab.
    pub(super) frontmost_window: Option<WindowId>,
    /// OS-drag tab payloads received before the main [`WindowState`] exists.
    /// Startup pasteboard / OLE deliveries can arrive before `do_resumed`
    /// inserts `main_window_id`; queue them so the destination tab is created
    /// after main is available instead of silently dropping the payload.
    pub(super) pending_os_drag_payloads: Vec<crate::os_drag::TabPayload>,
    /// Native winit paths collected by source window until the current event-loop turn ends.
    pub(super) pending_winit_file_drops: HashMap<WindowId, Vec<PathBuf>>,
    /// Optional theme loader, set by `run_with`. Used to reload a theme
    /// by name live.
    pub(crate) theme_loader: Option<ThemeLoader>,
    /// Optional keymap loader, set by `run_with`.
    pub(crate) keymap_loader: Option<KeymapLoader>,
    /// Proxy used to wake the idle event loop. `None` in tests that
    /// construct `App` directly via [`App::new`] without a real event loop.
    pub(super) event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    /// One committed GPU context survives window closure and owns every recovery attempt.
    gpu_recovery: Option<gpu_recovery::GpuRecovery>,
    /// Bounded workers for openability probes and native direct-open dispatch.
    pub(in crate::app) path_workers: Option<path_target::PathWorkers>,
    /// Current native home captured once for deterministic `~/` target resolution.
    pub(super) home_dir: Option<PathBuf>,
    /// Explicit config path used by an isolated runtime smoke; ordinary runs use the user path.
    pub(super) runtime_config_path: Option<PathBuf>,
    /// Local hostname used to reject foreign-authority OSC 7 snapshots.
    pub(super) local_hostname: String,
    /// Hidden Linux package-smoke state; absent during ordinary application runs.
    pub(super) runtime_smoke: Option<runtime_smoke::RuntimeSmokeState>,
    /// Minimum interval between two successive frames. Defaults to 1/60s
    /// and is updated in `resumed` from the current monitor's reported
    /// refresh rate. Used by the RedrawRequested handler to skip an
    /// over-render and by `about_to_wait` to schedule the next vsync
    /// boundary via `ControlFlow::WaitUntil`. See perf audit #9.
    pub(super) frame_period: Duration,
    /// The monitor's own reported period, kept separately so the degrade
    /// decision stays reversible.
    ///
    /// `frame_period` is the *resolved* period and is overwritten with the
    /// software cap while degrading. Resolving a later decision from it would
    /// read the cap back as if it were the monitor's rate, so clearing degrade
    /// could not restore the monitor's cadence and the window stayed at 40 fps
    /// until restart. Every resolution reads this field instead; only the
    /// monitor probe writes it.
    pub(super) monitor_frame_period: Duration,
    /// True when the no-GPU degrade path is engaged (software rasterizer
    /// detected or forced via `[appearance].software_render_mode`). When set,
    /// `frame_period` is replaced by the software cap and per-frame scrollbar
    /// fade animation is suppressed so the CPU isn't asked to rasterize at full
    /// refresh. Resolved after the renderer is created and re-resolved on an
    /// explicit config reload.
    pub(super) software_render_degrade: bool,
    /// Legacy observation of main pacing deferral; owner redraw state, not this flag, arms deadlines.
    pub(super) pending_redraw: bool,
    /// Legacy child-deferral observation, never the runtime wake-fold authority.
    pub(super) pending_redraw_windows: HashSet<WindowId>,
    /// Typed owner-addressed deadlines armed by the most recent event-loop fold.
    redraw_due: Vec<redraw::DueWork>,
    pub(super) warm_window_pool: Vec<WarmWindow>,
    /// Translation bundle. Rebuilt when the user picks a new locale in
    /// the preferences "Language" dropdown.
    pub(super) i18n: sonicterm_ui::i18n::I18n,
    /// Optional platform hook that takes a serialized tab payload and
    /// hands it off to the OS-level drag-and-drop system
    /// (`NSPasteboard` on macOS, OLE `DoDragDrop` on Windows). When
    /// set, [`Self::tear_out_tab`] checks whether the cursor sits outside every
    /// SonicTerm-owned window and invokes the sink. The local tab is detached
    /// only if the sink returns an explicit `DragAck::Accepted`; current
    /// platform paths preserve it and fall back to in-process tear-out.
    /// Installed by the platform shell via
    /// [`crate::shell::MacShell::with_os_drag_sink`] /
    /// [`crate::shell::WindowsShell::with_os_drag_sink`].
    pub(crate) os_drag_sink: Option<Arc<dyn crate::os_drag::OsDragSink>>,
    /// Platform OS-drag backend. Distinct from `os_drag_sink` (wire-format
    /// publication): Windows drives OLE `DoDragDrop`; macOS currently publishes
    /// the pasteboard payload and posts a cancelled outcome without cursor capture.
    /// Installed by the platform bin (`sonicterm-mac` / `sonicterm-windows`)
    /// at startup. `None` in tests + on platforms without an impl.
    pub(super) os_drag_backend: Option<Box<dyn os_drag::OsTabDragBackend>>,
    /// Shared mailbox the [`os_drag::OsTabDragBackend`] writes pending
    /// drag outcomes into. Drained by `do_user_event` on every
    /// `UserEvent::DragMoved` / `DragEnded` wake.
    pub(super) os_drag_pending: Arc<os_drag::PendingDragOutcome>,
    /// Shared tab-bar snapshot registry. The App publishes the live
    /// per-window tab bar layout into this on every redraw (see
    /// `publish_os_drag_bar_snapshot`); a Phase-C2 OS-drag backend
    /// reads from it inside its drop callback (Windows
    /// IDropTarget::Drop / macOS NSDraggingDestination::performDrop)
    /// to resolve the raw screen-coordinate drop into a real
    /// `(WindowId, slot)` pair before posting a `DroppedOnBar` outcome.
    pub(super) os_drag_bars: Arc<os_drag::TabBarRegistry>,
    /// Captured window and tab identity until the native drag outcome is consumed.
    pub(super) os_drag_source: Option<(WindowId, sonicterm_ui::tabs::TabId)>,
    /// View → Toggle Tab Bar state. When `false`, the menubar Toggle
    /// Tab Bar action has hidden the tab bar chrome. Defaults to
    /// `true`. Exposed via [`Self::tab_bar_visible`] so the renderer
    /// + hit-test code can read it on each frame.
    pub(super) tab_bar_visible: bool,
    /// Broadcast-input mode. When enabled, bytes typed into `source_pane`
    /// are mirrored into matching receiver panes after the source PTY write.
    pub(super) broadcast: BroadcastState,
    /// Quit confirmation guard for the Cmd+Q chord. A single press arms it and
    /// shows a red "press again" prompt; the app exits on a second press during
    /// the confirmation window. See [`quit_hold`].
    pub(super) quit_hold: quit_hold::QuitHold,
    /// One-shot hook fired the first time the winit `ApplicationHandler::
    /// resumed` callback runs — i.e. when NSApp / the platform event
    /// loop is fully initialized but BEFORE we hand control back to
    /// winit's `run_app`. macOS uses this slot to install the native
    /// NSMenu; calling `setMainMenu` earlier (before winit builds the
    /// AppKit loop) leaves AppKit with only the default
    /// `Apple, sonicterm-mac` menubar.
    pub(crate) on_resumed: Option<Box<dyn FnOnce() + Send>>,

    /// One-shot hook fired the moment the main window has been created
    /// (immediately after `event_loop.create_window` succeeds, before the first
    /// redraw is requested). Receives the `raw-window-handle` of the
    /// window. Windows uses this slot to install the muda menubar,
    /// which requires the HWND at install time. Unused on macOS.
    pub(super) on_window_ready: Option<Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>>,
    /// Test-only redraw request counter. Every
    /// production code path that calls `window.request_redraw()` after
    /// a `run_action` dispatch also bumps this counter in lock-step.
    /// Tests assert against this rather than the live winit window
    /// (which has no public introspection API). Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub redraw_request_count: std::sync::atomic::AtomicUsize,
    /// Test-only counter incremented on every call to
    /// [`Self::reap_empty_child`]. Lets tests
    /// distinguish "child window cleanup went through the unified reap
    /// contract" from "a direct `windows.remove` happened" — both would
    /// shrink the `windows` map, but only the former nulls out straggler
    /// `redraw_target`s and fires the reap trace. Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub reap_call_count: std::sync::atomic::AtomicUsize,
    /// Test-only viewport override. When
    /// `Some((outer, cell_w, cell_h))`, [`Self::compute_active_pane_rects`]
    /// uses `outer` instead of fetching the renderer's logical size and
    /// [`Self::resize_visible_panes`] uses `(cell_w, cell_h)` instead of
    /// the renderer's `cell_size()`. Lets tests exercise the production
    /// `close_active_pane` path (Grid + PtyHandle resize wiring) without
    /// a live wgpu surface. Stays `None` in release builds whose tests
    /// don't touch it.
    #[doc(hidden)]
    pub test_viewport_override: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
    /// Compatibility reducer observations; live topology decisions and native effect targets belong to App.
    pub(crate) machine: sonicterm_app_core::AppStateMachine,
    window_keys: crate::window_key_boundary::WindowKeyRegistry,
}

impl App {
    /// Insert a window and register its owner as one operation.
    ///
    /// The two steps are inseparable, so they are not offered separately. A
    /// window inserted without an owner is not merely uncharged, it is absent
    /// from hierarchy accounting entirely, and nothing later recovers it:
    /// [`Self::reconcile_pane_owners`] and [`Self::reattribute_pane_owners`]
    /// both skip a window whose owner is `None`, so its panes never get owners
    /// either and the periodic sampler passes over the whole subtree forever.
    ///
    /// Registering here rather than at each call site is the same reasoning
    /// [`Self::reconcile_pane_owners`] applies to panes: a rule every call site
    /// must remember is a rule one call site will forget, and the forgotten one
    /// is silent — the window works, and only the memory report is missing it.
    ///
    /// Panes already in `window` are adopted by this call. A window populated
    /// after insertion instead reconciles when those panes arrive.
    pub(super) fn insert_window_registered(&mut self, id: WindowId, mut window: WindowState) {
        self.frame_counters_sealed.set(true);
        if let Some(app) = self.frame_counters.as_mut() {
            // the App's gate is on, every registered window and its renderer count.
            if window.redraw.frame_counters.is_none() {
                window.redraw.frame_counters =
                    Some(app.window_counters(self.main_window_id == Some(id)));
            }
            app.dispatch.register_native(id);
            if let Some(renderer) = window.renderer.as_mut() {
                renderer.set_frame_counting(true);
            }
        }
        window.refresh_monitor_period();
        let owner_prepared = window.owner.is_some();
        let key = self.window_keys.intern(id);
        if let Some(native) = &window.window {
            native.set_title(&compose_window_title(key, &window.custom_window_name));
        }
        self.windows.insert(id, window);
        if !owner_prepared {
            self.register_window_owner(id);
        }
    }

    /// Register a window in the governor hierarchy and record its owner.
    ///
    /// Private, and deliberately: reaching it goes through
    /// [`Self::insert_window_registered`], so registration cannot drift away
    /// from the insertion it belongs to.
    ///
    /// Idempotent by construction: a window that already has an owner keeps
    /// it, so a re-insert during tab transfer cannot create a second owner
    /// that never closes. That is the ratchet shape — an owner registered
    /// twice and released once leaves the hierarchy permanently over-counted.
    fn register_window_owner(&mut self, id: WindowId) {
        self.register_window_owner_inner(id);
        // A window arrives with its panes already populated, so registering
        // the window without them would leave every pane unowned until the
        // next sampling pass.
        //
        // Re-attribution rather than plain reconciliation: a window built by
        // tear-out receives panes that already carry an owner, parented below
        // the window they left. Reconciliation only adopts *ownerless* panes,
        // so it skips exactly those and leaves the source window counting a
        // pane it no longer holds — which refuses that window's close.
        self.reattribute_pane_owners();
    }

    fn register_window_owner_inner(&mut self, id: WindowId) {
        let root = self.governor.root_owner();
        // Cloned before the window borrow: `ResourceGovernor` is a handle over
        // an `Arc<Ledger>`, so this shares the ledger rather than copying it.
        let governor_handle = self.governor.clone();
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` tracks no entry for this id, so an owner created
            // here would have no window to retain or close it.
            return;
        };
        if window.owner.is_some() {
            // When: this `window` already holds an owner, so creating another
            // would leave a duplicate hierarchy node no one closes.
            return;
        }
        let owner =
            self.governor.create_child(root, OwnerKind::Window, tracking_only_owner_limits());
        match owner {
            Ok(owner) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.owner = Some(OwnerGuard::new(governor_handle, owner));
                }
            }
            Err(error) => {
                // A window that cannot register still works; it is invisible
                // to hierarchy accounting until the next insert. Failing the
                // window would trade a diagnostic gap for a lost window.
                tracing::warn!(
                    target: "memory",
                    ?error,
                    "window owner registration failed; hierarchy accounting will omit it"
                );
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _dispatch = self.frame_dispatch_scope();
        self.do_resumed(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let _dispatch = self.frame_dispatch_scope();
        self.note_frame_user_wake();
        let started = self.frame_clock_start();
        self.do_user_event(event_loop, event);
        self.note_frame_dispatch(frame_counters::DispatchKind::UserEvent, started);
        self.emit_frame_lines(None);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        let _dispatch = self.frame_dispatch_scope();
        let started = self.frame_clock_start();
        let redraw = started.is_some() && matches!(event, WindowEvent::RedrawRequested);
        // The dispatch may close the window, so whether it counts is read before it runs.
        let counted = started.is_some() && self.begin_window_handler(win_id);
        if redraw {
            // the gate is on, a redraw is counted and takes its panes' pending flushes.
            self.note_redraw_requested(win_id);
        }
        self.do_window_event(event_loop, win_id, event);
        if let Some(started) = started {
            // the gate is on, the window's handler time is recorded.
            self.note_window_handler(win_id, started, counted);
            self.emit_frame_lines(Some((win_id, redraw)));
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        let _dispatch = self.frame_dispatch_scope();
        self.note_frame_wake(&cause);
        let started = self.frame_clock_start();
        self.do_new_events(event_loop, cause);
        self.note_frame_dispatch(frame_counters::DispatchKind::NewEvents, started);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _dispatch = self.frame_dispatch_scope();
        let started = self.frame_clock_start();
        self.do_about_to_wait(event_loop);
        self.note_frame_dispatch(frame_counters::DispatchKind::AboutToWait, started);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        let _dispatch = self.frame_dispatch_scope();
        // Forward to sonicterm-logging so every Cmd+Q / WM_CLOSE /
        // last-window exit lands in sonicterm.log. See
        // `crates/sonicterm-logging/src/exit_trace.rs`.
        self.finish_frame_lines();
        sonicterm_logging::record_loop_exiting();
    }
}

#[cfg(test)]
#[path = "native_window_title_tests.rs"]
mod native_window_title_tests;

#[cfg(all(test, any(windows, unix)))]
mod pty_test_support;

#[cfg(all(test, any(windows, unix)))]
#[path = "close_baseline_tests.rs"]
mod close_baseline_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
