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

/// What the probe does with a requested covered-window trim on this loop turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrimDispatch {
    /// No trim is requested.
    Idle,
    /// The App has not received `Occluded(true)` yet, so the trim waits.
    Wait,
    /// The App is covered: call the hook now, once.
    Call,
    /// The covered hold ended before the App was covered, so the hook is never reached.
    Lapsed,
}

/// Decide a requested trim: it is called on the first turn the App holds `Occluded(true)`, and
/// lapses when the covered hold ends first. Delivery wins over the end of the hold on one turn.
pub(crate) fn trim_dispatch(
    pending: bool,
    delivered: Option<bool>,
    hold_over: bool,
) -> TrimDispatch {
    if !pending {
        TrimDispatch::Idle
    } else if delivered == Some(true) {
        TrimDispatch::Call
    } else if hold_over {
        TrimDispatch::Lapsed
    } else {
        TrimDispatch::Wait
    }
}

/// Whether the cover expects an occlusion change and delivers it synthetically when none arrives:
/// macOS reports occlusion natively, Windows reports none, so there the App gets no synthetic one.
pub(crate) fn occlusion_wait_applies(host: Host) -> bool {
    host == Host::Posix
}

/// What the probe does with one native `CursorMoved` on the measurement window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerArrival {
    /// The first native move before GO, or one at the last native position: the window opened under
    /// a still pointer, so the event is dropped and counted.
    AtRest,
    /// The pointer moved: physical input, which voids the run.
    Moved,
}

/// How a native `CursorMoved` at `position` is treated on `host`, given `last`, the previous native
/// position, and `measuring`, whether GO was written. On Windows a window that opens under a still
/// pointer receives a `CursorMoved` there, so the first one before GO and any at the same position
/// are at rest; macOS treats every one as motion.
pub(crate) fn native_pointer_arrival(
    host: Host,
    last: Option<(f64, f64)>,
    position: (f64, f64),
    measuring: bool,
) -> PointerArrival {
    // Both positions are the whole physical pixels Win32 reports, so exact equality is the test.
    // With no baseline after GO, the move is a pointer entering the window, not the opening's move.
    let still = match last {
        Some(last) => last == position,
        None => !measuring,
    };
    if host == Host::Windows && still {
        // When: on Windows the pointer has not moved since the window opened under it.
        PointerArrival::AtRest
    } else {
        PointerArrival::Moved
    }
}

/// The measurement window's last native pointer position and the moves dropped as at rest. Only
/// native events reach it: the probe dispatches its own synthetic moves to the App directly.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativePointer {
    last: Option<(f64, f64)>,
    rest_dropped: u64,
}

impl NativePointer {
    /// Take one native move to `position` on `host`, `measuring` once GO was written; true when it is
    /// at rest, so it is dropped and counted. Every native move becomes the baseline for the next.
    pub(crate) fn arrive(&mut self, host: Host, position: (f64, f64), measuring: bool) -> bool {
        let at_rest =
            native_pointer_arrival(host, self.last, position, measuring) == PointerArrival::AtRest;
        self.last = Some(position);
        if at_rest {
            // When: the move is at rest, it is counted for result.json.
            self.rest_dropped += 1;
        }
        at_rest
    }

    /// Native moves dropped because the pointer was at rest.
    pub(crate) fn rest_dropped(&self) -> u64 {
        self.rest_dropped
    }
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
    /// The end of the first presenting dispatch after the sighting.
    presented_at: Option<Instant>,
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

    /// The presented-frame count after a dispatch that ended at `ended`; a count past the
    /// sighting's shows the image, and the first such dispatch's end is kept.
    pub(crate) fn observe_frames(&mut self, frames: u64, ended: Instant) {
        if self.seen.is_some_and(|(_, frames_at_sighting)| frames > frames_at_sighting) {
            self.presented = true;
            self.presented_at.get_or_insert(ended);
        }
    }

    /// The end of the first presenting dispatch after the sighting, if one presented.
    pub(crate) fn presented_at(&self) -> Option<Instant> {
        self.presented_at
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

/// How long after S11/release switches away from its image a frame must present: the media-free barrier.
pub(crate) const MEDIA_FREE_WAIT: Duration = Duration::from_secs(5);

/// How long after S11/release switches back a frame showing the image must present: the reshow barrier.
pub(crate) const RESHOW_WAIT: Duration = Duration::from_secs(10);

/// Where a frame barrier stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarrierProgress {
    /// No qualifying frame yet, and the bound has time left.
    Waiting,
    /// A qualifying frame presented after the act.
    Done,
    /// No qualifying frame within the bound; the run ends invalid.
    Expired,
}

/// A phase that ends at the first frame presented after its act, read from `successful_frame_count`.
/// The baseline is captured at the act itself, so frames presented before it never satisfy it; the
/// reshow barrier also needs the frame's image atlas to hold an item, never just its capacity.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameBarrier {
    act_at: Instant,
    frames_at_act: u64,
    wait: Duration,
    needs_image_item: bool,
    done_at: Option<Instant>,
}

