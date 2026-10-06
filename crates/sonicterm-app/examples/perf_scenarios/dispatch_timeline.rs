//! The per-sample dispatch timeline, harness side: the adapter over the App's arm and take methods, and
//! the outer-callback and invocation recorders the App's records are joined against.
//!
//! Every call into the App's dispatch-timeline API, every type of it, and the recorders sit behind
//! `#[cfg(perf_dispatch_timeline_api)]`. perf-compare passes that cfg to both sides only when both trees
//! define the API, so the overlaid harness also builds on a tree that predates it; that build compiles the
//! no-call fallback below, which reports the timeline unavailable. No probe path calls this module yet:
//! the experiment's sample lifecycle wires it in, so timed runs are unchanged until then.
// Nothing calls the adapter or the recorders until the experiment's sample lifecycle wires them in.
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

/// What arming returned. `Unavailable` is this build's own answer when it lacks the cfg; every other
/// variant is the App's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArmResult {
    /// This build does not call the API.
    Unavailable,
    /// Armed under this token.
    Armed(api::Token),
    /// The App records no timeline.
    NotRecorded,
    /// The App's frame-counter gate is off.
    GateOff,
    /// No window holds the pane.
    NoPane,
    /// A timeline is already armed under this token.
    StillArmed(api::Token),
    /// The App can issue no more tokens.
    Exhausted,
}

