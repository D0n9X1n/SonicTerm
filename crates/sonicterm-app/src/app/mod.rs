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

/// One charged pane class and its exact production seam-cap contribution.
///
/// The three grid classes share one storage allocation: `GridVisible` carries
/// that cap, while `GridHistory` and `GridAlternate` carry zero rather than
/// pretending each region may allocate another full grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneSeamCapTerm {
    /// Class charged by the pane-retention pass.
    pub class: ResourceClass,
    /// Bytes this class contributes to the pane backstop.
    pub bytes: usize,
}

/// Return the exact production pane seam inventory by charged class.
///
/// Every class charged by the pane-retention pass appears exactly once. Tests
/// compare this inventory to that production path, so a new charged class must
/// state its owning cap contribution before the build can pass.
#[must_use]
pub const fn pane_seam_cap_terms() -> [PaneSeamCapTerm; 8] {
    [
        PaneSeamCapTerm {
            class: ResourceClass::GridVisible,
            bytes: sonicterm_grid::grid::MAX_GRID_CELLS as usize
                * std::mem::size_of::<sonicterm_types::Cell>(),
        },
        PaneSeamCapTerm { class: ResourceClass::GridHistory, bytes: 0 },
        PaneSeamCapTerm { class: ResourceClass::GridAlternate, bytes: 0 },
        PaneSeamCapTerm {
            class: ResourceClass::ParserCapture,
            bytes: sonicterm_vt::vt::MAX_MEDIA_PAYLOAD_BYTES
                + sonicterm_vt::vt::MAX_ESCAPE_SEQUENCE_BYTES,
        },
        PaneSeamCapTerm {
            class: ResourceClass::ProtocolMetadata,
            bytes: sonicterm_grid::hyperlink::MAX_HYPERLINK_METADATA_BYTES,
        },
        PaneSeamCapTerm {
            class: ResourceClass::InlineMediaRetained,
            bytes: media::MAX_RETAINED_INLINE_IMAGE_BYTES,
        },
        PaneSeamCapTerm {
            class: ResourceClass::PtyOutput,
            bytes: sonicterm_io::pty::max_queued_output_ring_bytes(),
        },
        PaneSeamCapTerm {
            class: ResourceClass::PtyInput,
            bytes: sonicterm_io::pty::max_pty_queued_input_bytes(),
        },
    ]
}

const fn pane_seam_cap_sum_bytes() -> usize {
    let terms = pane_seam_cap_terms();
    let mut total = 0usize;
    let mut index = 0usize;
    while index < terms.len() {
        total += terms[index].bytes;
        index += 1;
    }
    total
}

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

/// Owner limits: seam caps enforce, the governor backstops.
///
/// Enforcement stays with the per-seam caps that are already tested and
/// falsified. The governor's limit is [`PANE_COMMITTED_BUDGET_BYTES`], derived
/// from those caps and set above them, so it is a tripwire for a seam that has
/// stopped bounding rather than a second bound that must agree with the first.
///
/// Window and process owners stay untracked: their content is the sum of their
/// panes, each already held to its own budget, and a second aggregate limit
/// would be the drift surface this design avoids.
fn pane_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: PANE_COMMITTED_BUDGET_BYTES,
        class_bytes: enum_map::enum_map! { _ => usize::MAX },
        class_items: enum_map::enum_map! { _ => None },
    }
}

/// Owner limits that track without constraining.
///
/// Used for window and process owners, whose retention is the sum of the panes
/// beneath them. Each pane is already held to
/// [`PANE_COMMITTED_BUDGET_BYTES`], so an aggregate limit here would add a
/// second figure to keep in agreement without catching anything the per-pane
/// backstop misses.
fn tracking_only_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map::enum_map! { _ => usize::MAX },
        class_items: enum_map::enum_map! { _ => None },
    }
}

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

/// Runs the two-phase governor close and returns any refusal to the caller.
fn close_owner(
    governor: &ResourceGovernor,
    owner: ResourceOwnerId,
) -> Result<(), sonicterm_types::BudgetError> {
    governor.begin_close(owner).and_then(|()| governor.finish_close(owner))
}

fn open_url_effect(url: &str) -> std::io::Result<()> {
    sonicterm_cfg::url_open::open(url)
}

/// Closes a governor owner when the thing that owned it drops.
///
/// The charge on a pane is released by `CommittedReservation::Drop`, and its
/// doc comment states why that is correct: *there is no teardown site to
/// forget*. The owner beside it had no such guarantee — it was a plain
/// `Option<ResourceOwnerId>` that vanished when the pane dropped, leaving the
/// governor holding a record that never closed.
///
/// Measured before this: 80 of 80 owners still `Open` after 40 create/destroy
/// cycles, and `OwnerRegistry` has `get` and `insert` and **no `remove`**, so
/// each one is retained for the life of the process along with its `RwLock`,
/// `Mutex`, and two `EnumMap`s over every resource class.
///
/// Six pane-removal sites across four files reach `panes.remove`. Patching
/// each is how the original defect happened; this makes the close a property
/// of ownership instead.
pub(crate) struct OwnerGuard {
    governor: ResourceGovernor,
    owner: ResourceOwnerId,
}

impl OwnerGuard {
    /// Take responsibility for closing `owner` when this drops.
    pub(crate) fn new(governor: ResourceGovernor, owner: ResourceOwnerId) -> Self {
        Self { governor, owner }
    }

    /// The owner this guard will close.
    pub(crate) fn id(&self) -> ResourceOwnerId {
        self.owner
    }
}

impl std::fmt::Debug for OwnerGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OwnerGuard").field("owner", &self.owner).finish()
    }
}

// Lifecycle: dropping an `OwnerGuard` closes `owner` in the governor, releasing
// its ledger record; a refusal leaves that record retained rather than retried.
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        // Charges must already be gone: `finish_close` refuses an owner still
        // holding them. `PaneState` declares `charges` before `owner`, and
        // Rust drops fields in declaration order, so the reservations release
        // before this runs.
        if let Err(error) = close_owner(&self.governor, self.owner) {
            tracing::warn!(
                target: "memory",
                ?error,
                owner = ?self.owner,
                "owner did not close on drop; its record is retained for the process lifetime"
            );
        }
    }
}

/// Install a provisional pane owner only after every committed charge moves.
fn install_transferred_pane_owner(
    pane: &mut PaneState,
    provisional: OwnerGuard,
) -> Result<Option<OwnerGuard>, sonicterm_resource::CommittedBatchTransferError> {
    let owner = provisional.id();
    sonicterm_resource::CommittedReservation::transfer_batch(pane.charges.values_mut(), owner)?;
    Ok(pane.owner.replace(provisional))
}

static NEXT_SYNTHETIC_CHILD_WINDOW_TAG: AtomicU64 = AtomicU64::new(1);

// Ordering: `NEXT_SYNTHETIC_CHILD_WINDOW_TAG.fetch_add` uses `Relaxed`; only the
// uniqueness of each returned tag matters, never its order against other writes.
fn next_synthetic_child_window_id() -> WindowId {
    let tag = NEXT_SYNTHETIC_CHILD_WINDOW_TAG.fetch_add(1, Ordering::Relaxed);
    WindowId::from(u64::MAX - tag)
}

/// Stable synthetic `WindowId` addressing the main window entry without a live
/// winit window.
///
/// Lets a test seed the main entry in the window map directly. `u64::MAX` is
/// collision-free because real OS window ids never reach it. Production never
/// constructs this id: window creation uses the real `window.id()` and clears
/// any pre-existing synthetic entry first.
#[doc(hidden)]
pub fn synthetic_main_window_id() -> WindowId {
    WindowId::from(u64::MAX)
}

/// Which terminal window currently owns the OS-frontmost focus.
///
/// Keymap dispatch and the menubar drain consume this to decide where a chord
/// like Cmd+T / Cmd+W / Cmd+\\ should land.
///
/// `Other` covers any non-terminal SonicTerm window; it explicitly does NOT
/// route terminal actions and falls back to main as a safe default.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmostKind {
    /// No window has focus, or recorded id is stale.
    None,
    /// Main terminal window is OS-frontmost.
    Main,
    /// A torn-out child terminal window is OS-frontmost. Carries the
    /// window id so the caller can index `windows`.
    Child(WindowId),
    /// A non-terminal SonicTerm window is frontmost. Terminal actions fall
    /// back to main.
    Other,
}

/// Read a window's screen-global inner origin + inner size into the
/// pure helper struct used by the drag-merge module. Falls back to
/// (0, 0) origin if the platform refuses to report position (e.g. on
/// some Wayland configurations); on such platforms the drag-merge
/// path is best-effort.
/// Screen-global inner origin and inner size, as the drag-merge module's
/// pure geometry struct.
///
/// A platform that refuses to report position reports a `(0, 0)` origin, which
/// leaves drag-merge best-effort there rather than failing the drag outright.
pub(super) fn window_geom(w: &Window) -> crate::tab_drag::WindowGeom {
    let origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
    let size = w.inner_size();
    crate::tab_drag::WindowGeom { inner_origin: origin, inner_size: (size.width, size.height) }
}

/// This window's scale factor, as the `f32` the geometry helpers expect.
#[inline]
pub(super) fn window_dpi(w: &Window) -> f32 {
    w.scale_factor() as f32
}

/// Wrap clipboard text for paste, applying DECSET 2004 bracketed-paste
/// guards (`ESC [ 200 ~` / `ESC [ 201 ~`) when the active pane has
/// requested bracketed paste. Pure function, exported for unit tests.
pub fn wrap_paste(text: &str, bracketed: bool) -> Vec<u8> {
    sonicterm_types::encode_payload(
        &sonicterm_types::UserPayload::Text(text.to_owned()),
        sonicterm_types::PasteTarget { bracketed, dialect: sonicterm_types::ShellDialect::Unknown },
        usize::MAX,
    )
    .expect("text and paste guards fit the address space")
}

/// Quote a single path or word for POSIX-shell paste. Re-exported from the
/// shared `sonicterm-types` implementation so file drops on macOS and Windows
/// paste the same bytes. Kept at this path so existing `super::shell_quote_posix`
/// imports continue to resolve.
pub use sonicterm_types::shell_quote_posix;

/// Compute the absolute viewport-top row for "scroll to previous / next
/// prompt". Returns `None` if there is no prompt in the requested
/// direction. Pure function so tests can drive it without a window.
pub fn pick_prompt_target(
    grid: &sonicterm_grid::grid::Grid,
    current_top_abs: u64,
    forward: bool,
) -> Option<u64> {
    let pick = if forward {
        grid.prompt_after(current_top_abs)
    } else {
        // When: `forward` is unset, so the search runs backward from
        // `current_top_abs` toward older scrollback instead of newer output.
        grid.prompt_before(current_top_abs)
    };
    pick.map(|p| p.start_row)
}

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

/// Preserve every config value for platforms without an additional policy.
pub(crate) fn identity_config_normalizer() -> ConfigNormalizer {
    Box::new(|config| (config, Vec::new()))
}

#[derive(Debug)]
struct PendingPointerMotion {
    bytes: [u8; 64],
    len: usize,
    profile: Option<(MouseTracking, bool, bool)>,
}

impl Default for PendingPointerMotion {
    fn default() -> Self {
        Self { bytes: [0; 64], len: 0, profile: None }
    }
}

impl PendingPointerMotion {
    fn validate_profile(&mut self, profile: Option<(MouseTracking, bool, bool)>) {
        let Some(current) = profile else {
            // When: profile is unavailable, retain the position until a later retry can validate it.
            return;
        };
        if self.profile.is_some_and(|previous| previous != current)
            || matches!(current.0, MouseTracking::Off | MouseTracking::Button)
        {
            // A changed or disabled tracking mode cannot receive a deferred position.
            self.len = 0;
        }
        self.profile = Some(current);
    }

    fn replace(&mut self, bytes: &[u8]) {
        // SGR reports contain at most three u32 fields; both supported encodings fit this fixed slot.
        assert!(bytes.len() <= self.bytes.len());
        self.bytes[..bytes.len()].copy_from_slice(bytes);
        self.len = bytes.len();
    }

    fn take(&mut self) -> Vec<u8> {
        let bytes = self.bytes[..self.len].to_vec();
        self.len = 0;
        bytes
    }

    fn flush(
        &mut self,
        send: impl FnOnce(Vec<u8>) -> Result<(), sonicterm_io::pty::PtyInputError>,
    ) -> Result<(), sonicterm_io::pty::PtyInputError> {
        use sonicterm_io::pty::PtyInputError;
        if self.len == 0 {
            // When: self.len is zero, do not consume a writer queue slot.
            return Ok(());
        }
        match send(self.take()) {
            Err(PtyInputError::QueueFull(bytes)) => {
                // QueueFull retains the latest position for a later non-rendering wake.
                self.replace(&bytes);
                Ok(())
            }
            result => result,
        }
    }

    fn send_ordered(
        &mut self,
        bytes: Vec<u8>,
        send: impl FnOnce(Vec<u8>) -> Result<(), sonicterm_io::pty::PtyInputError>,
    ) -> Result<(), sonicterm_io::pty::PtyInputError> {
        use sonicterm_io::pty::{pty_input_message_allowed, PtyInputError};
        let prefix_len = self.len;
        let pending = self.take();
        if prefix_len == 0 || !pty_input_message_allowed(prefix_len.saturating_add(bytes.len())) {
            // When: prefix_len is zero or the combined length exceeds the cap, give discrete input the queue slot.
            return send(bytes);
        }
        let mut combined = Vec::with_capacity(prefix_len + bytes.len());
        combined.extend_from_slice(&pending);
        combined.extend_from_slice(&bytes);
        send(combined).map_err(|error| {
            // Rejection attribution covers only discrete input; stale motion cannot replay after it.
            let strip = |mut rejected: Vec<u8>| {
                rejected.drain(..prefix_len);
                rejected
            };
            match error {
                PtyInputError::QueueFull(bytes) => PtyInputError::QueueFull(strip(bytes)),
                PtyInputError::WriterDisconnected(bytes) => {
                    PtyInputError::WriterDisconnected(strip(bytes))
                }
                PtyInputError::MessageTooLarge(bytes) => {
                    PtyInputError::MessageTooLarge(strip(bytes))
                }
            }
        })
    }
}

/// Producer-assigned input category retained without inspecting terminal bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyInputSource {
    /// Encoded physical or logical key event.
    Keyboard,
    /// Clipboard text, including bracketed-paste framing.
    Paste,
    /// Quoted paths dropped onto a terminal.
    FileDrop,
    /// Committed input-method text.
    Ime,
    /// Terminal mouse-button press or release.
    PointerButton,
    /// Terminal pointer movement, with or without a held button.
    PointerMotion,
    /// Mouse-wheel reports or translated arrow sequences.
    Wheel,
    /// Terminal focus-in or focus-out notification.
    FocusReport,
    /// Reply generated by the terminal parser.
    TerminalReply,
    /// Initial shell draft for an opened script.
    ScriptDraft,
    /// Input supplied directly through the backend-free intent API.
    StateMachine,
}

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

fn pty_input_rejected_event(
    pane_id: u64,
    source: PtyInputSource,
    error: sonicterm_io::pty::PtyInputError,
    diagnostics: sonicterm_io::pty::PtyInputDiagnostics,
) -> UserEvent {
    let reason = error.to_string();
    let rejected_bytes = error.into_bytes().len();
    UserEvent::PtyInputRejected { pane_id, source, rejected_bytes, reason, diagnostics }
}

/// Compatibility no-op; FontStack owns fallback discovery and this helper sends no events.
pub fn build_async_fallback_loader_for_proxy(_proxy: EventLoopProxy<UserEvent>) {}

/// Build the waker a GPU device calls after it stops accepting work.
///
/// It posts [`UserEvent::GpuDeviceStateChanged`]. The device calls it inline on
/// the thread that raised the error, at most once per transition. The callback
/// never blocks and takes no app, window, or renderer lock: it only tries the
/// proxy's private mutex. Only a call of this waker holds that mutex, so every
/// device stop posts at least one wake; a call skips its wake only while
/// another call is posting one, after the device has already stopped.
pub(crate) fn gpu_device_state_waker(
    proxy: EventLoopProxy<UserEvent>,
) -> sonicterm_gpu::device_errors::DeviceStateWaker {
    // Windows' proxy is `Send` but not `Sync`, and the waker must be both.
    let proxy = std::sync::Mutex::new(proxy);
    std::sync::Arc::new(move || {
        let Ok(guard) = proxy.try_lock() else {
            // When: `try_lock` fails, another call is posting a wake for this stopped device.
            return;
        };
        // `EventLoopClosed` means the app is shutting down and needs no wake.
        let _ = guard.send_event(UserEvent::GpuDeviceStateChanged);
    })
}

mod child_window;
pub use child_window::{
    apply_dpi_to_renderer_if_present, child_window_dpi_changed_handles_no_renderer,
    child_window_resized_handles_no_renderer, resize_renderer_and_panes_if_present,
};
mod command_events;
use command_events::{append_bounded_command_events, notify_command_done};
pub use command_events::{poll_command_events_for_child_window, poll_command_events_for_tab_state};
mod config_apply;
mod event_loop;
mod frame_pacing;
pub use frame_pacing::{
    effective_frame_period, should_defer_streaming_redraw, should_degrade_for_software_render,
    should_flush_pending_pty_redraw, software_render_frame_period,
};
mod gpu_recovery;
mod gpu_recovery_worker;
pub mod hovered_url;
pub mod invariants;
mod key_encoding;
mod keyboard_protocol;
pub use keyboard_protocol::HeldKey;
mod keymap_dispatch;
mod media;
pub mod memory_snapshot;
mod misc;
pub mod os_drag;
mod overlays;
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
#[cfg(windows)]
use privilege::force_refresh_window_tab_privileges;
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
mod shared_gpu;
mod spawn_pane;
mod tab_state;
pub use tab_state::{refresh_active_tab_title, TabState};
pub mod tab_transfer;
mod tear_out;
pub use tear_out::{PendingTearOut, TearOutTiming};
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

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
struct PendingForegroundProbe {
    /// Earliest instant at which the foreground process must be sampled again.
    due: Instant,
    /// Whether output activity is forbidden from postponing this deadline.
    fixed: bool,
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
    #[cfg(windows)]
    /// Bounded foreground-process sample armed by accepted input or quiet output.
    foreground_probe_wake: Option<PendingForegroundProbe>,
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
    /// `#[doc(hidden)]` rather than `#[cfg(test)]` because the test
    /// living in `tests/os_drag_cleanup.rs` is an INTEGRATION test
    /// that compiles the crate without `cfg(test)`.
    #[doc(hidden)]
    pub(super) test_post_snapshot_hook: Option<Box<dyn FnOnce(&mut App) + Send>>,
    /// Deferred app-exit request, set by a quit action or by a close that
    /// leaves no active terminal window. `do_about_to_wait` drains it by
    /// calling `el.exit()`; the flag exists because those paths have no
    /// `ActiveEventLoop` handle.
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
    /// (immediately after `el.create_window` succeeds, before the first
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
    /// Apply native capability policy and emit each resulting diagnostic once.
    fn normalize_config(normalizer: &ConfigNormalizer, config: Config) -> Config {
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
        // Seed the process-global tab width cap from config before any tab
        // bar is laid out, so a configured value takes effect on the very
        // first frame (hot-reload updates it later via apply_new_config).
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
                    tracing::warn!(%error, "path workers unavailable");
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
            os_drag_handoff_started: false,
            governor,
            windows: HashMap::new(),
            pty_reaper,
            session_finished: None,
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

    /// Test seam: queue a command event on a pane without running a shell.
    ///
    /// Lets a test drive command-status and badge behavior from synthetic
    /// events instead of waiting on real process transitions.
    #[doc(hidden)]
    pub fn __test_push_pane_command_event(
        &mut self,
        pane_id: u64,
        event: CommandEvent,
        at: Instant,
        duration: Option<Duration>,
    ) {
        if let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            pane.command_events.lock().push(PaneCommandEvent { event, at, duration });
        }
    }

