//! The per-sample dispatch timeline, harness side: the adapter over the App's arm and take methods, the
//! outer-callback and invocation recorders the App's records are joined against, and the sample lifecycle
//! the probe drives (arm at injection, take at every close).
//!
//! Every call into the App's dispatch-timeline API, every type of it, and the recorders sit behind
//! `#[cfg(perf_dispatch_timeline_api)]`. perf-compare passes that cfg to both sides only when both trees
//! define the API, so the overlaid harness also builds on a tree that predates it; that build compiles the
//! no-call fallback below, whose samples record the timeline unavailable. Recorder buffers are allocated
//! only when the App returns an armed token, and are then reused from sample to sample.
// Each build constructs only its own adapter's outcomes and facade; the rest is exercised by tests alone.
#![allow(dead_code)]

/// Whether this build calls the App's dispatch-timeline API: the effective cfg.
pub(crate) const API_ENABLED: bool = cfg!(perf_dispatch_timeline_api);

/// The cell an appearance witness watches, in the harness's own terms; the adapter converts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WitnessTarget {
    /// The absolute row.
    pub(crate) abs_row: u64,
    /// The column.
    pub(crate) col: u16,
    /// The character expected at that cell.
    pub(crate) character: char,
    /// The scrollback eviction count the row is relative to.
    pub(crate) scrollback_evicted: u64,
    /// The screen epoch.
    pub(crate) screen_epoch: u64,
    /// The size generation.
    pub(crate) size_generation: u64,
}

/// What arming returned, over the token type: the App's own in the adapter, a stand-in in tests.
/// `Unavailable` is this build's own answer when it lacks the cfg; every other variant is the App's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimelineArm<Token> {
    /// This build does not call the API.
    Unavailable,
    /// Armed under this token.
    Armed(Token),
    /// The App records no timeline.
    NotRecorded,
    /// The App's frame-counter gate is off.
    GateOff,
    /// No window holds the pane.
    NoPane,
    /// A timeline is already armed under this token.
    StillArmed(Token),
    /// The App can issue no more tokens.
    Exhausted,
}

/// What take returned, over the timeline type. `Unavailable` is this build's own answer when it lacks the
/// cfg; every other variant is the App's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TimelineTake<Timeline> {
    /// This build does not call the API.
    Unavailable,
    /// The frozen timeline.
    Timeline(Timeline),
    /// The App recorded no timeline for this token.
    NotRecorded,
    /// The App's frame-counter gate is off.
    GateOff,
    /// The App holds another arming, or none.
    Mismatch,
    /// This token's timeline was already transferred.
    AlreadyTaken,
}

/// The adapter's arm outcome over the App's token.
pub(crate) type ArmResult = TimelineArm<api::Token>;
/// The adapter's take outcome over the App's timeline.
pub(crate) type TakeResult = TimelineTake<api::Timeline>;

/// What one sample's timeline came to, recorded on the sample as `dispatch_timeline`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// This build does not call the API.
    Unavailable,
    /// The App records no timeline (arm or take).
    NotRecorded,
    /// The App's frame-counter gate was off (arm or take).
    GateOff,
    /// No window held the pane at arm.
    NoPane,
    /// A timeline was already armed at arm: a harness defect, since every close takes it.
    StillArmed,
    /// The App could issue no more tokens.
    Exhausted,
    /// A timeline was taken and the harness record is complete.
    Recorded,
    /// A timeline was taken but the harness record is incomplete or overflowed.
    RecordedIncomplete,
    /// The App held another arming at take.
    Mismatch,
    /// The token's timeline had already been transferred.
    AlreadyTaken,
}

impl Disposition {
    /// The disposition's name in `result.json`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::NotRecorded => "not-recorded",
            Self::GateOff => "gate-off",
            Self::NoPane => "no-pane",
            Self::StillArmed => "still-armed",
            Self::Exhausted => "exhausted",
            Self::Recorded => "recorded",
            Self::RecordedIncomplete => "recorded-incomplete",
            Self::Mismatch => "mismatch",
            Self::AlreadyTaken => "already-taken",
        }
    }
}

