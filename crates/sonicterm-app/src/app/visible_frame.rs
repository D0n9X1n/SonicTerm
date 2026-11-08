//! Visible-only frame ownership shared by the main and child adapters.
//!
//! Validate topology before cloning a source or taking a lock. Owned sources
//! outlive parser guards; images are copied only after every visible parser is
//! held. This is not an atomic grid/media generation: workers merge decoded
//! media later, and their redraw plus the renderer's media identity heals it.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::{Mutex, MutexGuard};
use sonicterm_render_model::{AckReceipt, FrameSource, InlineImage, PaneRender};
use sonicterm_ui::pane::Rect;
use sonicterm_vt::vt::Parser;
use winit::window::WindowId;

use super::{
    viewport_anchor::{reconcile_held_viewports, FrameViewports, ViewportAnchor},
    App, WindowState,
};

/// Event-loop-owned topology failures, never a reason to retry on a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutInvalid {
    MissingTabState,
    DuplicatePane(u64),
    MissingPane(u64),
    ActiveNotLeaf(u64),
    ZoomDisagrees(u64),
    VisibleDisagrees,
}

/// A collection that cannot assemble a complete visible frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameUnavailable {
    /// Closing or reaped window/tab: silent skip.
    NoLayout,
    /// Broken event-loop topology: skip all assembly, no contention deadline.
    StructuralInvalid(LayoutInvalid),
    /// Only a visible lock not obtained within the collection budget can arm the existing contention floor.
    Contended { pane_id: u64, images: bool },
}

/// Why one collection missed, for its window's counters. The kinds are disjoint, so their sum is
/// the collection's failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MissKind {
    /// A parser was probed and not obtained, or obtained only after the deadline.
    Parser,
    /// The shared deadline had already passed before this pane's parser was probed.
    BudgetExpired,
    /// Every parser was held but an image store was busy.
    Images,
}

/// What one collection attempt measured, read by its caller for the window's counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct CollectionNote {
    /// Time spent taking parsers: from the first acquisition to the last guard or the miss, in us.
    pub(super) parser_wait_us: u64,
    /// The miss and the visible-pane order index (layout order) of the pane it happened on.
    pub(super) miss: Option<(usize, MissKind)>,
}

/// The requested maximum a collection waits, in total across its visible panes, for busy parsers.
///
/// A PTY worker relocks its parser every few tens of microseconds during a flood, so a nonwaiting
/// probe rarely lands between sections. A short timed wait queues the collection among the parser's
/// waiters, and the worker's fair release hands the lock to a selected waiter, not necessarily this
/// one. Scheduler preemption can still overrun this budget.
pub(super) const COLLECTION_LOCK_BUDGET: Duration = Duration::from_millis(2);

/// How a collection takes one visible parser: a nonwaiting probe, then a wait bounded by a deadline.
///
/// Production uses the parking_lot mutex and the monotonic clock. Tests substitute a scripted
/// acquisition and clock to pin the shared-deadline and late-guard contracts deterministically.
pub(super) trait ParserAcquire {
    /// The current instant on the clock the deadline was taken from.
    fn now(&self) -> Instant;
    /// Take the parser only if it is free right now.
    fn probe<'a>(&self, pane_id: u64, parser: &'a Mutex<Parser>) -> Option<MutexGuard<'a, Parser>>;
    /// Runs only after a probe failed while the parser was busy; production does nothing here.
    fn after_failed_probe(&self, pane_id: u64);
    /// Wait for the parser until `deadline`, parking so a fair release can hand it over.
    fn wait_until<'a>(
        &self,
        pane_id: u64,
        parser: &'a Mutex<Parser>,
        deadline: Instant,
    ) -> Option<MutexGuard<'a, Parser>>;
}

/// The production acquisition: parking_lot's mutex and `Instant::now`.
pub(super) struct TimedParserAcquire;

impl ParserAcquire for TimedParserAcquire {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn probe<'a>(
        &self,
        _pane_id: u64,
        parser: &'a Mutex<Parser>,
    ) -> Option<MutexGuard<'a, Parser>> {
        parser.try_lock()
    }

    fn after_failed_probe(&self, _pane_id: u64) {}

    fn wait_until<'a>(
        &self,
        _pane_id: u64,
        parser: &'a Mutex<Parser>,
        deadline: Instant,
    ) -> Option<MutexGuard<'a, Parser>> {
        parser.try_lock_until(deadline)
    }
}

