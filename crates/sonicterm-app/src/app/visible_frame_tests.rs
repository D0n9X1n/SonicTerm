use super::*;
pub(super) use crate::app::{next_pane_id, TabState};
pub(super) use sonicterm_cfg::{
    config::Config,
    keymap::{Direction, Keymap},
    theme::Theme,
};
pub(super) use sonicterm_grid::grid::{CellFlags, Color, Grid};
pub(super) use sonicterm_ui::pane::PaneTree;
pub(super) use std::{sync::Barrier, time::Duration};

/// Two tabs and two visible panes; the active pane is deliberately second in layout order.
pub(super) fn fixture(child: bool, zoom: bool) -> (App, WindowId, u64, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let window = if child {
        app.__test_seed_child_window(&["visible", "inactive"])
    } else {
        app.__test_seed_tab("inactive");
        app.main_window_id.unwrap()
    };
    let right = next_pane_id();
    let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
        Grid::new(20, 3),
        None,
        Arc::clone(&app.capture_staging_pool),
    )));
    let pane = PaneState::new_with_media_pool(parser, None, &app.inline_media_pool);
    let state = app.windows.get_mut(&window).unwrap();
    state.tabs.activate(0);
    let left = state.tab_states[0].active_pane;
    let inactive = state.tab_states[1].active_pane;
    state.panes.insert(right, pane);
    assert!(state.tab_states[0].tree.split(left, Direction::Right, right));
    state.tab_states[0].active_pane = right;
    if zoom {
        assert!(state.tab_states[0].tree.toggle_zoom(right));
    }
    for id in [left, right, inactive] {
        state.panes[&id].inline_images.lock().push(InlineImage {
            id,
            row: 0,
            col: 0,
            width: 1,
            height: 1,
            bgra: Arc::from([0, 0, 0, 255]),
        });
    }
    (app, window, left, right, inactive)
}

/// Exercise the same role adapter called by each production RedrawRequested branch.
pub(super) fn sources(
    app: &mut App,
    window: WindowId,
    child: bool,
) -> Result<VisibleFrameSources, FrameUnavailable> {
    let outer = Rect::new(0.0, 0.0, 400.0, 120.0);
    if child {
        app.child_visible_frame_sources(window, outer)
    } else {
        app.main_visible_frame_sources(outer)
    }
}

