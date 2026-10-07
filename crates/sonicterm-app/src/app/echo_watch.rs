//! The echo watch: where one typed character first appeared, the flush that published it, and the
//! worker's token decision for that flush. It exists only while the App's frame-counter gate is on.
//!
//! A pane's watch is armed by the perf harness with a nonzero token, written by the pane's VT worker
//! (section facts, flush publication and delivery), and taken once by the harness. Writers check the
//! token, the slot's state and the arm instant under the slot lock, so a writer paused across a take
//! or a re-arm changes nothing. Lock order is parser, then slot: the worker may hold the parser while
//! it takes the slot, and the App APIs take only the slot.

use std::{
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use parking_lot::{Mutex, MutexGuard};
use sonicterm_grid::grid::Grid;
use winit::window::WindowId;

use super::echo_timeline::{EchoTimelineKindV1, EchoTimelineTakeV1, FrozenFlood, SlotTimeline};

/// The grid state a scrollback-absolute row index is valid under: rows evicted, screen
/// incarnation and resize generation. Any change means the index may name another row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EchoRowIdentity {
    /// `Grid::scrollback_evicted` when read.
    pub scrollback_evicted: u64,
    /// `Grid::screen_epoch` when read.
    pub screen_epoch: u64,
    /// `Grid::size_generation` when read.
    pub size_generation: u64,
}

impl EchoRowIdentity {
    /// The identity `grid` holds now.
    pub fn of(grid: &Grid) -> Self {
        Self {
            scrollback_evicted: grid.scrollback_evicted(),
            screen_epoch: grid.screen_epoch(),
            size_generation: grid.size_generation(),
        }
    }
}

/// The cell a watch waits for: a scrollback-absolute row, a column and the character, valid under
/// the identity it was armed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoWatchTarget {
    /// Row counted from the oldest retained history row.
    pub abs_row: u64,
    /// Column of the echoed cell.
    pub col: u16,
    /// The character that must be in that cell.
    pub character: char,
    /// The grid identity the row index was read under.
    pub identity: EchoRowIdentity,
}

impl EchoWatchTarget {
    /// Whether `grid` holds the target character at the target cell now.
    fn present_in(&self, grid: &Grid) -> bool {
        grid.row_at_abs(self.abs_row)
            .and_then(|row| row.get(usize::from(self.col)))
            .is_some_and(|cell| cell.ch == self.character)
    }
}

/// One arming of a pane's echo watch; nonzero and never reused within an App.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArmToken(u64);

impl ArmToken {
    /// The token's number, for logs and assertions.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// What `App::arm_echo_watch` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmOutcome {
    /// The watch is armed with this token.
    Armed(ArmToken),
    /// The frame-counter gate is off, so no pane has a watch.
    GateOff,
    /// No window holds the pane.
    NoPane,
    /// The App has issued every token it can.
    Exhausted,
}

/// What `App::take_echo_watch` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeOutcome {
    /// The armed watch's record; the watch is now taken and disarmed.
    Trace(EchoTrace),
    /// The frame-counter gate is off, so no pane has a watch.
    GateOff,
    /// No window holds the pane.
    NoPane,
    /// The watch holds another token, or none; it is left alone.
    Mismatch,
    /// This token's record was already taken; the watch is left alone.
    AlreadyTaken,
}

/// The parse section in which the target first went from absent to present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoAppearance {
    /// The output generation the section's batch publishes.
    pub generation: u64,
    /// When the section finished parsing.
    pub parsed_at: Instant,
    /// Whether the parser had a synchronized update (DEC 2026) set after that section.
    pub sync_open: bool,
}

/// The first flush published after the appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoPublication {
    /// This watch's publication number; delivery is recorded only for the same number.
    pub seq: u64,
    /// When the publication was recorded, under the slot lock, before the token decision.
    pub published_at: Instant,
    /// The flush's redraw target; `None` for an untargeted flush.
    pub window: Option<WindowId>,
    /// Whether an earlier flush was still pending when this one was published.
    pub coalesced: bool,
}