/// Validated layout order and the active pane's actual position within it.
#[derive(Debug)]
struct VisibleLayout {
    rects: Vec<(u64, Rect)>,
    active_pos: usize,
}

/// Immutable owned handles and event-loop viewport metadata for one visible pane.
struct VisibleSource {
    id: u64,
    rect: Rect,
    parser: Arc<Mutex<Parser>>,
    images: Arc<super::media::InlineImageStore>,
    viewport_top_abs: Option<u64>,
    viewport_anchor: ViewportAnchor,
}

/// Source handles own the mutexes for the entire lifetime of the borrowed guards.
pub(super) struct VisibleFrameSources {
    entries: Vec<VisibleSource>,
    /// The App's custody totals when it counts; then a collection times its guard custody and dispatch.
    counted: Option<Arc<super::guard_custody::CustodyTotals>>,
    /// The App's UI span store when it counts; then a collection records each parser guard it holds.
    spans: Option<super::guard_correlation::SharedSpanStore>,
    pub(super) tab_index: usize,
    pub(super) active_pos: usize,
    /// The last collection's measurement; its caller takes it into the window's counters.
    note: std::cell::Cell<Option<CollectionNote>>,
    #[cfg(test)]
    image_visits: std::cell::RefCell<Vec<u64>>,
    #[cfg(test)]
    image_clones: std::cell::RefCell<Vec<(u64, usize)>>,
}

/// One parser guard per validated visible pane, in layout order.
pub(super) type ParserGuards<'a> = Vec<(u64, MutexGuard<'a, Parser>, Rect)>;

/// Guarded visible grids and separately sampled media; never a copied-grid frame.
pub(super) struct HeldVisibleFrame<'a, S = ()> {
    pub(super) snapshot: S,
    pub(super) guards: ParserGuards<'a>,
    /// The frame's guard custody when its App counts; declared after `guards`, so it records after them.
    pub(super) custody: Option<super::guard_custody::GuardCustody>,
    /// The frame's dispatch interval when its App counts.
    pub(super) dispatch: Option<super::guard_custody::DispatchClock>,
    pub(super) images: Vec<Arc<Vec<InlineImage>>>,
}

impl VisibleFrameSources {
    /// Capture only visible owned handles after the complete id-only layout validation.
    fn capture(window: &WindowState, outer: Rect) -> Result<Self, FrameUnavailable> {
        if window.tabs.active().is_none() {
            // When: `window.tabs.active()` is absent, closing/reaping requires a silent skip.
            return Err(FrameUnavailable::NoLayout);
        }
        let tab_index = window.tabs.active_index();
        let tab = window
            .tab_states
            .get(tab_index)
            .ok_or(FrameUnavailable::StructuralInvalid(LayoutInvalid::MissingTabState))?;
        let layout = validate_layout(
            &tab.tree.leaves(),
            tab.tree.layout(outer),
            tab.active_pane,
            tab.tree.zoomed_pane_id(),
            |id| window.panes.contains_key(&id),
        )
        .map_err(FrameUnavailable::StructuralInvalid)?;
        let mut entries = Vec::with_capacity(layout.rects.len());
        for (id, rect) in layout.rects {
            // Lookup is exact, not filter_map: a missing pane never becomes a partial frame.
            let pane = window
                .panes
                .get(&id)
                .ok_or(FrameUnavailable::StructuralInvalid(LayoutInvalid::MissingPane(id)))?;
            entries.push(VisibleSource {
                id,
                rect,
                parser: Arc::clone(&pane.parser),
                images: Arc::clone(&pane.inline_images),
                viewport_top_abs: pane.viewport_top_abs,
                viewport_anchor: pane.viewport_anchor,
            });
        }
        Ok(Self {
            entries,
            counted: None,
            spans: None,
            tab_index,
            active_pos: layout.active_pos,
            note: std::cell::Cell::new(None),
            #[cfg(test)]
            image_visits: Default::default(),
            #[cfg(test)]
            image_clones: Default::default(),
        })
    }

    /// The last collection's measurement, once; `None` before a collection or after it was taken.
    pub(super) fn take_note(&self) -> Option<CollectionNote> {
        self.note.take()
    }

    /// The validated active pane; its index is not assumed to be zero.
    pub(super) fn active_id(&self) -> u64 {
        self.entries[self.active_pos].id
    }

