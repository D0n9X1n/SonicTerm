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
    time::Instant,
};

use parking_lot::{Mutex, MutexGuard};
use sonicterm_render_model::{AckReceipt, FrameSource, InlineImage, PaneRender};
use sonicterm_ui::pane::Rect;
use sonicterm_vt::vt::Parser;
use winit::window::WindowId;

use super::{
    viewport_anchor::{reconcile_held_viewports, FrameViewports, ViewportAnchor},
    App, PaneState, WindowState,
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
    /// Only a failed visible try-lock can arm the existing contention floor.
    Contended { pane_id: u64, images: bool },
}

/// Validated layout order and the active pane's actual position within it.
#[derive(Debug)]
struct VisibleLayout {
    rects: Vec<(u64, Rect)>,
    active_pos: usize,
}

/// Validate live topology using ids only; hidden parser and image state is never read.
fn validate_layout(
    leaves: &[u64],
    rects: Vec<(u64, Rect)>,
    active: u64,
    zoom: Option<u64>,
    exists: impl Fn(u64) -> bool,
) -> Result<VisibleLayout, LayoutInvalid> {
    let mut unique = HashSet::with_capacity(leaves.len());
    for &id in leaves {
        if !unique.insert(id) {
            // When: `unique` already contains `id`, two leaves claim the same pane.
            return Err(LayoutInvalid::DuplicatePane(id));
        }
        if !exists(id) {
            // When: `exists` rejects a leaf, presenting a subset would conceal broken topology.
            return Err(LayoutInvalid::MissingPane(id));
        }
    }
    if !unique.contains(&active) {
        // When: `active` is not a leaf, no coherent active parser index can be supplied.
        return Err(LayoutInvalid::ActiveNotLeaf(active));
    }
    if let Some(zoom) = zoom {
        // When: `zoom` is set, its owner must also be the active live leaf.
        if zoom != active || !unique.contains(&zoom) {
            // When: `zoom` does not identify the active live leaf, do not guess another pane.
            return Err(LayoutInvalid::ZoomDisagrees(zoom));
        }
    }
    let expected = zoom.map_or_else(|| leaves.to_vec(), |id| vec![id]);
    let actual: Vec<_> = rects.iter().map(|(id, _)| *id).collect();
    if actual != expected {
        // When: `actual` differs from tree order, the renderer cannot receive a coherent layout.
        return Err(LayoutInvalid::VisibleDisagrees);
    }
    let Some(active_pos) = actual.iter().position(|id| *id == active) else {
        // When: `active_pos` is absent, even a nonempty layout cannot supply the active grid.
        return Err(LayoutInvalid::ActiveNotLeaf(active));
    };
    Ok(VisibleLayout { rects, active_pos })
}