    /// Test seam: the command status a tab currently reports.
    ///
    /// `None` when no main window or no tab sits at `tab_idx`.
    #[doc(hidden)]
    pub fn __test_command_status_for_tab(&self, tab_idx: usize) -> Option<CommandStatus> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.command.clone())
    }

    /// Test seam: the badge a tab would render at `now`.
    ///
    /// Badge text depends on whether the tab is the active one, so this
    /// resolves activeness the same way the tab bar does.
    #[doc(hidden)]
    pub fn __test_tab_badge(&self, tab_idx: usize, now: Instant) -> Option<&'static str> {
        let tabs = self.main_tabs()?;
        tabs.tabs()
            .get(tab_idx)
            .and_then(|tab| tab.command.clone().badge(now, tab_idx == tabs.active_index()))
    }
}

impl App {
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

    /// Test-only: `true` if `win_id` has a deferred redraw queued in
    /// [`Self::pending_redraw_windows`] (the child-window coalescing latch).
    #[doc(hidden)]
    pub fn __test_child_redraw_deferred(&self, win_id: WindowId) -> bool {
        self.pending_redraw_windows.contains(&win_id)
    }

    /// Test-only: report whether any live window has unconsumed owner-local input.
    #[doc(hidden)]
    pub fn __test_input_dirty(&self) -> bool {
        self.windows.values().any(|window| window.redraw.input_pending())
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

    /// Translate a UI message id. See [`sonicterm_ui::i18n::I18n::t`]. Returns
    /// the key itself if no bundle (active or English fallback) has it,
    /// so the UI never renders an empty label.
    pub fn t(&self, key: &str) -> String {
        self.i18n.t(key)
    }

    /// Translate with `{ $name }` arguments. See
    /// [`sonicterm_ui::i18n::I18n::t_args`].
    pub fn t_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.i18n.t_args(key, Some(args))
    }

    /// Currently active locale tag (e.g. `"en"`, `"zh-CN"`).
    pub fn locale(&self) -> String {
        self.i18n.locale()
    }

    /// Live-apply a new locale. Persists the choice to `self.config.locale`.
    /// Pass `""` to mean "auto-detect from OS locale".
    pub fn set_locale(&mut self, requested: &str) {
        self.palette_pointer_capture = None;
        self.config.locale = requested.to_string();
        self.i18n = sonicterm_ui::i18n::I18n::new(if requested.is_empty() {
            None
        } else {
            // When: `requested` names a locale tag, so it selects the bundle
            // directly instead of leaving the OS default to decide.
            Some(requested)
        });
        self.command_palette.set_locale(&self.i18n);
        if self.command_palette.is_open() {
            // Locale changes do not advance grid revisions, so wake the window hosting the palette.
            self.request_redraw_for_overlay(self.palette_attached_window);
        }
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
            self.main_tabs().map(|t| t.len()).unwrap_or(0),
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
    /// This is the `ActiveEventLoop`-free counterpart to `el.exit()` for
    /// keymap/tab-close paths; `do_about_to_wait` drains the flag. OS window
    /// close handlers with an event-loop handle may still call `el.exit()`
    /// directly after this predicate becomes true.
    pub(super) fn request_exit_if_no_active_windows(&mut self) {
        if self.should_exit() {
            self.pending_exit = true;
        }
    }

    /// is the main window currently hidden / drained?
    /// `true` when the main `WindowState` is gone OR its `hidden` latch
    /// is set. The two shapes mean the same thing operationally — no
    /// visible main — so callers don't need to discriminate.
    #[doc(hidden)]
    pub fn main_is_hidden(&self) -> bool {
        match self.main() {
            Some(ws) => ws.hidden,
            None => true,
        }
    }

    /// Test-only: read the main window's `hidden` latch via the unified
    /// accessor.
    #[doc(hidden)]
    pub fn __test_main_hidden(&self) -> bool {
        self.main_is_hidden()
    }

    /// Test-only: drive the production `hide_main_window` path from
    /// integration tests (the helper itself is `pub(super)`).
    #[doc(hidden)]
    pub fn __test_hide_main_window(&mut self) {
        self.hide_main_window();
    }

    /// Test-only: read the deferred-exit flag, which a quit action or a close
    /// that leaves no active terminal window sets.
    #[doc(hidden)]
    pub fn __test_pending_exit(&self) -> bool {
        self.pending_exit
    }

    /// Unified "did this close just empty the affected window?" check
    /// for the keymap path. Mirrors what the mouse-click close-button
    /// path in `window_event.rs` and the OS `CloseRequested` arm do —
    /// hide the main window (or exit, on the last window) when its
    /// tabs vec is empty, and reap child windows the same way the drag-
    /// merge path does. The flag set here is drained in
    /// `do_about_to_wait`.
    pub(super) fn reap_empty_main_window_after_close(&mut self) {
        if !self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) {
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

    /// Test-only: force-set the main window's `hidden` latch so
    /// post-merge drain-policy tests can simulate the "main already
    /// retired" state without driving a real winit close event.
    #[doc(hidden)]
    pub fn __test_set_main_hidden(&mut self, v: bool) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.hidden = v;
        }
    }

    fn active_pane_id(&self) -> Option<u64> {
        self.main_active_pane_id()
    }

    fn main_active_pane_id(&self) -> Option<u64> {
        let ws = self.main()?;
        let i = ws.tabs.active_index();
        ws.tab_states.get(i).map(|t| t.active_pane)
    }

    fn active_pane_id_for_kind(&self, kind: FrontmostKind) -> Option<u64> {
        match kind {
            FrontmostKind::Child(id) => {
                let ws = self.windows.get(&id)?;
                let i = ws.tabs.active_index();
                ws.tab_states.get(i).map(|t| t.active_pane)
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                self.main_active_pane_id()
            }
        }
    }

    fn active_pane(&self) -> Option<&PaneState> {
        let id = self.active_pane_id()?;
        self.pane_by_id(id)
    }

    fn pane_by_id(&self, pane_id: u64) -> Option<&PaneState> {
        self.windows.values().find_map(|ws| ws.panes.get(&pane_id))
    }