/// The probe's `ApplicationHandler` method an outer callback is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum OuterKind {
    /// `window_event`.
    WindowEvent,
    /// `user_event`.
    UserEvent,
    /// `new_events`.
    NewEvents,
    /// `about_to_wait`.
    AboutToWait,
    /// `resumed`.
    Resumed,
}

/// What a forwarded invocation was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InvocationKind {
    /// A window event; `redraw` when it was `RedrawRequested`.
    WindowEvent {
        /// Whether the event was `RedrawRequested`.
        redraw: bool,
    },
    /// A user event.
    UserEvent,
    /// `new_events`.
    NewEvents,
    /// `about_to_wait`.
    AboutToWait,
    /// `resumed`.
    Resumed,
    /// A direct `App::run_action` call, not an `ApplicationHandler` callback.
    RunAction,
}

/// One forwarded call as the probe describes it: its kind, its window and whether the harness made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Invocation {
    /// What it was.
    pub(crate) kind: InvocationKind,
    /// The target window as `u64::from(WindowId)`; 0 for a kind that names no window.
    pub(crate) window: u64,
    /// Whether the harness generated it.
    pub(crate) synthetic: bool,
}

impl Invocation {
    /// An invocation of `kind` for `window`, or none, made by the harness when `synthetic`.
    pub(crate) fn of(
        kind: InvocationKind,
        window: Option<winit::window::WindowId>,
        synthetic: bool,
    ) -> Self {
        Self { kind, window: window.map_or(0, u64::from), synthetic }
    }
}

/// The App's dispatch-timeline API, called only in a build with the cfg.
#[cfg(perf_dispatch_timeline_api)]
pub(crate) mod api {
    use sonicterm_app::app::{
        App, AppearanceTargetV1, DispatchTimelineArmV1, DispatchTimelineTakeV1,
        DispatchTimelineToken, DispatchTimelineV1,
    };

    use super::{ArmResult, TakeResult, WitnessTarget};

    /// The App's arming token.
    pub(crate) type Token = DispatchTimelineToken;
    /// The App's frozen timeline.
    pub(crate) type Timeline = DispatchTimelineV1;

    /// Arm the timeline for `pane_id`'s window, with `witness` when one is given.
    pub(crate) fn arm(app: &mut App, pane_id: u64, witness: Option<WitnessTarget>) -> ArmResult {
        let witness = witness.map(|target| AppearanceTargetV1 {
            abs_row: target.abs_row,
            col: target.col,
            character: target.character,
            scrollback_evicted: target.scrollback_evicted,
            screen_epoch: target.screen_epoch,
            size_generation: target.size_generation,
        });
        arm_result(app.arm_dispatch_timeline_v1(pane_id, witness))
    }

    /// Take what `token` recorded.
    pub(crate) fn take(app: &mut App, token: Token) -> TakeResult {
        take_result(app.take_dispatch_timeline_v1(token))
    }

    /// The harness's reading of an arm outcome; every variant is named, so a new one fails the build.
    pub(crate) fn arm_result(outcome: DispatchTimelineArmV1) -> ArmResult {
        match outcome {
            DispatchTimelineArmV1::Armed(token) => ArmResult::Armed(token),
            DispatchTimelineArmV1::NotRecorded => ArmResult::NotRecorded,
            DispatchTimelineArmV1::GateOff => ArmResult::GateOff,
            DispatchTimelineArmV1::NoPane => ArmResult::NoPane,
            DispatchTimelineArmV1::StillArmed(token) => ArmResult::StillArmed(token),
            DispatchTimelineArmV1::Exhausted => ArmResult::Exhausted,
        }
    }

