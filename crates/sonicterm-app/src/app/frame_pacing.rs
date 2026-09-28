//! Redraw pacing: streaming deferral, PTY burst flushing, the software-render frame
//! period, and lock-contention retries.

use super::*;

/// Vsync coalescing gate shared by the main-window (`window_event.rs`) and
/// torn-out child-window (`child_window.rs`) `RedrawRequested` arms.
///
/// Returns `true` when a `RedrawRequested` should be DEFERRED to the next
/// frame boundary instead of rendering now. A redraw is deferred only when
/// both hold:
/// - it is *streaming-driven* — a fresh PTY burst, or not input-driven at
///   all. A PURE input redraw (`was_dirty` with no concurrent `pty_burst`:
///   resize/selection-drag/IME/theme) renders immediately; gating those adds
///   perceptible latency. Crucially a typing echo is BOTH dirty and
///   a burst, and counts as streaming so it coalesces rather
///   than rendering per echo chunk.
/// - `since_last_render < frame_period` — we already drew inside this vsync
///   window, so another draw now would just burn a frame.
///
/// Extracted as a pure fn so main and child use byte-identical
/// coalescing logic AND it is unit-testable without a winit loop. Deferral
/// is what lets a bursty `ls -al` coalesce to one frame per vsync; on a
/// torn-out child the same gate also stops the render path from busy-spinning
/// and starving the VT thread's parser lock.
#[must_use]
pub fn should_defer_streaming_redraw(
    was_dirty: bool,
    pty_burst: bool,
    software_render: bool,
    since_last_render: std::time::Duration,
    frame_period: std::time::Duration,
) -> bool {
    // A typing echo remains streaming work even while this owner's input cause is pending.
    // `software_render`: on a CPU rasterizer EVERY frame is
    // expensive (full-screen software raster), so even *pure* input redraws
    // are coalesced to the frame cap — fast typing in a TUI like Claude Code
    // would otherwise force a full-screen raster per keystroke and peg the
    // CPU. Costs at most one frame (~33ms) of extra input latency, which is
    // an acceptable trade only because rendering is already slow here. The
    // hardware-GPU path passes `false` and keeps input redraws immediate.
    let streaming = software_render || pty_burst || !was_dirty;
    streaming && since_last_render < frame_period
}

/// Whether coalesced PTY output is due for a redraw.
///
/// Output is held back to batch a burst into one frame; it is released once it
/// has grown past the byte threshold or waited out the latency cap, so a slow
/// trickle still reaches the screen instead of waiting for more bytes.
#[must_use]
pub fn should_flush_pending_pty_redraw(pending_bytes: usize, pending_for: Duration) -> bool {
    pending_bytes >= PTY_REDRAW_FLUSH_BYTES || pending_for >= PTY_REDRAW_MAX_LATENCY
}

/// Effective frame period given the software-render and IME-composing state.
/// On the hardware path this is the monitor period unchanged. On the software
/// path it's the 40 fps cap, dropped lower only while an IME composition is
/// active.
#[must_use]
pub fn effective_frame_period(
    software_render: bool,
    composing: bool,
    monitor_period: Duration,
) -> Duration {
    if software_render && composing {
        SOFTWARE_RENDER_COMPOSE_FRAME_PERIOD
    } else if software_render {
        // When: `software_render` rasterizes on the CPU with no preedit in
        // flight, so the 40 fps cap applies rather than the lower composing one.
        SOFTWARE_RENDER_FRAME_PERIOD
    } else {
        // When: `software_render` is unset, so a real GPU presents and the panel's
        // own refresh governs rather than any CPU-oriented cap.
        monitor_period
    }
}

/// Resolve the effective frame period for the no-GPU case.
///
/// When `degrade` is true the result is [`SOFTWARE_RENDER_FRAME_PERIOD`],
/// whatever the monitor reports. This is an override, not a `max()`: a monitor
/// slower than the cap — a 30 Hz panel in a VM or over RDP, which is where
/// software rendering usually runs — is resolved to the cap too, asking for
/// more frames than the panel presents. That is long-standing and deliberate;
/// the wording is written this way because "clamped to at least" describes a
/// `max()` this function does not perform.
///
/// With `degrade` false the monitor period passes through unchanged, so the
/// hardware-GPU path is untouched.
///
/// `monitor_period` must be the monitor's own period, never a previously
/// resolved value — passing the resolved period back in makes the decision
/// one-way, because a resolution taken while degrading returns the cap.
#[must_use]
pub fn software_render_frame_period(degrade: bool, monitor_period: Duration) -> Duration {
    if degrade {
        SOFTWARE_RENDER_FRAME_PERIOD
    } else {
        // When: `degrade` is unset, so the hardware path presents at whatever
        // cadence the panel reports and nothing here narrows it.
        monitor_period
    }
}

/// Whether to engage the no-GPU degrade path, combining the config mode with
/// runtime detection. `Auto` follows detection; `Force` always
/// degrades; `Off` never does.
#[must_use]
pub fn should_degrade_for_software_render(
    mode: sonicterm_cfg::config::SoftwareRenderMode,
    detected: bool,
) -> bool {
    use sonicterm_cfg::config::SoftwareRenderMode as M;
    match mode {
        M::Auto => detected,
        M::Force => true,
        M::Off => false,
    }
}

impl App {
    /// Preserve pending input and arm a future main-window collection retry without changing frame timestamps.
    #[doc(hidden)]
    pub fn defer_redraw_on_lock_contention(&mut self, was_dirty: bool) {
        if let Some(id) = self.main_window_id {
            self.defer_window_lock_contention(id, was_dirty, Instant::now());
        }
    }

    pub(super) fn defer_window_lock_contention(
        &mut self,
        id: WindowId,
        was_dirty: bool,
        now: Instant,
    ) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` no longer names a window, no retry state may keep the event loop awake.
            return;
        };
        let period = effective_frame_period(
            self.software_render_degrade,
            window.ime.is_composing(),
            window.redraw.monitor_period,
        );
        window.arm_contention_retry(now, period);
        if self.main_window_id == Some(id) {
            self.pending_redraw = true;
        } else {
            // When: `id` is not `main_window_id`, retain the retry in the child's independent wake set.
            self.pending_redraw_windows.insert(id);
        }
        if was_dirty && !window.redraw.input_pending() {
            window.mark_redraw(redraw::RedrawCause::Input);
        }
        window.redraw.deferred = true;
    }

    /// Preserve a child redraw delayed by pacing without extending an existing contention deadline.
    #[doc(hidden)]
    pub fn defer_child_redraw(&mut self, win_id: WindowId, was_dirty: bool) {
        self.pending_redraw_windows.insert(win_id);
        if let Some(window) = self.windows.get_mut(&win_id) {
            if was_dirty && !window.redraw.input_pending() {
                window.mark_redraw(redraw::RedrawCause::Input);
            }
            window.redraw.deferred = true;
        }
    }
}

#[cfg(test)]
#[path = "frame_pacing_tests.rs"]
mod frame_pacing_tests;

#[cfg(test)]
#[path = "redraw_coalescing_tests.rs"]
mod redraw_coalescing_tests;
