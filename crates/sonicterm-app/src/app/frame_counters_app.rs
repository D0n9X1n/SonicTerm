//! App-owned counter admission, window dispatch and gated wake accounting.

use super::*;

impl super::super::App {
    /// Turn this App's frame counters on whatever the log filter says. Only an App that has
    /// created no window, pane or renderer accepts it, so no reader ever sees the gate change.
    ///
    /// # Errors
    ///
    /// Returns [`FrameCountersTooLate`] once the App has created a window or a pane.
    #[doc(hidden)]
    pub fn force_frame_counters_on(&mut self) -> Result<(), FrameCountersTooLate> {
        if self.frame_counters_sealed.get() {
            // When: `frame_counters_sealed` is set, a window or pane exists and may have read the gate.
            return Err(FrameCountersTooLate);
        }
        self.frame_counters.get_or_insert_with(AppFrameCounters::new);
        Ok(())
    }

    /// The start of a timed dispatch; `None`, with no clock read, when the gate is off.
    pub(in crate::app) fn frame_clock_start(&self) -> Option<Instant> {
        self.frame_counters.as_ref().map(|_| Instant::now())
    }

    /// Count a `new_events` wake by its cause.
    pub(in crate::app) fn note_frame_wake(&mut self, cause: &winit::event::StartCause) {
        if let Some(counters) = self.frame_counters.as_mut() {
            // The gate is on, so every wake counts; none authorizes a line by itself.
            counters.note_wake(cause);
        }
    }

    /// Count a `user_event` wake.
    pub(in crate::app) fn note_frame_user_wake(&mut self) {
        if let Some(counters) = self.frame_counters.as_mut() {
            // The gate is on, so every user event counts.
            counters.wake_user += 1;
        }
    }

    /// Count status line work with `update`, when the gate is on.
    pub(in crate::app) fn note_status_line(
        &mut self,
        update: impl FnOnce(&mut StatusLineCounters),
    ) {
        if let Some(counters) = self.frame_counters.as_mut() {
            // The gate is on, so the status line work is counted.
            update(&mut counters.status_line);
        }
    }

    /// Add the time since `started` to the startup step `step` picks, when the gate is on.
    pub(in crate::app) fn note_startup(
        &mut self,
        started: Instant,
        step: impl FnOnce(&mut StartupSpans) -> &mut u64,
    ) {
        if let Some(counters) = self.frame_counters.as_mut() {
            // The gate is on, so the step's time is recorded.
            let elapsed_ns = Instant::now().saturating_duration_since(started).as_nanos();
            *step(&mut counters.startup) += u64::try_from(elapsed_ns).unwrap_or(u64::MAX);
        }
    }

    /// Apply `update` to the hover counters, when the gate is on; nothing runs when it is off.
    pub(in crate::app) fn note_hover(&mut self, update: impl FnOnce(&mut HoverCounters)) {
        if let Some(counters) = self.frame_counters.as_mut() {
            // The gate is on, so the hover work is counted.
            update(&mut counters.hover);
        }
    }

    /// Record a dispatch started at `started`, when the gate is on.
    pub(in crate::app) fn note_frame_dispatch(
        &mut self,
        kind: DispatchKind,
        started: Option<Instant>,
    ) {
        if let (Some(started), Some(counters)) = (started, self.frame_counters.as_mut()) {
            // the gate is on, the dispatch's duration is recorded.
            counters.record_dispatch(kind, micros_between(started, Instant::now()));
        }
    }

