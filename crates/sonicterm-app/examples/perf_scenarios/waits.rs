//! Bounded waits whose expiry ends a run as invalid.
//!
//! A managed checkpoint's `.done`, Startup's first presented frame and, in S11, a frame known to
//! show the registered image can each fail to arrive. Each wait is a small pure type here, so its
//! expiry is pinned without an event loop. On Windows S11's image may also never register or never
//! reach the image atlas; those runs end blocked, and every check takes the host as a parameter.

use std::time::{Duration, Instant};

use crate::scenarios::Host;

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
/// `Occluded` state, if any arrived, and `host` decides the likely cause it names.
pub(crate) fn first_present_missing_reason(
    wait: Duration,
    redraws: u64,
    native_occlusion: Option<bool>,
    host: Host,
) -> String {
    // winit reports occlusion only on a change, so a window that opens already hidden sends none.
    let native = match native_occlusion {
        None => "no native occlusion event arrived",
        Some(true) => "the last native event was Occluded(true)",
        Some(false) => "the last native event was Occluded(false)",
    };
    // macOS hides a window on another Space; Windows has none, but presents nothing in a locked session.
    let cause = match host {
        Host::Posix => "a full-screen app on its display, which keeps the window on a hidden Space",
        Host::Windows => "a locked or disconnected session, in which Windows presents no frame",
    };
    // No present does not prove occlusion, even after Occluded(false), so the run only suspects it.
    format!(
        "no frame presented within {} s of the window opening ({redraws} RedrawRequested; {native}), so the run is treated as a suspected occlusion; the likely cause is {cause}",
        wait.as_secs()
    )
}

/// How long after S11's image phase starts the scan may take to see the image registered on
/// Windows, the same bound the first frame gets.
pub(crate) const IMAGE_REGISTER_WAIT: Duration = FIRST_PRESENT_WAIT;

/// Why a role pane's program exiting ends the run as invalid, naming the pane and how it exited;
/// `None` once the run has `finished`, since panes then exit as the session tears down.
pub(crate) fn pane_exit_reason(
    finished: bool,
    pane_id: u64,
    was_clean: Option<bool>,
) -> Option<String> {
    if finished {
        // When: the run already has its outcome, an exit is teardown, not a lost workload.
        return None;
    }
    let how = match was_clean {
        Some(true) => "exited cleanly",
        Some(false) => "exited uncleanly",
        None => "ended with its exit status unknown",
    };
    Some(format!("pane {pane_id}'s program {how} before the run finished, so its role's workload did not run"))
}

/// Why `pane_id`'s program exiting invalidates the run, or `None`. The exit counts when the pane
/// is one of `role_panes`, or when it came before startup recorded the first role pane, since only
/// a role's pane exists then; after startup a pane outside the plan claims no role. Once the run has
/// `finished`, no exit counts.
pub(crate) fn role_exit_reason(
    pane_id: u64,
    was_clean: Option<bool>,
    role_panes: &[u64],
    startup_ended: bool,
    finished: bool,
) -> Option<String> {
    if startup_ended && !role_panes.contains(&pane_id) {
        // When: after startup a pane outside the roles exits, it carried no workload.
        return None;
    }
    pane_exit_reason(finished, pane_id, was_clean)
}

/// What S11's image phase has decided so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImageVerdict {
    /// Nothing is decided yet.
    Waiting,
    /// A frame known to show the image presented.
    Valid,
    /// The run cannot measure the image on this host; the reason names why.
    Blocked(String),
    /// No frame known to show the image presented in time.
    Invalid(String),
}

/// S11's verdict at `now`: whether the scan has seen the image `registered` by `register_deadline`,
/// whether the image atlas grew since startup (`None` when unread), and the present wait's `progress`.
///
/// On Windows an image not registered by its deadline is blocked, and a registered image the atlas
/// did not take is blocked before an expired present can call the run invalid. macOS only waits on
/// the present, as before.
pub(crate) fn image_verdict(
    host: Host,
    registered: bool,
    register_deadline: Instant,
    atlas_grew: Option<bool>,
    progress: ImageProgress,
    now: Instant,
) -> ImageVerdict {
    let windows = host == Host::Windows;
    if windows && !registered && now >= register_deadline {
        // When: ConPTY dropped or broke the OSC 1337 sequence, nothing will ever register.
        return ImageVerdict::Blocked(format!(
            "OSC 1337 image not registered within {} s of the image phase starting",
            IMAGE_REGISTER_WAIT.as_secs()
        ));
    }
    if windows && progress != ImageProgress::Waiting && atlas_grew == Some(false) {
        // When: a frame decided the phase but the image atlas never grew, the image was not drawn.
        return ImageVerdict::Blocked(
            "OSC 1337 image not promoted: the image registered, but the image atlas did not grow"
                .to_owned(),
        );
    }
    match progress {
        ImageProgress::Presented => ImageVerdict::Valid,
        ImageProgress::Expired => ImageVerdict::Invalid(format!(
            "the scan saw the image registered, but no frame presented within {} s after it, so no frame is known to show the image",
            IMAGE_PRESENT_WAIT.as_secs()
        )),
        ImageProgress::Waiting => ImageVerdict::Waiting,
    }
}

/// Whether the cover expects an occlusion change and delivers it synthetically when none arrives:
/// macOS reports occlusion natively, Windows reports none, so there the App gets no synthetic one.
pub(crate) fn occlusion_wait_applies(host: Host) -> bool {
    host == Host::Posix
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