/// How the worker decided the published flush's output event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoDeliveryOutcome {
    /// The event was sent to the event loop.
    Sent,
    /// An earlier event was outstanding; its service reads this flush.
    Suppressed,
    /// The event loop refused the send.
    Refused,
}

/// The worker's decision for the published flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoDelivery {
    /// The publication this decision belongs to.
    pub seq: u64,
    /// What the decision was.
    pub outcome: EchoDeliveryOutcome,
    /// When the decision was recorded, after the send call for `Sent`.
    pub decided_at: Instant,
}

/// What one armed watch recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoTrace {
    /// The cell the watch waited for.
    pub target: EchoWatchTarget,
    /// When the watch was armed; no earlier fact is accepted.
    pub armed_at: Instant,
    /// The first absent-to-present section, if one was seen.
    pub appearance: Option<EchoAppearance>,
    /// Some section read an identity other than the armed one.
    pub identity_changed: bool,
    /// A section found the target already present before any appearance.
    pub pre_present: bool,
    /// A section found the target absent again after its appearance.
    pub lost: bool,
    /// The first flush published after the appearance.
    pub publication: Option<EchoPublication>,
    /// The worker's decision for that flush.
    pub delivery: Option<EchoDelivery>,
    /// Whether the pane is shown in the main window's active tab when the trace was taken.
    pub shown: bool,
}

impl EchoTrace {
    /// A fresh record for `target` armed at `armed_at`.
    fn new(target: EchoWatchTarget, armed_at: Instant) -> Self {
        Self {
            target,
            armed_at,
            appearance: None,
            identity_changed: false,
            pre_present: false,
            lost: false,
            publication: None,
            delivery: None,
            shown: false,
        }
    }
}

/// The slot's lifecycle under one token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    /// Never armed.
    Idle,
    /// Armed; writers may record.
    Armed,
    /// Taken; nothing changes until the next arm.
    Taken,
}

/// The token's timeline, when its arming owned its window's timeline.
#[derive(Debug)]
enum TimelineSlot {
    /// No timeline: not an owner arming, or its timeline ended or was released.
    Absent,
    /// The owner arming's timeline, recording while armed and frozen by its take.
    Recording(Box<SlotTimeline>),
    /// The frozen timeline was transferred.
    Transferred,
}

/// The slot behind the lock: the token, its state, its record and its timeline.
#[derive(Debug)]
struct EchoSlot {
    token: u64,
    state: SlotState,
    trace: Option<EchoTrace>,
    next_seq: u64,
    timeline: TimelineSlot,
}

impl EchoSlot {
    /// The record writers may change: only for `token`, only while armed.
    fn armed_trace(&mut self, token: u64) -> Option<&mut EchoTrace> {
        self.armed_parts(token).map(|(trace, _)| trace)
    }

    /// The record and the recording timeline writers may change: only for `token`, only while armed.
    fn armed_parts(&mut self, token: u64) -> Option<(&mut EchoTrace, Option<&mut SlotTimeline>)> {
        if self.token != token || self.state != SlotState::Armed {
            // When: the slot holds another token or is not armed, a writer's change is discarded.
            return None;
        }
        let timeline = match &mut self.timeline {
            TimelineSlot::Recording(timeline) => Some(&mut **timeline),
            TimelineSlot::Absent | TimelineSlot::Transferred => None,
        };
        self.trace.as_mut().map(|trace| (trace, timeline))
    }
}

/// What `EchoWatch::take` found, before the App adds whether the pane is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotTake {
    /// The armed record; the slot is now taken.
    Trace(EchoTrace),
    /// Another token, or none.
    Mismatch,
    /// This token's record was already taken.
    AlreadyTaken,
}

/// A recorded publication a later delivery may complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PublicationToken {
    token: u64,
    seq: u64,
}

/// One pane's echo watch, shared by the pane and its VT worker while the gate is on.
#[derive(Debug)]
pub(crate) struct EchoWatch {
    /// The armed token, or 0 for none; writers load it before any lock.
    armed: AtomicU64,
    slot: Mutex<EchoSlot>,
}