    /// A `RedrawRequested` for window `id`: count it and take the pending flush of each pane it
    /// draws, the active tab's leaves or only its zoomed pane. A hidden pane's flush stays
    /// pending until a redraw shows it. The tree is walked in place, allocating nothing.
    pub(in crate::app) fn note_redraw_requested(&mut self, id: winit::window::WindowId) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: the window is gone, there is nothing to count.
            return;
        };
        let Some(counters) = window.redraw.frame_counters.as_deref_mut() else {
            // When: the window's `frame_counters` is None, the gate is off; no flush atomic is touched.
            return;
        };
        counters.redraw_requested += 1;
        let Some(tab) = window.tab_states.get(window.tabs.active_index()) else {
            // When: no tab state matches `active_index`, the window draws no pane and takes no flush.
            return;
        };
        let (panes, now_ns) = (&window.panes, flush_clock_ns());
        for_each_shown_pane(&tab.tree, &mut |pane_id| {
            if let Some(pane) = panes.get(&pane_id).and_then(|pane| pane.frame_counters.as_ref()) {
                // the shown pane counts, its pending flush is taken.
                counters.take_flush(&pane.pending_flush, now_ns);
            }
        });
    }

    /// Record a `window_event` dispatch for window `id` started at `started`. `counted` says
    /// whether the window counted when the dispatch began. A window the dispatch closed gets
    /// this time before its `final=1` line prints and its totals move to `closed_windows`.
    pub(in crate::app) fn note_window_handler(
        &mut self,
        id: winit::window::WindowId,
        started: Instant,
        counted: bool,
    ) {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off and no clock is read.
            return;
        };
        let elapsed_us = micros_between(started, Instant::now());
        let live = self
            .windows
            .get_mut(&id)
            .and_then(|window| window.redraw.frame_counters.as_deref_mut());
        let closing = app.closing.as_mut().filter(|closing| counted && closing.id == id);
        if let Some(counters) = live {
            // the window still exists and counts, its handler time is recorded.
            counters.note_handler(elapsed_us);
        } else if let Some(closing) = closing {
            // When: `closing` holds this window, its own dispatch closed it; the time joins it first.
            closing.counters.note_handler(elapsed_us);
        } else if counted {
            // When: the window `counted` but already retired, the time joins closed_windows.
            app.closed_windows.record_histogram_us("handler", HistogramUnit::Millis, elapsed_us);
        }
        app.dispatching = None;
        app.finish_closing();
    }

    /// Start window `id`'s `window_event` dispatch, returning whether the window counts. If
    /// the dispatch closes it, its `final=1` line waits until its handler time is recorded.
    pub(in crate::app) fn begin_window_handler(&mut self, id: winit::window::WindowId) -> bool {
        let Some(app) = self.frame_counters.as_mut() else {
            // When: `frame_counters` is None, the gate is off and nothing is written.
            return false;
        };
        // A window an earlier dispatch closed without recording its time is finalized now.
        app.finish_closing();
        app.dispatching = Some(id);
        self.windows.get(&id).is_some_and(|window| window.redraw.frame_counters.is_some())
    }

    /// Count one serviced output event (`PaneOutput` or `RequestRedraw`) for window `id`.
    pub(in crate::app) fn note_user_request_redraw(&mut self, id: winit::window::WindowId) {
        let window = self.windows.get_mut(&id);
        if let Some(counters) =
            window.and_then(|window| window.redraw.frame_counters.as_deref_mut())
        {
            // the window counts, the output's redraw request is recorded.
            counters.note_user_request();
        }
    }

    /// Open one dispatch on the event-loop thread for this App.
    pub(in crate::app) fn frame_dispatch_scope(&self) -> DispatchScope {
        // Each dispatch starts a fresh once-per-window redraw record.
        self.dispatch_redraws.begin();
        DispatchScope::enter(
            self.frame_counters.as_ref().map(|counters| Arc::clone(&counters.dispatch)),
        )
    }

    /// The counter handles for a pane being created; `None` when the gate is off. The first
    /// pane seals the gate with a plain write to event-loop-owned state.
    pub(in crate::app) fn pane_frame_counters(&self) -> Option<PaneFrameCounters> {
        self.frame_counters_sealed.set(true);
        // The gate is now fixed, so the foreground worker's statistics are too.
        self.fg_probes.seal_stats(
            self.frame_counters.as_ref().map(|counters| Arc::clone(&counters.fg_worker)),
        );
        self.frame_counters
            .as_ref()
            .map(|counters| PaneFrameCounters::new(Arc::clone(&counters.vt)))
    }
}

/// Visit each pane a tab with `tree` draws: only the zoomed pane while one is zoomed, otherwise
/// every leaf in order. It recurses in place, so a redraw allocates nothing to find its panes.
pub(crate) fn for_each_shown_pane(
    tree: &sonicterm_ui::pane::PaneTree,
    visit: &mut impl FnMut(u64),
) {
    if let Some(zoomed) = tree.zoomed_pane_id() {
        visit(zoomed);
    } else {
        // When: `zoomed_pane_id` is None, every leaf of the tab is drawn.
        for_each_leaf(tree, visit);
    }
}

/// Visit every leaf of `tree`, first subtree first.
pub(super) fn for_each_leaf(tree: &sonicterm_ui::pane::PaneTree, visit: &mut impl FnMut(u64)) {
    match tree {
        sonicterm_ui::pane::PaneTree::Leaf { id, .. } => visit(*id),
        sonicterm_ui::pane::PaneTree::Split { first, second, .. } => {
            for_each_leaf(first, visit);
            for_each_leaf(second, visit);
        }
    }
}
