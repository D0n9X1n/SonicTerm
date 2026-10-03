//! Per-pane runtime state: the parser, PTY handle, redraw target and command-event
//! queue, plus pane id allocation and hit testing.

use super::*;

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate the next process-unique pane id.
// Ordering: `NEXT_PANE_ID.fetch_add` uses `Relaxed`; each caller needs a distinct
// id, and no other memory is published through this counter.
#[doc(hidden)]
pub fn next_pane_id() -> u64 {
    NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Return the pane whose half-open rectangle contains `(point_x, point_y)`.
pub(super) fn pane_id_at_point(
    rects: &[(u64, sonicterm_ui::pane::Rect)],
    point_x: f32,
    point_y: f32,
) -> Option<u64> {
    rects.iter().find_map(|(id, rect)| rect.contains(point_x, point_y).then_some(*id))
}

/// Per-pane runtime state. The parser is shared with a per-pane VT thread
/// that drains the pty out-channel; the pty handle owns the writer side.
///
/// `redraw_target` identifies the window that owns the pane. The main thread
/// swaps the `WindowId` when a pane migrates to a torn-out child; VT workers
/// read the current id after coalescing and send a typed redraw event. Native
/// window APIs remain confined to the winit event-loop thread.
pub struct PaneState {
    /// Governor charges this pane holds, one per resource class.
    ///
    /// Committed reservations rather than repeated reserve/release: a pane's
    /// retention rises and falls continuously, and a release/re-reserve pair
    /// on every sample opens a window where the ledger disagrees with reality.
    /// `try_grow` and `shrink` move a live charge in place, so the figure is
    /// never briefly wrong.
    ///
    /// Released by `Drop` when the pane is dropped, which is the same property
    /// that made the inline-media charge correct: there is no teardown site to
    /// forget.
    pub(crate) charges: HashMap<ResourceClass, sonicterm_resource::CommittedReservation>,
    /// This pane's owner in the governor hierarchy, below its window's.
    ///
    /// Assigned when the pane is inserted into a window rather than at
    /// construction: `PaneState::new` is called from a dozen sites that have
    /// no governor in scope, and threading one through all of them would be a
    /// larger change than the ownership it establishes.
    ///
    /// Held as an [`OwnerGuard`] so the owner closes when the pane drops.
    /// Declared *after* `charges` deliberately: Rust drops fields in
    /// declaration order, and `finish_close` refuses an owner that still holds
    /// charges, so the reservations must release first.
    pub(crate) owner: Option<OwnerGuard>,
    pub parser: Arc<Mutex<Parser>>,
    /// Completed nonempty VT batches; travels with this pane across window transfers.
    pub(crate) output_generation: Arc<AtomicU64>,
    /// Last scheduler snapshot acknowledged by this pane's owning event-loop window.
    pub(crate) observed_output_generation: u64,
    /// Capture progress seen at the previous retention sample.
    ///
    /// A media capture holds its staging buffer until its terminator arrives,
    /// and the terminator is not guaranteed to — a killed transfer or dropped
    /// link leaves it pinned until the pane dies. The parser cannot tell that
    /// from a slow transfer, having no clock. The sampler has one, so it
    /// remembers what the capture had received last time and cancels only a
    /// capture that has not moved across consecutive samples.
    pub(crate) last_capture_progress: Option<usize>,
    /// How many consecutive samples have seen `last_capture_progress`
    /// unchanged.
    ///
    /// A count rather than a flag because one unchanged reading proves only
    /// one sample interval of silence, and a transfer merely slower than that
    /// interval reads as stalled — cancelling it costs the user a picture they
    /// were waiting for. Requiring the figure to hold still twice buys a
    /// second interval of evidence, so the threshold is the full
    /// `2 × RETENTION_SAMPLE_INTERVAL` the cancellation reports.
    pub(crate) capture_stall_samples: u8,
    pub pty: Option<PtyHandle>,
    /// Native teardown reservation follows the PTY across transfers and outlives direct PTY drop.
    pub(super) reap_slot: Option<sonicterm_resource::ReapSlot>,
    /// Latest unsent mouse position; fixed storage follows the pane across window transfers.
    pub(super) pending_pointer_motion: PendingPointerMotion,
    /// Whether a resize failure has been reported since the last success.
    pub(crate) resize_warned: std::sync::atomic::AtomicBool,
    pub redraw_target: Arc<Mutex<Option<WindowId>>>,
    /// Absolute row (scrollback-relative) that should appear at the top of
    /// the visible viewport. `None` = "follow the live tail" (default).
    ///
    /// A compatibility projection of the pane's private viewport anchor: it is
    /// rebased as history evicts rows, so a pinned row keeps the same text, and
    /// frames rewrite it before they read it. A direct write is adopted when the
    /// anchor next resolves it, not at assignment: a reader preview resolves it
    /// against the current eviction count, and the next frame or scrollback
    /// reload commits the pin there. Assigning the value it already holds is not
    /// observable; use [`PaneState::pin_viewport_top`] to repin immediately,
    /// even to the same row. The render layer clamps it to the live screen.
    pub viewport_top_abs: Option<u64>,
    /// Eviction identity behind `viewport_top_abs`; see `viewport_anchor`.
    pub(super) viewport_anchor: viewport_anchor::ViewportAnchor,
    /// Cached foreground-process identity/privilege plus the last probe time.
    ///
    /// The probe walks the whole process table, so it must not run on every
    /// render. The 500 ms title-refresh TTL keeps names and Windows elevation
    /// responsive without reviving the measured idle CPU regression.
    pub fg_proc_cache:
        Option<(std::time::Instant, Option<sonicterm_io::proc_info::ForegroundProcess>)>,
    /// Cross-thread queue populated by the VT loop when OSC 133 command
    /// lifecycle markers are parsed for this pane.
    pub command_events: Arc<Mutex<Vec<PaneCommandEvent>>>,
    /// Per-pane DECTCEM cursor-visibility flag (`CSI ?25h/l`). Written
    /// by the VT loop, read by the render path for the active pane.
    /// **Per-pane (not per-window)** so the Arc travels with the pane
    /// when a tab is torn out into a new window — pre-fix the Arc
    /// lived on `WindowState`, so tear-out's destination got a fresh
    /// Arc and the moved pane's VT thread kept writing to an orphaned
    /// AtomicBool that nobody read. Init `true`.
    pub cursor_visible: Arc<std::sync::atomic::AtomicBool>,
    /// Coherent keyboard modes, Kitty flags, and protocol epoch published after each parser batch.
    pub keyboard_input: Arc<AtomicU64>,
    /// Pointer-routing modes (`Parser::pointer_input_snapshot`) published after each parser batch,
    /// so pointer handlers route without taking the parser lock.
    pub pointer_input: Arc<std::sync::atomic::AtomicU8>,
    /// Decoded inline media images captured from terminal protocols.
    pub inline_images: Arc<Mutex<Vec<sonicterm_render_model::InlineImage>>>,
    /// This pane's share of the process-wide inline-media total.
    ///
    /// Co-owned with the pane's VT worker. The worker ends when its shell
    /// exits, but the pane stays on screen with its images, so a charge held
    /// only by the worker would be released while the pixels are still
    /// retained. Held here so the charge is returned when the pane — and with
    /// it the image store — is actually dropped.
    pub(crate) inline_media_charge: media::SharedInlineMediaCharge,
    /// Counter handles shared with this pane's VT worker; `Some` only when the App's gate is on.
    pub(crate) frame_counters: Option<super::frame_counters::PaneFrameCounters>,
}

#[derive(Debug, Clone)]
pub struct PaneCommandEvent {
    pub event: CommandEvent,
    pub at: Instant,
    pub duration: Option<Duration>,
}

impl PaneState {
    /// Build a pane around an existing parser and optional PTY, charging its
    /// inline media to the process-default pool.
    ///
    /// The governor owner is left unset here and assigned when the pane is
    /// inserted into a window, so a pane that is built but never inserted
    /// registers no owner to close.
    #[doc(hidden)]
    pub fn new(parser: Arc<Mutex<Parser>>, pty: Option<PtyHandle>) -> Self {
        Self::new_with_media_pool(parser, pty, &media::InlineMediaPool::process_default())
    }

    /// Build a pane whose inline media charges `media_pool`.
    ///
    /// App pane creators pass the app's pool; a test that measures media
    /// budgets passes a private one, so panes other tests create cannot change
    /// what it observes.
    pub(crate) fn new_with_media_pool(
        parser: Arc<Mutex<Parser>>,
        pty: Option<PtyHandle>,
        media_pool: &Arc<media::InlineMediaPool>,
    ) -> Self {
        let (keyboard_input, pointer_input) = {
            let parser = crate::app::frame_counters::lock_parser(&parser);
            (parser.keyboard_input_snapshot(), parser.pointer_input_snapshot())
        };
        Self {
            // Assigned when the pane is inserted into a window.
            owner: None,
            charges: HashMap::new(),
            parser,
            output_generation: Arc::new(AtomicU64::new(0)),
            observed_output_generation: 0,
            last_capture_progress: None,
            capture_stall_samples: 0,
            pty,
            reap_slot: None,
            pending_pointer_motion: PendingPointerMotion::default(),
            resize_warned: std::sync::atomic::AtomicBool::new(false),
            redraw_target: Arc::new(Mutex::new(None)),
            viewport_top_abs: None,
            viewport_anchor: viewport_anchor::ViewportAnchor::default(),
            fg_proc_cache: None,
            command_events: Arc::new(Mutex::new(Vec::new())),
            cursor_visible: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            keyboard_input: Arc::new(AtomicU64::new(keyboard_input)),
            pointer_input: Arc::new(std::sync::atomic::AtomicU8::new(pointer_input)),
            inline_images: Arc::new(Mutex::new(Vec::new())),
            inline_media_charge: media_pool.new_charge(),
            frame_counters: None,
        }
    }

    /// Decode the pointer-routing modes the VT worker last published, without the parser lock.
    // Ordering: pointer_input loads Relaxed; one store carries all five bits, and pointer events and mode changes are unordered anyway.
    pub(crate) fn pointer_modes(&self) -> sonicterm_vt::vt::PointerModes {
        sonicterm_vt::vt::PointerModes::from_bits(
            self.pointer_input.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Test-only: publish the keyboard and pointer snapshots of `parser`, which the caller
    /// holds, as the VT worker does after a batch.
    // Ordering: keyboard_input and pointer_input store Relaxed self-contained snapshots with no dependent reads.
    #[doc(hidden)]
    pub fn __test_publish_input_modes(&self, parser: &Parser) {
        self.keyboard_input
            .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
        self.pointer_input
            .store(parser.pointer_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
    }

    /// Resize this pane's PTY, reporting the first failure of a failing run.
    ///
    /// The grid is already resized and stays committed when the native call
    /// fails; there is no rollback and no automatic retry.
    // Ordering: `resize_warned` uses `Relaxed`; it gates one log line and guards no other state.
    pub(super) fn resize_pty(&self, pane_id: u64, cols: u16, rows: u16) {
        let Some(pty) = self.pty.as_ref() else {
            // When: `self.pty` is `None`, this pane has no native geometry to resize.
            return;
        };
        let Err(error) = (pty.resize)(cols, rows) else {
            // When: the resize applied, clear the latch so a later failure reports again.
            self.resize_warned.store(false, std::sync::atomic::Ordering::Relaxed);
            return;
        };
        // Report only the first failure of a run; a later success clears the latch.
        if !self.resize_warned.swap(true, std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(target: "sonicterm_app::app", pane_id, cols, rows, %error, "pty resize failed");
        }
    }
}