    /// Admit a new user gesture unless the pane's owning window is READONLY.
    fn admits_new_user_input(&self, pane_id: u64) -> bool {
        !self.windows.values().any(|window| {
            window.panes.contains_key(&pane_id)
                && window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only)
        })
    }

    fn request_redraw_all_terminal_windows(&self) {
        for (id, ws) in &self.windows {
            if Some(*id) == self.main_window_id {
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            } else {
                // When: `id` is not `main_window_id`, so the redraw is requested
                // on the torn-out child's own surface rather than main's.
                ws.request_redraw();
            }
        }
    }

    fn terminal_key_targets(&self, source_pane: u64) -> BTreeSet<u64> {
        let mut targets = BTreeSet::from([source_pane]);
        if matches!(
            self.broadcast,
            BroadcastState::On {
                source_pane: broadcast_source,
                ..
            } if broadcast_source == source_pane
        ) {
            // A matching broadcast source includes
            // every receiver so each can negotiate its own keyboard encoding.
            targets.extend(self.broadcast_receivers());
        }
        targets
    }

    // Ordering: keyboard_input is a self-contained Relaxed snapshot; encoding and ownership share this exact loaded word.
    fn encoded_terminal_key_writes(
        &self,
        event: &winit::event::KeyEvent,
        modifiers: ModifiersState,
        targets: &BTreeSet<u64>,
        mut previous: Option<&mut keyboard_protocol::KeyRoutes>,
        synthetic: bool,
    ) -> Vec<(u64, keyboard_protocol::EncodedKey)> {
        use keyboard_protocol::{encode_routed_key, KeyboardSnapshot};
        let native = key_encoding::native_key_event(event);
        targets
            .iter()
            .filter_map(|pane_id| {
                let (window_id, pane) = self
                    .windows
                    .iter()
                    .find_map(|(id, window)| window.panes.get(pane_id).map(|pane| (*id, pane)))?;
                let state =
                    KeyboardSnapshot::from_bits(pane.keyboard_input.load(Ordering::Relaxed));
                let held = previous.as_ref().and_then(|routes| routes.get(pane_id).copied());
                if held.is_some_and(|held| !held.compatible(state, cfg!(windows))) {
                    // When: a held route crosses a protocol boundary, remove it permanently until a fresh press.
                    if let Some(routes) = previous.as_mut() {
                        routes.remove(pane_id);
                    }
                    return None;
                }
                if let Some(reason) =
                    state.refusal_reason(cfg!(windows), native.is_some(), synthetic)
                {
                    // When: native input is unavailable, report only destination metadata and the refusal reason.
                    tracing::warn!(?window_id, pane_id, reason, "native keyboard input refused");
                }
                encode_routed_key(
                    state,
                    cfg!(windows),
                    native,
                    synthetic,
                    event.repeat,
                    held,
                    || {
                        key_encoding::encode_key(
                            event,
                            modifiers,
                            state.kitty_flags(),
                            state.modes(),
                            self.config.terminal.keypad_mode,
                        )
                    },
                )
                .map(|encoded| (*pane_id, encoded))
            })
            .collect()
    }

    fn dispatch_terminal_key_writes(
        &mut self,
        writes: Vec<(u64, keyboard_protocol::EncodedKey)>,
    ) -> keyboard_protocol::KeyRoutes {
        keyboard_protocol::dispatch_key_writes(writes, |pane_id, bytes| {
            self.write_to_pane(pane_id, bytes, PtyInputSource::Keyboard)
        })
    }

    // Ordering: keyboard_input loads Relaxed; cleanup validates one complete epoch without locking parser output.
    fn release_window_native_keys(&mut self, window_id: WindowId) {
        use keyboard_protocol::KeyboardSnapshot;
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: window_id is no longer live, no accepted key ownership remains to drain.
            return;
        };
        let pressed = std::mem::take(&mut window.pty_pressed_keys);
        let ordered: std::collections::BTreeMap<_, _> = pressed.into_iter().collect();
        let mut writes = std::collections::BTreeMap::<u64, Vec<u8>>::new();
        for (pane_id, held) in ordered.into_values().flat_map(|routes| routes.into_iter()) {
            let Some(pane) = self.pane_by_id(pane_id) else {
                // When: pane_id has closed, no live input queue can receive its cleanup.
                continue;
            };
            let snapshot = KeyboardSnapshot::from_bits(pane.keyboard_input.load(Ordering::Relaxed));
            if let Some(bytes) = held.focus_release(snapshot, cfg!(windows)) {
                // When: focus_release returns bytes, combine this pane's releases into one queue admission.
                writes.entry(pane_id).or_default().extend(bytes);
            }
        }
        for (pane_id, bytes) in writes {
            tracing::debug!(
                ?window_id,
                pane_id,
                synthetic_cleanup = true,
                "native keyboard focus release"
            );
            self.write_to_pane(pane_id, bytes, PtyInputSource::Keyboard);
        }
    }

    /// Test-only mirror of the normal KeyboardInput dispatch order: try every
    /// keymap spelling before encoding bytes for PTY forwarding.
    #[doc(hidden)]
    pub fn __test_dispatch_key_or_encode_pty(
        &mut self,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
    ) -> (Option<Action>, Option<Vec<u8>>) {
        self.__test_dispatch_key_or_encode_pty_with_drain(key, mods, false)
    }

    /// Test-only mirror of the child-window KeyboardInput action path.
    /// The production child handler drains `pending_new_window` immediately
    /// after `run_action`; this helper exposes the same post-dispatch state
    /// without requiring a live `ActiveEventLoop`.
    // Ordering: keyboard_input loads Relaxed as one self-contained modes, Kitty, and epoch snapshot.
    #[doc(hidden)]
    pub fn __test_dispatch_key_or_encode_pty_with_drain(
        &mut self,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
        simulate_drain: bool,
    ) -> (Option<Action>, Option<Vec<u8>>) {
        for key_str in key_to_strings(key, mods) {
            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                // When: `keymap` resolves `key_str` to an action, so binding
                // dispatch is tried before falling back to PTY byte encoding.
                if keymap_dispatch::terminal_input_passthrough_binding(&key_str, &action) {
                    // When: this `action` is a passthrough binding, so the key
                    // belongs to the terminal and the next spelling is tried.
                    continue;
                }
                if self.run_action(&action) {
                    // When: `run_action` consumed the chord, so the caller gets
                    // the action and no encoded bytes reach the PTY.
                    if simulate_drain {
                        self.pending_new_window = None;
                    }
                    return (Some(action), None);
                }
            }
        }
        let snapshot = keyboard_protocol::KeyboardSnapshot::from_bits(
            self.active_pane().map(|pane| pane.keyboard_input.load(Ordering::Relaxed)).unwrap_or(0),
        );
        (None, encode_logical_with_modes(key, mods, snapshot.kitty_flags(), snapshot.modes()))
    }

    fn write_to_pane(&mut self, pane_id: u64, bytes: Vec<u8>, source: PtyInputSource) -> bool {
        // Test-only ledger: skipped entirely in production so we don't
        // lock+clone+push on every PTY write (— unbounded
        // growth + per-keystroke overhead over a long session).
        if self.pty_write_log_enabled {
            self.test_pty_writes.lock().push((pane_id, bytes.clone()));
        }
        let Some(p) = self.windows.values_mut().find_map(|window| window.panes.get_mut(&pane_id))
        else {
            // When: find_map cannot resolve pane_id, its input has no live destination.
            return false;
        };
        let queued =
            Self::queue_pane_input(self.event_loop_proxy.as_ref(), p, pane_id, source, bytes);
        #[cfg(windows)]
        if queued && source != PtyInputSource::PointerMotion {
            // Accepted discrete input can launch a silent command; coalesced motion must not schedule process probes.
            self.arm_foreground_probe_after_input(Instant::now());
        }
        queued
    }

    /// Deliver an explicitly targeted PTY effect through the same bounded queue as native input.
    pub(crate) fn dispatch_pty_write_effect(
        &mut self,
        effect: &sonicterm_app_core::AppEffect,
        source: PtyInputSource,
    ) -> bool {
        let sonicterm_app_core::AppEffect::PtyWrite { pane, data } = effect else {
            // When: the effect is not a PTY write, this boundary cannot accept its work.
            return false;
        };
        self.write_to_pane(pane.0, data.to_vec(), source)
    }

    fn queue_pane_input(
        proxy: Option<&EventLoopProxy<UserEvent>>,
        pane: &mut PaneState,
        pane_id: u64,
        source: PtyInputSource,
        bytes: Vec<u8>,
    ) -> bool {
        let profile =
            if source == PtyInputSource::PointerMotion || pane.pending_pointer_motion.len != 0 {
                // Profile checks never block discrete input behind parser output.
                pane.parser.try_lock().map(|parser| {
                    (parser.mouse_tracking(), parser.mouse_sgr_enabled(), parser.grid().is_alt())
                })
            } else {
                // When: source is discrete and no motion is pending, the parser is irrelevant to admission.
                None
            };
        pane.pending_pointer_motion.validate_profile(profile);
        let Some(pty) = pane.pty.as_ref() else {
            // When: a pane has no writer, input cannot acquire delivery ownership.
            return false;
        };
        if source == PtyInputSource::PointerMotion {
            // When: source is PointerMotion, coalesce the turn's positions in fixed pane-owned storage.
            if profile.is_some_and(|current| {
                matches!(current.0, MouseTracking::Off | MouseTracking::Button)
            }) {
                // When: profile disables motion, do not retain a stale native report.
                return false;
            }
            pane.pending_pointer_motion.replace(&bytes);
            return true;
        }
        if profile.is_none() {
            // An unknown profile at a discrete barrier supersedes motion rather than delaying the key or replaying stale bytes.
            pane.pending_pointer_motion.len = 0;
        }
        match pane.pending_pointer_motion.send_ordered(bytes, |bytes| {
            #[cfg(test)]
            let submitted = mod_tests::submission_snapshot(&bytes);
            pty.send_input_nonblocking(bytes)?;
            #[cfg(test)]
            if let Some(bytes) = submitted {
                // For a scoped test, record only the bytes the PTY accepted.
                mod_tests::record_submission(pane_id, bytes);
            }
            Ok(())
        }) {
            Ok(()) => true,
            Err(error) => {
                // Refused discrete input preserves attribution without retaining its payload.
                Self::report_pty_input_rejection(
                    proxy,
                    pane_id,
                    source,
                    error,
                    pty.input_diagnostics(),
                );
                false
            }
        }
    }

    fn flush_pointer_motion(&mut self, now: Instant) -> Option<Instant> {
        let mut pending = false;
        for window in self.windows.values_mut() {
            for (&pane_id, pane) in &mut window.panes {
                if pane.pending_pointer_motion.len == 0 {
                    // When: pending_pointer_motion.len is zero, idle panes need no parser or writer work.
                    continue;
                }
                let profile = pane.parser.try_lock().map(|parser| {
                    (parser.mouse_tracking(), parser.mouse_sgr_enabled(), parser.grid().is_alt())
                });
                pane.pending_pointer_motion.validate_profile(profile);
                let Some(pty) = pane.pty.as_ref() else {
                    // When: a pane loses its PTY, discard its unsendable position without arming a timer.
                    pane.pending_pointer_motion.len = 0;
                    continue;
                };
                if profile.is_none() {
                    // When: profile is unavailable, retry without dropping the latest position or writing unvalidated bytes.
                    pending = true;
                    continue;
                }
                if let Err(error) = pane.pending_pointer_motion.flush(|bytes| {
                    #[cfg(test)]
                    let submitted = mod_tests::submission_snapshot(&bytes);
                    pty.send_input_nonblocking(bytes)?;
                    #[cfg(test)]
                    if let Some(bytes) = submitted {
                        // For a scoped test, a successful flush is real queue admission.
                        mod_tests::record_submission(pane_id, bytes);
                    }
                    Ok(())
                }) {
                    // When: flush returns error, the disconnected writer reports once because the pending slot is consumed.
                    Self::report_pty_input_rejection(
                        self.event_loop_proxy.as_ref(),
                        pane_id,
                        PtyInputSource::PointerMotion,
                        error,
                        pty.input_diagnostics(),
                    );
                }
                pending |= pane.pending_pointer_motion.len != 0;
            }
        }
        pending.then_some(now + Duration::from_millis(10))
    }

    fn queue_pty_input(
        proxy: Option<&EventLoopProxy<UserEvent>>,
        pty: &sonicterm_io::pty::PtyHandle,
        pane_id: u64,
        source: PtyInputSource,
        bytes: Vec<u8>,
    ) -> bool {
        #[cfg(test)]
        let submitted = mod_tests::submission_snapshot(&bytes);
        if let Err(error) = pty.send_input_nonblocking(bytes) {
            // When: `send_input_nonblocking` refuses input, report metadata rather than retaining or replaying the payload.
            Self::report_pty_input_rejection(
                proxy,
                pane_id,
                source,
                error,
                pty.input_diagnostics(),
            );
            return false;
        }
        #[cfg(test)]
        if let Some(bytes) = submitted {
            // For a scoped test, observe the standalone write only after admission.
            mod_tests::record_submission(pane_id, bytes);
        }
        true
    }

    fn report_pty_input_rejection(
        proxy: Option<&EventLoopProxy<UserEvent>>,
        pane_id: u64,
        source: PtyInputSource,
        error: sonicterm_io::pty::PtyInputError,
        diagnostics: sonicterm_io::pty::PtyInputDiagnostics,
    ) {
        let event = pty_input_rejected_event(pane_id, source, error, diagnostics);
        Self::deliver_pty_input_rejection(
            proxy.map(|proxy| |event| proxy.send_event(event).map_err(|closed| closed.0)),
            event,
        );
    }

    fn deliver_pty_input_rejection(
        send_event: Option<impl FnOnce(UserEvent) -> Result<(), UserEvent>>,
        event: UserEvent,
    ) {
        let event = match send_event {
            Some(send_event) => {
                // When: `send_event` exists, defer logging only after the event loop accepts ownership.
                match send_event(event) {
                    Ok(()) => {
                        // When: `send_event` succeeds, the event loop owns logging and current-window attribution.
                        return;
                    }
                    Err(event) => event,
                }
            }
            None => event,
        };
        if let UserEvent::PtyInputRejected {
            pane_id,
            source,
            rejected_bytes,
            reason,
            diagnostics,
        } = event
        {
            // Delivery failure preserves overload evidence without payload or stale window attribution.
            tracing::warn!(
                pane_id,
                window_id = ?None::<WindowId>,
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
                "terminal input was not queued"
            );
        }
    }

    /// Execute explicit effects at live boundaries; observational variants only emit diagnostics.
    pub(crate) fn dispatch_effects(
        &mut self,
        effects: smallvec::SmallVec<[sonicterm_app_core::AppEffect; 4]>,
    ) {
        use sonicterm_app_core::AppEffect;
        for effect in effects {
            match effect {
                AppEffect::PtyWrite { .. } => {
                    self.dispatch_pty_write_effect(&effect, PtyInputSource::StateMachine);
                }
                AppEffect::ClipboardSet { text } => {
                    // When: the effect is `ClipboardSet`, so nonempty `text` is
                    // written and empty text stays a no-op contract sentinel.
                    if !text.is_empty() {
                        // When: `text` carries a payload, so it replaces the
                        // clipboard; empty text would clear what the user copied.
                        if let Some(cb) = self.clipboard.as_mut() {
                            // When: a `cb` handle exists, so the write is
                            // attempted and a backend refusal is not fatal here.
                            let _ = cb.set_text(text);
                        }
                    }
                    // Empty text sentinel for CopySelection:
                    // the boundary's existing `copy_selection` already
                    // resolved the selection; the sentinel exists so
                    // the Intent→Effect contract is observable in
                    // tests, and carries no text payload.
                }
                AppEffect::OpenURL { url } => {
                    if let Err(error) = open_url_effect(&url) {
                        tracing::warn!(%error, "failed to open URL effect");
                    }
                }
                AppEffect::Quit => {
                    self.pending_exit = true;
                }
                AppEffect::Render { window, .. } | AppEffect::RenderDirtyRect { window, .. } => {
                    self.request_effect_redraw(window);
                }
                // ── PTY class ─────────────────────────────────────────
                //
                // PtyClose: the per-pane `PtyHandle::Drop` impl already
                // SIGKILLs the child (CLAUDE.md §4 land-mine). Removing
                // the pane entry from `WindowState.panes` is what
                // actually triggers the drop. We try the main window
                // first; if not found, scan child windows.
                AppEffect::PtyClose { pane } => {
                    let pane_id = pane.0;
                    let closed = self.close_pty_pane(pane_id);
                    tracing::debug!(target: "state_machine", pane = pane_id, closed, "dispatch_effects: PtyClose");
                }
                // ChildExitPropagate: observability — the renderer's
                // poll loop already noticed the child exit and updated
                // the per-pane status. Surface a structured log so the
                // session-restore layer (post-v1.0) can correlate.
                AppEffect::ChildExitPropagate { pane, status } => {
                    tracing::info!(target: "state_machine", pane = pane.0, status, "child exit observed");
                }
                // ChildSpawn: record-only at the boundary. Production
                // pane spawning flows through `App::spawn_pane` /
                // `spawn_tab_in_child`, which constructs the PTY
                // directly; the effect here is the observable contract.
                AppEffect::ChildSpawn { pane, argv0 } => {
                    tracing::debug!(target: "state_machine", pane = pane.0, %argv0, "dispatch_effects: ChildSpawn (record-only)");
                }
                // ── OS drag class ────────────────────────────────────
                //
                // The actual platform OS drag is initiated by the
                // tear-out / tab-drag path which talks directly to the
                // platform backend (NSPasteboard / OLE). The reducer
                // emits OsDragStart for observability + future
                // session-restore.
                AppEffect::OsDragStart { src_window, payload_tab } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        tab = payload_tab,
                        "dispatch_effects: OsDragStart (platform path owns the actual drag)"
                    );
                }
                // Native drag owns settlement; this observation must not commit the transfer a second time.
                AppEffect::OsDragEnd { src_window, committed } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        committed,
                        "dispatch_effects: OsDragEnd (observation-only)"
                    );
                }
                // ── Clipboard / notification side channels ───────────
                //
                // ClipboardRequest: async paste handshake. The actual
                // read happens through `clipboard.get_text()` at the
                // boundary's paste path; here we surface the request.
                AppEffect::ClipboardRequest { window, bracketed } => {
                    tracing::debug!(target: "state_machine", window = window.0, bracketed,
                        "dispatch_effects: ClipboardRequest (observation-only; native paste owns clipboard reads)");
                }
                // Notification: route through the existing
                // `notify_command_done` path (test capture friendly).
                AppEffect::Notification { title, body } => {
                    // When: the effect is `Notification`, so `title` and `body`
                    // are joined into the one line the notifier accepts.
                    let combined = if title.is_empty() { body } else { format!("{title}: {body}") };
                    notify_command_done(combined);
                }
                // ── Window ops ───────────────────────────────────────
                //
                // WindowOpen: defer to the existing pending-new-window
                // flag drained by event_loop on the next tick. The
                // platform-creation requires `&ActiveEventLoop` which
                // dispatch_effects doesn't carry — flagging keeps the
                // request observable without changing the dispatcher
                // signature.
                AppEffect::WindowOpen { role, initial_size } => {
                    self.pending_new_window = Some(self.window_request(None));
                    tracing::debug!(
                        target: "state_machine",
                        ?role,
                        ?initial_size,
                        "dispatch_effects: WindowOpen queued (drained by event_loop)"
                    );
                }
                // WindowClose is observational; only live native topology decides close and last-window exit.
                AppEffect::WindowClose { window } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        "dispatch_effects: WindowClose (platform path closes via WindowEvent::CloseRequested)"
                    );
                }
                // WindowResize: programmatic resize. winit's
                // `set_inner_size` is the API; since `LogicalSize` here
                // is f64 cells (not pixels) per the reducer's contract,
                // emit a redraw so the boundary re-measures.
                AppEffect::WindowResize { window, size } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        w = size.width,
                        h = size.height,
                        "dispatch_effects: WindowResize (observability)"
                    );
                    self.request_effect_redraw(window);
                }
                // WindowMove: record-only; OS already moved the window.
                AppEffect::WindowMove { window, pos } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        x = pos.x,
                        y = pos.y,
                        "dispatch_effects: WindowMove (record-only)"
                    );
                }
                // WindowSetTitle is observational; only explicit window naming may change native titles.
                AppEffect::WindowSetTitle { window, title } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        %title,
                        "dispatch_effects: WindowSetTitle (observation-only)"
                    );
                }
                // TimerSchedule / TimerCancel: record-only. No reducer path
                // emits them; redraw pacing sets winit's
                // `ControlFlow::WaitUntil` directly.
                AppEffect::TimerSchedule { id, at } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        ?at,
                        "dispatch_effects: TimerSchedule (record-only — winit ControlFlow drives pacing)"
                    );
                }
                AppEffect::TimerCancel { id } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        "dispatch_effects: TimerCancel (record-only)"
                    );
                }
                // ── Menubar ──────────────────────────────────────────
                //
                // MenubarUpdate: log-only on every platform. The macOS and
                // Windows menubars are built from `menu::blueprint`, and
                // `menubar_bridge` carries only menu clicks back to the app.
                AppEffect::MenubarUpdate(model) => {
                    tracing::debug!(
                        target: "state_machine",
                        items = model.items.len(),
                        "dispatch_effects: MenubarUpdate (platform path owns NSMenu/muda mutation)"
                    );
                }
                // ── Log ──────────────────────────────────────────────
                //
                // LogEvent: forward to tracing at the requested level.
                AppEffect::LogEvent { level, target, msg } => {
                    use sonicterm_app_core::LogLevel;
                    // `target` is &'static str from the reducer but
                    // tracing's `target:` slot needs a literal at the
                    // call site, so capture both as fields instead.
                    match level {
                        LogLevel::Trace => {
                            tracing::trace!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Debug => {
                            tracing::debug!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Info => {
                            tracing::info!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Warn => {
                            tracing::warn!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Error => {
                            tracing::error!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                    }
                }
                // `AppEffect` is #[non_exhaustive]; future variants
                // surface here as an unrouted log until wired.
                _ => {
                    tracing::trace!(target: "state_machine", "dispatch_effects: unrouted effect {:?}", effect);
                }
            }
        }
    }

    fn close_pty_pane(&mut self, pane_id: u64) -> bool {
        let mut retired = None;
        let mut resize_main = false;
        let mut redraw_main = false;

        if let Some(ws) = self.main_mut() {
            // When: `main_mut` resolves a window, so its tabs are searched for
            // the pane before any child window is considered.
            let active_tab = ws.tabs.active_index();
            for (tab_idx, st) in ws.tab_states.iter_mut().enumerate() {
                let leaves = st.tree.leaves();
                if !leaves.contains(&pane_id) {
                    // When: this tab's `leaves` exclude `pane_id`, so its split
                    // tree does not hold the pane being closed.
                    continue;
                }
                if leaves.len() > 1 && st.tree.close(pane_id) {
                    if st.active_pane == pane_id {
                        st.active_pane =
                            leaves.into_iter().find(|id| *id != pane_id).unwrap_or(st.active_pane);
                        // The search was scanning the grid that just went
                        // away. Its matches, their coordinates, and the
                        // revision it recorded all describe that grid.
                        if let Some(search) = st.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                    }
                    if tab_idx == active_tab {
                        resize_main = true;
                        redraw_main = true;
                    }
                }
                break;
            }
            retired = ws.remove_pane(pane_id);
        }

        if resize_main {
            self.resize_visible_panes();
        }
        if redraw_main {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
        if let Some(pane) = retired {
            // When: retired holds the main pane, transfer its PTY before returning without scanning child windows.
            self.retire_pane(pane);
            return true;
        }

        for ws in self.windows.values_mut() {
            let mut resize_child = false;
            let mut redraw_child = false;
            let active_tab = ws.tabs.active_index();
            for (tab_idx, st) in ws.tab_states.iter_mut().enumerate() {
                let leaves = st.tree.leaves();
                if !leaves.contains(&pane_id) {
                    // When: this tab's `leaves` exclude `pane_id`, so this child's
                    // split tree does not hold the pane being closed.
                    continue;
                }
                if leaves.len() > 1 && st.tree.close(pane_id) {
                    if st.active_pane == pane_id {
                        st.active_pane =
                            leaves.into_iter().find(|id| *id != pane_id).unwrap_or(st.active_pane);
                        // The search was scanning the grid that just went
                        // away. Its matches, their coordinates, and the
                        // revision it recorded all describe that grid.
                        if let Some(search) = st.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                    }
                    if tab_idx == active_tab {
                        resize_child = true;
                        redraw_child = true;
                    }
                }
                break;
            }
            if let Some(pane) = ws.remove_pane(pane_id) {
                // When: remove_pane returns custody, finish child layout before ending the window borrow and retiring its PTY.
                if resize_child {
                    child_window::resize_visible_panes_in_child(ws);
                }
                if redraw_child {
                    ws.request_redraw();
                }
                retired = Some(pane);
                break;
            }
        }

        if let Some(pane) = retired {
            self.retire_pane(pane);
            true
        } else {
            // When: retired is empty, no window owned the requested pane and no native teardown was submitted.
            false
        }
    }

    /// Resolve a live window to its stable backend-free key; absent windows have no key.
    pub fn window_key(&self, id: WindowId) -> Option<sonicterm_types::WindowKey> {
        self.windows.contains_key(&id).then(|| self.window_keys.get(id)).flatten()
    }

    fn request_effect_redraw(&self, key: sonicterm_types::WindowKey) -> bool {
        let Some(window) = self.window_keys.resolve(key).and_then(|id| self.windows.get(&id))
        else {
            // When: `key` names no live window, never redirect operational work to main or frontmost.
            return false;
        };
        window.request_redraw();
        self.redraw_request_count.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn observe_intent(&mut self, intent: sonicterm_app_core::AppIntent) {
        // Compatibility reducer state is observational; its synthetic effects never mutate live topology.
        let _ = self.machine.handle(intent);
    }

    /// Execute supported explicit-target work; retain lifecycle reports as non-operational compatibility observations.
    pub fn dispatch_intent(&mut self, intent: sonicterm_app_core::AppIntent) {
        use sonicterm_app_core::{AppEffect, AppIntent};
        match intent {
            AppIntent::PtyWrite { pane, bytes } => {
                self.write_to_pane(pane.0, bytes.to_vec(), PtyInputSource::StateMachine);
            }
            AppIntent::PtyExit { pane, status } => {
                self.dispatch_effects(smallvec::smallvec![
                    AppEffect::ChildExitPropagate { pane, status },
                    AppEffect::PtyClose { pane }
                ]);
            }
            AppIntent::PtyBurst { pane, .. } | AppIntent::ForegroundProcChanged { pane, .. } => {
                if let Some(id) = self
                    .windows
                    .iter()
                    .find_map(|(id, window)| window.panes.contains_key(&pane.0).then_some(*id))
                {
                    if let Some(key) = self.window_key(id) {
                        self.request_effect_redraw(key);
                    }
                }
            }
            AppIntent::RedrawRequested { window } => {
                self.request_effect_redraw(window);
            }
            AppIntent::Key { window, pressed: true, .. }
            | AppIntent::ImeStart { window }
            | AppIntent::ImeEnd { window }
            | AppIntent::ImePreedit { window, .. }
            | AppIntent::HoverUrl { window, .. }
            | AppIntent::ScrollUp { window, .. }
            | AppIntent::ScrollDown { window, .. }
            | AppIntent::ScrollPageUp { window }
            | AppIntent::ScrollPageDown { window }
            | AppIntent::ScrollToTop { window }
            | AppIntent::ScrollToBottom { window }
            | AppIntent::ScrollToCursor { window }
            | AppIntent::MouseWheel { window, .. } => {
                self.request_effect_redraw(window);
            }
            AppIntent::ImeCommit { window, text } | AppIntent::Paste { window, text, .. } => {
                if let Some(id) = self.window_keys.resolve(window) {
                    let pane = self
                        .windows
                        .get(&id)
                        .and_then(|state| state.tab_states.get(state.tabs.active_index()))
                        .map(|tab| tab.active_pane);
                    if let Some(pane) = pane.filter(|pane| self.admits_new_user_input(*pane)) {
                        self.write_to_pane(pane, text.into_bytes(), PtyInputSource::StateMachine);
                    }
                }
            }
            AppIntent::ClickUrl { url, .. } => {
                self.dispatch_effects(smallvec::smallvec![AppEffect::OpenURL { url }])
            }
            AppIntent::Exit => self.pending_exit = true,
            other => self.observe_intent(other),
        }
    }

    fn broadcast_from(&mut self, active_id: u64, bytes: Vec<u8>, source: PtyInputSource) {
        let BroadcastState::On { source_pane, .. } = self.broadcast else {
            // When: `broadcast` is not `On`, so there is no fan-out group and the
            // bytes belong to the focused pane alone.
            return;
        };
        if active_id != source_pane {
            // When: `active_id` is not the `source_pane` that armed the
            // broadcast, so typing here must not fan out to the group.
            return;
        }
        let receivers = self.broadcast_receivers();
        for pane_id in receivers {
            self.write_to_pane(pane_id, bytes.clone(), source);
        }
    }

    pub(crate) fn broadcast_receivers(&self) -> std::collections::BTreeSet<u64> {
        let BroadcastState::On { scope, source_pane } = self.broadcast else {
            // When: `broadcast` is not `On`, so no `scope` or `source_pane`
            // defines a group and the receiver set is empty.
            return Default::default();
        };
        self.broadcast_receivers_for(scope, source_pane)
    }

    /// Return render-only participants, including the live source; never use this set for PTY fan-out.
    pub(crate) fn broadcast_participants(&self) -> std::collections::BTreeSet<u64> {
        let BroadcastState::On { source_pane, .. } = self.broadcast else {
            // When: broadcast is Off, no pane needs safety chrome.
            return Default::default();
        };
        if self.pane_by_id(source_pane).is_none() {
            // When: source_pane is gone, mirrored input is inert even if other tabs survive.
            return Default::default();
        }
        let mut participants = self.broadcast_receivers();
        participants.insert(source_pane);
        participants
    }

    fn clear_closed_broadcast_source(&mut self) {
        if let BroadcastState::On { source_pane, .. } = self.broadcast {
            if self.pane_by_id(source_pane).is_none() {
                // A closed source needs its safety chrome erased in every surviving window before sleeping.
                self.broadcast = BroadcastState::Off;
                self.request_redraw_all_terminal_windows();
            }
        }
    }

    fn broadcast_receivers_for(
        &self,
        scope: BroadcastScope,
        source_pane: u64,
    ) -> std::collections::BTreeSet<u64> {
        let mut receivers = std::collections::BTreeSet::new();
        for ws in self.windows.values() {
            match scope {
                BroadcastScope::Tab => {
                    // When: `scope` is `Tab`, so only panes sharing the source's
                    // own tab receive the fan-out.
                    if let Some((tab_idx, _)) = ws
                        .tab_states
                        .iter()
                        .enumerate()
                        .find(|(_, tab)| tab.tree.leaves().contains(&source_pane))
                    {
                        // When: a tab's `leaves` hold `source_pane`, so that tab's
                        // panes are the receiver set for this window.
                        receivers.extend(sonicterm_ui::broadcast::receiving_panes(
                            &ws.tab_states,
                            scope,
                            source_pane,
                            tab_idx,
                        ));
                        break;
                    }
                }
                BroadcastScope::AllTabs => {
                    receivers.extend(sonicterm_ui::broadcast::receiving_panes(
                        &ws.tab_states,
                        scope,
                        source_pane,
                        ws.tabs.active_index(),
                    ));
                }
            }
        }
        receivers.retain(|pane_id| self.admits_new_user_input(*pane_id));
        receivers
    }

    /// Test-only: active broadcast source pane, if broadcast is enabled.
    #[doc(hidden)]
    pub fn __test_broadcast_source(&self) -> Option<u64> {
        match self.broadcast {
            BroadcastState::On { source_pane, .. } => Some(source_pane),
            BroadcastState::Off => None,
        }
    }

    /// Test-only: receiver panes under the current broadcast state.
    #[doc(hidden)]
    pub fn __test_broadcast_receivers(&self) -> std::collections::BTreeSet<u64> {
        self.broadcast_receivers()
    }

    /// Test-only: clear the PTY write ledger before a broadcast assertion.
    #[doc(hidden)]
    pub fn __test_enable_pty_write_log(&mut self) {
        self.pty_write_log_enabled = true;
        self.test_pty_writes.lock().clear();
    }

    /// Test-only: snapshot logged `(pane_id, bytes)` PTY writes.
    #[doc(hidden)]
    pub fn __test_pty_write_log(&self) -> Vec<(u64, Vec<u8>)> {
        self.test_pty_writes.lock().clone()
    }

    /// Test-only: drive the same write + broadcast fan-out as normal input.
    #[doc(hidden)]
    pub fn __test_write_to_pane_with_broadcast(&mut self, pane_id: u64, bytes: Vec<u8>) {
        self.write_to_pane(pane_id, bytes.clone(), PtyInputSource::Keyboard);
        self.broadcast_from(pane_id, bytes, PtyInputSource::Keyboard);
    }

    /// Test-only: child render pane ids with the broadcast participant flag that
    /// would be passed into `sonicterm_render_model::PaneRender`.
    #[doc(hidden)]
    pub fn __test_child_broadcast_render_flags(&self, id: WindowId) -> Option<Vec<(u64, bool)>> {
        let child = self.windows.get(&id)?;
        let tab_idx = child.tabs.active_index();
        let panes = child.tab_states.get(tab_idx)?.tree.leaves();
        let participants = self.broadcast_participants();
        Some(panes.into_iter().map(|pane| (pane, participants.contains(&pane))).collect())
    }

    /// Test-only: how many tabs the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_tab_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|c| c.tabs.len())
    }

    /// Test-only: how many panes the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_pane_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|c| c.panes.len())
    }

    /// Test-only: pane ids owned by the named child window.
    #[doc(hidden)]
    pub fn __test_child_pane_ids(&self, id: WindowId) -> Option<Vec<u64>> {
        self.windows.get(&id).map(|c| c.panes.keys().copied().collect())
    }

    /// Test-only: install the headless pane-viewport seam on the main window
    /// so resize wiring runs without a renderer.
    #[doc(hidden)]
    pub fn __test_set_main_pane_viewport(
        &mut self,
        outer: sonicterm_ui::pane::Rect,
        cell_w: f32,
        cell_h: f32,
    ) -> bool {
        self.__test_synthetic_main();
        self.test_viewport_override = Some((outer, cell_w, cell_h));
        true
    }

    /// Test-only: drive main-window active-tab pane resizing through the same
    /// helper used by production window resize and tab activation.
    #[doc(hidden)]
    pub fn __test_resize_visible_panes(&mut self) {
        self.resize_visible_panes();
    }

    /// Test-only: activate a main tab through the same production helper used
    /// by keyboard/mouse tab activation.
    #[doc(hidden)]
    pub fn __test_invoke_activate_main_tab(&mut self, idx: usize) -> bool {
        self.activate_main_tab(idx)
    }

    /// Test-only: install the headless per-window pane-viewport seam on a child
    /// so the split/close resize wiring runs without a renderer.
    #[doc(hidden)]
    pub fn __test_set_child_pane_viewport(
        &mut self,
        id: WindowId,
        outer: sonicterm_ui::pane::Rect,
        cell_w: f32,
        cell_h: f32,
    ) -> bool {
        match self.windows.get_mut(&id) {
            Some(c) => {
                c.test_pane_viewport = Some((outer, cell_w, cell_h));
                true
            }
            None => false,
        }
    }

    /// Test-only: split the active pane of the named child window to the right,
    /// driving the same `split_active_pane_in_child` path the keymap uses.
    #[doc(hidden)]
    pub fn __test_child_split_active_right(&mut self, id: WindowId) -> bool {
        self.split_active_pane_in_child(id, sonicterm_cfg::keymap::Direction::Right)
    }

    /// Test-only: grid (cols, rows) of a specific pane in the named child.
    #[doc(hidden)]
    pub fn __test_child_pane_grid_size(&self, id: WindowId, pane_id: u64) -> Option<(u16, u16)> {
        let pane = self.windows.get(&id)?.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        Some((grid.cols, grid.rows))
    }

    /// Test-only: the active pane id in the named child's active tab.
    #[doc(hidden)]
    pub fn __test_child_active_pane(&self, id: WindowId) -> Option<u64> {
        let child = self.windows.get(&id)?;
        let tab_idx = child.tabs.active_index();
        child.tab_states.get(tab_idx).map(|st| st.active_pane)
    }

    /// Test-only: `true` when the named child pane's scrollbar is currently
    /// inside its idle-visible window (i.e. `mark_active` fired recently).
    /// Used to assert wheel-scroll / view_top jumps light the auto-hide bar
    /// on torn-out windows the same way they do on the main window.
    #[doc(hidden)]
    pub fn __test_child_scrollbar_active(&self, id: WindowId, pane_id: u64) -> Option<bool> {
        let st = self.windows.get(&id)?.scrollbar_vis.get(&pane_id)?;
        let idle_ms = match st.last_active {
            Some(t) => t.elapsed().as_millis() as u64,
            None => u64::MAX,
        };
        Some(idle_ms < scrollbar_visibility::IDLE_HIDE_MS)
    }

    /// Test-only: whether the child pane is currently marked as right-edge hovered.
    #[doc(hidden)]
    pub fn __test_child_scrollbar_near_edge(&self, id: WindowId, pane_id: u64) -> Option<bool> {
        self.windows.get(&id)?.scrollbar_vis.get(&pane_id).map(|st| st.mouse_near_right_edge)
    }

    /// Test-only: clear child scrollbar hover state, mirroring CursorLeft.
    #[doc(hidden)]
    pub fn __test_clear_child_scrollbar_hover(&mut self, id: WindowId) -> bool {
        self.clear_scrollbar_hover_in_child(id)
    }

    /// Test-only: write a child pane's `viewport_top_abs` through the same
    /// production path the scrollbar uses (`set_child_pane_view_top`), so a
    /// test can drive a scroll and observe the visibility side effect.
    #[doc(hidden)]
    pub fn __test_child_set_pane_view_top(
        &mut self,
        id: WindowId,
        pane_id: u64,
        view_top: u64,
        live_top: u64,
    ) {
        let at = self
            .windows
            .get(&id)
            .and_then(|window| window.panes.get(&pane_id))
            .map(|pane| viewport_anchor::ViewportBaseline::of(pane.parser.lock().grid()))
            .unwrap_or_default();
        self.set_child_pane_view_top(id, pane_id, view_top, live_top, at);
    }

    /// Test-only: set the last cursor position for a synthetic child window.
    #[doc(hidden)]
    pub fn __test_set_child_cursor_pos(&mut self, id: WindowId, x: f64, y: f64) -> bool {
        match self.windows.get_mut(&id) {
            Some(c) => {
                c.cursor_pos = (x, y);
                true
            }
            None => false,
        }
    }

    /// Test-only: refresh a child window's scrollbar hover state from its last
    /// cursor position, mirroring the production CursorMoved branch.
    #[doc(hidden)]
    pub fn __test_refresh_child_scrollbar_hover_from_cursor(&mut self, id: WindowId) -> bool {
        self.refresh_scrollbar_hover_from_cursor_in_child(id)
    }

    /// Test-only: seed a synthetic child WindowState without constructing a
    /// real winit Window / GpuRenderer. The pane/tab bookkeeping mirrors a
    /// tear-out child, but `window` and `renderer` stay `None` so cargo-test
    /// can exercise App-level multi-window ownership invariants headlessly.
    #[doc(hidden)]
    pub fn __test_seed_child_window(&mut self, titles: &[&str]) -> WindowId {
        self.__test_synthetic_main();
        let id = next_synthetic_child_window_id();
        let mut tabs = TabBar::new();
        let mut tab_states = Vec::new();
        let mut panes = HashMap::new();
        for title in titles {
            let pane_id = next_pane_id();
            let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
                Grid::new(80, 24),
                None,
                Arc::clone(&self.capture_staging_pool),
            )));
            panes.insert(
                pane_id,
                PaneState::new_with_media_pool(parser, None, &self.inline_media_pool),
            );
            tabs.push(Tab::new(*title));
            tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        let child = WindowState {
            // Registered when the window is inserted.
            owner: None,
            role: WindowRole::Terminal,
            custom_window_name: String::new(),
            window: None,
            renderer: None,
            tabs,
            tab_states,
            panes,
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
            pty_pressed_keys: HashMap::new(),
            last_render: Instant::now(),
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
            path_probe: path_target::PathProbeState::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.insert_window_registered(id, child);
        id
    }

    /// Test-only: inspect drag-gesture residue on a specific
    /// child window so an integration test can assert
    /// [`Self::cancel_drag_session`] clears EVERY window's state, not
    /// just the main one.
    #[doc(hidden)]
    pub fn __test_child_pressed_tab(&self, id: WindowId) -> Option<Option<usize>> {
        self.windows.get(&id).map(|ws| ws.pressed_tab)
    }

    /// Test seam: whether a window is tracking a held mouse button.
    ///
    /// `None` when `id` names no tracked window, which distinguishes an
    /// unknown window from one with no button held.
    #[doc(hidden)]
    pub fn __test_child_mouse_down(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.mouse_down)
    }

    /// Test seam: whether a window has a tab drag in progress.
    ///
    /// `None` when `id` names no tracked window.
    #[doc(hidden)]
    pub fn __test_child_has_drag_session(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.drag_session.is_some())
    }

    /// Test seam: whether a window is a drop target for the current drag.
    ///
    /// `None` when `id` names no tracked window.
    #[doc(hidden)]
    pub fn __test_child_has_drag_target(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.drag_target.is_some())
    }

    /// Test-only: seed the headless drag-chip
    /// marker on a window so a subsequent [`Self::cancel_drag_session`]
    /// can be observed to have cleared it. Returns `false` if the window
    /// id is unknown. The marker is the cross-platform stand-in for
    /// `renderer.set_drag_chip(_)` on `renderer: None` test windows —
    /// production code flips it in the same loop iteration as the real
    /// renderer call, so the assertion fails if the per-window iteration
    /// is ever removed.
    #[doc(hidden)]
    pub fn __test_set_window_drag_chip_marker(&mut self, id: WindowId, present: bool) -> bool {
        if let Some(ws) = self.windows.get_mut(&id) {
            ws.test_drag_chip_marker = Some(present);
            true
        } else {
            // When: `windows` tracks no entry for this id, so no drag-chip marker
            // could be seeded and the caller is told the seam did nothing.
            false
        }
    }

    /// Test-only: read the drag-chip marker for
    /// a window. `None` ⇒ window absent OR marker never seeded;
    /// `Some(true)` ⇒ marker set & not yet cleared by cancel;
    /// `Some(false)` ⇒ marker was set and cancel ran on this window.
    #[doc(hidden)]
    pub fn __test_window_drag_chip_marker(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).and_then(|ws| ws.test_drag_chip_marker)
    }

    /// Test-only convenience: same as
    /// [`Self::__test_set_window_drag_chip_marker`] but for the
    /// synthetic main window (id from [`synthetic_main_window_id`]).
    #[doc(hidden)]
    pub fn __test_set_main_drag_chip_marker(&mut self, present: bool) -> bool {
        self.__test_set_window_drag_chip_marker(synthetic_main_window_id(), present)
    }

    /// Test-only convenience: read the main window's drag-chip marker.
    #[doc(hidden)]
    pub fn __test_main_drag_chip_marker(&self) -> Option<bool> {
        self.__test_window_drag_chip_marker(synthetic_main_window_id())
    }

    /// Test-only: seed drag-gesture residue on a specific child
    /// window — `pressed_tab`, `mouse_down`, and a synthetic
    /// `drag_session` — without driving a real winit pointer event
    /// sequence. Returns true on success.
    #[doc(hidden)]
    pub fn __test_seed_child_drag_residue(
        &mut self,
        id: WindowId,
        pressed_tab: Option<usize>,
        mouse_down: bool,
        with_drag_session: bool,
    ) -> bool {
        let Some(ws) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so there is no child
            // state to seed drag residue onto.
            return false;
        };
        ws.pressed_tab = pressed_tab;
        ws.mouse_down = mouse_down;
        if with_drag_session {
            ws.drag_session = ws
                .tabs
                .tabs()
                .get(pressed_tab.unwrap_or(0))
                .map(|tab| crate::tab_drag::DragSession::new(id, tab.id, (0.0, 0.0)));
        }
        true
    }

    /// Test-only: install a frontmost child id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// `frontmost_window` subsumes a separate focused-child field;
    /// this kept the old name so the existing regression tests don't
    /// need touching, but it now drives the unified tracker.
    #[doc(hidden)]
    pub fn __test_set_focused_child(&mut self, id: Option<WindowId>) {
        self.__test_synthetic_main();
        self.frontmost_window = id;
    }

    /// Test-only: read back the current frontmost-child id.
    /// returns `Some(id)` when `frontmost_window` points
    /// at a non-main entry, mirroring the old `focused_child` semantics.
    #[doc(hidden)]
    pub fn __test_focused_child(&self) -> Option<WindowId> {
        match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        }
    }

    /// Test-only: read back the current `frontmost_window`.
    #[doc(hidden)]
    pub fn __test_frontmost_window(&self) -> Option<WindowId> {
        self.frontmost_window
    }

    /// Test-only: install a `frontmost_window` id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// Used by regression tests to assert that
    /// keymap-dispatched actions route to the right window's tab vec.
    #[doc(hidden)]
    pub fn __test_set_frontmost_window(&mut self, id: Option<WindowId>) {
        self.frontmost_window = id;
    }

    /// Test-only: resolve a chord string through the App's keymap.
    /// Used by `child_window_tab_actions_dispatch.rs` to
    /// pin down that the chords the child-window handler now dispatches
    /// (cmd+1, cmd+2, cmd+Right, cmd+Left) actually resolve to their
    /// expected Action variants.
    #[doc(hidden)]
    pub fn __test_keymap_lookup(&self, keys: &str) -> Option<Action> {
        self.keymap.lookup(keys).cloned()
    }

    /// Test-only: read the window the command palette is currently
    /// attached to. `None` = main window OR closed; `Some(id)` = that
    /// child window. Used by overlay-routing regression tests.
    #[doc(hidden)]
    pub fn __test_palette_attached_window(&self) -> Option<WindowId> {
        self.palette_attached_window
    }

    /// Test-only: whether the command palette is currently open.
    #[doc(hidden)]
    pub fn __test_palette_open(&self) -> bool {
        self.command_palette.is_open()
    }

    /// Test-only: command palette query text.
    #[doc(hidden)]
    pub fn __test_palette_query(&self) -> &str {
        self.command_palette.query()
    }

    /// Test-only: command palette caret byte offset.
    #[doc(hidden)]
    pub fn __test_palette_cursor(&self) -> usize {
        self.command_palette.cursor()
    }

    /// Test-only: replace the command-palette query and refresh its selection.
    #[doc(hidden)]
    pub fn __test_set_palette_query(&mut self, query: &str) {
        self.palette_pointer_capture = None;
        self.command_palette.set_query(query);
    }

    /// Test-only: drive command-palette core editing without constructing a
    /// platform-private winit `KeyEvent`.
    #[doc(hidden)]
    pub fn __test_command_palette_text_edit(
        &mut self,
        key: &winit::keyboard::Key,
        modifiers: ModifiersState,
    ) -> bool {
        if !self.command_palette.is_open() || self.palette_ime_is_composing() {
            // When: `command_palette` is shut, or an IME preedit owns its input,
            // so a core text edit would corrupt composition or edit nothing.
            return self.command_palette.is_open();
        }
        if self.command_palette.mode()
            == sonicterm_ui::command_palette::CommandPaletteMode::TabColor
        {
            // When: `TabColor` mode owns the keystroke, so it counts as handled
            // without editing the query text behind the picker.
            return true;
        }
        let Some(edit) = text_edit::core_text_edit_for_key(key, modifiers) else {
            // When: `core_text_edit_for_key` maps this key to no edit, so the
            // query is untouched and the key is reported unhandled.
            return false;
        };
        self.command_palette.apply_text_edit(edit);
        self.request_redraw_for_overlay(self.palette_attached_window);
        true
    }

    /// Test-only: enter tab-rename mode with a known value.
    #[doc(hidden)]
    pub fn __test_start_rename_tab(&mut self, title: &str) {
        self.command_palette.start_rename_tab(title);
    }

    /// Test-only: drive command-palette key handling by logical key.
    #[doc(hidden)]
    pub fn __test_command_palette_handle_key(&mut self, key: &winit::keyboard::Key) -> bool {
        self.command_palette_handle_logical_key(key)
    }

    /// Test-only: drive command-palette IME handling.
    #[doc(hidden)]
    pub fn __test_command_palette_handle_ime(&mut self, event: &winit::event::Ime) -> bool {
        self.command_palette_handle_ime(event)
    }

    /// Test-only: describe where the main window will anchor the OS IME
    /// candidate area.
    #[doc(hidden)]
    pub fn __test_main_ime_candidate_anchor_kind(&self) -> &'static str {
        if self.command_palette.is_open() && self.palette_attached_window.is_none() {
            "palette"
        } else {
            // When: `command_palette` is shut, or `palette_attached_window` names
            // a child, so the main window's IME anchor is the terminal grid.
            "terminal"
        }
    }

    /// Test-only: read the main window notification bubble message.
    #[doc(hidden)]
    pub fn __test_main_notification_message(&self) -> Option<&str> {
        self.main().and_then(|ws| ws.notification.as_ref()).map(|bubble| bubble.message.as_str())
    }

    /// Test-only: whether the main notification is ongoing.
    #[doc(hidden)]
    pub fn __test_main_notification_ongoing(&self) -> Option<bool> {
        self.main()
            .and_then(|ws| ws.notification.as_ref())
            .map(|bubble| bubble.expires_at.is_none())
    }

    /// Test-only: install a notification with a specific expiration.
    #[doc(hidden)]
    pub fn __test_show_notification_until(
        &mut self,
        kind: FrontmostKind,
        level: NotificationLevel,
        message: &str,
        expires_at: Option<std::time::Instant>,
    ) {
        self.show_notification_for_kind_until(kind, level, message.to_string(), expires_at);
    }

    /// Test-only: run notification expiry and return the next wake time.
    #[doc(hidden)]
    pub fn __test_expire_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.expire_notifications(now)
    }

    /// Test-only: read a child window notification bubble message.
    #[doc(hidden)]
    pub fn __test_child_notification_message(&self, id: WindowId) -> Option<&str> {
        self.windows
            .get(&id)
            .and_then(|ws| ws.notification.as_ref())
            .map(|bubble| bubble.message.as_str())
    }

    /// Test-only invoker for `open_search_in_child`. Mirrors the
    /// pattern used by `__test_invoke_close_active_tab_in_child` so
    /// integration tests can assert the stale-id no-op contract for
    /// overlay routing.
    #[doc(hidden)]
    pub fn __test_invoke_open_search_in_child(&mut self, id: WindowId) -> bool {
        self.open_search_in_child(id)
    }

    /// Test-only: open main search and install a known query.
    #[doc(hidden)]
    pub fn __test_set_main_search_query(&mut self, query: &str) -> bool {
        self.open_search();
        let Some(ws) = self.main_mut() else {
            // When: `main_mut` resolves nothing, so no window holds the search
            // session the query was meant to seed.
            return false;
        };
        let i = ws.tabs.active_index();
        let Some(tab) = ws.tab_states.get_mut(i) else {
            // When: `tab_states` has no entry at the active index `i`, so no tab
            // carries the search state to install into.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the seam refuses rather
            // than fabricating a session the user never opened.
            return false;
        };
        let Some(pane) = ws.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so there is no grid
            // for `set_query` to match the term against.
            return false;
        };
        search.set_query(query, pane.parser.lock().grid());
        true
    }

    /// Test-only: install a known query in an open child search field.
    #[doc(hidden)]
    pub fn __test_set_child_search_query(&mut self, id: WindowId, query: &str) -> bool {
        if !self.open_search_in_child(id) {
            // When: `open_search_in_child` could not open search for this id, so
            // there is no session for the query to land in.
            return false;
        }
        let Some(ws) = self.windows.get_mut(&id) else {
            // When: `windows` no longer tracks this id, so the child vanished
            // between opening search and installing the query.
            return false;
        };
        let i = ws.tabs.active_index();
        let Some(tab) = ws.tab_states.get_mut(i) else {
            // When: `tab_states` has no entry at the active index `i`, so the
            // child carries no tab to install the query into.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the seam refuses rather
            // than fabricating a session.
            return false;
        };
        let Some(pane) = ws.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so there is no grid
            // for `set_query` to match against.
            return false;
        };
        search.set_query(query, pane.parser.lock().grid());
        true
    }

    /// Test-only: apply a core edit to main or child search through the same
    /// shared state operation used by production routing.
    #[doc(hidden)]
    pub fn __test_search_text_edit(
        &mut self,
        id: Option<WindowId>,
        key: &winit::keyboard::Key,
        modifiers: ModifiersState,
    ) -> bool {
        let Some(edit) = text_edit::search_text_edit_for_key(key, modifiers) else {
            // When: `search_text_edit_for_key` maps this key to no edit, so the
            // search term is untouched and the key is reported unhandled.
            return false;
        };
        let target = id.or(self.main_window_id);
        let Some(target) = target else {
            // When: neither the supplied id nor `main_window_id` yields a
            // `target`, so no window owns the search this edit would change.
            return false;
        };
        let Some(ws) = self.windows.get_mut(&target) else {
            // When: `windows` no longer tracks `target`, so the window closed
            // between resolving it and applying the edit.
            return false;
        };
        if ws.ime.is_composing() {
            // When: `ime` is mid-composition, so the key belongs to the preedit
            // and a core edit would cut the composition in half.
            return true;
        }
        let i = ws.tabs.active_index();
        let Some(tab) = ws.tab_states.get_mut(i) else {
            // When: `tab_states` has no entry at the active index `i`, so no tab
            // holds the search this edit would change.
            return false;
        };
        let Some(search) = tab.search.as_mut() else {
            // When: this `tab` has no open `search`, so the edit is refused
            // rather than opening a session the user did not ask for.
            return false;
        };
        let Some(pane) = ws.panes.get(&tab.active_pane) else {
            // When: `panes` cannot resolve `tab.active_pane`, so re-matching the
            // term has no grid to search.
            return false;
        };
        search.apply_text_edit(edit, pane.parser.lock().grid());
        true
    }

    /// Test-only: read main or child search query and caret.
    #[doc(hidden)]
    pub fn __test_search_query_cursor(&self, id: Option<WindowId>) -> Option<(&str, usize)> {
        let target = id.or(self.main_window_id)?;
        let ws = self.windows.get(&target)?;
        let search = ws.tab_states.get(ws.tabs.active_index())?.search.as_ref()?;
        Some((search.query.as_str(), search.cursor()))
    }

    /// Test-only: seed main IME preedit state.
    #[doc(hidden)]
    pub fn __test_set_main_ime_preedit(&mut self, text: &str) -> bool {
        let Some(ws) = self.main_mut() else {
            // When: `main_mut` resolves nothing, so no window holds the IME state
            // this preedit would seed.
            return false;
        };
        ws.ime.handle_preedit(text, Some((text.len(), text.len())));
        true
    }

    /// Test-only: install an in-memory clipboard buffer. This avoids depending
    /// on the OS clipboard in headless integration tests while exercising the
    /// same `set_clipboard_text` / `paste_clipboard` dispatch paths.
    #[doc(hidden)]
    pub fn __test_set_memory_clipboard(&mut self, text: &str) {
        self.test_clipboard_text = Some(text.to_string());
    }

    /// Test-only: read the in-memory clipboard buffer if installed.
    #[doc(hidden)]
    pub fn __test_memory_clipboard(&self) -> Option<String> {
        self.test_clipboard_text.clone()
    }

    /// Test-only: make clipboard writes fail before either clipboard seam changes.
    #[doc(hidden)]
    pub fn __test_set_clipboard_write_failure(&mut self, enabled: bool) {
        self.test_clipboard_write_failure = enabled;
    }

    /// Test-only: drain the PTY write ledger populated by `write_to_pane`.
    #[doc(hidden)]
    pub fn __test_drain_pty_writes(&self) -> Vec<(u64, Vec<u8>)> {
        std::mem::take(&mut *self.test_pty_writes.lock())
    }

    /// Test-only: exercise file-drop path paste routing without a platform drop event.
    #[doc(hidden)]
    pub fn __test_paste_file_paths_for_kind(
        &mut self,
        kind: FrontmostKind,
        paths: Vec<std::path::PathBuf>,
    ) {
        self.paste_file_paths_for_kind(kind, paths);
    }

    /// Bind an unbound headless selection to a synthetic window's active pane,
    /// matching production mouse selection creation.
    fn bind_test_selection(
        window: &WindowState,
        selection: Option<Selection>,
    ) -> Option<Selection> {
        let mut selection = selection?;
        if selection.pane_id.is_some() {
            // When: `selection` already names a `pane_id`, so rebinding it would
            // move the caller's range onto a different pane.
            return Some(selection);
        }
        let pane_id = window.tab_states.get(window.tabs.active_index())?.active_pane;
        let pane = window.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        selection = selection.with_content_state(
            pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        );
        Some(selection)
    }

    /// Test-only: set the synthetic main window's selection.
    #[doc(hidden)]
    pub fn __test_set_main_selection(&mut self, selection: Option<Selection>) -> bool {
        let Some(id) = self.main_window_id else {
            // When: `main_window_id` is unset, so no main window exists to carry
            // the selection.
            return false;
        };
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` no longer tracks this id, so the content state the
            // selection binds to cannot be read.
            return false;
        };
        let selection = Self::bind_test_selection(window, selection);
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window disappeared between binding and assignment, so the
            // bound selection has nowhere to be stored.
            return false;
        };
        window.selection = selection;
        true
    }

    /// Test-only: set a synthetic child window's selection.
    #[doc(hidden)]
    pub fn __test_set_child_selection(
        &mut self,
        id: WindowId,
        selection: Option<Selection>,
    ) -> bool {
        let Some(window) = self.windows.get(&id) else {
            // When: `windows` tracks no entry for this id, so the content state
            // the selection binds to cannot be read.
            return false;
        };
        let selection = Self::bind_test_selection(window, selection);
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window disappeared between binding and assignment, so the
            // bound selection has nowhere to be stored.
            return false;
        };
        window.selection = selection;
        true
    }

    /// Test seam: give a tracked window a live winit window and renderer.
    ///
    /// Lets a test promote a synthetic headless entry into one that can render,
    /// without going through real window creation. `false` when `id` is unknown.
    #[doc(hidden)]
    pub fn __test_attach_window_renderer(
        &mut self,
        id: WindowId,
        window: Arc<Window>,
        renderer: GpuRenderer,
    ) -> bool {
        let Some(state) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so there is no state to
            // hold the window handle or its renderer.
            return false;
        };
        state.window = Some(window);
        state.renderer = Some(renderer);
        true
    }

    /// Test seam: the selection a window currently holds.
    ///
    /// The outer `None` means `id` is unknown; the inner `None` means the
    /// window is tracked but has no selection.
    #[doc(hidden)]
    pub fn __test_window_selection(&self, id: WindowId) -> Option<Option<Selection>> {
        self.windows.get(&id).map(|state| state.selection)
    }

    /// Test seam: the pane targeted by a window's real renderer flash state.
    #[doc(hidden)]
    pub fn __test_window_pane_focus_flash_target(&self, id: WindowId) -> Option<u64> {
        self.windows.get(&id)?.renderer.as_ref()?.__test_pane_focus_flash_target()
    }

    /// Test seam: one pixel of a window's software-rendered frame, as BGRA.
    ///
    /// Lets a test assert what the CPU rasterizer actually produced. `None`
    /// when the window is unknown, has no renderer, or the frame is absent.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __test_window_software_frame_pixel_bgra(
        &self,
        id: WindowId,
        x: u32,
        y: u32,
    ) -> Option<[u8; 4]> {
        self.windows.get(&id)?.renderer.as_ref()?.__test_software_frame_pixel_bgra(x, y)
    }

    /// Test seam: force the no-GPU degrade path on or off.
    ///
    /// Bypasses runtime detection so a test can exercise software-render
    /// pacing on a machine that has a working GPU.
    #[doc(hidden)]
    pub fn __test_set_software_render_degrade(&mut self, degrade: bool) {
        self.software_render_degrade = degrade;
    }

    /// Test seam: whether the main window has a redraw waiting on the gate.
    ///
    /// Lets a test assert that a redraw was coalesced rather than drawn.
    #[doc(hidden)]
    pub fn __test_main_redraw_deferred(&self) -> bool {
        self.pending_redraw
    }

    /// Test seam: observe a window's frame timestamp independently of its contention deadline.
    #[doc(hidden)]
    pub fn __test_window_last_render(&self, id: WindowId) -> Option<Instant> {
        self.windows.get(&id).map(|window| window.last_render)
    }

    /// Test seam: backdate a window's last-render instant.
    ///
    /// Frame pacing measures elapsed time since the last render, so moving
    /// this lets a test cross a frame boundary without waiting. `false` when
    /// `id` is unknown.
    #[doc(hidden)]
    pub fn __test_set_window_last_render(&mut self, id: WindowId, last_render: Instant) -> bool {
        let Some(state) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no render timestamp
            // exists to backdate.
            return false;
        };
        state.last_render = last_render;
        true
    }

    /// Test seam: a window's cell width, cell height, and top inset.
    ///
    /// These are the metrics pane layout divides by, so a test can check
    /// geometry against production's planned active-pane origin. Returns `None`
    /// before layout or after its geometry is invalidated.
    #[doc(hidden)]
    pub fn __test_window_cell_geometry(&self, id: WindowId) -> Option<(f32, f32, f32)> {
        let window = self.windows.get(&id)?;
        let renderer = window.renderer.as_ref()?;
        let pane = window.tab_states.get(window.tabs.active_index())?.active_pane;
        let [_, top] = renderer.pane_grid_origin(pane)?;
        let (cell_w, cell_h) = renderer.cell_size();
        Some((cell_w, cell_h, top))
    }

    /// Test-only: feed bytes into a child pane's parser.
    #[doc(hidden)]
    pub fn __test_advance_child_pane_parser(
        &self,
        id: WindowId,
        pane_id: u64,
        bytes: &[u8],
    ) -> bool {
        let Some(pane) = self.windows.get(&id).and_then(|c| c.panes.get(&pane_id)) else {
            // When: neither `windows` nor its `panes` resolve the request, so the
            // bytes have no parser to advance.
            return false;
        };
        pane.parser.lock().advance(bytes);
        true
    }

    /// Test-only: clear all dirty row flags for a child pane.
    #[doc(hidden)]
    pub fn __test_clear_child_pane_dirty(&self, id: WindowId, pane_id: u64) -> bool {
        let Some(pane) = self.windows.get(&id).and_then(|c| c.panes.get(&pane_id)) else {
            // When: neither `windows` nor its `panes` resolve the request, so no
            // grid exists whose dirty rows could be cleared.
            return false;
        };
        pane.parser.lock().grid_mut().clear_dirty();
        true
    }

    /// Test-only: count dirty rows for a child pane.
    #[doc(hidden)]
    pub fn __test_child_pane_dirty_count(&self, id: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&id)?.panes.get(&pane_id)?;
        Some(pane.parser.lock().grid().dirty_count())
    }

    /// Test-only: seed child IME preedit state.
    #[doc(hidden)]
    pub fn __test_set_child_ime_preedit(&mut self, id: WindowId, text: &str) -> bool {
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no child holds the
            // IME state this preedit would seed.
            return false;
        };
        child.ime.handle_preedit(text, Some((text.len(), text.len())));
        true
    }

    /// Test-only: read whether a child IME composition is active.
    #[doc(hidden)]
    pub fn __test_child_ime_composing(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|child| child.ime.is_composing())
    }

    /// Test-only: read whether the main window is in read-only copy mode.
    #[doc(hidden)]
    pub fn __test_main_read_only(&self) -> bool {
        self.main().and_then(|ws| ws.copy_mode.as_ref()).is_some_and(|mode| mode.is_read_only())
    }

    /// Test-only: read whether a child window is in read-only copy mode.
    #[doc(hidden)]
    pub fn __test_child_read_only(&self, id: WindowId) -> Option<bool> {
        self.windows
            .get(&id)
            .map(|child| child.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only()))
    }

    /// Test-only: seed the headless renderer-focus marker for a child window.
    #[doc(hidden)]
    pub fn __test_set_child_renderer_focus_marker(&mut self, id: WindowId, focused: bool) -> bool {
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no child carries the
            // renderer-focus marker to update.
            return false;
        };
        child.test_renderer_focus_marker = Some(focused);
        true
    }

    /// Test-only: read the headless renderer-focus marker for a child window.
    #[doc(hidden)]
    pub fn __test_child_renderer_focus_marker(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).and_then(|child| child.test_renderer_focus_marker)
    }

    /// Test-only: invoke the child focus transition handler without constructing
    /// a winit `ActiveEventLoop`.
    #[doc(hidden)]
    pub fn __test_handle_child_focus_changed(&mut self, id: WindowId, focused: bool) {
        self.handle_window_focus_changed(id, focused);
    }

    /// classify [`Self::frontmost_window`] without
    /// borrowing anything mutably. Returns:
    ///   * `FrontmostKind::None` if no sonic window has been focused yet,
    ///     focus is currently outside every sonic window, or the recorded
    ///     id no longer matches any live window (stale-id race).
    ///   * `FrontmostKind::Main` if the recorded id matches the main
    ///     window we currently own.
    ///   * `FrontmostKind::Child(id)` if the recorded id matches a live
    ///     entry in [`Self::windows`].
    ///   * `FrontmostKind::Other` for any non-terminal window — actions
    ///     should fall through to the safe
    ///     main-window default in that case.
    ///
    /// Pure read; no mutation, no logging. The keymap_dispatch arms call
    /// this first, then route to the matching mutator + redraw target.
    /// Borrow the main window's [`WindowState`] from `self.windows`, keyed by
    /// [`Self::main_window_id`]. Returns `None` before `do_resumed` has run
    /// (no main window yet) or if the entry is missing for any reason.
    ///
    /// Every reader of the main window's renderer, tabs, and panes goes
    /// through this helper or its `_mut` counterpart.
    #[doc(hidden)]
    pub fn main(&self) -> Option<&WindowState> {
        let id = self.main_window_id?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::main`].
    #[doc(hidden)]
    pub fn main_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)
    }

    /// Borrow the main window's `Arc<Window>` from its [`WindowState`].
    /// Sole source of truth for the main window handle. Returns `None`
    /// before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_window(&self) -> Option<&Arc<Window>> {
        self.windows.get(&self.main_window_id?)?.window.as_ref()
    }

    /// borrow the main window's `GpuRenderer`
    /// from its `WindowState`. Sole source of truth for the main
    /// renderer.
    /// Returns `None` before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_renderer(&self) -> Option<&GpuRenderer> {
        self.windows.get(&self.main_window_id?)?.renderer.as_ref()
    }

    /// Mutable counterpart of [`Self::main_renderer`].
    #[doc(hidden)]
    pub fn main_renderer_mut(&mut self) -> Option<&mut GpuRenderer> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)?.renderer.as_mut()
    }

    /// borrow the main window's [`TabBar`] from
    /// its [`WindowState`]. Sole source of truth (legacy `App.tabs` was
    /// Returns `None` before `do_resumed` /
    /// `__test_synthetic_main` has populated the shadow entry.
    #[doc(hidden)]
    pub fn main_tabs(&self) -> Option<&TabBar> {
        Some(&self.windows.get(&self.main_window_id?)?.tabs)
    }

    /// Mutable counterpart of [`Self::main_tabs`].
    #[doc(hidden)]
    pub fn main_tabs_mut(&mut self) -> Option<&mut TabBar> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tabs)
    }

    /// borrow the main window's `Vec<TabState>`
    /// from its [`WindowState`]. Sole source of truth.
    #[doc(hidden)]
    pub fn main_tab_states(&self) -> Option<&[TabState]> {
        Some(self.windows.get(&self.main_window_id?)?.tab_states.as_slice())
    }

    /// Mutable counterpart of [`Self::main_tab_states`].
    #[doc(hidden)]
    pub fn main_tab_states_mut(&mut self) -> Option<&mut Vec<TabState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tab_states)
    }

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

    /// Test-only: snapshot the governor's process root.
    #[doc(hidden)]
    pub fn __test_governor_snapshot_root(&self) -> sonicterm_types::ResourceSnapshot {
        self.governor
            .snapshot(self.governor.root_owner())
            .expect("the process root always snapshots")
    }

    /// Test-only: move a pane from one window to another.
    ///
    /// Mirrors what tab tear-out does — remove from the source map, insert
    /// into the destination — so the test exercises the real ownership
    /// consequence rather than a simulation of it.
    #[doc(hidden)]
    pub fn __test_move_pane_between_windows(
        &mut self,
        source: WindowId,
        destination: WindowId,
        pane_id: u64,
    ) -> bool {
        let Some(pane) = self.windows.get_mut(&source).and_then(|w| w.remove_pane(pane_id)) else {
            // When: `source` yields no `pane_id`, so nothing was detached and both
            // windows keep the panes they had.
            return false;
        };
        let Some(window) = self.windows.get_mut(&destination) else {
            // When: `destination` no longer resolves, so the already-removed pane
            // has nowhere to land and drops with its PTY.
            return false;
        };
        window.panes.insert(pane_id, pane);
        self.reattribute_pane_owners();
        true
    }

    /// Test-only: snapshot any owner.
    #[doc(hidden)]
    pub fn __test_owner_snapshot(
        &self,
        owner: ResourceOwnerId,
    ) -> Option<sonicterm_types::ResourceSnapshot> {
        self.governor.snapshot(owner).ok()
    }

    /// Test-only: measure one pane's retention through the reporting seam.
    #[doc(hidden)]
    pub fn __test_pane_retention(
        &self,
        window: WindowId,
        pane_id: u64,
    ) -> Option<retention::PaneRetention> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        retention::measure_pane(pane)
    }

    /// Test-only: the governor amounts a pane currently holds, by class.
    #[doc(hidden)]
    pub fn __test_pane_charges(
        &self,
        window: WindowId,
        pane_id: u64,
    ) -> Option<HashMap<ResourceClass, sonicterm_types::ResourceAmount>> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        Some(pane.charges.iter().map(|(class, held)| (*class, held.committed_amount())).collect())
    }

    /// Test-only: a pane's total charged bytes across every class.
    #[doc(hidden)]
    pub fn __test_pane_charge_total(&self, window: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        Some(pane.charges.values().map(|held| held.committed_amount().bytes).sum())
    }

    /// Test-only: media captures currently in flight on a pane.
    ///
    /// Distinct from the retained-bytes figure: a cancelled capture and a
    /// completed one both report zero bytes, and the slow-transfer test turns
    /// on which of those happened.
    #[doc(hidden)]
    pub fn __test_pane_capture_count(&self, window: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        pane.parser.try_lock().map(|parser| parser.live_capture_count())
    }

    /// Test-only: the byte ceiling the governor holds a pane owner to.
    ///
    /// Read back from the ledger rather than from the constant, so a limit that
    /// is computed correctly and never installed fails the assertion that uses
    /// this.
    #[doc(hidden)]
    pub fn __test_pane_owner_limit(&self, window: WindowId) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.values().next()?;
        let owner = pane.owner.as_ref()?.id();
        self.governor.snapshot(owner).ok().map(|snapshot| snapshot.owner_bytes_limit)
    }

    /// Test-only: set a child pane's scrollback limit.
    #[doc(hidden)]
    pub fn __test_set_child_pane_scrollback(
        &mut self,
        window: WindowId,
        pane_id: u64,
        limit: usize,
    ) -> bool {
        let Some(pane) = self.windows.get_mut(&window).and_then(|w| w.panes.get_mut(&pane_id))
        else {
            // When: neither `window` nor its `panes` resolve the request, so no
            // grid exists whose scrollback `limit` could be set.
            return false;
        };
        pane.parser.lock().grid_mut().set_scrollback_limit(limit);
        true
    }

    /// Test-only: run a retention sample regardless of the interval.
    ///
    /// The production sampler is interval-gated and level-gated, neither of
    /// which a test should wait on or install a subscriber for. This drives
    /// the same charging pass the sampler runs.
    #[doc(hidden)]
    pub fn __test_force_retention_sample(&mut self) {
        self.reconcile_pane_owners();
        self.__test_charge_pane_owners();
    }

    /// Test-only: whether `owner` is still open in the governor.
    ///
    /// A closed owner's record is dropped, so this reports `false` for both a
    /// closed owner and an owner that never existed. That is the answer the
    /// callers want — "is this still holding resources" — and it stays correct
    /// whichever way the ledger represents a finished owner.
    #[doc(hidden)]
    pub fn __test_owner_is_open(&self, owner: ResourceOwnerId) -> bool {
        self.governor
            .snapshot(owner)
            .map(|snapshot| snapshot.owner_state != sonicterm_types::OwnerState::Closed)
            .unwrap_or(false)
    }

    /// Test-only: a window's owner id, if it registered one.
    #[doc(hidden)]
    pub fn __test_window_owner(&self, id: WindowId) -> Option<ResourceOwnerId> {
        self.windows.get(&id).and_then(|window| window.owner.as_ref()).map(OwnerGuard::id)
    }

    /// Test-only: one pane's owner id, if it has one.
    #[doc(hidden)]
    pub fn __test_pane_owner(&self, window: WindowId, pane_id: u64) -> Option<ResourceOwnerId> {
        self.windows
            .get(&window)?
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.owner.as_ref())
            .map(OwnerGuard::id)
    }

    /// Test-only: how many panes in a window have owners.
    #[doc(hidden)]
    pub fn __test_child_pane_owner_count(&self, id: WindowId) -> Option<usize> {
        self.windows
            .get(&id)
            .map(|window| window.panes.values().filter(|pane| pane.owner.is_some()).count())
    }

    /// Test-only: the pane owner ids in a window, sorted for comparison.
    #[doc(hidden)]
    pub fn __test_child_pane_owners(&self, id: WindowId) -> Vec<u64> {
        let mut owners: Vec<u64> = self
            .windows
            .get(&id)
            .map(|window| {
                window
                    .panes
                    .values()
                    .filter_map(|pane| pane.owner.as_ref())
                    .map(|owner| owner.id().get())
                    .collect()
            })
            .unwrap_or_default();
        owners.sort_unstable();
        owners
    }

    /// Test-only invoker for [`Self::reconcile_pane_owners`].
    #[doc(hidden)]
    pub fn __test_reconcile_pane_owners(&mut self) {
        self.reconcile_pane_owners();
    }

    /// Reconcile pane owners against every window's actual pane set.
    ///
    /// Panes are inserted at a dozen sites, several inside borrows where the
    /// governor is not reachable, and threading registration through all of
    /// them is the "every call site must remember" pattern that produces the
    /// one forgotten site. Reconciling instead means there is no site to
    /// forget: a pane without an owner gets one, and an owner whose pane is
    /// gone is closed.
    ///
    /// Runs from the periodic retention sampler rather than per frame, so its
    /// cost is bounded by that interval regardless of how often panes move.
    pub(super) fn reconcile_pane_owners(&mut self) {
        for window in self.windows.values_mut() {
            window.reconcile_pane_owners();
            for pane in window.panes.values_mut() {
                if pane.reap_slot.is_none() {
                    if let Some(pty) = pane.pty.as_mut() {
                        match self.pty_reaper.reserve(pty) {
                            Ok(slot) => pane.reap_slot = Some(slot),
                            Err(reason) => {
                                tracing::debug!(
                                    ?reason,
                                    "PTY teardown reservation refused; retirement will retry once"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Re-parent pane owners whose window has changed, and close their old ones.
    ///
    /// A `PaneState` carries its `owner` field when tab tear-out moves it
    /// between windows, but the owner itself was created *below the source
    /// window's* owner and the governor has no move operation. Left alone, the
    /// source window keeps reporting a pane it no longer has and the
    /// destination reports none for a pane it does — which makes "what does
    /// this window hold" wrong in both directions, and that question is the
    /// entire reason the hierarchy exists.
    ///
    /// Detected by comparing each pane owner's recorded parent against the
    /// window it now lives in, so this needs no hook at the move sites: a pane
    /// that never moved has a matching parent and costs one snapshot read.
    ///
    /// The old owner is closed rather than abandoned. Existing committed
    /// charges move as one class-preserving batch before the guard changes, so
    /// parser contention cannot leave the destination owner empty. The fresh
    /// owner has the same pane limits, so a rejection means an internal ledger
    /// invariant failed; the owned provisional guard closes before that failure
    /// stops the move, while every token remains on the old owner.
    pub(super) fn reattribute_pane_owners(&mut self) {
        let window_ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for window_id in window_ids {
            let Some(window) = self.windows.get(&window_id) else {
                // When: `window_id` no longer resolves, so no pane set remains
                // whose owners could be reattributed.
                continue;
            };
            let Some(window_owner) = window.owner.as_ref().map(OwnerGuard::id) else {
                // When: this `window` holds no owner, so there is no destination
                // parent to move its pane owners onto.
                continue;
            };

            let misattributed: Vec<u64> = window
                .panes
                .iter()
                .filter_map(|(pane_id, pane)| {
                    let owner = pane.owner.as_ref()?.id();
                    let parent = self.governor.snapshot(owner).ok()?.parent?;
                    (parent != window_owner).then_some(*pane_id)
                })
                .collect();

            for pane_id in misattributed {
                let new_owner = match self.governor.create_child(
                    window_owner,
                    OwnerKind::AppPane,
                    pane_owner_limits(),
                ) {
                    Ok(owner) => owner,
                    Err(error) => {
                        // When: `create_child` returns `Err(error)`, no provisional
                        // owner exists and source attribution remains unchanged.
                        tracing::warn!(
                            target: "memory",
                            ?error,
                            pane = pane_id,
                            "pane owner reattribution could not create its destination owner"
                        );
                        continue;
                    }
                };
                let provisional = OwnerGuard::new(self.governor.clone(), new_owner);
                let transferred = {
                    let Some(pane) =
                        self.windows.get_mut(&window_id).and_then(|w| w.panes.get_mut(&pane_id))
                    else {
                        // When: `pane_id` vanished after the owner was created, the
                        // empty provisional guard below must close it immediately.
                        drop(provisional);
                        continue;
                    };
                    install_transferred_pane_owner(pane, provisional)
                };
                match transferred {
                    Ok(stale) => drop(stale),
                    Err(error) => {
                        panic!(
                            "pane {pane_id} owner reattribution violated governor invariants: {error}"
                        );
                    }
                }
            }
        }
        // Ownerless panes may coexist with moved panes when a populated window
        // is first registered; adopt them after reattribution finishes.
        self.reconcile_pane_owners();
    }

    /// Close a window's owner and every pane owner below it.
    ///
    /// Called from window teardown. Owners are closed leaf-first because the
    /// governor refuses to finish closing a parent with open children — which
    /// is the invariant that makes a leaked pane owner visible rather than
    /// silent.
    /// Close the governor owners held by a window already removed from the map.
    ///
    /// Takes the `WindowState` rather than looking it up, because the
    /// production close paths remove the window *before* releasing its
    /// registries — so a lookup-based release returns early and closes
    /// nothing. That is exactly what happened: the release ran, found no
    /// window, and returned, leaving every owner `Open` for the life of the
    /// process.
    pub(super) fn release_owners_of(&mut self, window: &mut WindowState) {
        // Charges first. `finish_close` refuses an owner that still holds
        // them, and the previous order took `pane.owner` while leaving
        // `pane.charges` populated — so every close returned
        // `OwnerHasLiveCharges` and stopped at `Closing`.
        // Charges first, then drop the guards: each closes its owner on drop,
        // and `finish_close` refuses an owner still holding charges.
        for pane in window.panes.values_mut() {
            pane.charges.clear();
            drop(pane.owner.take());
        }
        drop(window.owner.take());
    }

    pub(super) fn release_window_owner(&mut self, id: WindowId) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` no longer resolves, so the map exposes no guards to
            // drain and the owner records are already unreachable.
            return;
        };
        // Charges must be released before the owner closes.
        //
        // `finish_close` refuses an owner that still holds charges, and this
        // took `pane.owner` while leaving `pane.charges` populated — so every
        // close returned `OwnerHasLiveCharges`, the `let _` discarded it, and
        // the owner stopped at `Closing` forever. Measured: 80 of 80 owners
        // still open after 40 create/destroy cycles.
        //
        // `reattribute_pane_owners` already does this in the right order,
        // twelve lines away.
        for pane in window.panes.values_mut() {
            pane.charges.clear();
            drop(pane.owner.take());
        }
        drop(window.owner.take());
    }

    /// The main window's pane map.
    ///
    /// `None` before a main window exists, which distinguishes "no window yet"
    /// from "a window holding no panes".
    pub fn main_panes(&self) -> Option<&HashMap<u64, PaneState>> {
        Some(&self.windows.get(&self.main_window_id?)?.panes)
    }

    /// Mutable counterpart of [`Self::main_panes`]. NOTE: this borrows
    /// the full main [`WindowState`] mutably via `windows.get_mut`, so
    /// callers needing panes + tabs/tab_states/renderer in one expression
    /// must instead `let ws = self.main_mut()?;` and field-disjoint
    /// split-borrow.
    #[doc(hidden)]
    pub fn main_panes_mut(&mut self) -> Option<&mut HashMap<u64, PaneState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.panes)
    }

    /// borrow the main window's selection
    /// `Option<Selection>` from its [`WindowState`]. Sole source of
    /// truth.
    /// Returns `None` (no main window) — `Some(None)` (no selection)
    /// — `Some(Some(_))` (active selection).
    #[doc(hidden)]
    pub fn main_selection(&self) -> Option<&Option<Selection>> {
        Some(&self.windows.get(&self.main_window_id?)?.selection)
    }

    /// Mutable counterpart of [`Self::main_selection`].
    #[doc(hidden)]
    pub fn main_selection_mut(&mut self) -> Option<&mut Option<Selection>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.selection)
    }

    /// borrow the main window's
    /// `ModifiersState` from its [`WindowState`]. Returns
    /// `ModifiersState::empty()` if the main window does not yet
    /// exist (safe default — no modifiers held).
    #[doc(hidden)]
    pub fn main_modifiers(&self) -> ModifiersState {
        self.main_window_id
            .and_then(|id| self.windows.get(&id))
            .map(|ws| ws.modifiers)
            .unwrap_or_else(ModifiersState::empty)
    }

    /// replace the main window's selection.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn selection_set(&mut self, sel: Option<Selection>) {
        if let Some(ws) = self.main_mut() {
            ws.selection = sel;
        }
    }

    /// replace the main window's copy-mode state.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn copy_mode_set(&mut self, st: Option<CopyModeState>) {
        if let Some(ws) = self.main_mut() {
            ws.copy_mode = st;
        }
    }

    /// borrow the [`WindowState`] of whichever terminal
    /// window is OS-frontmost. Falls back to the main window when no
    /// frontmost has been recorded yet (matches the safe default in
    /// [`Self::frontmost_kind`]).
    #[doc(hidden)]
    pub fn frontmost(&self) -> Option<&WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::frontmost`].
    #[doc(hidden)]
    pub fn frontmost_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get_mut(&id)
    }

    /// Which terminal window currently holds OS focus.
    ///
    /// Keymap dispatch routes window-scoped chords by this, so a stale or
    /// unfocused id resolves to `None` rather than defaulting to main.
    #[doc(hidden)]
    pub fn frontmost_kind(&self) -> FrontmostKind {
        let Some(id) = self.frontmost_window else {
            // When: `frontmost_window` recorded nothing, so focus is unknown and
            // callers fall back to main rather than guessing a target.
            return FrontmostKind::None;
        };
        if let Some(w) = self.main_window() {
            // When: `main_window` exists, so its identity is checked before the
            // recorded `id` is treated as a torn-out child.
            if w.id() == id {
                // When: `w` carries the focused `id`, so the chord lands on main
                // and the child lookup below is unnecessary.
                return FrontmostKind::Main;
            }
        }
        if self.windows.contains_key(&id) {
            // When: `windows` still tracks `id` after the main check, so focus
            // sits on a live torn-out child.
            return FrontmostKind::Child(id);
        }
        // Recorded id doesn't match anything live (rare: window closed
        // between the focus event and the action dispatch). Treat as
        // "no frontmost" so callers fall back to the main-window default.
        FrontmostKind::None
    }

    /// if [`Self::frontmost_window`] is `Some(_)`
    /// but classifies as `None` (recorded id no longer matches any
    /// live window), clear it. Called by keymap_dispatch arms BEFORE
    /// falling back to main, so the next dispatch doesn't retry the
    /// dead id. Returns `true` if a stale id was cleared (purely
    /// informational; callers ignore it today).
    #[doc(hidden)]
    pub fn clear_stale_frontmost(&mut self) -> bool {
        if self.frontmost_window.is_some() && self.frontmost_kind() == FrontmostKind::None {
            // When: `frontmost_window` names a window `frontmost_kind` can no
            // longer classify, so the record outlived the window it points at.
            self.frontmost_window = None;
            return true;
        }
        false
    }

    /// Test-only invoker for [`Self::close_active_tab_in_child`]. Exists
    /// because the helper is `pub(super)` and tests live outside the
    /// `app` module tree.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_tab_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::reap_empty_child`]. Pins
    /// `App::transfer_tab` onto
    /// the unified empty-window cleanup contract: a stale id is a
    /// silent no-op (no panic, no spurious `windows` mutation), which
    /// is the only behaviour we can reliably pin without a live
    /// `WindowState` (needs a wgpu surface + winit `Window`).
    #[doc(hidden)]
    pub fn __test_invoke_reap_empty_child(&mut self, id: WindowId) {
        self.reap_empty_child(id);
    }

    /// Test-only invoker for [`Self::close_tab_at_in_child`] — the
    /// per-index helper the close-button (×) hit-test path uses in a
    /// torn-out child window's tab bar.
    #[doc(hidden)]
    pub fn __test_invoke_close_tab_at_in_child(&mut self, id: WindowId, idx: usize) -> bool {
        self.close_tab_at_in_child(id, idx)
    }

    /// Test-only invoker for [`Self::close_active_pane_or_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane_or_tab_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_pane_or_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::next_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_next_tab_in_child(&mut self, id: WindowId) -> bool {
        self.next_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::prev_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_prev_tab_in_child(&mut self, id: WindowId) -> bool {
        self.prev_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::activate_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_activate_tab_in_child(&mut self, id: WindowId, idx: usize) -> bool {
        self.activate_tab_in_child(id, idx)
    }

    /// Test-only invoker for [`Self::split_active_pane_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_split_active_pane_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.split_active_pane_in_child(id, dir)
    }

    /// Test-only invoker for [`Self::close_active_pane_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_pane_in_child(id)
    }

    /// Test-only invoker for [`Self::close_active_pane`] (the main-window
    /// pane close path). Pairs with [`Self::test_viewport_override`] so
    /// tests can exercise the production close path — including the
    /// post-close `resize_visible_panes` call that re-fits the surviving
    /// sibling's Grid + PtyHandle — without a live wgpu renderer.
    /// See `crates/sonicterm-app/tests/per_pane_resize.rs`.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane(&mut self) {
        self.close_active_pane();
    }

    /// Test-only invoker for [`Self::focus_pane_dir_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_focus_pane_dir_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.focus_pane_dir_in_child(id, dir)
    }

    /// Test-only invoker for [`Self::toggle_active_pane_zoom_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_toggle_active_pane_zoom_in_child(&mut self, id: WindowId) -> bool {
        self.toggle_active_pane_zoom_in_child(id)
    }

    /// Test-only invoker for [`Self::resize_active_split_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_resize_active_split_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.resize_active_split_in_child(id, dir)
    }

    /// Test-only: count of tabs in the main App.
    #[doc(hidden)]
    pub fn __test_main_tab_count(&self) -> usize {
        self.main_tabs().map(|t| t.len()).unwrap_or(0)
    }

    /// Test-only: read the `pending_new_window` flag. Set by the
    /// `Action::NewWindow` dispatcher arm; consumed by
    /// `drain_pending_window_creates` (which needs a live
    /// `ActiveEventLoop` and so can't run in a unit test). The flag
    /// is the testable seam.
    #[doc(hidden)]
    pub fn __test_pending_new_window(&self) -> bool {
        self.pending_new_window.is_some()
    }

    /// Test seam for deferred in-process tear-out requests.
    #[doc(hidden)]
    pub fn __test_pending_tear_out(&self) -> Option<(WindowId, usize, Option<(i32, i32)>)> {
        self.pending_tear_out
            .as_ref()
            .map(|t| (t.source_window, t.source_tab_idx, t.drop_screen_pos))
    }

    /// test seam: read the `pending_os_teardown` flag set
    /// by `handle_os_drag_ended` on the `DroppedOnEmpty` branch.
    #[doc(hidden)]
    pub fn __test_pending_os_teardown(&self) -> bool {
        self.pending_os_teardown
    }

    /// test seam: directly set `pending_os_teardown` so
    /// the race test can simulate the `DroppedOnEmpty` branch without
    /// forging a full OS-drag pending state.
    #[doc(hidden)]
    pub fn __test_set_pending_os_teardown(&mut self, v: bool) {
        self.pending_os_teardown = v;
    }

    /// test seam: drive `drain_pending_os_teardown` from
    /// integration tests (no `ActiveEventLoop` needed — the teardown
    /// drain doesn't create windows; only the window-create drain
    /// does).
    #[doc(hidden)]
    pub fn __test_drain_pending_os_teardown(&mut self) {
        self.drain_pending_os_teardown();
    }

    /// Test-only: count of entries in `self.windows`. Used by the
    /// `new_window_*` regression tests to assert that a real drain
    /// would change the windows-map cardinality (the post-drain
    /// state itself requires an `ActiveEventLoop`).
    ///
    /// the shadow main entry inserted by
    /// [`Self::do_resumed`] is excluded so existing call sites that
    /// expected this to be "number of torn-out child terminal windows"
    /// keep meaning "number of torn-out child terminal windows".
    #[doc(hidden)]
    pub fn __test_windows_len(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// Test-only: install a synthetic `drag_target` so the
    /// cross-window-merge gate can be exercised without driving a
    /// live winit cursor through `CursorMoved`.
    /// Pure decision used by the CursorMoved tear-out branch: would a
    /// call to `tear_out_tab` right now be a guaranteed no-op (because
    /// we have only one tab AND no cross-window drop target)? Hoisted
    /// out of `tear_out_tab` so the CursorMoved caller can decide
    /// *whether to invoke at all* and, crucially, leave gesture state
    /// (`pressed_tab`, `mouse_down`) intact when the answer is "yes".
    /// Without this gate, the production sequence (lone tab → cursor
    /// crosses tear-out threshold → cursor finally enters another
    /// window's bar) is impossible: the threshold trip would clear the
    /// gesture before the user ever reaches a sibling bar.
    #[doc(hidden)]
    pub fn __test_set_drag_target(
        &mut self,
        target: Option<crate::tab_drag::DropTarget<WindowId>>,
    ) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.drag_target = target;
        }
    }

    /// Test-only: remove a window from `self.windows`
    /// without going through the production teardown paths. Used by
    /// `os_drag_cleanup.rs` to simulate the "window vanished between
    /// snapshot collection and iteration" race that `cancel_drag_session`
    /// tolerates via its `windows.get_mut(...) else { continue }` branch
    /// . Returns `true` if
    /// the window existed and was removed, `false` otherwise.
    #[doc(hidden)]
    pub fn __test_remove_window(&mut self, id: WindowId) -> bool {
        self.release_window_owner(id);
        self.windows.remove(&id).is_some()
    }

    /// Test-only: drop a window without the explicit owner release first.
    ///
    /// [`Self::__test_remove_window`] calls `release_window_owner`, which takes
    /// each pane's owner in the right order before the window is removed. That
    /// hides what the struct does on its own, and the struct has to be right:
    /// a window removed from the map without that call, or held in the map
    /// until the process tears down, closes its owners purely by field drop
    /// order. This models that path.
    #[doc(hidden)]
    pub fn __test_drop_window_without_release(&mut self, id: WindowId) -> bool {
        self.windows.remove(&id).is_some()
    }

    /// Test-only: install a callback
    /// that fires INSIDE [`Self::cancel_drag_session`], AFTER the
    /// `self.windows.keys()` snapshot is collected but BEFORE the
    /// per-id iteration body runs. Lets tests exercise the exact
    /// `get_mut(&id).else { continue }` race-tolerance branch by
    /// removing (or inserting) a window in between.
    #[doc(hidden)]
    pub fn __test_set_post_snapshot_hook<F>(&mut self, f: F)
    where
        F: FnOnce(&mut App) + Send + 'static,
    {
        self.test_post_snapshot_hook = Some(Box::new(f));
    }

    /// How many torn-out child windows are live.
    ///
    /// The shadow main entry is excluded, so this counts only windows a user
    /// tore off rather than every entry in the map.
    #[doc(hidden)]
    pub fn child_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// `1` if the shadow main entry is present in
    /// [`Self::windows`], else `0`. Used by every "count torn-out
    /// child windows" path so they keep the
    /// same number.
    #[inline]
    #[doc(hidden)]
    pub fn shadow_main_count(&self) -> usize {
        match self.main_window_id {
            Some(id) if self.windows.contains_key(&id) => 1,
            _ => 0,
        }
    }

    /// number of windows in the unified
    /// [`Self::windows`] map.
    /// Used by the regression suite to pin the rename + role tagging.
    #[doc(hidden)]
    pub fn unified_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// count entries in [`Self::windows`] whose
    /// role matches the argument. Today every entry is `Terminal`;
    #[doc(hidden)]
    pub fn windows_with_role(&self, role: crate::app::WindowRole) -> usize {
        self.windows
            .iter()
            .filter(|(id, w)| w.role == role && Some(**id) != self.main_window_id)
            .count()
    }

    /// Test-only: seed a synthetic tab with one pane that has no PTY
    /// attached (just a Parser owning a fresh Grid). Lets integration
    /// Read-back of [`Self::main_window_id`] for tests.
    #[doc(hidden)]
    pub fn __test_main_window_id(&self) -> Option<WindowId> {
        self.main_window_id
    }

    // ShadowMainSnapshot helpers deleted — dpi + hovered_url
    // now live exclusively on WindowState.

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

    /// tests exercise tab/pane bookkeeping without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab(&mut self, title: &str) -> u64 {
        // ensure the synthetic main WindowState
        // entry exists before seeding. Future PRs B2b/c/d delete the
        // App.tabs/tab_states/panes fields outright, so seed writes
        // MUST land in `self.main_mut()` to survive that migration.
        self.__test_synthetic_main();
        let pane_id = next_pane_id();
        let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
            Grid::new(80, 24),
            None,
            Arc::clone(&self.capture_staging_pool),
        )));
        let media_pool = Arc::clone(&self.inline_media_pool);
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, PaneState::new_with_media_pool(parser, None, &media_pool));
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        pane_id
    }

    /// for tests that build an `App` without
    /// `do_resumed` running, insert a synthetic main `WindowState`
    /// entry (window=None, renderer=None) under a stable synthetic
    /// `WindowId` so test seeders can route writes through
    /// [`Self::main_mut`]. No-op if `main_window_id` is already set.
    /// In production [`Self::do_resumed`] detects the synthetic entry
    /// and removes it before inserting the real one.
    #[doc(hidden)]
    pub fn __test_synthetic_main(&mut self) {
        if self.main_window_id.is_some() {
            // When: `main_window_id` is already set, so seeding a second entry
            // would displace the identity live state is keyed by.
            return;
        }
        let id = synthetic_main_window_id();
        let ws = WindowState {
            // Registered when the window is inserted.
            owner: None,
            role: WindowRole::Terminal,
            custom_window_name: String::new(),
            window: None,
            renderer: None,
            tabs: TabBar::new(),
            tab_states: Vec::new(),
            panes: HashMap::new(),
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
            pty_pressed_keys: HashMap::new(),
            last_render: Instant::now(),
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
            path_probe: path_target::PathProbeState::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.insert_window_registered(id, ws);
        self.main_window_id = Some(id);
    }

    /// tests exercise tab/pane bookkeeping with a reply-capable parser but
    /// without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab_with_reply(
        &mut self,
        title: &str,
    ) -> (u64, crossbeam_channel::Receiver<Vec<u8>>) {
        self.__test_synthetic_main();
        let pane_id = next_pane_id();
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
            Grid::new(80, 24),
            Some(tx),
            Arc::clone(&self.capture_staging_pool),
        )));
        let media_pool = Arc::clone(&self.inline_media_pool);
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, PaneState::new_with_media_pool(parser, None, &media_pool));
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        (pane_id, rx)
    }

    /// Test-only: seed an existing synthetic pane parser with the app's
    /// current theme defaults. Mirrors the production spawn path without
    /// requiring a live PTY or reply-forwarder thread.
    #[doc(hidden)]
    pub fn __test_seed_pane_theme_colors(&mut self, pane_id: u64) -> bool {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so there is no parser whose
            // theme reply slots could be seeded.
            return false;
        };
        let mut parser = pane.parser.lock();
        seed_parser_theme_colors(&mut parser, &self.theme);
        true
    }

    /// Test-only: feed bytes into an existing pane parser. Used by integration
    /// tests that need to assert reply bytes from the real pane parser.
    // Ordering: keyboard_input publishes the complete Relaxed snapshot, with no dependent memory reads.
    #[doc(hidden)]
    pub fn __test_advance_pane_parser(&self, pane_id: u64, bytes: &[u8]) -> bool {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so the `bytes` have no parser
            // to advance and are dropped rather than misrouted.
            return false;
        };
        let mut parser = pane.parser.lock();
        parser.advance(bytes);
        pane.keyboard_input.store(parser.keyboard_input_snapshot(), Ordering::Relaxed);
        true
    }

    /// Test-only: read-only access to the internal panes map so tests
    /// can assert "this pane id is gone after detach".
    #[doc(hidden)]
    pub fn __test_pane_ids(&self) -> Vec<u64> {
        self.main().map(|ws| ws.panes.keys().copied().collect()).unwrap_or_default()
    }

    /// Test-only: read a pane's current `viewport_top_abs`. Used
    /// scrollback-scroll wiring tests to assert wheel + Scroll-keymap
    /// dispatch actually mutates the canonical field.
    #[doc(hidden)]
    pub fn __test_pane_viewport_top_abs(&self, pane_id: u64) -> Option<Option<u64>> {
        self.main()?.panes.get(&pane_id).map(|p| p.viewport_top_abs)
    }

    /// Test-only: synthesize scrollback by feeding `n` numbered lines and
    /// returns the resulting `scrollback_len()`. Each line is 4 chars +
    /// CRLF so callers can predict the row count.
    #[doc(hidden)]
    pub fn __test_grow_pane_scrollback(&self, pane_id: u64, n: u32) -> u64 {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so no scrollback was grown and
            // the reported row count is zero.
            return 0;
        };
        let mut buf = Vec::with_capacity((n as usize) * 8);
        for i in 0..n {
            use std::io::Write;
            let _ = write!(&mut buf, "{:04}\r\n", i % 10_000);
        }
        let mut parser = pane.parser.lock();
        parser.advance(&buf);
        parser.grid().scrollback_len() as u64
    }

    /// Test-only: viewport rows of a pane.
    #[doc(hidden)]
    pub fn __test_pane_viewport_rows(&self, pane_id: u64) -> Option<u16> {
        let pane = self.main()?.panes.get(&pane_id)?;
        Some(pane.parser.lock().grid().rows)
    }

    /// Test-only: current grid size for a pane.
    #[doc(hidden)]
    pub fn __test_pane_grid_size(&self, pane_id: u64) -> Option<(u16, u16)> {
        let pane = self.main()?.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        Some((grid.cols, grid.rows))
    }

    /// Test-only: id of the active pane in a given tab. Returns `None`
    /// when `tab_idx` is out of range. Used by `split_focus.rs` to
    /// assert that splitting a pane plus the click-to-focus path
    /// actually flips the focused leaf.
    #[doc(hidden)]
    pub fn __test_active_pane_in_tab(&self, tab_idx: usize) -> Option<u64> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.active_pane)
    }

    /// Test-only: set the active pane in `tab_idx` to `pane_id`. The
    /// click-to-focus logic in `window_event.rs` is the production
    /// caller; tests exercise the same state transition without
    /// driving a synthetic winit `MouseInput` event.
    #[doc(hidden)]
    pub fn __test_set_active_pane(&mut self, tab_idx: usize, pane_id: u64) -> bool {
        if let Some(st) = self.main_tab_states_mut().and_then(|ts| ts.get_mut(tab_idx)) {
            st.active_pane = pane_id;
            true
        } else {
            // When: `main_tab_states_mut` resolves no entry at `tab_idx`, so no
            // tab exists whose focus could be repointed.
            false
        }
    }

    /// Test-only: drive `split_active(Direction::Right)`. Mirrors the
    /// `Action::SplitRight` dispatch but skips the `Action` round-trip.
    #[doc(hidden)]
    pub fn __test_split_active_right(&mut self) {
        self.split_active(sonicterm_cfg::keymap::Direction::Right);
    }

    /// Test-only: tab count.
    #[doc(hidden)]
    pub fn __test_tab_count(&self) -> usize {
        self.main_tabs().map(|t| t.len()).unwrap_or(0)
    }

    /// Test-only: pending OS-drag payload count.
    #[doc(hidden)]
    pub fn __test_pending_os_drag_payload_count(&self) -> usize {
        self.pending_os_drag_payloads.len()
    }

    /// Test-only: drain queued OS-drag payloads after a synthetic main has
    /// been inserted. Mirrors the production `do_resumed` drain point without
    /// constructing a real winit window.
    #[doc(hidden)]
    pub fn __test_drain_pending_os_drag_payloads(&mut self) {
        self.drain_pending_os_drag_payloads();
    }

    /// Test-only: number of leaf panes in the given tab. Returns
    /// `None` when the tab index is out of range. Used by the
    /// `close_pane_or_tab_semantics` regression suite to assert that
    /// `Action::CloseActivePaneOrTab` shrinks the active tab's pane
    /// tree rather than the tab bar when the tab still has > 1 pane.
    #[doc(hidden)]
    pub fn __test_pane_count_in_tab(&self, tab_idx: usize) -> Option<usize> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.tree.leaves().len())
    }

    /// Test-only: install an `OsDragSink` so [`Self::try_os_drag_handoff`]
    /// can be exercised without going through the platform entry point.
    #[doc(hidden)]
    pub fn __test_set_os_drag_sink(&mut self, sink: Arc<dyn crate::os_drag::OsDragSink>) {
        self.os_drag_sink = Some(sink);
    }

    /// Install the platform OS handoff backend. `sonicterm-mac` supplies its
    /// pasteboard publisher; `sonicterm-windows` supplies OLE `DoDragDrop`.
    /// Tests use it via
    /// [`Self::__test_set_os_drag_backend`] to inject a mock.
    #[doc(hidden)]
    pub fn set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// Test-only: install a mock [`os_drag::OsTabDragBackend`].
    #[doc(hidden)]
    pub fn __test_set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// Test-only: hand out the shared pending-outcome mailbox
    /// so tests can drive [`Self::handle_os_drag_ended`] without
    /// constructing a real [`winit::event_loop::EventLoopProxy`].
    #[doc(hidden)]
    pub fn __test_os_drag_pending(&self) -> Arc<os_drag::PendingDragOutcome> {
        self.os_drag_pending.clone()
    }

    /// Test-only: seed the in-flight source bookkeeping that
    /// [`Self::begin_os_tab_drag`] normally sets. Used by tests that
    /// drive the dispatcher directly without first calling
    /// `begin_os_tab_drag`.
    #[doc(hidden)]
    pub fn __test_set_os_drag_source(&mut self, source: Option<(WindowId, usize)>) {
        self.os_drag_source = source
            .and_then(|(window, index)| self.tab_id_at(window, index).map(|tab| (window, tab)));
    }

    /// build an [`os_drag::AppHandle`] tied to the App's
    /// event-loop proxy and the shared pending-outcome mailbox. The
    /// returned handle is what gets passed to
    /// [`os_drag::OsTabDragBackend::begin_session`] so the backend can
    /// post `DragMoved` / `DragEnded` events back to the main loop.
    ///
    /// Returns `None` when no event-loop proxy has been wired. In that
    /// case the OS drag is not startable, which the caller treats as
    /// "fall back to the existing within-process tear_out path".
    pub fn os_drag_app_handle(&self) -> Option<os_drag::AppHandle> {
        self.event_loop_proxy.clone().map(|p| {
            os_drag::AppHandle::with_pending_and_bars(
                p,
                self.os_drag_pending.clone(),
                self.os_drag_bars.clone(),
            )
            .with_main_window(self.main_window_id)
        })
    }

    /// Hand out an `Arc` clone of the shared [`os_drag::TabBarRegistry`].
    /// Platform glue (e.g. `sonicterm-windows::os_drag_win`) calls this to
    /// stash a reference for use inside the OLE IDropTarget::Drop
    /// callback, where the AppHandle isn't always available.
    pub fn os_drag_bar_registry(&self) -> Arc<os_drag::TabBarRegistry> {
        self.os_drag_bars.clone()
    }

    /// Publish the current tab bar layout for `window` into the shared
    /// registry. Called from the App's per-frame render path with
    /// already-resolved screen coordinates (caller is responsible for
    /// converting logical-px / window-local to screen via
    /// winit's `Window::outer_position`).
    pub fn publish_os_drag_bar_snapshot(&self, snapshot: os_drag::TabBarSnapshot) {
        self.os_drag_bars.publish(snapshot);
    }

    /// Convenience: build a [`os_drag::TabBarSnapshot`] from the main
    /// window's current geometry + tab bar and publish it. No-op if the
    /// main window or renderer aren't yet initialized (pre-`resumed`).
    /// Called from the per-frame `RedrawRequested` handler so the
    /// snapshot registry tracks every visible tab-bar state change.
    pub(super) fn publish_main_window_tab_bar(&self) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(w) = self.main_window() else {
            // When: `main_window` does not exist yet, so there is no surface whose
            // tab-bar geometry could be published.
            return;
        };
        let Some(r) = self.main_renderer() else {
            // When: `main_renderer` is absent, so tab-bar height and insets cannot
            // be measured and any published rect would be invented.
            return;
        };
        let inner_origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = w.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let empty_tabs_pub = sonicterm_ui::tabs::TabBar::new();
        let layout = TabBarLayout::compute_with_height(
            self.main_tabs().unwrap_or(&empty_tabs_pub),
            raster_w,
            r.tab_bar_logical_height(),
        )
        .with_top_offset(r.tab_bar_y_offset())
        .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(w.id()), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    /// Remove a window's snapshot from the registry (called on window
    /// close). Safe to call with `None` (matches main-window convention).
    pub fn remove_os_drag_bar_snapshot(&self, window: Option<WindowId>) {
        self.os_drag_bars.remove(window);
    }

    /// Publish the tab bar snapshot for the child window keyed by `id`.
    /// No-op if the child isn't found. Called from the child's redraw
    /// path right after `Renderer::render`.
    pub fn publish_child_window_tab_bar(&self, id: WindowId) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(child) = self.windows.get(&id) else {
            // When: `id` tracks no window, so there is no child tab bar to
            // publish geometry for.
            return;
        };
        let Some(win) = child.window.as_ref() else {
            // When: this `child` has no live window, so screen geometry cannot be
            // measured and the snapshot would carry stale coordinates.
            return;
        };
        let inner_origin = win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = win.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let Some(r) = child.renderer.as_ref() else {
            // When: this `child` has no renderer, so tab-bar height is unknown and
            // the layout below has no metrics to compute against.
            return;
        };
        let layout =
            TabBarLayout::compute_with_height(&child.tabs, raster_w, r.tab_bar_logical_height())
                .with_top_offset(r.tab_bar_y_offset())
                .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(id), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    fn finish_tab_drag(
        &mut self,
        session: crate::tab_drag::DragSession<WindowId>,
        action: crate::tab_drag::DragAction<WindowId>,
        tear_out: impl FnOnce(&mut Self, WindowId, usize),
    ) -> bool {
        let Some(index) = self.tab_index_of_id(session.source_window, session.source_tab) else {
            // When: `session.source_tab` is absent from `source_window`, cancel rather than moving its former slot's occupant.
            self.cancel_drag_session();
            return false;
        };
        match action {
            crate::tab_drag::DragAction::ReturnToOriginalBar => {
                // When: `ReturnToOriginalBar` keeps the captured tab in place, no topology mutation is owed.
            }
            crate::tab_drag::DragAction::ReorderTab { to } => {
                if let Some(window) = self.windows.get_mut(&session.source_window) {
                    window.reorder_tab(index, to);
                }
            }
            crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                // When: MergeIntoWindow selects target, preserve the transfer refusal instead of reporting a completed drag.
                let moved = if self.main_window_id == Some(session.source_window) {
                    self.merge_main_into_child(index, target)
                } else {
                    // When: session.source_window is not main_window_id, keep child-specific source close policy.
                    self.merge_child_into_target(session.source_window, index, target)
                };
                if !moved {
                    // When: moved is false, source custody is restored and the gesture must report no movement.
                    return false;
                }
            }
            crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                tear_out(self, session.source_window, index);
            }
        }
        if let Some(window) = self.windows.get(&session.source_window) {
            window.request_redraw();
        }
        true
    }

    /// begin an OS-level tab drag session via the installed
    /// backend. Returns `true` when the backend was invoked, `false`
    /// when no backend is installed or no event-loop proxy exists (in
    /// which case the caller falls back to the existing tear_out path).
    ///
    /// Captures the stable tab id before the native backend can process queued topology changes.
    pub fn begin_os_tab_drag(
        &mut self,
        source_window: WindowId,
        source_tab_idx: usize,
        payload_json: String,
        drag_image_png: Vec<u8>,
    ) -> bool {
        let Some(source_tab) = self.tab_id_at(source_window, source_tab_idx) else {
            // When: `source_tab_idx` no longer names a live tab, refuse before the native backend takes the gesture.
            return false;
        };
        let Some(handle) = self.os_drag_app_handle() else {
            // When: no `os_drag_app_handle` can be built, so the platform has no
            // drag context and recording a source would strand it.
            return false;
        };
        let Some(backend) = self.os_drag_backend.as_mut() else {
            // When: no `os_drag_backend` is installed, so nothing can carry the
            // session and the source must not be recorded.
            return false;
        };
        self.os_drag_source = Some((source_window, source_tab));
        backend.begin_session(handle, source_window, source_tab_idx, payload_json, drag_image_png);
        true
    }

    /// does the installed backend own the gesture end-to-end?
    /// `try_os_drag_handoff` consults this to decide whether to skip
    /// the legacy `OsDragSink` after `begin_os_tab_drag` returns —
    /// running both on Windows would invoke `DoDragDrop` twice.
    pub fn os_drag_backend_handles_full_gesture(&self) -> bool {
        self.os_drag_backend.as_ref().map(|b| b.handles_full_gesture()).unwrap_or(false)
    }

    pub(super) fn owns_native_drop_target(&self) -> bool {
        self.os_drag_backend.as_ref().is_some_and(|backend| backend.owns_native_drop_target())
    }

    pub(super) fn native_drop_attributes(&self, attrs: WindowAttributes) -> WindowAttributes {
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_drag_and_drop(!self.owns_native_drop_target())
        }
        #[cfg(not(windows))]
        {
            attrs
        }
    }

    /// Register the selected native drop owner before revealing a window or transferring live panes.
    pub fn register_window_with_os_drag_backend(
        &mut self,
        window_id: WindowId,
        window: &std::sync::Arc<winit::window::Window>,
    ) -> Result<(), String> {
        if self.os_drag_backend.is_none() {
            // When: os_drag_backend is absent, winit retains its default file-drop ownership.
            return Ok(());
        }
        let handle = self
            .os_drag_app_handle()
            .ok_or_else(|| "native drop event loop unavailable".to_string())?;
        self.os_drag_backend.as_mut().unwrap().register_window(handle, window_id, window)
    }

    pub(super) fn release_child_window_registries(&mut self, window_id: WindowId) {
        self.cancel_window_rename(window_id);
        self.cancel_tab_edit(window_id);
        self.pending_redraw_windows.remove(&window_id);
        if let Some(workers) = &self.path_workers {
            // A closed window leaves no waiting probe; one already executing is
            // discarded on arrival.
            workers.cancel_window(window_id);
        }
        self.window_keys.remove(window_id);
        self.os_drag_bars.remove(Some(window_id));
        if let Some(backend) = self.os_drag_backend.as_mut() {
            if let Err(error) = backend.unregister_window(window_id) {
                // Failed revocation leaves native custody with the backend for its final cleanup.
                tracing::error!(?window_id, %error, "native drop-target release failed");
            }
        }
    }

    /// dispatcher entry point for `UserEvent::DragMoved`.
    /// Drains the mailbox; currently a no-op beyond logging — the
    /// drag-chip overlay is rendered from `tab_drag` state, not from
    /// the OS cursor stream. Reserved for future "highlight drop
    /// target in destination bar" feedback.
    pub fn handle_os_drag_moved(&mut self) -> Option<(i32, i32)> {
        let pos = self.os_drag_pending.take_moved();
        if let Some(p) = pos {
            tracing::trace!(?p, "os_drag_session: cursor moved");
        }
        pos
    }

    /// dispatcher entry point for `UserEvent::DragEnded`.
    /// Drains the mailbox outcome and routes it: `DroppedOnBar` →
    /// [`Self::transfer_tab`]; `Cancelled` → [`Self::cancel_drag_session`];
    /// `DroppedOnEmpty` is left for the existing tear_out path (this
    /// dispatcher just clears the in-flight bookkeeping). Returns the
    /// outcome that was processed for tests to assert on.
    pub fn handle_os_drag_ended(&mut self) -> Option<os_drag::DragOutcome> {
        let outcome = self.os_drag_pending.take_ended()?;
        let source = self.os_drag_source.take().and_then(|(window, tab)| {
            self.tab_index_of_id(window, tab).map(|index| (window, index, tab))
        });
        match outcome {
            os_drag::DragOutcome::DroppedOnBar { target_window, target_slot } => {
                // When: the drop landed on a bar, so `target_window` and
                // `target_slot` name where the dragged tab should be inserted.
                let Some((src_win, src_idx, _)) = source else {
                    // When: no `source` was recorded, so there is no tab to move
                    // and the stale drag state is cancelled instead.
                    tracing::warn!(
                        "os_drag_session: DroppedOnBar arrived with no recorded source — cancelling"
                    );
                    self.cancel_drag_session();
                    return Some(outcome);
                };
                // `source` / `target` are `Option<WindowId>`, where
                // `None` means "the App's main window". The
                // backend always reports a concrete WindowId on the
                // source side, but the *target* may legitimately be the
                // main window. Detect that by comparing against the
                // App's `window` field.
                let src_opt = (self.main_window_id != Some(src_win)).then_some(src_win);
                let tgt_opt = target_window.filter(|id| Some(*id) != self.main_window_id);
                if let Err(e) = self.transfer_tab(src_opt, src_idx, tgt_opt, target_slot) {
                    tracing::warn!(?e, "os_drag_session: transfer_tab refused — cancelling");
                    self.cancel_drag_session();
                }
            }
            os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos } => {
                tracing::debug!(
                    ?drop_screen_pos,
                    "os_drag_session: DroppedOnEmpty — in-process tear-out"
                );
                // replace the legacy
                // out-of-process tear-out (child-window via
                // `spawn_tearout_child` → `Command::new`) with an
                // in-process create. Enqueue a typed `PendingTearOut`
                // request carrying the recorded source tab handle and
                // the Win32 cursor screen position; the next
                // event-loop tick drains it via the existing
                // `drain_pending_window_creates` slot, which now
                // builds the child window directly from the reusable
                // helper extracted from `tear_out.rs`.
                if let Some((src_win, src_idx, source_tab)) = source {
                    self.pending_tear_out = Some(PendingTearOut {
                        source_window: src_win,
                        source_tab_idx: src_idx,
                        source_tab_id: Some(source_tab),
                        drop_screen_pos: Some(drop_screen_pos),
                    });
                } else {
                    // When: no source was recorded, so no tab can be torn out and
                    // the drop is logged rather than acted on.
                    tracing::warn!(
                        "os_drag_session: DroppedOnEmpty without recorded source — no tear-out"
                    );
                }
                // Do NOT call `cancel_drag_session` inline
                // here. The `DroppedOnEmpty` path triggers a
                // tear-out-spawn that creates a brand new top-level
                // window via the `pending_new_window` /
                // `pending_tear_out` drain. If we cancel inline,
                // cross-window drag-residue cleanup runs BEFORE the
                // new window exists, racing the spawn and potentially
                // freezing Explorer's drag thread on Windows when the
                // OLE drop-target tear-down sequence overlaps with new
                // HWND creation. Defer cancellation to
                // `drain_pending_os_teardown`, which runs AFTER
                // `drain_pending_window_creates` at the event-loop
                // boundary. Order matters; this flag controls only
                // WHEN cancel runs, not WHETHER — the all-windows
                // loop still runs unconditionally on drain (preserves
                // the `os_drag_cleanup.rs:172-201` idempotence
                // guarantee).
                self.pending_os_teardown = true;
            }
            os_drag::DragOutcome::Cancelled => {
                self.cancel_drag_session();
            }
        }
        Some(outcome)
    }

    /// Test-only: drive the OS-drag handoff path with a forced "cursor
    /// is outside any window" precondition (trivially true in tests
    /// since no winit window is created). Returns the same bool as the
    /// internal implementation: `true` = source-tab was detached,
    /// `false` = source tab preserved.
    #[doc(hidden)]
    pub fn __test_try_os_drag_handoff(&mut self, index: usize) -> bool {
        self.try_os_drag_handoff(index)
    }

    /// Test-only: inspect and mutate the drag-gesture state
    /// (`pressed_tab`, `mouse_down`) so an integration test can
    /// reproduce the production sequence "tab pressed → cursor
    /// crosses tear-out threshold → eventually drops on sibling
    /// window" without needing a live winit `ActiveEventLoop`.
    #[doc(hidden)]
    pub fn __test_pressed_tab(&self) -> Option<usize> {
        self.main().and_then(|ws| ws.pressed_tab)
    }

    /// Test seam: whether the main window is tracking a held mouse button.
    ///
    /// Reports `false` when no main window exists, so a caller sees the same
    /// "nothing held" answer either way.
    #[doc(hidden)]
    pub fn __test_mouse_down(&self) -> bool {
        self.main().map(|ws| ws.mouse_down).unwrap_or(false)
    }

    /// Test seam: set which tab the main window treats as pressed.
    ///
    /// Seeds a synthetic main window first, so a test can drive tab-press
    /// behavior without a live winit window.
    #[doc(hidden)]
    pub fn __test_set_pressed_tab(&mut self, v: Option<usize>) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.pressed_tab = v;
        }
    }

    /// Test seam: set whether the main window holds a mouse button.
    ///
    /// Seeds a synthetic main window first, so drag gestures can be driven
    /// without real pointer events.
    #[doc(hidden)]
    pub fn __test_set_mouse_down(&mut self, v: bool) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.mouse_down = v;
        }
    }

    /// Test-only: borrow the redraw target Arc for a given pane id,
    /// so a test can assert the per-pane redraw indirection survives
    /// state transfers.
    #[doc(hidden)]
    pub fn __test_pane_redraw_target(&self, id: u64) -> Option<Arc<Mutex<Option<WindowId>>>> {
        self.main()?.panes.get(&id).map(|p| p.redraw_target.clone())
    }

    /// Test-only: install or clear a pane's PTY handle so tear-out tests
    /// can verify ownership moves without spawning a real shell.
    #[doc(hidden)]
    pub fn __test_set_pane_pty(&mut self, id: u64, pty: Option<PtyHandle>) -> bool {
        let Some(pane) = self.main_mut().and_then(|ws| ws.panes.get_mut(&id)) else {
            // When: `id` resolves to no pane, so the supplied `pty` has no owner
            // and is dropped instead of installed.
            return false;
        };
        pane.pty = pty;
        true
    }

    /// Test-only: report whether a pane still has a PTY handle.
    #[doc(hidden)]
    pub fn __test_pane_pty_present(&self, id: u64) -> Option<bool> {
        self.main()?.panes.get(&id).map(|pane| pane.pty.is_some())
    }

    /// Read-only accessor used by tests and (eventually) the
    /// renderer to honor the View → Toggle Tab Bar menu item.
    #[doc(hidden)]
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }

    /// cancel an in-flight drag session. Wired
    /// to the ESC key handler in `window_event.rs` (any window's
    /// `WindowEvent::KeyboardInput` with `NamedKey::Escape` clears
    /// the App's drag_session AND every per-window drag_session) so
    /// the gesture is abandoned with the source tab left in place.
    /// Returns `true` if a drag session was actively cleared, `false`
    /// when no drag was in progress.
    #[doc(hidden)]
    pub fn cancel_drag_session(&mut self) -> bool {
        let mut had = false;
        // (defensive): snapshot window-id keys BEFORE the
        // mutation loop. The loop body calls `clear_drag_chip` /
        // `request_redraw`, neither of which mutate `self.windows`
        // today, but a future per-window handler (or a winit reentrant
        // callback on Windows under heavy load) could
        // insert/remove a window mid-iteration. Iterating a snapshot of
        // `Vec<WindowId>` is panic-free and matches intent: cancel
        // residue on the set of windows that exist RIGHT NOW. The
        // all-windows loop runs UNCONDITIONALLY — never short-circuit;
        // `os_drag_cleanup.rs:172-201` asserts this on a re-armed
        // second invocation.
        let ids: Vec<_> = self.windows.keys().copied().collect();
        // Invoke the test-only
        // post-snapshot hook AFTER `ids` is collected but BEFORE the
        // iteration body starts. The `take()` releases the hook so it
        // never re-fires, and (more importantly) leaves no live borrow
        // on `self` — the closure can freely mutate `self.windows`,
        // which is the exact race we need to exercise to prove the
        // `get_mut(&id).else { continue }` arm below fires. Always
        // `None` in production (the setter is `__test_*`-gated).
        if let Some(hook) = self.test_post_snapshot_hook.take() {
            hook(self);
        }
        // clear ALL per-window drag residue, not just
        // drag_session / drag_target. Previously `pressed_tab` and
        // `mouse_down` were only cleared on the main window, and the
        // renderer's `drag_chip` overlay was never cleared by this path
        // at all — so an OS-drag end (which bypasses the normal
        // MouseInput::Released handlers in window_event.rs / child_window.rs
        // that DO clear drag_chip) left a stale grey chip rectangle
        // floating in empty pane space until the next render forced a
        // refresh. Iterate every WindowState (main + children) and wipe
        // the lot.
        for id in ids {
            let Some(ws) = self.windows.get_mut(&id) else {
                // When: `windows` dropped `id` between the snapshot and this
                // iteration, so its residue is already gone with the window.
                continue;
            };
            if ws.drag_session.take().is_some() {
                had = true;
            }
            ws.drag_target = None;
            ws.pressed_tab = None;
            ws.mouse_down = false;
            window_event::cancel_pointer_gesture(&mut ws.pointer_gesture);
            // also abandon any scrollbar/splitter drag residue —
            // a global drag-cancel should leave no gesture half-held on any
            // window, mirroring the focus-loss cleanup.
            ws.scrollbar_drag = None;
            ws.splitter_drag = None;
            // Clear the renderer's persistent
            // drag-chip overlay AND the headless-test marker via a single
            // helper so production and test paths can never diverge. The
            // per-frame emitter keeps drawing
            // whatever Some(_) value sits in the renderer, so leaving it
            // behind ships a stale chip until something else triggers a
            // set_drag_chip(None). For headless test windows the renderer
            // is None — the `test_drag_chip_marker` mirror is what
            // the cleanup tests assert against.
            ws.clear_drag_chip();
            // Force a repaint so the cleared chip actually leaves the
            // screen instead of waiting for the next external event.
            if let Some(w) = ws.window.as_ref() {
                w.request_redraw();
            }
        }
        self.os_drag_handoff_started = false;
        had
    }

    /// Move a live tab transactionally; `None` selects main, and any refusal retains the original source state.
    #[doc(hidden)]
    pub fn transfer_tab(
        &mut self,
        source: Option<WindowId>,
        source_idx: usize,
        target: Option<WindowId>,
        target_idx: usize,
    ) -> Result<(), TransferError> {
        let source = source.or(self.main_window_id).ok_or(TransferError::SourceMissing)?;
        let target = target.or(self.main_window_id).ok_or(TransferError::TargetMissing)?;
        if !self.windows.contains_key(&source) {
            // When: the explicit source disappeared, report its absence before any destination preparation.
            return Err(TransferError::SourceMissing);
        }
        self.validate_transfer_destination(target)?;
        if source == target {
            // When: source equals target, reorder in place without detaching or reattributing its panes.
            let window = self.windows.get_mut(&source).ok_or(TransferError::SourceMissing)?;
            if source_idx >= window.tabs.len() || source_idx >= window.tab_states.len() {
                // When: source_idx exceeds tabs or tab_states, reject before reorder can change a different live tab.
                return Err(TransferError::SourceIndexOutOfBounds);
            }
            window.reorder_tab(source_idx, target_idx);
            return Ok(());
        }
        let origin = if self.main_window_id == Some(source) {
            tear_out::TearOutSource::Main(source)
        } else {
            // When: source is not main_window_id, rollback must restore the child rather than the main strip.
            tear_out::TearOutSource::Child(source)
        };
        let transaction = self
            .detach_for_tear_out(origin, source_idx)
            .ok_or(TransferError::SourceIndexOutOfBounds)?;
        transaction.attach(self, target, target_idx)?;
        self.frontmost_window = Some(target);
        if let Some(window) = self.windows.get(&target).and_then(|state| state.window.as_ref()) {
            window.focus_window();
            window.request_redraw();
        }
        if Some(target) == self.main_window_id && self.main_is_hidden() {
            self.show_main_window();
        }
        let source_empty = self.windows.get(&source).is_some_and(|window| window.tabs.is_empty());
        if source_empty {
            if Some(source) == self.main_window_id {
                self.hide_main_window();
            } else {
                // When: source is not main_window_id, transferred charges are committed and the empty child owner can now close.
                self.reap_empty_child(source);
            }
        } else if Some(source) == self.main_window_id {
            // When: the nonempty source is main_window_id, complete its newly active pane layout.
            self.resize_visible_panes();
        } else if let Some(window) = self.windows.get_mut(&source) {
            // When: a nonempty child source remains live, complete its neighbour layout without replacing focus.
            child_window::resize_visible_panes_in_child(window);
        }
        Ok(())
    }
}

