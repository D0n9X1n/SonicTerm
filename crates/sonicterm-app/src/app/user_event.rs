//! Typed messages delivered to the one App event-loop owner.

use super::*;

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
    /// Launch requests were queued by a platform boundary: terminals, direct commands or
    /// script drafts. The name is kept from when only scripts were queued, because the perf
    /// harness compiles against older trees that match on it.
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
    /// An assistive technology asked a terminal window's adapter for its tree, an action, or to stop.
    Accessibility {
        /// The window whose adapter asked.
        window_id: WindowId,
        /// What it asked.
        request: AccessibilityRequest,
    },
    /// Request a frame of this window unconditionally, after command maintenance. The
    /// application itself no longer sends it; harnesses and tests do.
    RequestRedraw(WindowId),
    /// A window's display link fired. Accepted only while `generation` is the window's running link
    /// interval and a link-paced admission is pending.
    DisplayLinkTick {
        /// The window whose link fired.
        window_id: WindowId,
        /// The link generation the native target read when it fired.
        generation: u64,
        /// The display's target time for the frame, for diagnostics only.
        target: Instant,
    },
    /// A VT worker flushed output for this pane, which `window_id` held when it was sent. At
    /// most one is outstanding per pane; the event loop acknowledges it and requests a frame
    /// only when the pane's window shows new output or changed chrome.
    PaneOutput {
        /// The pane's redraw target when the event was sent.
        window_id: WindowId,
        /// The pane whose output was flushed.
        pane_id: u64,
    },
    /// A previously-deferred font fallback family finished loading in the
    /// `sonicterm_text::async_fallback` background thread. The handler walks
    /// every live window's `GpuRenderer`, calls `clear_shape_cache()` (which
    /// bumps `style_rev` and drops the shape / row / line caches), and issues
    /// `window.request_redraw()` so the next frame re-shapes through the newly
    /// available face and the user's tofu cells get replaced by real glyphs.
    ClearShapeCache,
    /// The foreground-probe worker stored results, or stopped; drain its result map.
    ForegroundProbeReady,
    /// The font catalog worker enumerated the installed monospace families.
    FontCatalogReady {
        /// Installed monospace families, sorted.
        families: Vec<String>,
    },
    /// A font fallback worker published faces for `notice_id`, the notice of the body stack in
    /// `window_id`'s renderer. The handler acknowledges it and requests a frame only when that
    /// generation is not applied yet; an event for a replaced notice does nothing.
    FontFallbackReady {
        /// The window whose renderer installed the waker.
        window_id: WindowId,
        /// The notice that completed.
        notice_id: u64,
    },
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
        /// How that child ended. `Unknown` is not a crash — it holds the
        /// pane open, the same as an unclean exit.
        status: sonicterm_types::PaneExitStatus,
        /// `status` as a clean flag: `Some(true)` for code 0, `Some(false)` for any other code or a
        /// signal, `None` when unknown. Kept because the perf harness also builds against older
        /// trees, whose event carries only this field.
        was_clean: Option<bool>,
    },
    /// A launcher's command could not start; the message names only the program's file name.
    LaunchFailed {
        /// User-facing explanation of why the command did not run.
        message: String,
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
        /// The pane whose program asked for the write.
        pane_id: u64,
        /// UTF-8 text requested by the terminal application.
        text: String,
    },
    /// One VT batch's OSC 52 requests, counted without payloads, after its writes; refusals
    /// reach the App only through this summary.
    ClipboardAttempts {
        /// The pane whose program sent the requests.
        pane_id: u64,
        /// How many there were and what happened to the last one.
        attempts: spawn_pane::ClipAttempts,
    },
    /// A batch's `OSC 9;4` progress change for the platform progress surfaces; `None` clears.
    ProgramProgress {
        /// The pane whose program reported it.
        pane_id: u64,
        /// The active report, or `None` when the pane's progress ended.
        report: Option<sonicterm_vt::vt::ProgressReport>,
    },
    /// A batch's OSC 9 or OSC 777 notifications: the first one and how many followed it.
    ProgramNotify {
        /// The pane whose program sent them.
        pane_id: u64,
        /// The OSC 777 title, unsanitized; OSC 9 has none.
        title: Option<String>,
        /// The first notification's text, unsanitized.
        body: String,
        /// Notifications in the same batch after the first.
        more: u32,
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
    /// One item of an applied default-handler plan finished; posted from any thread.
    DefaultHandlerResult {
        /// The run the item belongs to; a result for another run is ignored.
        request: u64,
        /// The item's index in that run's plan.
        index: usize,
        /// What happened to it.
        outcome: crate::default_handlers::HandlerOutcome,
    },
    /// A default-handler run's deadline passed; items still missing report as pending.
    DefaultHandlerDeadline {
        /// The run whose deadline passed.
        request: u64,
    },
    /// The Memory Usage report's worker sampled the OS figures for one refresh.
    MemoryReportSampled {
        /// The refresh this sample belongs to; an older one is dropped.
        generation: u64,
        /// Footprint, threads, and each pane shell's process tree.
        sample: Box<memory_report::OsSample>,
    },
}