    /// The harness's reading of a take outcome; every variant is named, so a new one fails the build.
    pub(crate) fn take_result(outcome: DispatchTimelineTakeV1) -> TakeResult {
        match outcome {
            DispatchTimelineTakeV1::Timeline(timeline) => TakeResult::Timeline(timeline),
            DispatchTimelineTakeV1::NotRecorded => TakeResult::NotRecorded,
            DispatchTimelineTakeV1::GateOff => TakeResult::GateOff,
            DispatchTimelineTakeV1::Mismatch => TakeResult::Mismatch,
            DispatchTimelineTakeV1::AlreadyTaken => TakeResult::AlreadyTaken,
        }
    }
}

/// Without the cfg the App is never called: nothing is armed or taken, and the timeline is unavailable.
#[cfg(not(perf_dispatch_timeline_api))]
pub(crate) mod api {
    use sonicterm_app::app::App;

    use super::{ArmResult, TakeResult, WitnessTarget};

    /// This build never names the App's token, so no value of it exists.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Token {}

    /// This build never names the App's timeline, so no value of it exists.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Timeline {}

    /// Nothing is armed in this build.
    pub(crate) fn arm(_app: &mut App, _pane_id: u64, _witness: Option<WitnessTarget>) -> ArmResult {
        ArmResult::Unavailable
    }

    /// No token exists in this build, so take can never be called.
    pub(crate) fn take(_app: &mut App, token: Token) -> TakeResult {
        match token {}
    }
}

/// The harness's half of the timeline: each outer `ApplicationHandler` callback of the probe and each
/// forwarded invocation inside it, so App dispatches can be joined to the callback that carried them.
#[cfg(perf_dispatch_timeline_api)]
pub(crate) mod recorders {
    use std::time::{Duration, Instant};

    use super::{Invocation, InvocationKind, OuterKind};

    /// Outer callbacks one sample holds.
    pub(crate) const OUTER_CAPACITY: usize = 128;
    /// Invocations one sample holds.
    pub(crate) const INVOCATION_CAPACITY: usize = 256;