/// The bound on one watch's size; the watch is not charged to the pane ledger.
pub(crate) const ECHO_WATCH_MAX_BYTES: usize = 256;
const _: () = assert!(std::mem::size_of::<EchoWatch>() <= ECHO_WATCH_MAX_BYTES);

impl Default for EchoWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoWatch {
    /// An idle, unarmed watch.
    pub(crate) fn new() -> Self {
        Self {
            armed: AtomicU64::new(0),
            slot: Mutex::new(EchoSlot {
                token: 0,
                state: SlotState::Idle,
                trace: None,
                next_seq: 1,
                timeline: TimelineSlot::Absent,
            }),
        }
    }

    /// Take the slot lock; tests count each acquisition on this thread.
    fn lock_slot(&self) -> MutexGuard<'_, EchoSlot> {
        #[cfg(test)]
        SLOT_LOCKS.with(|locks| locks.set(locks.get() + 1));
        self.slot.lock()
    }

    /// Replace the whole slot with a fresh record for `token`, without a timeline.
    #[cfg(test)]
    pub(crate) fn arm(&self, token: ArmToken, target: EchoWatchTarget, armed_at: Instant) {
        self.arm_with(token, target, armed_at, None);
    }

    /// Replace the whole slot with a fresh record for `token` and its owner `timeline`, if any,
    /// then publish `token` to writers. A previous token's timeline is freed.
    // Ordering: armed stores Release after the slot is replaced, so a writer that Acquire-loads
    // the token finds its slot under the lock.
    pub(super) fn arm_with(
        &self,
        token: ArmToken,
        target: EchoWatchTarget,
        armed_at: Instant,
        timeline: Option<Box<SlotTimeline>>,
    ) {
        let mut slot = self.lock_slot();
        slot.token = token.0;
        slot.state = SlotState::Armed;
        slot.trace = Some(EchoTrace::new(target, armed_at));
        slot.next_seq = 1;
        slot.timeline = timeline.map_or(TimelineSlot::Absent, TimelineSlot::Recording);
        self.armed.store(token.0, Ordering::Release);
    }

    /// Take `token`'s record, leaving the slot taken and the watch disarmed.
    #[cfg(test)]
    pub(crate) fn take(&self, token: ArmToken) -> SlotTake {
        self.take_with(token, None)
    }

    /// Take `token`'s record, leaving the slot taken and the watch disarmed. The owner's take passes
    /// its window's `flood` entries, which freeze its recording timeline under the same lock.
    // Ordering: armed stores Release under the slot lock; a writer that still loads the old
    // token is rejected by the slot's state.
    pub(super) fn take_with(&self, token: ArmToken, flood: Option<FrozenFlood>) -> SlotTake {
        let mut slot = self.lock_slot();
        if slot.token != token.0 {
            // When: the slot holds another token, or is idle with token 0, it is left untouched.
            return SlotTake::Mismatch;
        }
        if slot.state == SlotState::Taken {
            // When: `state` is `Taken`, this token's record is gone; a second take changes nothing.
            return SlotTake::AlreadyTaken;
        }
        slot.state = SlotState::Taken;
        self.armed.store(0, Ordering::Release);
        if let (TimelineSlot::Recording(timeline), Some(flood)) = (&mut slot.timeline, flood) {
            // The owner's take freezes its recording timeline; no writer can append after this.
            timeline.freeze(flood);
        }
        match slot.trace {
            Some(trace) => SlotTake::Trace(trace),
            None => SlotTake::Mismatch,
        }
    }

    /// Transfer `token`'s frozen timeline once. A timeline that never recorded, ended before its
    /// take, or was never frozen answers `NotRecorded`; the slot keeps no copy after a transfer.
    pub(crate) fn transfer_timeline(&self, token: ArmToken) -> EchoTimelineTakeV1 {
        let mut slot = self.lock_slot();
        if slot.token != token.0 || token.0 == 0 {
            // When: the slot holds another token, or none, it is left untouched.
            return EchoTimelineTakeV1::Mismatch;
        }
        let frozen = match &slot.timeline {
            TimelineSlot::Transferred => {
                // When: `Transferred`, this token's timeline already left the slot.
                return EchoTimelineTakeV1::AlreadyTaken;
            }
            TimelineSlot::Absent => {
                // When: `Absent`, this arming owned no timeline, or it ended before its take.
                return EchoTimelineTakeV1::NotRecorded;
            }
            TimelineSlot::Recording(timeline) => timeline.frozen(),
        };
        if slot.state != SlotState::Taken {
            // When: `state` is not `Taken`, `take_echo_watch` has not frozen the record.
            return EchoTimelineTakeV1::NotTaken;
        }
        if !frozen {
            // When: `frozen` is false, the record was taken without its owner's freeze: not complete evidence.
            return EchoTimelineTakeV1::NotRecorded;
        }
        let TimelineSlot::Recording(timeline) =
            std::mem::replace(&mut slot.timeline, TimelineSlot::Transferred)
        else {
            unreachable!("the timeline was matched as recording under this lock");
        };
        timeline.transfer().map_or(EchoTimelineTakeV1::NotRecorded, EchoTimelineTakeV1::Timeline)
    }

    /// Free `token`'s recording timeline unless its take froze it: its owner ended (rearm, pane
    /// transfer, window close) before complete evidence existed. The ordinary watch is unchanged.
    pub(crate) fn end_timeline(&self, token: ArmToken) {
        let mut slot = self.lock_slot();
        let unfrozen =
            matches!(&slot.timeline, TimelineSlot::Recording(timeline) if !timeline.frozen());
        if slot.token == token.0 && unfrozen {
            slot.timeline = TimelineSlot::Absent;
        }
    }

    /// The pane is retired: disarm the watch and free its timeline under the slot lock, whatever
    /// worker handles survive, so a later writer changes nothing.
    // Ordering: armed stores Release under the slot lock, as at take; a stale writer's token is
    // rejected by the slot's state.
    pub(crate) fn retire(&self) {
        let mut slot = self.lock_slot();
        if slot.state == SlotState::Armed {
            slot.state = SlotState::Taken;
        }
        slot.timeline = TimelineSlot::Absent;
        self.armed.store(0, Ordering::Release);
    }

    /// Apply `change` to `token`'s recording timeline at `at`, with the appearance generation and
    /// the time since arm, when the slot is still armed with it and `at` is no earlier than its arm.
    pub(super) fn write_timeline(
        &self,
        token: ArmToken,
        at: Instant,
        change: impl FnOnce(&mut SlotTimeline, Option<u64>, std::time::Duration),
    ) {
        run_timeline_pause();
        let mut slot = self.lock_slot();
        let Some((trace, Some(timeline))) = slot.armed_parts(token.0) else {
            // When: the slot holds another token, is not armed, or records no timeline, nothing changes.
            return;
        };
        let Some(elapsed) = at.checked_duration_since(trace.armed_at) else {
            // When: `at` precedes `armed_at`, the event belongs to no sample and is never recorded.
            return;
        };
        change(timeline, trace.appearance.map(|appeared| appeared.generation), elapsed);
    }

    /// The armed token, or 0; the only cost an unarmed writer pays.
    // Ordering: armed loads Acquire, pairing with arm's Release store.
    pub(crate) fn armed_token(&self) -> u64 {
        self.armed.load(Ordering::Acquire)
    }

    /// The target armed under `token` and the facts its record already holds, if the slot is
    /// still armed with it. A worker resumes from those facts, so a fresh handle never re-derives one.
    pub(crate) fn target_for(&self, token: u64) -> Option<(EchoWatchTarget, u8)> {
        self.lock_slot().armed_trace(token).map(|trace| {
            let recorded = [
                (trace.appearance.is_some(), FACT_APPEARED),
                (trace.pre_present, FACT_PRE_PRESENT),
                (trace.identity_changed, FACT_IDENTITY_CHANGED),
                (trace.lost, FACT_LOST),
            ]
            .into_iter()
            .filter(|(held, _)| *held)
            .fold(0, |bits, (_, fact)| bits | fact);
            (trace.target, recorded)
        })
    }

    /// Apply `change` to `token`'s record when the slot is still armed with it and `at` is no
    /// earlier than its arm instant; otherwise the change is discarded. Returns whether it applied.
    #[cfg(test)]
    pub(crate) fn record(
        &self,
        token: u64,
        at: Instant,
        change: impl FnOnce(&mut EchoTrace),
    ) -> bool {
        self.record_with_timeline(token, at, |trace, _| change(trace))
    }

    /// `record`, with the recording timeline, if any, under the same lock.
    fn record_with_timeline(
        &self,
        token: u64,
        at: Instant,
        change: impl FnOnce(&mut EchoTrace, Option<&mut SlotTimeline>),
    ) -> bool {
        run_writer_pause();
        let mut slot = self.lock_slot();
        match slot.armed_parts(token) {
            Some((trace, timeline)) if at >= trace.armed_at => {
                change(trace, timeline);
                true
            }
            _ => false,
        }
    }

    /// Record one flush publication, once, after an appearance: no lock and no clock while the
    /// watch is unarmed; otherwise the instant is read under the slot lock.
    pub(crate) fn record_publication(
        &self,
        window: Option<WindowId>,
        coalesced: bool,
        now: &mut impl FnMut() -> Instant,
    ) -> Option<PublicationToken> {
        let token = self.armed_token();
        if token == 0 {
            // When: no token is armed, a flush costs one load: no slot lock and no clock read.
            return None;
        }
        run_writer_pause();
        let mut slot = self.lock_slot();
        let seq = slot.next_seq;
        let trace = slot.armed_trace(token)?;
        if trace.appearance.is_none() || trace.publication.is_some() {
            // When: nothing has appeared yet, or a publication is recorded, this flush is not the echo's.
            return None;
        }
        let published_at = now();
        if published_at < trace.armed_at {
            // When: `published_at` precedes `armed_at`, it belongs to an earlier sample and is discarded.
            return None;
        }
        trace.publication = Some(EchoPublication { seq, published_at, window, coalesced });
        slot.next_seq = seq + 1;
        Some(PublicationToken { token, seq })
    }

    /// Record the token decision for `publication`, if the slot still holds it and none is recorded.
    pub(crate) fn record_delivery(
        &self,
        publication: PublicationToken,
        outcome: EchoDeliveryOutcome,
        decided_at: Instant,
    ) {
        run_delivery_pause();
        let mut slot = self.lock_slot();
        let Some(trace) = slot.armed_trace(publication.token) else {
            // When: the slot was taken or re-armed after the publication, the decision is discarded.
            return;
        };
        let published = trace.publication.is_some_and(|recorded| recorded.seq == publication.seq);
        if published && trace.delivery.is_none() {
            trace.delivery = Some(EchoDelivery { seq: publication.seq, outcome, decided_at });
        }
    }

    /// The parse section's first read, under the parser guard before parsing: whether the target
    /// is present and the identity unchanged. `None`, at the cost of one load, while unarmed.
    pub(crate) fn pre_read(&self, cache: &Cell<EchoCache>, grid: &Grid) -> Option<SectionRead> {
        let token = self.armed_token();
        if token == 0 {
            // When: no token is armed, the section reads nothing else.
            return None;
        }
        let target = self.cached_target(cache, token)?;
        Some(SectionRead {
            token,
            target,
            present: target.present_in(grid),
            same_identity: EchoRowIdentity::of(grid) == target.identity,
        })
    }

    /// The section's second read, after parsing: derive this section's facts and record each new
    /// one under a single slot lock.
    pub(crate) fn post_read(
        &self,
        cache: &Cell<EchoCache>,
        before: SectionRead,
        grid: &Grid,
        section: SectionFacts,
    ) {
        let after_present = before.target.present_in(grid);
        let after_identity = EchoRowIdentity::of(grid) == before.target.identity;
        let mut cached = cache.get();
        let mut facts = 0_u8;
        if !before.same_identity || !after_identity {
            facts |= FACT_IDENTITY_CHANGED;
        } else {
            // When: `same_identity` and `after_identity` both held, the cell's presence decides the facts.
            let appeared = cached.recorded & FACT_APPEARED != 0;
            if before.present && !appeared {
                facts |= FACT_PRE_PRESENT;
            }
            if !before.present && after_present && section.consumed > 0 && !appeared {
                facts |= FACT_APPEARED;
            }
            if appeared && !after_present {
                facts |= FACT_LOST;
            }
        }
        let new_facts = facts & !cached.recorded;
        if new_facts == 0 {
            // When: `new_facts` is 0, every fact this section saw is recorded; the slot is not locked again.
            return;
        }
        cached.recorded |= new_facts;
        cache.set(cached);
        self.record_with_timeline(before.token, section.parsed_at, |trace, timeline| {
            if new_facts & FACT_IDENTITY_CHANGED != 0 {
                trace.identity_changed = true;
            }
            if new_facts & FACT_PRE_PRESENT != 0 {
                trace.pre_present = true;
            }
            if new_facts & FACT_APPEARED != 0 && trace.appearance.is_none() {
                trace.appearance = Some(EchoAppearance {
                    generation: section.generation,
                    parsed_at: section.parsed_at,
                    sync_open: section.sync_open,
                });
                // The appearing section's latest chunk read; an unstamped or pre-arm read records nothing.
                let read = section
                    .latest_read_at
                    .and_then(|read_at| read_at.checked_duration_since(trace.armed_at));
                if let (Some(timeline), Some(elapsed)) = (timeline, read) {
                    timeline.record(elapsed, Some(EchoTimelineKindV1::LatestChunkRead));
                }
            }
            if new_facts & FACT_LOST != 0 {
                trace.lost = true;
            }
        });
    }

    /// The target for `token`: from the worker's cache, else fetched once under the slot lock.
    fn cached_target(&self, cache: &Cell<EchoCache>, token: u64) -> Option<EchoWatchTarget> {
        let cached = cache.get();
        if cached.token == token {
            // When: this token's target was already fetched, no lock is taken.
            return cached.target;
        }
        let fetched = self.target_for(token);
        let target = fetched.map(|(target, _)| target);
        let recorded = fetched.map_or(0, |(_, recorded)| recorded);
        cache.set(EchoCache { token, target, recorded });
        target
    }
}