impl FrameBarrier {
    /// Start a barrier at its act, `act_at`, with `frames_at_act` presented so far.
    pub(crate) fn new(
        act_at: Instant,
        frames_at_act: u64,
        wait: Duration,
        needs_image_item: bool,
    ) -> Self {
        Self { act_at, frames_at_act, wait, needs_image_item, done_at: None }
    }

    /// One dispatch ended at `now` with `frames` presented and `image_items` in the image atlas.
    ///
    /// Expiry wins: a qualifying frame observed at or after the bound is ignored, so a redraw that
    /// finishes late cannot meet the barrier before the step loop's expiry check runs.
    pub(crate) fn observe(&mut self, now: Instant, frames: u64, image_items: usize) {
        if now >= self.act_at + self.wait {
            // When: `now` is at or past the barrier's bound, it has already expired; nothing meets it.
            return;
        }
        let qualifying =
            frames > self.frames_at_act && (!self.needs_image_item || image_items >= 1);
        if self.done_at.is_none() && qualifying {
            // When: the first frame after the act qualifies, it ends the barrier at `now`.
            self.done_at = Some(now);
        }
    }

    /// When the qualifying frame presented, once it has.
    pub(crate) fn done_at(&self) -> Option<Instant> {
        self.done_at
    }

    /// The barrier's bound. Only the probe reads it, so it is built where the probe is.
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn wait(&self) -> Duration {
        self.wait
    }

    /// When the harness must wake to expire the barrier; `None` once it is met.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.done_at.is_none().then(|| self.act_at + self.wait)
    }

    /// The barrier's state at `now`: a met barrier never expires.
    pub(crate) fn progress(&self, now: Instant) -> BarrierProgress {
        if self.done_at.is_some() {
            BarrierProgress::Done
        } else if now >= self.act_at + self.wait {
            BarrierProgress::Expired
        } else {
            BarrierProgress::Waiting
        }
    }
}

/// The invalid reason when `phase`'s barrier saw no qualifying frame within `wait` of its act.
pub(crate) fn barrier_expired_reason(phase: &str, wait: Duration) -> String {
    format!(
        "phase {phase}: no qualifying frame presented within {} s of its act, so the run stopped",
        wait.as_secs()
    )
}

/// When a hold of `hold_ms` ends: counted from `anchor`, the end of an earlier phase, when known,
/// else from `started`, so a checkpoint that resolves late between them does not lengthen it.
pub(crate) fn anchored_hold_end(
    anchor: Option<Instant>,
    started: Instant,
    hold_ms: u64,
) -> Instant {
    anchor.unwrap_or(started) + Duration::from_millis(hold_ms)
}

/// The Unix time from which a checkpoint's memory reading reflects `delay` after its anchor.
pub(crate) fn fresh_after_unix_s(anchor_unix_s: f64, delay: Duration) -> f64 {
    anchor_unix_s + delay.as_secs_f64()
}

/// How long a checkpoint's memory sampling may retry, from its first attempt.
pub(crate) const SAMPLE_WINDOW: Duration = Duration::from_millis(500);
/// The least time between two attempts at a checkpoint's memory sample.
pub(crate) const SAMPLE_RETRY: Duration = Duration::from_millis(50);
/// The most attempts at one checkpoint's memory sample.
pub(crate) const SAMPLE_ATTEMPTS: u32 = 10;

/// Where a checkpoint's memory sampling stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SamplingState {
    /// Every attempt so far was partial and another may still be taken.
    Active,
    /// An attempt measured every pane.
    Complete,
    /// Every attempt was partial and the window or the attempt count ran out.
    Exhausted,
}

impl SamplingState {
    /// The spelling `result.json` records.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SamplingState::Active => "active",
            SamplingState::Complete => "complete",
            SamplingState::Exhausted => "exhausted",
        }
    }
}

/// One checkpoint's memory sampling: attempts are taken until one is complete, at most
/// `SAMPLE_ATTEMPTS` of them, `SAMPLE_RETRY` apart, all inside `SAMPLE_WINDOW` from creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CheckpointSampling {
    /// The checkpoint's index; its samples carry it.
    pub(crate) index: usize,
    /// Attempts recorded so far.
    pub(crate) attempts: u32,
    /// When the last attempt was recorded; the creation time before any.
    pub(crate) last_attempt_at: Instant,
    /// Whether the last attempt measured every pane.
    pub(crate) last_attempt_complete: bool,
    /// Creation plus `SAMPLE_WINDOW`; no attempt is issued at or after it.
    pub(crate) deadline: Instant,
    pub(crate) state: SamplingState,
}