    /// One outer callback, its times in nanoseconds after the arm.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct OuterCallbackV1 {
        /// When it entered; the arm itself for a callback clipped at arm.
        pub(crate) enter_ns: u64,
        /// When it returned; 0 while it is open.
        pub(crate) return_ns: u64,
        /// Its position in the sample.
        pub(crate) id: u32,
        /// Which method it is.
        pub(crate) kind: OuterKind,
        /// Still open when the sample was frozen (`OpenAtTake`): clipped, not missing.
        pub(crate) open: bool,
        /// Already running when the sample was armed, so its record starts at the arm.
        pub(crate) clipped_at_arm: bool,
    }

    /// One forwarded invocation, its times in nanoseconds after the arm.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct InvocationV1 {
        /// When the forward called into the App.
        pub(crate) enter_ns: u64,
        /// When that call returned.
        pub(crate) return_ns: u64,
        /// The target window as `u64::from(WindowId)`; 0 for a kind that names no window.
        pub(crate) window: u64,
        /// Its position in the sample.
        pub(crate) id: u32,
        /// The outer callback it ran inside; `u32::MAX` when none was open.
        pub(crate) parent: u32,
        /// What it was.
        pub(crate) kind: InvocationKind,
        /// Whether the harness generated it (synthetic input).
        pub(crate) synthetic: bool,
    }

    /// One sample's record storage: exactly `OUTER_CAPACITY` outer callbacks and `INVOCATION_CAPACITY`
    /// invocations, allocated once and moved, never copied, from recorder to frozen record and back.
    #[derive(Debug)]
    pub(crate) struct Buffers {
        /// Outer callbacks, in order.
        pub(crate) outer: Vec<OuterCallbackV1>,
        /// Invocations, in order.
        pub(crate) invocations: Vec<InvocationV1>,
    }

    impl Buffers {
        /// Empty storage at its full capacity; the only allocation a sample's recording makes.
        pub(crate) fn new() -> Self {
            Self {
                outer: Vec::with_capacity(OUTER_CAPACITY),
                invocations: Vec::with_capacity(INVOCATION_CAPACITY),
            }
        }

        /// The bytes the storage holds: its capacity, whatever its length.
        pub(crate) fn payload_bytes(&self) -> usize {
            self.outer.capacity() * std::mem::size_of::<OuterCallbackV1>()
                + self.invocations.capacity() * std::mem::size_of::<InvocationV1>()
        }
    }

    /// What one sample recorded; it owns the storage the recorder used, until `recycle` hands it back.
    #[derive(Debug)]
    pub(crate) struct Frozen {
        /// The records.
        pub(crate) buffers: Buffers,
        /// The instant every offset in `buffers` is relative to: the recorder's own arm until
        /// `rebase_onto` moves them onto the App timeline's `armed_at`.
        pub(crate) origin: Instant,
        /// A callback or invocation could not be joined: an orphan, a nest, an unmatched or reversed
        /// return, an invocation outside its parent, an unreturned invocation, or a time before the arm
        /// or not representable.
        pub(crate) incomplete: bool,
        /// A buffer was full; the record is excluded, never trimmed to fit.
        pub(crate) overflow: bool,
    }

    impl Frozen {
        /// Move every offset onto `app_armed_at`, the App timeline's own origin, so the harness records join
        /// the App's by time. The App arms before the recorder starts, so each offset grows by the gap
        /// between them. A recorder that started before the App armed, or an offset that would leave the
        /// `u64` range, makes the sample incomplete and leaves the offsets as they were.
        pub(crate) fn rebase_onto(&mut self, app_armed_at: Instant) {
            let Some(delta) = offset_ns(self.origin.checked_duration_since(app_armed_at)) else {
                // When: the harness origin precedes the App's or the gap is unrepresentable, nothing joins.
                self.incomplete = true;
                return;
            };
            let shift = |offset: u64| offset.checked_add(delta);
            let outer_shifted: Option<Vec<(u64, u64)>> = self
                .buffers
                .outer
                .iter()
                .map(|record| Some((shift(record.enter_ns)?, shift(record.return_ns)?)))
                .collect();
            let invocations_shifted: Option<Vec<(u64, u64)>> = self
                .buffers
                .invocations
                .iter()
                .map(|record| Some((shift(record.enter_ns)?, shift(record.return_ns)?)))
                .collect();
            let (Some(outer_shifted), Some(invocations_shifted)) =
                (outer_shifted, invocations_shifted)
            else {
                // When: an offset would leave the `u64` range, the records stay on the harness origin.
                self.incomplete = true;
                return;
            };
            for (record, (enter_ns, return_ns)) in self.buffers.outer.iter_mut().zip(outer_shifted)
            {
                record.enter_ns = enter_ns;
                // An open outer callback keeps its 0 return, which marks it open, not a time.
                record.return_ns = if record.open { 0 } else { return_ns };
            }
            for (record, (enter_ns, return_ns)) in
                self.buffers.invocations.iter_mut().zip(invocations_shifted)
            {
                record.enter_ns = enter_ns;
                record.return_ns = return_ns;
            }
            self.origin = app_armed_at;
        }

        /// Hand the storage back, emptied, for the next sample's recorder.
        pub(crate) fn recycle(mut self) -> Buffers {
            self.buffers.outer.clear();
            self.buffers.invocations.clear();
            self.buffers
        }
    }

    /// An open record: its index, or `Dropped` when the buffer was full for it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Open {
        /// The record at this index.
        Recorded(usize),
        /// Not stored: the buffer was full.
        Dropped,
    }

    /// Nanoseconds after the arm for an instant `since` past it; `None` when the instant precedes the arm
    /// (`since` is `None`) or the offset does not fit a `u64`, which is rejected, never saturated.
    pub(crate) fn offset_ns(since: Option<Duration>) -> Option<u64> {
        u64::try_from(since?.as_nanos()).ok()
    }

    /// Records one sample's outer callbacks and invocations from its arm. Any invalid order or containment
    /// marks the sample incomplete, and the mark is never cleared.
    #[derive(Debug)]
    pub(crate) struct Recorder {
        armed_at: Instant,
        buffers: Buffers,
        open_outer: Option<Open>,
        open_invocation: Option<Open>,
        incomplete: bool,
        overflow: bool,
    }

    impl Recorder {
        /// A recorder armed at `armed_at` on `buffers` (new, or recycled from a previous sample). `running`
        /// is the outer callback the arm runs inside: its record starts at the arm, clipped, so the
        /// invocations it makes after the arm have a parent.
        pub(crate) fn arm(armed_at: Instant, running: Option<OuterKind>, buffers: Buffers) -> Self {
            let mut recorder = Self {
                armed_at,
                buffers,
                open_outer: None,
                open_invocation: None,
                incomplete: false,
                overflow: false,
            };
            recorder.buffers.outer.clear();
            recorder.buffers.invocations.clear();
            if let Some(kind) = running {
                // When: the arm runs inside a callback, that callback's record starts here.
                recorder.push_outer(kind, 0, true);
            }
            recorder
        }

        /// Nanoseconds from the arm to `at`; a time before the arm or past the `u64` range marks the
        /// sample incomplete.
        fn offset(&mut self, at: Instant) -> u64 {
            match offset_ns(at.checked_duration_since(self.armed_at)) {
                Some(offset) => offset,
                None => {
                    self.incomplete = true;
                    0
                }
            }
        }

        /// Open an outer callback record, or count the overflow when the buffer is full.
        fn push_outer(&mut self, kind: OuterKind, enter_ns: u64, clipped_at_arm: bool) {
            if self.buffers.outer.len() == OUTER_CAPACITY {
                // When: the outer buffer is full, the sample overflows and this callback is dropped.
                self.overflow = true;
                self.open_outer = Some(Open::Dropped);
                return;
            }
            let id = self.buffers.outer.len() as u32;
            self.buffers.outer.push(OuterCallbackV1 {
                enter_ns,
                return_ns: 0,
                id,
                kind,
                open: true,
                clipped_at_arm,
            });
            self.open_outer = Some(Open::Recorded(self.buffers.outer.len() - 1));
        }

        /// An outer callback entered at `at`. Outer callbacks never nest and never overlap the previous one.
        pub(crate) fn enter_outer(&mut self, kind: OuterKind, at: Instant) {
            let enter_ns = self.offset(at);
            if self.open_outer.is_some() {
                // When: another outer callback is still open, the two cannot be joined.
                self.incomplete = true;
            }
            if self.buffers.outer.last().is_some_and(|previous| enter_ns < previous.return_ns) {
                // When: this callback enters before the previous one returned, the order is invalid.
                self.incomplete = true;
            }
            self.push_outer(kind, enter_ns, false);
        }

        /// The open outer callback returned at `at`. A return with none open, a return before its entry, a
        /// return while one of its invocations is still open, or a return before one of its returned
        /// invocations returned is a defect.
        pub(crate) fn exit_outer(&mut self, at: Instant) {
            let return_ns = self.offset(at);
            if self.open_invocation.is_some() {
                // When: an invocation is still open, it would extend past its parent's return.
                self.incomplete = true;
            }
            match self.open_outer.take() {
                Some(Open::Recorded(index)) => {
                    let id = self.buffers.outer[index].id;
                    // Invocations are sequential, so the parent's last child returned last among its children.
                    let last_child_return = self
                        .buffers
                        .invocations
                        .iter()
                        .rev()
                        .find(|invocation| invocation.parent == id)
                        .map(|invocation| invocation.return_ns);
                    let record = &mut self.buffers.outer[index];
                    record.return_ns = return_ns;
                    record.open = false;
                    let reversed = return_ns < record.enter_ns;
                    let outlived = last_child_return.is_some_and(|child| return_ns < child);
                    self.incomplete |= reversed || outlived;
                }
                Some(Open::Dropped) => {}
                None => self.incomplete = true,
            }
        }

        /// The probe forwarded `invocation` to the App at `at`. Its parent is the open outer callback; with
        /// none open it is an orphan. Invocations never nest, never overlap the previous one, and never
        /// start before their parent.
        pub(crate) fn enter_invocation(&mut self, invocation: Invocation, at: Instant) {
            let enter_ns = self.offset(at);
            if self.open_invocation.is_some() {
                // When: another invocation is still open, the two cannot be joined.
                self.incomplete = true;
            }
            if self.buffers.invocations.last().is_some_and(|previous| enter_ns < previous.return_ns)
            {
                // When: this invocation enters before the previous one returned, the order is invalid.
                self.incomplete = true;
            }
            let parent = match self.open_outer {
                Some(Open::Recorded(index)) => {
                    let outer = self.buffers.outer[index];
                    // An invocation must lie inside its parent callback.
                    self.incomplete |= enter_ns < outer.enter_ns;
                    outer.id
                }
                Some(Open::Dropped) => u32::MAX,
                None => {
                    self.incomplete = true;
                    u32::MAX
                }
            };
            if self.buffers.invocations.len() == INVOCATION_CAPACITY {
                // When: the invocation buffer is full, the sample overflows and this invocation is dropped.
                self.overflow = true;
                self.open_invocation = Some(Open::Dropped);
                return;
            }
            let id = self.buffers.invocations.len() as u32;
            self.buffers.invocations.push(InvocationV1 {
                enter_ns,
                return_ns: 0,
                window: invocation.window,
                id,
                parent,
                kind: invocation.kind,
                synthetic: invocation.synthetic,
            });
            self.open_invocation = Some(Open::Recorded(self.buffers.invocations.len() - 1));
        }

        /// The open invocation returned at `at`; a return with none open, or before its entry, is a defect.
        pub(crate) fn exit_invocation(&mut self, at: Instant) {
            let return_ns = self.offset(at);
            match self.open_invocation.take() {
                Some(Open::Recorded(index)) => {
                    let record = &mut self.buffers.invocations[index];
                    record.return_ns = return_ns;
                    let reversed = return_ns < record.enter_ns;
                    self.incomplete |= reversed;
                }
                Some(Open::Dropped) => {}
                None => self.incomplete = true,
            }
        }

        /// Freeze the sample at take, moving the storage into the record: nothing is copied. An outer
        /// callback still open stays `open` (`OpenAtTake`), which the window end clips. An invocation still
        /// open is an unreturned call, so incomplete.
        pub(crate) fn freeze(self) -> Frozen {
            Frozen {
                incomplete: self.incomplete || self.open_invocation.is_some(),
                overflow: self.overflow,
                origin: self.armed_at,
                buffers: self.buffers,
            }
        }
    }
}