/// A fact the worker has recorded for its cached token, so it locks the slot once per fact.
const FACT_APPEARED: u8 = 1;
const FACT_PRE_PRESENT: u8 = 1 << 1;
const FACT_IDENTITY_CHANGED: u8 = 1 << 2;
const FACT_LOST: u8 = 1 << 3;

/// The VT worker's view of the watch: the token it last fetched, that token's target, and the
/// facts it has recorded for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EchoCache {
    token: u64,
    target: Option<EchoWatchTarget>,
    recorded: u8,
}

/// A section's first read of an armed watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SectionRead {
    token: u64,
    target: EchoWatchTarget,
    present: bool,
    same_identity: bool,
}

/// What the section that just parsed contributes to a fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SectionFacts {
    /// Bytes the section consumed.
    pub(crate) consumed: usize,
    /// When it finished parsing.
    pub(crate) parsed_at: Instant,
    /// The generation its batch publishes.
    pub(crate) generation: u64,
    /// Whether a synchronized update is set after it.
    pub(crate) sync_open: bool,
    /// The read stamp of the chunk this section consumed; `None` for an unstamped chunk.
    pub(crate) latest_read_at: Option<Instant>,
}

impl super::App {
    /// Arm pane `pane_id`'s echo watch for `target` with a new token. Takes no parser lock.
    #[doc(hidden)]
    pub fn arm_echo_watch(&mut self, pane_id: u64, target: EchoWatchTarget) -> ArmOutcome {
        self.arm_echo_watch_at(pane_id, target, Instant::now())
    }