    /// Copy the visible geometry for scrollbar/native role-specific work.
    pub(super) fn rects(&self) -> Vec<(u64, Rect)> {
        self.entries.iter().map(|entry| (entry.id, entry.rect)).collect()
    }

    /// Capture scheduling identity before the first lock, then take parsers followed by images.
    ///
    /// `before_first_lock` is the generation-capture seam for owner-addressed scheduling. Parsers are
    /// taken under one [`COLLECTION_LOCK_BUDGET`] deadline; image stores are only probed.
    /// Any miss drops all earlier guards and image clones before returning to the retry adapter.
    pub(super) fn try_collect<S>(
        &self,
        before_first_lock: impl FnOnce() -> S,
    ) -> Result<HeldVisibleFrame<'_, S>, FrameUnavailable> {
        self.try_collect_with(&TimedParserAcquire, COLLECTION_LOCK_BUDGET, before_first_lock)
    }

    /// [`Self::try_collect`] with an explicit parser acquisition and budget, the seam its tests drive.
    // Lock order: parser then images; test observations borrow image_visits then image_clones briefly.
    pub(super) fn try_collect_with<S>(
        &self,
        acquire: &impl ParserAcquire,
        budget: Duration,
        before_first_lock: impl FnOnce() -> S,
    ) -> Result<HeldVisibleFrame<'_, S>, FrameUnavailable> {
        let snapshot = before_first_lock();
        // One absolute deadline for every visible parser: a later pane gets only what remains.
        let started = acquire.now();
        let deadline = started + budget;
        // The parser wait and the note are diagnostics: measured only while the App counts, so a
        // counters-off collection reads the clock exactly as the deadline needs and no more.
        let measuring = self.counted.is_some();
        let waited_us = || {
            measuring.then(|| {
                u64::try_from(acquire.now().saturating_duration_since(started).as_micros())
                    .unwrap_or(u64::MAX)
            })
        };
        let note = |parser_wait_us: Option<u64>, miss: Option<(usize, MissKind)>| {
            // Only a counting App measured `parser_wait_us`, so only it leaves a note.
            if let Some(parser_wait_us) = parser_wait_us {
                self.note.set(Some(CollectionNote { parser_wait_us, miss }));
            }
        };
        // The span collection's identity is issued before the first acquisition, with no lock taken; declared
        // first, so it drops last, after every guard and the custody that closes it.
        let mut collection =
            self.spans.as_ref().map(super::guard_correlation::SpanCollection::open);
        // Custody and dispatch start at the first guard acquired, and only when the App counts. Declared
        // before `guards`, so an unwind drops every guard before the timing records.
        let mut timing: Option<(
            super::guard_custody::GuardCustody,
            super::guard_custody::DispatchClock,
        )> = None;
        let mut guards = Vec::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            let parser = match acquire_parser(acquire, entry.id, &entry.parser, deadline) {
                Ok(parser) => parser,
                Err(kind) => {
                    // When: a parser is contended, the earlier guards are released, then their custody closes.
                    drop(guards);
                    drop(timing);
                    note(waited_us(), Some((index, kind)));
                    return Err(FrameUnavailable::Contended { pane_id: entry.id, images: false });
                }
            };
            if timing.is_none() {
                timing = self.counted.as_ref().map(|totals| {
                    let (custody, dispatch) = super::guard_custody::start(totals);
                    (custody.with_spans(collection.take()), dispatch)
                });
            }
            if let Some((custody, _)) = timing.as_mut() {
                custody.note_guard(entry.id);
            }
            guards.push((entry.id, parser, entry.rect));
        }
        let parser_wait_us = waited_us();
        let mut images = Vec::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            #[cfg(test)]
            self.image_visits.borrow_mut().push(entry.id);
            let Some(store) = entry.images.try_snapshot() else {
                // When: an image store is contended, the guards are released, then their custody closes.
                drop(images);
                drop(guards);
                drop(timing);
                note(parser_wait_us, Some((index, MissKind::Images)));
                return Err(FrameUnavailable::Contended { pane_id: entry.id, images: true });
            };
            #[cfg(test)]
            self.image_clones.borrow_mut().push((entry.id, store.len()));
            images.push(store);
        }
        let (custody, dispatch) = timing.unzip();
        note(parser_wait_us, None);
        Ok(HeldVisibleFrame { snapshot, guards, custody, dispatch, images })
    }

    /// Resolve the captured viewport anchors against the held grids and commit the same projections.
    pub(super) fn reconcile_viewports(
        &self,
        panes: &mut crate::app::PaneMap,
        guards: &ParserGuards<'_>,
    ) -> Result<FrameViewports, FrameUnavailable> {
        // Validate every destination before committing any projection; event-loop topology is stable here.
        for entry in &self.entries {
            if !panes.contains_key(&entry.id) {
                // When: `panes` lacks `entry.id`, discard the frame before any viewport write.
                return Err(FrameUnavailable::StructuralInvalid(LayoutInvalid::MissingPane(
                    entry.id,
                )));
            }
        }
        if guards.len() != self.entries.len()
            || guards
                .iter()
                .zip(&self.entries)
                .any(|((id, _, rect), entry)| *id != entry.id || *rect != entry.rect)
        {
            // When: `guards` do not cover the validated `entries`, no partial grid projection is acceptable.
            return Err(FrameUnavailable::StructuralInvalid(LayoutInvalid::VisibleDisagrees));
        }
        for entry in &self.entries {
            let pane = panes
                .get_mut(&entry.id)
                .ok_or(FrameUnavailable::StructuralInvalid(LayoutInvalid::MissingPane(entry.id)))?;
            pane.viewport_anchor = entry.viewport_anchor;
            pane.viewport_top_abs = entry.viewport_top_abs;
        }
        Ok(reconcile_held_viewports(
            panes,
            guards.iter().map(|(id, parser, _)| (*id, &**parser)),
            self.active_id(),
        ))
    }
}