/// One sample's timeline lifecycle, over the token type so tests can inject every arm and take outcome.
#[cfg(perf_dispatch_timeline_api)]
pub(crate) mod lifecycle {
    use std::time::Instant;

    use super::recorders::{Buffers, Recorder};
    use super::{Disposition, OuterKind, TimelineArm, TimelineTake};

    /// A taken timeline's own origin: the instant the App armed it, which the harness records join to.
    pub(crate) trait AppOrigin {
        /// When the App armed the timeline.
        fn app_armed_at(&self) -> Instant;
    }

    impl AppOrigin for super::api::Timeline {
        fn app_armed_at(&self) -> Instant {
            self.armed_at
        }
    }

    /// A sample's timeline: armed with a token and a recorder, or settled with a disposition.
    #[derive(Debug)]
    pub(crate) enum Lifecycle<Token> {
        /// Nothing was armed; the disposition is final.
        Unarmed(Disposition),
        /// Armed under `token`, recording from the arm.
        Armed {
            /// The App's token.
            token: Token,
            /// The harness's recorder.
            recorder: Recorder,
        },
    }

    /// The lifecycle `outcome` starts. Only an armed token reads `clock` and takes storage: it reuses the
    /// recycled `storage` when there is some and allocates once otherwise. Any other outcome allocates
    /// nothing and reads no clock.
    pub(crate) fn arm<Token>(
        outcome: TimelineArm<Token>,
        running: Option<OuterKind>,
        storage: &mut Option<Buffers>,
        clock: impl FnOnce() -> Instant,
    ) -> Lifecycle<Token> {
        match outcome {
            TimelineArm::Armed(token) => {
                let buffers = storage.take().unwrap_or_else(Buffers::new);
                Lifecycle::Armed { token, recorder: Recorder::arm(clock(), running, buffers) }
            }
            TimelineArm::Unavailable => Lifecycle::Unarmed(Disposition::Unavailable),
            TimelineArm::NotRecorded => Lifecycle::Unarmed(Disposition::NotRecorded),
            TimelineArm::GateOff => Lifecycle::Unarmed(Disposition::GateOff),
            TimelineArm::NoPane => Lifecycle::Unarmed(Disposition::NoPane),
            TimelineArm::StillArmed(_) => Lifecycle::Unarmed(Disposition::StillArmed),
            TimelineArm::Exhausted => Lifecycle::Unarmed(Disposition::Exhausted),
        }
    }