/// What take returned. `Unavailable` is this build's own answer when it lacks the cfg; every other
/// variant is the App's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TakeResult {
    /// This build does not call the API.
    Unavailable,
    /// The frozen timeline.
    Timeline(api::Timeline),
    /// The App recorded no timeline for this token.
    NotRecorded,
    /// The App's frame-counter gate is off.
    GateOff,
    /// The App holds another arming, or none.
    Mismatch,
    /// This token's timeline was already transferred.
    AlreadyTaken,
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
    use std::time::Instant;

    /// Outer callbacks one sample holds.
    pub(crate) const OUTER_CAPACITY: usize = 128;
    /// Invocations one sample holds.
    pub(crate) const INVOCATION_CAPACITY: usize = 256;

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

    /// What one sample recorded.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Frozen {
        /// Outer callbacks, in order.
        pub(crate) outer: Vec<OuterCallbackV1>,
        /// Invocations, in order.
        pub(crate) invocations: Vec<InvocationV1>,
        /// A callback or invocation could not be joined: an orphan, a nest, an unmatched return, an
        /// unreturned invocation, or a time before the arm.
        pub(crate) incomplete: bool,
        /// A buffer was full; the record is excluded, never trimmed to fit.
        pub(crate) overflow: bool,
    }

    /// An open record: its index, or `Dropped` when the buffer was full for it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Open {
        Recorded(usize),
        Dropped,
    }

    /// Records one sample's outer callbacks and invocations from its arm.
    #[derive(Debug)]
    pub(crate) struct Recorder {
        armed_at: Instant,
        outer: Vec<OuterCallbackV1>,
        invocations: Vec<InvocationV1>,
        open_outer: Option<Open>,
        open_invocation: Option<Open>,
        incomplete: bool,
        overflow: bool,
    }

    impl Recorder {
        /// A recorder armed at `armed_at`. `running` is the outer callback the arm runs inside: its record
        /// starts at the arm, clipped, so the invocations it makes after the arm have a parent.
        pub(crate) fn arm(armed_at: Instant, running: Option<OuterKind>) -> Self {
            let mut recorder = Self {
                armed_at,
                outer: Vec::with_capacity(OUTER_CAPACITY),
                invocations: Vec::with_capacity(INVOCATION_CAPACITY),
                open_outer: None,
                open_invocation: None,
                incomplete: false,
                overflow: false,
            };
            if let Some(kind) = running {
                // When: the arm runs inside a callback, that callback's record starts here.
                recorder.push_outer(kind, 0, true);
            }
            recorder
        }

        /// Nanoseconds from the arm to `at`; a time before the arm marks the sample incomplete.
        fn offset(&mut self, at: Instant) -> u64 {
            match at.checked_duration_since(self.armed_at) {
                Some(after) => u64::try_from(after.as_nanos()).unwrap_or(u64::MAX),
                None => {
                    self.incomplete = true;
                    0
                }
            }
        }

        /// Open an outer callback record, or count the overflow when the buffer is full.
        fn push_outer(&mut self, kind: OuterKind, enter_ns: u64, clipped_at_arm: bool) {
            if self.outer.len() == OUTER_CAPACITY {
                // When: the buffer is full, the sample overflows and this callback is dropped.
                self.overflow = true;
                self.open_outer = Some(Open::Dropped);
                return;
            }
            let id = self.outer.len() as u32;
            self.outer.push(OuterCallbackV1 {
                enter_ns,
                return_ns: 0,
                id,
                kind,
                open: true,
                clipped_at_arm,
            });
            self.open_outer = Some(Open::Recorded(self.outer.len() - 1));
        }

        /// An outer callback entered at `at`. Outer callbacks never nest; one already open is a defect.
        pub(crate) fn enter_outer(&mut self, kind: OuterKind, at: Instant) {
            let enter_ns = self.offset(at);
            if self.open_outer.is_some() {
                // When: another outer callback is still open, the two cannot be joined.
                self.incomplete = true;
            }
            self.push_outer(kind, enter_ns, false);
        }

        /// The open outer callback returned at `at`; a return with none open is a defect.
        pub(crate) fn exit_outer(&mut self, at: Instant) {
            let return_ns = self.offset(at);
            match self.open_outer.take() {
                Some(Open::Recorded(index)) => {
                    self.outer[index].return_ns = return_ns;
                    self.outer[index].open = false;
                }
                Some(Open::Dropped) => {}
                None => self.incomplete = true,
            }
        }

        /// The probe forwarded `kind` to the App at `at`. Its parent is the open outer callback; with none
        /// open it is an orphan, and invocations never nest.
        pub(crate) fn enter_invocation(
            &mut self,
            kind: InvocationKind,
            window: u64,
            synthetic: bool,
            at: Instant,
        ) {
            let enter_ns = self.offset(at);
            if self.open_invocation.is_some() {
                // When: another invocation is still open, the two cannot be joined.
                self.incomplete = true;
            }
            let parent = match self.open_outer {
                Some(Open::Recorded(index)) => self.outer[index].id,
                Some(Open::Dropped) => u32::MAX,
                None => {
                    self.incomplete = true;
                    u32::MAX
                }
            };
            if self.invocations.len() == INVOCATION_CAPACITY {
                // When: the buffer is full, the sample overflows and this invocation is dropped.
                self.overflow = true;
                self.open_invocation = Some(Open::Dropped);
                return;
            }
            let id = self.invocations.len() as u32;
            self.invocations.push(InvocationV1 {
                enter_ns,
                return_ns: 0,
                window,
                id,
                parent,
                kind,
                synthetic,
            });
            self.open_invocation = Some(Open::Recorded(self.invocations.len() - 1));
        }

        /// The open invocation returned at `at`; a return with none open is a defect.
        pub(crate) fn exit_invocation(&mut self, at: Instant) {
            let return_ns = self.offset(at);
            match self.open_invocation.take() {
                Some(Open::Recorded(index)) => self.invocations[index].return_ns = return_ns,
                Some(Open::Dropped) => {}
                None => self.incomplete = true,
            }
        }

        /// What the sample recorded at take. An outer callback still open stays `open` (`OpenAtTake`):
        /// the window end clips it. An invocation still open is an unreturned call, so incomplete.
        pub(crate) fn freeze(&self) -> Frozen {
            Frozen {
                outer: self.outer.clone(),
                invocations: self.invocations.clone(),
                incomplete: self.incomplete || self.open_invocation.is_some(),
                overflow: self.overflow,
            }
        }
    }
}

#[cfg(test)]
#[path = "dispatch_timeline_tests.rs"]
mod dispatch_timeline_tests;