    /// `arm_echo_watch` with an explicit arm instant, so a test orders its events by offsets from it.
    pub(super) fn arm_echo_watch_at(
        &mut self,
        pane_id: u64,
        target: EchoWatchTarget,
        armed_at: Instant,
    ) -> ArmOutcome {
        if self.frame_counters.is_none() {
            // When: `frame_counters` is None, the gate is off; no pane has a watch and none is allocated.
            return ArmOutcome::GateOff;
        }
        let next_token = self.next_echo_arm;
        let Some(window) =
            self.windows.values_mut().find(|window| window.panes.contains_key(&pane_id))
        else {
            // When: no window holds pane_id, there is no watch to arm.
            return ArmOutcome::NoPane;
        };
        let pane = &window.panes[&pane_id];
        let Some(counters) = pane.frame_counters.as_ref() else {
            // When: the pane was created without counter handles, it has no watch.
            return ArmOutcome::GateOff;
        };
        let token = next_token;
        let Some(next) = token.checked_add(1).filter(|_| token != 0) else {
            // When: the counter cannot advance, every token has been issued.
            return ArmOutcome::Exhausted;
        };
        let watch = std::sync::Arc::clone(&counters.echo);
        // The PTY stamps reads exactly when it was created with the gate on, as every pane here was.
        let chunk_timestamps = pane.pty.is_some();
        let valid_permit = window.redraw.link_permit.filter(|_| window.redraw.link_permit_valid());
        let timeline = window.redraw.timeline.admit(
            pane_id,
            ArmToken(token),
            &watch,
            chunk_timestamps,
            valid_permit,
        );
        watch.arm_with(ArmToken(token), target, armed_at, timeline);
        self.next_echo_arm = next;
        ArmOutcome::Armed(ArmToken(token))
    }