    /// Close `lifecycle` at a sample's close: an armed one takes its token exactly once, freezes its
    /// recorder, moves a taken timeline's harness records onto the App's origin, and hands the storage back
    /// to `storage` for the next sample; an unarmed one keeps its disposition and takes nothing.
    pub(crate) fn close<Token, Timeline: AppOrigin>(
        lifecycle: Lifecycle<Token>,
        storage: &mut Option<Buffers>,
        take: impl FnOnce(Token) -> TimelineTake<Timeline>,
    ) -> Disposition {
        match lifecycle {
            Lifecycle::Unarmed(disposition) => disposition,
            Lifecycle::Armed { token, recorder } => {
                let outcome = take(token);
                let mut frozen = recorder.freeze();
                if let TimelineTake::Timeline(timeline) = &outcome {
                    // When: a timeline was taken, the harness records move onto its origin before judging.
                    frozen.rebase_onto(timeline.app_armed_at());
                }
                let complete = !frozen.incomplete && !frozen.overflow;
                *storage = Some(frozen.recycle());
                match outcome {
                    TimelineTake::Timeline(_) if complete => Disposition::Recorded,
                    TimelineTake::Timeline(_) => Disposition::RecordedIncomplete,
                    TimelineTake::Unavailable => Disposition::Unavailable,
                    TimelineTake::NotRecorded => Disposition::NotRecorded,
                    TimelineTake::GateOff => Disposition::GateOff,
                    TimelineTake::Mismatch => Disposition::Mismatch,
                    TimelineTake::AlreadyTaken => Disposition::AlreadyTaken,
                }
            }
        }
    }
}