/// Immutable owned handles and event-loop viewport metadata for one visible pane.
struct VisibleSource {
    id: u64,
    rect: Rect,
    parser: Arc<Mutex<Parser>>,
    images: Arc<Mutex<Vec<InlineImage>>>,
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
    pub(super) images: Vec<Vec<InlineImage>>,
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
            #[cfg(test)]
            image_visits: Default::default(),
            #[cfg(test)]
            image_clones: Default::default(),
        })
    }

    /// The validated active pane; its index is not assumed to be zero.
    pub(super) fn active_id(&self) -> u64 {
        self.entries[self.active_pos].id
    }

    /// Copy the visible geometry for scrollbar/native role-specific work.
    pub(super) fn rects(&self) -> Vec<(u64, Rect)> {
        self.entries.iter().map(|entry| (entry.id, entry.rect)).collect()
    }

    /// Capture scheduling identity before the first lock, then try parsers followed by images.
    ///
    /// `before_first_lock` is the generation-capture seam for owner-addressed scheduling.
    /// Any miss drops all earlier guards and image clones before returning to the retry adapter.
    // Lock order: parser then images; test observations borrow image_visits then image_clones briefly.
    pub(super) fn try_collect<S>(
        &self,
        before_first_lock: impl FnOnce() -> S,
    ) -> Result<HeldVisibleFrame<'_, S>, FrameUnavailable> {
        let snapshot = before_first_lock();
        // The span collection's identity is issued before the first try_lock, with no lock taken; declared
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
        for entry in &self.entries {
            let Some(parser) = entry.parser.try_lock() else {
                // When: a parser is contended, the earlier guards are released, then their custody closes.
                drop(guards);
                drop(timing);
                return Err(FrameUnavailable::Contended { pane_id: entry.id, images: false });
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
        let mut images = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            #[cfg(test)]
            self.image_visits.borrow_mut().push(entry.id);
            let Some(store) = entry.images.try_lock() else {
                // When: an image store is contended, the guards are released, then their custody closes.
                drop(images);
                drop(guards);
                drop(timing);
                return Err(FrameUnavailable::Contended { pane_id: entry.id, images: true });
            };
            images.push(store.clone());
            #[cfg(test)]
            self.image_clones.borrow_mut().push((entry.id, store.len()));
        }
        let (custody, dispatch) = timing.unzip();
        Ok(HeldVisibleFrame { snapshot, guards, custody, dispatch, images })
    }

    /// Resolve the captured viewport anchors against the held grids and commit the same projections.
    pub(super) fn reconcile_viewports(
        &self,
        panes: &mut HashMap<u64, PaneState>,
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

impl VisibleFrameSources {
    /// Bind a presented frame's receipts to the panes this collection held, by index; a receipt whose
    /// index or pane id disagrees with the layout is dropped.
    pub(super) fn bind(&self, receipts: Vec<AckReceipt>) -> Vec<AckTicket> {
        receipts
            .into_iter()
            .filter_map(|receipt| {
                let entry =
                    self.entries.get(receipt.index).filter(|entry| entry.id == receipt.pane_id)?;
                Some(AckTicket { receipt, parser: Arc::downgrade(&entry.parser) })
            })
            .collect()
    }

    /// Store a frame's receipts as the window's pending set. Receipts are non-empty only for a
    /// presented frame, which replaces the set; any other outcome leaves the set unchanged.
    pub(super) fn store_presented(&self, pending: &mut Vec<AckTicket>, receipts: Vec<AckReceipt>) {
        if receipts.is_empty() {
            // When: `receipts` is empty the frame did not present, so the pending set stays as it was.
            return;
        }
        *pending = self.bind(receipts);
    }

    /// Reconcile the viewports, then, only when that succeeds, apply `window`'s pending receipts to
    /// the held grids and empty the set. A failed reconciliation returns before any receipt is
    /// touched, so the set and every grid's dirt stay for the next successful collection.
    pub(super) fn reconcile_and_apply_receipts(
        &self,
        window: &mut WindowState,
        guards: &mut ParserGuards<'_>,
    ) -> Result<FrameViewports, FrameUnavailable> {
        let viewports = self.reconcile_viewports(&mut window.panes, guards)?;
        let pending = std::mem::take(&mut window.pending_receipts);
        let dropped = pending.iter().filter(|ticket| !apply_ticket(ticket, guards)).count();
        if dropped > 0 {
            if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
                counters.dirt_ack_dropped += dropped as u64;
            }
        }
        Ok(viewports)
    }
}

/// Apply one pending ticket through the guard the collection already holds; returns whether it
/// cleared. It is live only when a guard holds its pane and that guard's parser is the ticket's.
pub(super) fn apply_ticket(ticket: &AckTicket, guards: &mut ParserGuards<'_>) -> bool {
    let Some((_, parser, _)) = guards.iter_mut().find(|(id, _, _)| *id == ticket.receipt.pane_id)
    else {
        // When: no held guard has the receipt's `pane_id` (switched away, removed or moved), its dirt stays.
        return false;
    };
    if !std::ptr::eq(MutexGuard::mutex(parser), ticket.parser.as_ptr()) {
        // When: the id now names another parser, the receipt describes a pane that no longer exists.
        return false;
    }
    ticket.receipt.try_apply(parser.grid_mut())
}

/// The frame source the App lends to the renderer: it owns the visible parser guards and media
/// snapshots, so they are released when `lend` returns, before the frame is presented.
pub(super) struct HeldFrameSource<'guard, 'window> {
    pub(super) guards: ParserGuards<'guard>,
    /// The frame's guard custody; declared after `guards`, so it records right after the last release.
    pub(super) custody: Option<super::guard_custody::GuardCustody>,
    pub(super) images: Vec<Vec<InlineImage>>,
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
    images: &mut [Vec<InlineImage>],
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
        let id = self.main_window_id.ok_or(FrameUnavailable::NoLayout)?;
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
        let window = self.windows.get(&id).ok_or(FrameUnavailable::NoLayout)?;
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
            FrameUnavailable::Contended { images, .. } => {
                let window = self.windows.get_mut(&id);
                if let Some(counters) =
                    window.and_then(|window| window.redraw.frame_counters.as_deref_mut())
                {
                    // the App's gate is on, the busy lock is counted where it returns here.
                    counters.note_contention(images);
                }
                self.defer_window_lock_contention(id, was_dirty, now)
            }
            FrameUnavailable::NoLayout => {
                if let Some(window) = self.windows.get_mut(&id) {
                    let snapshot = window
                        .redraw
                        .attempt_causes
                        .take()
                        .unwrap_or_else(|| window.redraw.snapshot());
                    window.redraw.settle(snapshot, super::redraw::FrameSettlement::Settled, now);
                }
                // Closing tabs have no frame, no diagnostic, and no pending collection retry.
                if self.main_window_id == Some(id) {
                    self.pending_redraw = false;
                }
                self.pending_redraw_windows.remove(&id);
            }
            FrameUnavailable::StructuralInvalid(reason) => {
                if let Some(window) = self.windows.get_mut(&id) {
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
                if self.main_window_id == Some(id) {
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
