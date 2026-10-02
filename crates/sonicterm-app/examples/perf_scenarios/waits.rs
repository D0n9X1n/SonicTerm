//! Bounded waits whose expiry ends a run as invalid.
//!
//! A managed checkpoint's `.done`, Startup's first presented frame and, in S11, a frame known to
//! show the registered image can each fail to arrive. Each wait is a small pure type here, so its
//! expiry is pinned without an event loop.

use std::time::{Duration, Instant};

/// How long after the measurement window opens its first frame may take to present.
pub(crate) const FIRST_PRESENT_WAIT: Duration = Duration::from_secs(10);

/// How long after the scan sees S11's image registered a frame may take to present it.
pub(crate) const IMAGE_PRESENT_WAIT: Duration = Duration::from_secs(1);

/// Where a managed checkpoint's wait for the comparison script's `.done` stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckpointProgress {
    /// No `.done` yet, and the wait has time left.
    Waiting,
    /// `.done` appeared; `footprint` says whether the footprint file exists beside it.
    Answered { footprint: bool },
    /// No `.done` by the deadline.
    Expired,
}

/// The checkpoint wait's state at `now`, given whether `.done` and the footprint file exist.
pub(crate) fn checkpoint_progress(
    done: bool,
    footprint: bool,
    now: Instant,
    deadline: Instant,
) -> CheckpointProgress {
    // A `.done` found after the deadline was written before it; only the 50 ms poll was late.
    if done {
        CheckpointProgress::Answered { footprint }
    } else if now < deadline {
        CheckpointProgress::Waiting
    } else {
        CheckpointProgress::Expired
    }
}

/// The invalid reason for a checkpoint `stem` whose `.done` did not arrive within `wait`.
pub(crate) fn checkpoint_expired_reason(stem: &str, wait: Duration) -> String {
    format!(
        "checkpoint {stem} got no .done within {} s, so its footprint is missing and the run stopped before the next phase",
        wait.as_secs()
    )
}

/// Startup's bound on the first presented frame, counted from when the measurement window opens.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FirstPresentBound {
    due: Option<Instant>,
}

impl FirstPresentBound {
    /// Start the bound: the measurement window opened at `now`.
    pub(crate) fn window_opened(&mut self, now: Instant) {
        self.due = Some(now + FIRST_PRESENT_WAIT);
    }

    /// When Startup needs its next wake, never later than `run_deadline`. An idle App asks for
    /// no wake of its own, so the bound must wake the loop itself.
    pub(crate) fn wake(&self, run_deadline: Instant) -> Instant {
        self.due.map_or(run_deadline, |due| due.min(run_deadline))
    }

    /// Whether the bound has passed at `now` with no frame `presented`.
    pub(crate) fn expired(&self, now: Instant, presented: bool) -> bool {
        !presented && self.due.is_some_and(|due| now >= due)
    }
}

/// The invalid reason when no frame presented within `wait` of the window opening, after
/// `redraws` main-window `RedrawRequested` dispatches; `native_occlusion` is the last native
/// `Occluded` state, if any arrived.
pub(crate) fn first_present_missing_reason(
    wait: Duration,
    redraws: u64,
    native_occlusion: Option<bool>,
) -> String {
    // winit reports occlusion only on a change, so a window that opens already hidden sends none.
    let native = match native_occlusion {
        None => "no native occlusion event arrived",
        Some(true) => "the last native event was Occluded(true)",
        Some(false) => "the last native event was Occluded(false)",
    };
    format!(
        "the window was occluded during startup: no frame presented within {} s of the window opening ({redraws} RedrawRequested; {native}); the likely cause is a full-screen app on its display, which keeps the window on a hidden Space",
        wait.as_secs()
    )
}

/// Where S11's image phase stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageProgress {
    /// The image is not seen yet, or a frame showing it still has time to present.
    Waiting,
    /// A frame presented after the scan saw the image registered.
    Presented,
    /// No such frame presented within `IMAGE_PRESENT_WAIT`.
    Expired,
}

/// S11: when the scan saw the image registered, and whether a frame presented after that.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ImagePresent {
    /// When the scan first saw the image registered, and the presented-frame count then.
    seen: Option<(Instant, u64)>,
    /// One redraw is owed for the sighting and not yet requested.
    redraw_owed: bool,
    /// A frame presented after the sighting.
    presented: bool,
}

impl ImagePresent {
    /// The scan saw the image registered at `now`, with `frames` presented so far.
    pub(crate) fn saw_registration(&mut self, now: Instant, frames: u64) {
        if self.seen.is_none() {
            self.seen = Some((now, frames));
            // The frame that drew the image may have presented before this scan saw it. One full
            // redraw then guarantees a present known to follow the registration, if any can present.
            self.redraw_owed = true;
        }
    }

    /// Whether the scan has seen the image registered.
    pub(crate) fn seen(&self) -> bool {
        self.seen.is_some()
    }

    /// The presented-frame count after a dispatch; a count past the sighting's shows the image.
    pub(crate) fn observe_frames(&mut self, frames: u64) {
        if self.seen.is_some_and(|(_, frames_at_sighting)| frames > frames_at_sighting) {
            self.presented = true;
        }
    }

    /// Whether to request the owed redraw now; true at most once per sighting.
    pub(crate) fn take_redraw_request(&mut self) -> bool {
        std::mem::take(&mut self.redraw_owed)
    }

    /// When a frame showing the image must have presented, once the image is seen.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.seen.map(|(seen_at, _)| seen_at + IMAGE_PRESENT_WAIT)
    }

    /// The phase's state at `now`. A present is decisive whenever it is checked; only a sighting
    /// with no later present by the deadline expires.
    pub(crate) fn progress(&self, now: Instant) -> ImageProgress {
        if self.presented {
            ImageProgress::Presented
        } else if self.deadline().is_some_and(|deadline| now >= deadline) {
            ImageProgress::Expired
        } else {
            ImageProgress::Waiting
        }
    }
}

#[cfg(test)]
#[path = "waits_tests.rs"]
mod waits_tests;