/// The probe's per-sample facade: arm at injection, record the callbacks and invocations while armed, and
/// take at every close.
#[cfg(perf_dispatch_timeline_api)]
pub(crate) mod sample {
    use std::time::Instant;

    use sonicterm_app::app::App;

    use super::lifecycle::{self, Lifecycle};
    use super::recorders::Buffers;
    use super::{api, Disposition, Invocation, OuterKind};

    /// The probe's reused recorder storage between samples: `None` until a sample is first armed.
    pub(crate) type Storage = Option<Buffers>;

    /// One open sample's timeline.
    #[derive(Debug)]
    pub(crate) struct SampleTimeline(Lifecycle<api::Token>);

    impl SampleTimeline {
        /// Arm pane `pane_id`'s timeline at the sample's injection, inside the outer callback `running`.
        pub(crate) fn arm(
            app: &mut App,
            pane_id: u64,
            running: Option<OuterKind>,
            storage: &mut Storage,
        ) -> Self {
            Self(lifecycle::arm(api::arm(app, pane_id, None), running, storage, Instant::now))
        }

        /// Whether a recorder is running; only then is any instant read.
        pub(crate) fn is_armed(&self) -> bool {
            matches!(self.0, Lifecycle::Armed { .. })
        }

        /// An outer callback of `kind` entered.
        pub(crate) fn enter_outer(&mut self, kind: OuterKind) {
            if let Lifecycle::Armed { recorder, .. } = &mut self.0 {
                recorder.enter_outer(kind, Instant::now());
            }
        }