/// The real guarded PaneRender slice is the callback boundary, not a copied-grid substitute.
pub(super) fn assemble(
    app: &mut App,
    window: WindowId,
    sources: &VisibleFrameSources,
    held: &mut HeldVisibleFrame<'_>,
    callback: impl FnOnce(&mut [PaneRender<'_>]),
) {
    let state = app.windows.get_mut(&window).unwrap();
    let viewports = sources.reconcile_viewports(&mut state.panes, &held.guards).unwrap();
    let mut panes = pane_renders(
        &mut held.guards,
        &mut held.images,
        &viewports,
        sources.active_id(),
        &BTreeSet::new(),
        &HashMap::new(),
    );
    callback(&mut panes);
}

pub(super) fn app_free_try_lock_probe(
    sources: &VisibleFrameSources,
    id: u64,
) -> Option<MutexGuard<'_, Parser>> {
    sources.entries.iter().find(|entry| entry.id == id).unwrap().parser.try_lock()
}

/// Present a frame of `window`'s visible panes as the adapters do: collect, read one receipt per held
/// pane with `rows`, release the guards, and store the bound receipts as the pending set.
pub(super) fn present_receipts(
    app: &mut App,
    window: WindowId,
    child: bool,
    rows: sonicterm_render_model::AckRows,
) {
    let sources = sources(app, window, child).ok().unwrap();
    let held = sources.try_collect(|| {}).ok().unwrap();
    let receipts = held
        .guards
        .iter()
        .enumerate()
        .map(|(index, (id, parser, _))| AckReceipt::of(index, *id, parser.grid(), rows.clone()))
        .collect();
    drop(held);
    let tickets = sources.bind(receipts);
    app.windows.get_mut(&window).unwrap().pending_receipts = tickets;
}

/// The window's next collection through the production helper: reconcile, then apply its receipts.
pub(super) fn collect_next(
    app: &mut App,
    window: WindowId,
    child: bool,
) -> Result<(), FrameUnavailable> {
    let sources = sources(app, window, child)?;
    let mut held = sources.try_collect(|| {})?;
    let state = app.windows.get_mut(&window).unwrap();
    sources.reconcile_and_apply_receipts(state, &mut held.guards).map(|_| ())
}

/// The dirty rows of `pane` in `window`.
pub(super) fn dirty(app: &App, window: WindowId, pane: u64) -> Vec<usize> {
    app.windows[&window].panes[&pane].parser.lock().grid().dirty_rows().collect()
}

/// Every row of `pane`'s grid, as `dirty` reports a fully dirty pane.
pub(super) fn all_rows(app: &App, window: WindowId, pane: u64) -> Vec<usize> {
    (0..usize::from(app.windows[&window].panes[&pane].parser.lock().grid().rows)).collect()
}

/// The window's `dirt_ack_dropped` count.
pub(super) fn dropped(app: &App, window: WindowId) -> u64 {
    app.windows[&window]
        .redraw
        .frame_counters
        .as_ref()
        .map_or(0, |counters| counters.dirt_ack_dropped)
}

/// Turn on the window's frame counters and mark every visible pane dirty.
pub(super) fn counted_and_dirty(app: &mut App, window: WindowId, panes: &[u64]) {
    let state = app.windows.get_mut(&window).unwrap();
    state.redraw.frame_counters = Some(Box::default());
    for pane in panes {
        state.panes[pane].parser.lock().grid_mut().mark_all_dirty();
    }
}

/// A ticket for the pane held under `id` by `parser`, read from its grid as it is now.
pub(super) fn ticket_for(id: u64, parser: &Arc<Mutex<Parser>>) -> AckTicket {
    let receipt = AckReceipt::of(0, id, parser.lock().grid(), sonicterm_render_model::AckRows::All);
    AckTicket { receipt, parser: Arc::downgrade(parser) }
}

/// One ticket case: its label, the change made after the receipt, whether the ticket then applies,
/// and the dirty rows left (`None` keeps them all).
pub(super) type TicketChange = (&'static str, fn(&mut Parser), bool, Option<usize>);

/// Suppress or restore `window` the way production does: hide or show it, park or unpark its
/// redraw, or record or clear a stopped device generation.
pub(super) fn suppress(app: &mut App, window: WindowId, child: bool, mode: &str, suppressed: bool) {
    match (mode, child) {
        ("hide", false) if suppressed => app.hide_main_window(),
        ("hide", false) => app.show_main_window(),
        ("hide", true) => app.windows.get_mut(&window).unwrap().hidden = suppressed,
        ("park", _) => app.windows.get_mut(&window).unwrap().redraw.parked = suppressed,
        _ => {
            app.windows.get_mut(&window).unwrap().redraw.stopped_generation =
                suppressed.then_some(1)
        }
    }
}

/// Present a frame that issues, per held pane, the receipt `rows_for` gives its id (`None` issues
/// none for that pane, as a `Noop` plan does), and store them as the pending set.
pub(super) fn present_pane_receipts(
    app: &mut App,
    window: WindowId,
    child: bool,
    rows_for: impl Fn(u64) -> Option<sonicterm_render_model::AckRows>,
) {
    let sources = sources(app, window, child).ok().unwrap();
    let held = sources.try_collect(|| {}).ok().unwrap();
    let receipts = held
        .guards
        .iter()
        .enumerate()
        .filter_map(|(index, (id, parser, _))| {
            Some(AckReceipt::of(index, *id, parser.grid(), rows_for(*id)?))
        })
        .collect();
    drop(held);
    let tickets = sources.bind(receipts);
    app.windows.get_mut(&window).unwrap().pending_receipts = tickets;
}

/// The viewport slot that draws live row `live_row` of a `rows`-row view whose top is
/// `view_top_abs`, over `scrollback_len` history rows; `None` below the view.
pub(super) fn slot_of_live_row(
    scrollback_len: u64,
    view_top_abs: u64,
    rows: u16,
    live_row: u64,
) -> Option<u64> {
    (scrollback_len + live_row).checked_sub(view_top_abs).filter(|slot| *slot < u64::from(rows))
}

/// Custody and dispatch totals as (custodies, rendered dispatches, failed dispatches).
pub(super) fn custody_counts(totals: &crate::app::guard_custody::CustodyTotals) -> (u64, u64, u64) {
    use std::sync::atomic::Ordering;
    (
        totals.custodies.load(Ordering::Relaxed),
        totals.dispatches.load(Ordering::Relaxed),
        totals.dispatches_failed.load(Ordering::Relaxed),
    )
}

/// What a scripted acquisition does for one pane: succeed at once, or stay busy while the clock moves.
#[derive(Clone, Copy)]
pub(super) enum ScriptedOutcome {
    /// The probe takes the free parser.
    Free,
    /// The probe takes the free parser, but the clock moves by `late` before it returns, as under preemption.
    FreeLate { late: Duration },
    /// The probe fails; the timed wait advances the clock by `wait` and then takes the parser.
    BusyThenFree { wait: Duration },
    /// The probe fails; the timed wait advances the clock by `wait` and gives up.
    BusyThroughout { wait: Duration },
}

/// One acquisition call the scripted seam saw: the pane, the call, and the clock when it was made.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum AcquireCall {
    Probe(u64),
    Wait { pane_id: u64, deadline: Instant, at: Instant },
}

/// A deterministic acquisition over an injected clock: each pane follows its script, and every call is
/// recorded, so the shared-deadline and late-guard contracts are pinned without timing assumptions.
pub(super) struct ScriptedAcquire {
    clock: std::cell::Cell<Instant>,
    script: HashMap<u64, ScriptedOutcome>,
    pub(super) calls: std::cell::RefCell<Vec<AcquireCall>>,
    /// How far the clock moves while the hook after a failed probe runs.
    pub(super) hook_advance: std::cell::Cell<Duration>,
}

impl ScriptedAcquire {
    pub(super) fn new(script: &[(u64, ScriptedOutcome)]) -> Self {
        Self {
            clock: std::cell::Cell::new(Instant::now()),
            script: script.iter().copied().collect(),
            calls: Default::default(),
            hook_advance: Default::default(),
        }
    }
}

impl ParserAcquire for ScriptedAcquire {
    fn now(&self) -> Instant {
        self.clock.get()
    }

    fn probe<'a>(&self, pane_id: u64, parser: &'a Mutex<Parser>) -> Option<MutexGuard<'a, Parser>> {
        self.calls.borrow_mut().push(AcquireCall::Probe(pane_id));
        match self.script[&pane_id] {
            // The parser is genuinely free in these tests, so a scripted success takes the real guard.
            ScriptedOutcome::Free => parser.try_lock(),
            ScriptedOutcome::FreeLate { late } => {
                let guard = parser.try_lock();
                self.clock.set(self.clock.get() + late);
                guard
            }
            ScriptedOutcome::BusyThenFree { .. } | ScriptedOutcome::BusyThroughout { .. } => None,
        }
    }

    fn after_failed_probe(&self, _pane_id: u64) {
        self.clock.set(self.clock.get() + self.hook_advance.get());
    }

    fn wait_until<'a>(
        &self,
        pane_id: u64,
        parser: &'a Mutex<Parser>,
        deadline: Instant,
    ) -> Option<MutexGuard<'a, Parser>> {
        let at = self.clock.get();
        self.calls.borrow_mut().push(AcquireCall::Wait { pane_id, deadline, at });
        match self.script[&pane_id] {
            ScriptedOutcome::Free | ScriptedOutcome::FreeLate { .. } => parser.try_lock(),
            ScriptedOutcome::BusyThenFree { wait } => {
                self.clock.set(at + wait);
                parser.try_lock()
            }
            ScriptedOutcome::BusyThroughout { wait } => {
                self.clock.set(at + wait);
                None
            }
        }
    }
}

/// Production acquisition whose hook releases a held parser only after the nonwaiting probe failed.
pub(super) struct ReleaseAfterFailedProbe {
    pub(super) release: std::sync::mpsc::Sender<()>,
    pub(super) failed_probes: std::cell::Cell<usize>,
}

impl ParserAcquire for ReleaseAfterFailedProbe {
    fn now(&self) -> Instant {
        TimedParserAcquire.now()
    }

    fn probe<'a>(&self, pane_id: u64, parser: &'a Mutex<Parser>) -> Option<MutexGuard<'a, Parser>> {
        TimedParserAcquire.probe(pane_id, parser)
    }

    fn after_failed_probe(&self, _pane_id: u64) {
        self.failed_probes.set(self.failed_probes.get() + 1);
        // The holder may already have left if a previous hook released it; nothing then waits.
        let _ = self.release.send(());
    }

    fn wait_until<'a>(
        &self,
        pane_id: u64,
        parser: &'a Mutex<Parser>,
        deadline: Instant,
    ) -> Option<MutexGuard<'a, Parser>> {
        TimedParserAcquire.wait_until(pane_id, parser, deadline)
    }
}