/// Why a transfer rejected the gesture without losing the tab. Returned
/// by [`App::transfer_tab`]. A missing-target attach would otherwise
/// silently drop the detached `PaneState`, killing its child shell via
/// `PtyHandle::Drop`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum TransferError {
    /// `source` was `Some(id)` but the id is not in `App::windows`.
    SourceMissing,
    /// `target` was `Some(id)` but the id is not in `App::windows`.
    TargetMissing,
    /// `source_idx` is beyond the source window's tab vector.
    SourceIndexOutOfBounds,
    /// The destination has no usable presentation geometry yet.
    TargetNotReady,
    /// Active/visible relationships or pane custody are inconsistent.
    InvalidTopology,
    /// Existing charges cannot move to a valid destination owner.
    AccountingRefused,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.do_resumed(el);
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        self.do_user_event(el, event);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        self.do_window_event(el, win_id, event);
    }

    fn new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.do_new_events(_el, cause);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.do_about_to_wait(el);
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Forward to sonicterm-logging so every Cmd+Q / WM_CLOSE /
        // last-window exit lands in sonicterm.log. See
        // `crates/sonicterm-logging/src/exit_trace.rs`.
        sonicterm_logging::record_loop_exiting();
    }
}

#[cfg(test)]
#[path = "effect_cleanup_tests.rs"]
mod effect_cleanup_tests;

#[cfg(test)]
#[path = "native_window_title_tests.rs"]
mod native_window_title_tests;

#[cfg(test)]
#[path = "pty_input_tests.rs"]
mod pty_input_tests;

#[cfg(all(test, any(windows, unix)))]
mod pty_test_support;

#[cfg(all(test, any(windows, unix)))]
#[path = "close_baseline_tests.rs"]
mod close_baseline_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