/// A presented frame's receipt bound to the pane's ownership when it was presented. The `Weak` is
/// identity only: it is never upgraded, and it keeps the parser's address from being reused while
/// the ticket lives. Liveness is decided at application: the pane must be held by the window's
/// collection with the same parser.
pub(crate) struct AckTicket {
    pub(crate) receipt: AckReceipt,
    parser: std::sync::Weak<Mutex<Parser>>,
}

/// The frame source the App lends to the renderer: it owns the visible parser guards and media
/// snapshots, so they are released when `lend` returns, before the frame is presented.
pub(super) struct HeldFrameSource<'guard, 'window> {
    pub(super) guards: ParserGuards<'guard>,
    /// The frame's guard custody; declared after `guards`, so it records right after the last release.
    pub(super) custody: Option<super::guard_custody::GuardCustody>,
    pub(super) images: Vec<Arc<Vec<InlineImage>>>,
    pub(super) viewports: &'window FrameViewports,
    pub(super) active: u64,
    pub(super) broadcast: &'window BTreeSet<u64>,
    pub(super) scrollbar_alpha: &'window HashMap<u64, f32>,
}

impl FrameSource for HeldFrameSource<'_, '_> {
    fn lend<R>(
        mut self,
        assemble: impl for<'slice, 'grid> FnOnce(&'slice mut [PaneRender<'grid>]) -> R,
    ) -> R {
        let mut panes = pane_renders(
            &mut self.guards,
            &mut self.images,
            self.viewports,
            self.active,
            self.broadcast,
            self.scrollbar_alpha,
        );
        let assembled = assemble(&mut panes);
        // Release in order before the caller presents: the borrows, every guard, then the custody,
        // which records the interval to the last release.
        drop(panes);
        drop(self.guards);
        drop(self.custody);
        assembled
    }
}

/// Build real PaneRender borrows from the held grids, moving each media snapshot exactly once.
pub(super) fn pane_renders<'a>(
    guards: &'a mut ParserGuards<'_>,
    images: &mut [Arc<Vec<InlineImage>>],
    viewports: &FrameViewports,
    active: u64,
    broadcast: &BTreeSet<u64>,
    scrollbar_alpha: &HashMap<u64, f32>,
) -> Vec<PaneRender<'a>> {
    guards
        .iter_mut()
        .zip(images)
        .map(|((id, parser, rect), images)| PaneRender {
            id: *id,
            rect_px: sonicterm_render_model::geometry::PixelRect {
                x: rect.x as i32,
                y: rect.y as i32,
                w: rect.w as u32,
                h: rect.h as u32,
            },
            grid: parser.grid_mut(),
            viewport_top_abs: viewports.of(*id),
            is_active: *id == active,
            cursor_style: sonicterm_render_model::CursorStyle::default(),
            is_broadcast_participant: broadcast.contains(id),
            scrollbar_alpha: scrollbar_alpha.get(id).copied().unwrap_or(0.0),
            inline_images: std::mem::take(images),
        })
        .collect()
}