    /// Take pane `pane_id`'s record for `token`, disarming its watch. Takes no parser lock.
    #[doc(hidden)]
    pub fn take_echo_watch(&mut self, pane_id: u64, token: ArmToken) -> TakeOutcome {
        if self.frame_counters.is_none() {
            // When: `frame_counters` is None, the gate is off and no pane has a watch.
            return TakeOutcome::GateOff;
        }
        let Some(window) =
            self.windows.values_mut().find(|window| window.panes.contains_key(&pane_id))
        else {
            // When: no `window` has pane_id in its `panes`, its record is unreachable.
            return TakeOutcome::NoPane;
        };
        let Some(counters) = window.panes[&pane_id].frame_counters.as_ref() else {
            // When: the pane's `frame_counters` is None, it was created without a watch.
            return TakeOutcome::GateOff;
        };
        // Only this window's owner, for this pane and token, freezes and copies its ring.
        let flood = window.redraw.timeline.frozen_flood_for(pane_id, token);
        let owner = flood.is_some();
        let taken = counters.echo.take_with(token, flood);
        if owner && matches!(taken, SlotTake::Trace(_)) {
            // The owner's successful take ends the owner; its frozen timeline stays in the slot.
            window.redraw.timeline.end_for_pane(pane_id);
        }
        match taken {
            SlotTake::Trace(mut trace) => {
                trace.shown = self.pane_shown_in_main(pane_id);
                TakeOutcome::Trace(trace)
            }
            SlotTake::Mismatch => TakeOutcome::Mismatch,
            SlotTake::AlreadyTaken => TakeOutcome::AlreadyTaken,
        }
    }

