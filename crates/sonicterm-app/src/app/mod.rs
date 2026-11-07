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
    keymap::{BroadcastScope, Keymap},
    theme::Theme,
};
// Only the gated test hooks name `Action` through `use super::*`.
#[cfg(any(test, feature = "test-support"))]
use sonicterm_cfg::keymap::Action;
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

mod accessibility;
pub use accessibility::AccessibilityRequest;
pub use config_apply::StartupLoads;
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
mod dispatch_timeline;
pub use dispatch_timeline::{
    AppearanceLossV1, AppearanceRecordV1, AppearanceTargetV1, AttemptId, AttemptIncompleteReasonV1,
    AttemptOutcomeV1, AttemptPhasesV1, AttemptRecordV1, CallbackKindV1, DispatchEventKindV1,
    DispatchEventV1, DispatchId, DispatchTimelineArmV1, DispatchTimelineTakeV1,
    DispatchTimelineToken, DispatchTimelineV1, NotRenderedReasonV1, PhaseTransitionV1, PhaseV1,
    PublicationDecisionV1, PublicationSeq, SkipReasonV1, TransitionTargetV1, UnscopedKindV1,
    WitnessResolutionV1,
};
// Pins both dispatch-timeline signatures here, in the parent, in every build: a gate on a method, its
// impl or its module, or a changed type, fails a plain library build.
const _: () = {
    let _: fn(&mut App, u64, Option<AppearanceTargetV1>) -> DispatchTimelineArmV1 =
        App::arm_dispatch_timeline_v1;
    let _: fn(&mut App, DispatchTimelineToken) -> DispatchTimelineTakeV1 =
        App::take_dispatch_timeline_v1;
};
mod default_handlers;
mod echo_timeline;
pub use echo_timeline::{
    AdmissionDecisionV1, DeferReasonV1, EchoTimelineEventV1, EchoTimelineKindV1,
    EchoTimelineTakeV1, EchoTimelineV1, FloodServiceV1, OutputCheckOutcomeV1, PermitClearCauseV1,
    PermitSnapshotV1, TickIdentityV1,
};
mod echo_watch;
pub use echo_watch::{
    ArmOutcome, ArmToken, EchoAppearance, EchoDelivery, EchoDeliveryOutcome, EchoPublication,
    EchoRowIdentity, EchoTrace, EchoWatchTarget, TakeOutcome,
};
mod effects;
use effects::close_owner;
mod config_state;
mod event_loop;
mod fg_probe;
mod field_input;
mod field_pointer;
mod font_catalog;
mod font_state;
mod frame_counters;
mod guard_correlation;
pub use guard_correlation::{
    AbandonedSectionV1, GuardCorrelationTakeV1, GuardCorrelationV1, LossIntervalV1, PaneSectionsV1,
    PendingSectionV1, SectionRecordV1, SpanBatchV1, SpanRecordV1,
};
// Pins the take's signature here, in the parent, in every build: a gate on it fails a plain library build.
const _: () = {
    let _: fn(&mut App) -> GuardCorrelationTakeV1 = App::take_guard_correlation_v1;
};
mod guard_custody;
pub use frame_counters::{
    clock_epoch, CounterRecord, FrameCountersSnapshot, FrameCountersTooLate, HistogramBuckets,
};
mod frame_pacing;
pub use frame_pacing::{
    effective_frame_period, should_defer_streaming_redraw, should_degrade_for_software_render,
    should_flush_pending_pty_redraw, software_render_frame_period,
};
mod display_link;
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use display_link::{probe_native_display_link, NativeDisplayLinkReport};
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
mod about;
mod about_window;
mod keymap_dispatch;
mod live_font;
mod locale;
mod mcp;
mod media;
mod memory_report;
mod memory_report_window;
pub mod memory_snapshot;
mod misc;
mod native_title;
mod native_title_font;
pub mod os_drag;
mod overlays;
mod owners;
#[cfg(test)]
use owners::install_transferred_pane_owner;
pub(crate) use owners::OwnerGuard;
use owners::{pane_owner_limits, pane_seam_cap_sum_bytes, tracking_only_owner_limits};
pub use owners::{pane_seam_cap_terms, PaneSeamCapTerm};
mod pane_exit;
pub(crate) mod pane_signal;
pub(crate) use pane_signal::PaneSignal;
mod pane_launch;
mod pane_refresh;
use pane_refresh::update_terminal_ime_cursor_area;
pub use pane_refresh::{
    invalidate_selection_for_content, mark_all_panes_dirty, resize_all_panes,
    resize_panes_to_rects, seed_parser_theme_colors,
};
mod pane_state;
mod pane_target;
mod perf_control;
pub use perf_control::{
    PerfControlOutcomeV1, PerfControlPauseV1, PerfControlPhaseV1, PerfControlStopV1,
};
// Pins the phase-control and echo-timeline signatures here, in the parent, in every build: a gate on
// a method, its impl or its module, or a changed type, fails a plain library build.
const _: () = {
    let _: fn(&mut App, PerfControlPhaseV1) -> PerfControlOutcomeV1 = App::perf_control_phase;
    let _: fn(&mut App, u64, ArmToken) -> EchoTimelineTakeV1 = App::take_echo_timeline_v1;
};
use pane_state::pane_id_at_point;
pub use pane_state::{
    next_pane_id, CommandPhase, CommandQueue, LastCommand, PaneCommandEvent, PaneCommandState,
    PaneState,
};
mod jobs;
mod path_target;
mod perf_present;
// Pins both S10 attribution signatures here, in the parent, in every build: a gate on the methods, their
// impl or the whole `perf_present` module, or a changed type, fails a plain library build.
const _: () = {
    let _: fn(&mut App, u64, &str, &str, u32) -> Option<u64> = App::arm_s10_attribution;
    let _: fn(&mut App, u64) = App::disarm_s10_attribution;
};
mod privilege;
mod program_state;
use privilege::refresh_window_tab_privileges;
mod quit_hold;
mod reaper_driver;
mod redraw_target;
mod runtime_smoke;
pub use runtime_smoke::{RuntimeSmokeFailure, RuntimeSmokeScenario, RuntimeSmokeSpec};
mod output_event;
mod redraw;
mod render_timing;
pub mod renderer_retention;
pub mod retention;
pub use retention::{TrimDecision, TrimSkip};
mod scroll;
pub mod scrollbar_input;
pub mod scrollbar_visibility;
mod search_handle;
mod selection_gesture;
pub use selection_gesture::next_click_count;
use selection_gesture::{PointerCell, PointerGesture, PointerGestureOwner};
mod diagnostics;
mod session;
mod shell_token;
pub(in crate::app) use diagnostics::DiagnosticsState;
mod runtime_services;
pub(in crate::app) use runtime_services::RuntimeServices;
mod interaction;
pub(in crate::app) use interaction::InteractionState;
mod window_registry_state;
pub(in crate::app) use window_registry_state::WindowRegistry;
mod status_line;
pub(crate) use session::identity_config_normalizer;
pub use session::{build_async_fallback_loader_for_proxy, init_tracing_public};
mod shared_gpu;
pub(crate) use shared_gpu::{font_fallback_waker, gpu_device_state_waker};
mod reply_delivery;
mod report_windows;
mod spawn_pane;
mod splitter_input;
mod sync_clock;
mod sync_frame;
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
mod wheel_plan;
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
    configured_window_size, inherited_window_size, WindowRequest,
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
    /// The windows that exist, the main and frontmost window, each window key, and prepared windows.
    pub(in crate::app) registry: WindowRegistry,
    /// Cross-window interaction: the palette, edit targets, field pointer captures, OS drag and drop, broadcast.
    pub(in crate::app) interaction: InteractionState,
    /// Background services the App starts, polls and stops: probes, MCP, the reaper, GPU recovery.
    pub(in crate::app) services: RuntimeServices,
    /// Diagnostic and accounting state: counters, retention and trim samples, breadcrumbs.
    /// Test-only redraw request counter. Every
    /// production code path that calls `window.request_redraw()` after
    /// a `run_action` dispatch also bumps this counter in lock-step.
    /// Tests assert against this rather than the live winit window
    /// (which has no public introspection API). Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub redraw_request_count: std::sync::atomic::AtomicUsize,
    /// Test-only counter incremented on every call to
    /// `App::reap_empty_child`. Lets tests
    /// distinguish "child window cleanup went through the unified reap
    /// contract" from "a direct `windows.remove` happened" — both would
    /// shrink the `windows` map, but only the former nulls out straggler
    /// `redraw_target`s and fires the reap trace. Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub reap_call_count: std::sync::atomic::AtomicUsize,
    pub(in crate::app) diagnostics: DiagnosticsState,
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
    pub(super) config: Config,
    /// Where configuration comes from: normalizer, theme and keymap loaders, smoke config path and
    /// startup load outcomes.
    pub(in crate::app) configs: config_state::ConfigState,
    /// Font sizes, families and the installed-font catalog: what font controls reset to and read.
    pub(in crate::app) fonts: font_state::FontState,
    pub(super) keymap: Keymap,
    // The main window holds no state of its own here. Its renderer, tabs,
    // tab states, panes, selection, modifiers, copy mode, last render, cursor
    // visibility, and hover link all live in `self.registry.windows[main_window_id]`,
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
    /// Test-only copied-file list standing in for the native clipboard's file
    /// list. Installing either test override keeps every read off the desktop
    /// clipboard; `None` here then means no files are on the clipboard.
    #[doc(hidden)]
    pub(super) test_clipboard_files: Option<Vec<std::path::PathBuf>>,
    /// Test-only clipboard write rejection injected at the production write
    /// boundary. Disabled in every constructor so ordinary runs retain the real
    /// clipboard behavior and the in-memory success seam remains opt-in.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub(super) test_clipboard_write_failure: bool,
    /// Test-only PTY write ledger. `write_to_pane` records every boundary write
    /// here before resolving the pane to a real PTY, so headless tests can assert
    /// which pane an action targeted without constructing a process-backed PTY.
    #[doc(hidden)]
    pub(super) test_pty_writes: Arc<Mutex<Vec<(u64, Vec<u8>)>>>,
    /// Unit-test observation of actual PTY spawn inputs; absent from production builds.
    #[cfg(test)]
    test_pane_launches: std::cell::RefCell<Vec<(u64, pane_launch::PaneLaunch)>>,
    /// Unit-test record of each logged PTY write's pane and producer source, so tests can check attribution.
    #[cfg(test)]
    test_pty_write_sources: std::cell::RefCell<Vec<(u64, PtyInputSource)>>,
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
    /// The report windows (Memory Usage and About): their pending opens and shown text.
    pub(in crate::app) reports: report_windows::ReportWindows,
    /// Deferred in-process tab tear-out request from either drag/drop or the
    /// Move Tab to New Window action. Drained only while an ActiveEventLoop is
    /// available so every path uses the same native-window constructor.
    pub(super) pending_tear_out: Option<PendingTearOut>,
    /// Deferred `cancel_drag_session` request. Set by `handle_os_drag_ended`
    /// on the `DroppedOnEmpty` branch instead of cancelling inline,
    /// so any tear-out-spawn produced by the existing
    /// `pending_new_window` drain runs to completion BEFORE
    /// cross-window drag-residue cleanup mutates `self.registry.windows`.
    /// Drained by `App::drain_pending_os_teardown` AFTER
    /// `App::drain_pending_window_creates` at the natural event-loop
    /// boundary in `event_loop.rs::do_user_event`. The
    /// `cancel_drag_session` all-windows loop runs **unconditionally**
    /// when drained — this flag controls only WHEN it runs, not
    /// WHETHER, so the cleanup stays idempotent.
    pub(super) pending_os_teardown: bool,
    /// Test-only callback fired
    /// inside [`Self::cancel_drag_session`] AFTER the `self.registry.windows.keys()`
    /// snapshot is collected but BEFORE the per-id iteration body runs.
    /// Lets a regression test
    /// mutate `self.registry.windows` in the exact race window that the
    /// `get_mut(&id).else { continue }` arm is designed to tolerate.
    /// Consumed (`take()`-d) at the call site so the closure is invoked
    /// at most once per `cancel_drag_session` run and the mutable
    /// borrow on `self.registry.windows` is not held while it runs. Compiled only
    /// under `cfg(test)` or `test-support`, so release builds carry neither the
    /// field nor the call; an integration test installs it through
    /// `App::__test_set_post_snapshot_hook`.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub(super) test_post_snapshot_hook: Option<Box<dyn FnOnce(&mut App) + Send>>,
    /// Deferred app-exit request, set by a quit action or by a close that
    /// leaves no active terminal window. `do_about_to_wait` drains it by
    /// calling `event_loop.exit()`; the flag exists because those paths have
    /// no `ActiveEventLoop` handle.
    pub(super) pending_exit: bool,
    /// Unit-test observation of every scheduler trim decision, in pass order.
    #[cfg(test)]
    pub(super) test_scheduler_trims: Vec<(WindowId, retention::TrimDecision)>,
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
    /// Cached terminal disposition prevents a repeated finish from retiring panes or draining twice.
    session_finished: Option<bool>,
    /// This App's frame and lock counters; `Some` only when its gate is on, fixed for its lifetime.
    pub(super) frame_counters: Option<frame_counters::AppFrameCounters>,
    /// The windows asked for a frame in the current dispatch; see [`redraw::DispatchRedraws`].
    pub(super) dispatch_redraws: redraw::DispatchRedraws,
    /// Each terminal window's adapter and strip focus by window id, warm spares included.
    pub(super) accessibility:
        std::collections::HashMap<WindowId, accessibility::WindowAccessibility>,
    /// Proxy used to wake the idle event loop. `None` in tests that
    /// construct `App` directly via [`App::new`] without a real event loop.
    pub(super) event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    /// Bounded workers for openability probes and native direct-open dispatch.
    pub(in crate::app) path_workers: Option<path_target::PathWorkers>,
    /// Current native home captured once for deterministic `~/` target resolution.
    pub(super) home_dir: Option<PathBuf>,
    /// Local hostname used to reject foreign-authority OSC 7 snapshots.
    pub(super) local_hostname: String,
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
    /// The one clock every pacing read goes through: redraw admission and completion for both window
    /// roles, collection contention, the display-link tick and the wait fold. `Instant::now` in
    /// production; a test installs a fake through `Self::__test_set_dispatch_clock`.
    pub(super) dispatch_clock: fn() -> Instant,
    /// Legacy observation of main pacing deferral; owner redraw state, not this flag, arms deadlines.
    pub(super) pending_redraw: bool,
    /// Legacy child-deferral observation, never the runtime wake-fold authority.
    pub(super) pending_redraw_windows: HashSet<WindowId>,
    /// Typed owner-addressed deadlines armed by the most recent event-loop fold.
    redraw_due: Vec<redraw::DueWork>,
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
    /// The default-handler command's backend, confirmation and active run.
    pub(in crate::app) default_handlers: default_handlers::DefaultHandlersState,
    /// View → Toggle Tab Bar state. When `false`, the menubar Toggle
    /// Tab Bar action has hidden the tab bar chrome. Defaults to
    /// `true`. Exposed via [`Self::tab_bar_visible`] so the renderer
    /// + hit-test code can read it on each frame.
    pub(super) tab_bar_visible: bool,
    /// Status line strip: session toggles, memory sampler, reload errors and per-window pane facts.
    pub(super) status_line: status_line::StatusLineState,
    /// Program-reported progress for the taskbar, Dock or launcher, and the program toast.
    pub(super) program: program_state::ProgramState,
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
        self.diagnostics.frame_counters_sealed.set(true);
        if let Some(app) = self.frame_counters.as_mut() {
            // the App's gate is on, every registered window and its renderer count.
            if window.redraw.frame_counters.is_none() {
                window.redraw.frame_counters =
                    Some(app.window_counters(self.registry.main_window_id == Some(id)));
            }
            app.dispatch.register_native(id);
            if let Some(renderer) = window.renderer.as_mut() {
                renderer.set_frame_counting(true);
            }
        }
        window.refresh_monitor_period();
        let owner_prepared = window.owner.is_some();
        if window.role.is_report() {
            // A report window's title is its name, and it takes no window number,
            // so terminal windows keep consecutive numbers.
            if let Some(native) = &window.window {
                native.set_title(&window.custom_window_name);
            }
        } else {
            // When: `window.role` is not a report, intern a window number; its title is written below.
            self.registry.window_keys.intern(id);
        }
        self.registry.windows.insert(id, window);
        let _ = self.sync_native_title(id, Instant::now(), true);
        // The link takes the period the refresh above just read; only macOS installs one.
        self.install_native_display_link(id);
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
        let Some(window) = self.registry.windows.get(&id) else {
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
                if let Some(window) = self.registry.windows.get_mut(&id) {
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

#[cfg(test)]
#[path = "mod_native_window_title_tests.rs"]
mod native_window_title_tests;

#[cfg(all(test, any(windows, unix)))]
mod pty_test_support;

#[cfg(test)]
mod source_scan_support;

#[cfg(all(test, any(windows, unix)))]
#[path = "mod_close_baseline_tests.rs"]
mod close_baseline_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;

#[cfg(test)]
#[path = "mod_test_support_tests.rs"]
mod test_support_tests;

mod user_event;
pub use user_event::UserEvent;
mod application_handler;