impl App {
    /// Main render adapter: capture only that window's validated visible sources.
    pub(super) fn main_visible_frame_sources(
        &mut self,
        outer: Rect,
    ) -> Result<VisibleFrameSources, FrameUnavailable> {
        let id = self.registry.main_window_id.ok_or(FrameUnavailable::NoLayout)?;
        self.window_visible_frame_sources(id, outer)
    }

    /// Child render adapter: the explicit owner selects the same production collector.
    pub(super) fn child_visible_frame_sources(
        &mut self,
        id: WindowId,
        outer: Rect,
    ) -> Result<VisibleFrameSources, FrameUnavailable> {
        self.window_visible_frame_sources(id, outer)
    }

    fn window_visible_frame_sources(
        &mut self,
        id: WindowId,
        outer: Rect,
    ) -> Result<VisibleFrameSources, FrameUnavailable> {
        let window = self.registry.windows.get(&id).ok_or(FrameUnavailable::NoLayout)?;
        let mut sources = VisibleFrameSources::capture(window, outer)?;
        // A counting App times each frame's guard custody; otherwise no handle is cloned.
        sources.counted =
            self.frame_counters.as_ref().map(|counters| Arc::clone(&counters.custody));
        sources.spans = self
            .frame_counters
            .as_ref()
            .map(|counters| std::rc::Rc::clone(&counters.correlation.spans));
        Ok(sources)
    }

    /// Record one collection's `note` in `id`'s counters when the App counts: its parser wait, and
    /// for a miss its kind and the visible-pane index it happened on.
    pub(super) fn note_collection(&mut self, id: WindowId, note: Option<CollectionNote>) {
        let window = self.registry.windows.get_mut(&id);
        if let (Some(counters), Some(note)) =
            (window.and_then(|window| window.redraw.frame_counters.as_deref_mut()), note)
        {
            counters.note_collection(note.parser_wait_us, note.miss);
        }
    }

    /// Only genuine visible contention enters the existing retry-floor adapter.
    ///
    /// Structural invalidity consumes the captured causes and parks every owner frame deadline.
    /// The warning latch remains separate and resets only after held-frame reconciliation.
    pub(super) fn visible_frame_unavailable(
        &mut self,
        id: WindowId,
        why: FrameUnavailable,
        was_dirty: bool,
        now: Instant,
    ) {
        match why {
            FrameUnavailable::Contended { .. } => {
                // The miss itself is counted from the collection's note, which its caller recorded.
                self.defer_window_lock_contention(id, was_dirty, now)
            }
            FrameUnavailable::NoLayout => {
                if let Some(window) = self.registry.windows.get_mut(&id) {
                    let snapshot = window
                        .redraw
                        .attempt_causes
                        .take()
                        .unwrap_or_else(|| window.redraw.snapshot());
                    window.redraw.settle(snapshot, super::redraw::FrameSettlement::Settled, now);
                }
                // Closing tabs have no frame, no diagnostic, and no pending collection retry.
                if self.registry.main_window_id == Some(id) {
                    self.pending_redraw = false;
                }
                self.pending_redraw_windows.remove(&id);
            }
            FrameUnavailable::StructuralInvalid(reason) => {
                if let Some(window) = self.registry.windows.get_mut(&id) {
                    let snapshot = window
                        .redraw
                        .attempt_causes
                        .take()
                        .unwrap_or_else(|| window.redraw.snapshot());
                    window.redraw.park(snapshot);
                    if !std::mem::replace(&mut window.visible_frame_invalid, true) {
                        tracing::warn!(target: "frame_collection", ?id, ?reason, "invalid visible frame topology; assembly skipped");
                    }
                }
                if self.registry.main_window_id == Some(id) {
                    self.pending_redraw = false;
                }
                self.pending_redraw_windows.remove(&id);
                #[cfg(all(debug_assertions, not(test)))]
                debug_assert!(false, "invalid visible frame topology: {reason:?}");
            }
        }
    }
}

#[cfg(test)]
#[path = "visible_frame_tests.rs"]
mod visible_frame_tests;

#[cfg(test)]
#[path = "visible_frame_note_tests.rs"]
mod visible_frame_note_tests;

#[path = "visible_frame_acquire.rs"]
mod acquire;
use acquire::acquire_parser;
#[path = "visible_frame_receipts.rs"]
mod receipts;

#[path = "visible_frame_layout.rs"]
mod layout;
use layout::validate_layout;