    /// Test-only: the next token `arm_echo_watch` issues.
    #[doc(hidden)]
    pub fn __test_set_next_echo_arm(&mut self, next: u64) {
        self.next_echo_arm = next;
    }

    /// The pane `pane_id` in whichever window holds it.
    pub(super) fn find_pane(&self, pane_id: u64) -> Option<&super::PaneState> {
        self.windows.values().find_map(|window| window.panes.get(&pane_id))
    }

    /// Whether `pane_id` is drawn by the main window's active tab.
    fn pane_shown_in_main(&self, pane_id: u64) -> bool {
        let Some(main) = self.main() else {
            // When: there is no main window, no pane is shown in it.
            return false;
        };
        let Some(tab) = main.tab_states.get(main.tabs.active_index()) else {
            // When: no tab state matches the active index, nothing is shown.
            return false;
        };
        let mut shown = false;
        super::frame_counters::for_each_shown_pane(&tab.tree, &mut |shown_id| {
            shown |= shown_id == pane_id;
        });
        shown
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only: slot-lock acquisitions on this thread.
    static SLOT_LOCKS: Cell<u64> = const { Cell::new(0) };
    /// Test-only: run once at the next writer's pause point on this thread.
    static WRITER_PAUSE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    /// Test-only: run once before the next timeline write takes the slot lock on this thread.
    static TIMELINE_PAUSE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    /// Test-only: run once between the next publication and its delivery record on this thread.
    static DELIVERY_PAUSE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
impl ArmToken {
    /// Test-only: a token a test arms a watch with directly, at an instant it chooses.
    pub(crate) fn for_test(token: u64) -> Self {
        Self(token)
    }
}

#[cfg(test)]
impl EchoWatch {
    /// Test-only: the slot's record whatever its state, without taking it.
    pub(crate) fn peek(&self) -> Option<EchoTrace> {
        self.lock_slot().trace
    }

    /// Test-only: the timeline slot's state: `absent`, `recording`, `frozen` or `transferred`.
    pub(crate) fn peek_timeline(&self) -> &'static str {
        match &self.lock_slot().timeline {
            TimelineSlot::Absent => "absent",
            TimelineSlot::Recording(timeline) if timeline.frozen() => "frozen",
            TimelineSlot::Recording(_) => "recording",
            TimelineSlot::Transferred => "transferred",
        }
    }
}

/// Test-only: run `pause` once, on this thread, after the next flush's publication is recorded
/// and before its token decision is recorded.
#[cfg(test)]
pub(crate) fn pause_before_delivery(pause: impl FnOnce() + 'static) {
    DELIVERY_PAUSE.with(|slot| *slot.borrow_mut() = Some(Box::new(pause)));
}

/// Run and clear this thread's delivery pause, if a test set one.
fn run_delivery_pause() {
    #[cfg(test)]
    {
        let pause = DELIVERY_PAUSE.with(|slot| slot.borrow_mut().take());
        if let Some(pause) = pause {
            pause();
        }
    }
}

/// Test-only: run `pause` once, on this thread, where the next timeline write has its instant and
/// has not yet taken the slot lock.
#[cfg(test)]
pub(crate) fn pause_next_timeline_write(pause: impl FnOnce() + 'static) {
    TIMELINE_PAUSE.with(|slot| *slot.borrow_mut() = Some(Box::new(pause)));
}

/// Run and clear this thread's timeline-write pause, if a test set one.
fn run_timeline_pause() {
    #[cfg(test)]
    {
        let pause = TIMELINE_PAUSE.with(|slot| slot.borrow_mut().take());
        if let Some(pause) = pause {
            pause();
        }
    }
}

/// Test-only: slot-lock acquisitions made so far on this thread.
#[cfg(test)]
pub(crate) fn slot_locks() -> u64 {
    SLOT_LOCKS.with(Cell::get)
}

/// Test-only: run `pause` once, on this thread, where the next writer has loaded its token
/// and not yet taken the slot lock.
#[cfg(test)]
pub(crate) fn pause_next_writer(pause: impl FnOnce() + 'static) {
    WRITER_PAUSE.with(|slot| *slot.borrow_mut() = Some(Box::new(pause)));
}

/// Run and clear this thread's writer pause, if a test set one.
fn run_writer_pause() {
    #[cfg(test)]
    {
        let pause = WRITER_PAUSE.with(|slot| slot.borrow_mut().take());
        if let Some(pause) = pause {
            pause();
        }
    }
}

#[cfg(test)]
#[path = "echo_watch_tests.rs"]
mod echo_watch_tests;