        /// The open outer callback returned.
        pub(crate) fn exit_outer(&mut self) {
            if let Lifecycle::Armed { recorder, .. } = &mut self.0 {
                recorder.exit_outer(Instant::now());
            }
        }

        /// The probe forwarded `invocation` at `at`.
        pub(crate) fn enter_invocation(&mut self, invocation: Invocation, at: Instant) {
            if let Lifecycle::Armed { recorder, .. } = &mut self.0 {
                recorder.enter_invocation(invocation, at);
            }
        }

        /// The forwarded invocation returned at `at`.
        pub(crate) fn exit_invocation(&mut self, at: Instant) {
            if let Lifecycle::Armed { recorder, .. } = &mut self.0 {
                recorder.exit_invocation(at);
            }
        }

        /// Close the timeline at the sample's close: take an armed token once and recycle the storage.
        pub(crate) fn close(self, app: &mut App, storage: &mut Storage) -> Disposition {
            lifecycle::close(self.0, storage, |token| api::take(app, token))
        }
    }
}

/// Without the cfg a sample's timeline is never armed: nothing is called, stored or read, and every sample
/// records the timeline unavailable.
#[cfg(not(perf_dispatch_timeline_api))]
pub(crate) mod sample {
    use std::time::Instant;

    use sonicterm_app::app::App;

    use super::{api, ArmResult, Disposition, Invocation, OuterKind};

    /// This build stores nothing between samples.
    pub(crate) type Storage = ();

    /// One open sample's timeline: never armed in this build.
    #[derive(Debug)]
    pub(crate) struct SampleTimeline;

    impl SampleTimeline {
        /// Nothing is armed in this build: the adapter answers `Unavailable` without calling the App.
        pub(crate) fn arm(
            app: &mut App,
            pane_id: u64,
            _running: Option<OuterKind>,
            _storage: &mut Storage,
        ) -> Self {
            debug_assert_eq!(api::arm(app, pane_id, None), ArmResult::Unavailable);
            Self
        }

        /// Never armed in this build.
        pub(crate) fn is_armed(&self) -> bool {
            false
        }

        /// Nothing is recorded in this build.
        pub(crate) fn enter_outer(&mut self, _kind: OuterKind) {}

        /// Nothing is recorded in this build.
        pub(crate) fn exit_outer(&mut self) {}

        /// Nothing is recorded in this build.
        pub(crate) fn enter_invocation(&mut self, _invocation: Invocation, _at: Instant) {}

        /// Nothing is recorded in this build.
        pub(crate) fn exit_invocation(&mut self, _at: Instant) {}

        /// The timeline is unavailable in this build.
        pub(crate) fn close(self, _app: &mut App, _storage: &mut Storage) -> Disposition {
            Disposition::Unavailable
        }
    }
}

#[cfg(test)]
#[path = "dispatch_timeline_tests.rs"]
mod dispatch_timeline_tests;