impl CheckpointSampling {
    /// A fresh sampling for checkpoint `index`, created at `now`, with its own deadline.
    pub(crate) fn new(index: usize, now: Instant) -> Self {
        CheckpointSampling {
            index,
            attempts: 0,
            last_attempt_at: now,
            last_attempt_complete: false,
            deadline: now + SAMPLE_WINDOW,
            state: SamplingState::Active,
        }
    }
}

/// What the probe should do for a checkpoint's sampling this turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SamplingAction {
    /// Take nothing this turn.
    Idle,
    /// Take attempt `attempt`, from 1.
    Sample { attempt: u32 },
}

/// Decide, before any sample is taken, whether `sampling` takes an attempt at `now`. Expiry is
/// checked before a retry is issued, so however late a turn arrives no attempt falls outside the
/// window. The first attempt is always issued: the probe creates the state at that attempt's own
/// clock read, so it is taken at creation, inside the window.
pub(crate) fn sampling_step(sampling: &mut CheckpointSampling, now: Instant) -> SamplingAction {
    if sampling.state != SamplingState::Active {
        // When: the state is `Complete` or `Exhausted`, the checkpoint takes no more samples.
        return SamplingAction::Idle;
    }
    if sampling.attempts > 0 && now >= sampling.deadline {
        // When: an attempt was taken and `now >= sampling.deadline`, the window is spent; give up.
        sampling.state = SamplingState::Exhausted;
        return SamplingAction::Idle;
    }
    if sampling.attempts > 0 && now < sampling.last_attempt_at + SAMPLE_RETRY {
        // When: the last attempt was under `SAMPLE_RETRY` ago, the next is not yet due.
        return SamplingAction::Idle;
    }
    SamplingAction::Sample { attempt: sampling.attempts + 1 }
}

/// Record an attempt taken at `now` and whether it measured every pane.
pub(crate) fn sampling_record(sampling: &mut CheckpointSampling, complete: bool, now: Instant) {
    sampling.attempts += 1;
    sampling.last_attempt_at = now;
    sampling.last_attempt_complete = complete;
    sampling.state = if complete {
        SamplingState::Complete
    } else if sampling.attempts >= SAMPLE_ATTEMPTS {
        SamplingState::Exhausted
    } else {
        // When: the attempt was partial and attempts remain, sampling stays `Active`.
        SamplingState::Active
    };
}

/// One turn of a checkpoint's sampling at `now`: take the attempt `sampling_step` asks for, if any,
/// with `sample` (given the attempt number, returning whether it measured every pane), and record
/// it. Returns whether an attempt was taken; `sample` is not called otherwise.
pub(crate) fn sampling_turn(
    sampling: &mut CheckpointSampling,
    now: Instant,
    sample: impl FnOnce(u32) -> bool,
) -> bool {
    match sampling_step(sampling, now) {
        SamplingAction::Idle => false,
        SamplingAction::Sample { attempt } => {
            let complete = sample(attempt);
            sampling_record(sampling, complete, now);
            true
        }
    }
}

/// Where a pending checkpoint's footprint wait stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FootprintStatus {
    /// Unmanaged: no footprint is requested.
    Absent,
    /// Waiting for `.done`.
    Pending,
    /// `.done` arrived.
    Answered,
}

/// Whether a checkpoint may move on: its footprint is absent or answered, and its sampling is absent
/// or no longer `Active`.
pub(crate) fn checkpoint_ready(
    footprint: FootprintStatus,
    sampling: Option<&CheckpointSampling>,
) -> bool {
    footprint != FootprintStatus::Pending
        && sampling.is_none_or(|sampling| sampling.state != SamplingState::Active)
}

/// A pending checkpoint's next wake: `poll` after `now` while its footprint is pending, and the next
/// due attempt or the sampling deadline while sampling is `Active`, whichever is soonest. `None`
/// when nothing is pending.
pub(crate) fn checkpoint_wake(
    footprint: FootprintStatus,
    sampling: Option<&CheckpointSampling>,
    now: Instant,
    poll: Duration,
) -> Option<Instant> {
    let footprint_wake = (footprint == FootprintStatus::Pending).then(|| now + poll);
    let sampling_wake = sampling
        .filter(|sampling| sampling.state == SamplingState::Active)
        .map(|sampling| (sampling.last_attempt_at + SAMPLE_RETRY).min(sampling.deadline));
    [footprint_wake, sampling_wake].into_iter().flatten().min()
}

#[cfg(test)]
#[path = "waits_tests.rs"]
mod waits_tests;
