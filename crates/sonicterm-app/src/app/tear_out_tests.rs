//! Unit tests for tear-out identity and renderer adoption.
//!
//! A queued tear-out records where a tab sat. Positions move, so the request
//! also records which tab it means, and re-resolves the position from that id
//! when it is applied. These tests pin the re-resolution — the part that
//! decides whether the tab the user grabbed is the tab that moves.
//!
//! Renderer adoption is pinned here too. A pooled renderer is a real wgpu
//! renderer backed by an OS window and cannot be constructed in this unit-test
//! process, so that regression is checked at its production call site.

use super::*;

/// Detached tabs carry source geometry into both fresh and warm destination preparation.
#[test]
fn tear_out_constructor_uses_captured_dimensions() {
    let source = include_str!("tear_out.rs");
    let start = source.find("fn prepare_tear_out_destination(").unwrap();
    let end = start + source[start..].find("fn commit_torn_out_window(").unwrap();
    let prepare = &source[start..end];
    assert!(prepare.contains(".with_inner_size(request.inner_size)"));
    assert!(prepare.contains("apply_window_request("));
    assert!(!prepare.contains("LogicalSize::new(800.0, 500.0)"));
}

use sonicterm_cfg::{
    config::{Config, SubpixelAaMode},
    keymap::Keymap,
    theme::Theme,
};
use sonicterm_types::{ResourceAmount, ResourceClass, ResourceOwnerId};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

/// A renderer adopted from the warm pool must be brought to every live setting
/// that can change while it is hidden.
///
/// Runtime weight and theme actions update visible renderers only. The helper
/// proves the values independently of wgpu; the bounded source assertion proves
/// the production pooled-only branch applies them before scale rebuild. A real
/// pooled renderer requires a native event-loop window, device, and surface and
/// cannot be constructed in this unit-test process.
#[test]
fn pooled_renderer_adoption_applies_live_font_theme_and_tab_bar() {
    let mut config = Config::default();
    config.font.family = "SonicTerm Adoption Test".to_string();
    config.font.size = 19.0;
    config.font.line_height = 1.25;
    config.font.weight_scale = 2.75;
    config.font.subpixel_aa = SubpixelAaMode::Bgr;
    let theme = Theme { name: "Adoption Theme".to_string(), ..Theme::default() };

    let warm = live_renderer_settings(&config, &theme, false, ChildRendererOrigin::WarmPool);
    assert_eq!(
        warm.font,
        Some(LiveFontSettings {
            family: "SonicTerm Adoption Test",
            size: 19.0,
            line_height: 1.25,
            weight_scale: 2.75,
        })
    );
    assert_eq!(warm.theme.map(|theme| theme.name.as_str()), Some("Adoption Theme"));
    assert_eq!(warm.background, theme.colors.background.0.as_str());
    assert_eq!(warm.subpixel_aa, SubpixelAaMode::Bgr);
    assert!(!warm.tab_bar_visible);

    let fresh = live_renderer_settings(&config, &theme, false, ChildRendererOrigin::Fresh);
    assert_eq!(fresh.font, None, "fresh renderers must not rebuild an identical font atlas");
    assert!(fresh.theme.is_none(), "fresh renderers already received the constructor theme");
    assert_eq!(fresh.background, theme.colors.background.0.as_str());
    assert_eq!(fresh.subpixel_aa, SubpixelAaMode::Bgr);
    assert!(!fresh.tab_bar_visible);

    const SOURCE: &str = include_str!("tear_out.rs");
    let start = SOURCE
        .find("fn configure_child_renderer")
        .expect("the renderer-adoption function must exist");
    let body = &SOURCE[start..];
    let end = body.find("\n    pub(super) fn warm_window_pool_maintain").expect(
        "the test must stay bounded to configure_child_renderer rather than accepting a call elsewhere",
    );
    let body = &body[..end];
    let subpixel = body
        .find("renderer.set_subpixel_aa_mode(live.subpixel_aa)")
        .expect("every child renderer must receive live LCD presentation policy");
    let universal = body
        .find("renderer.set_tab_bar_visible(")
        .expect("every child renderer must receive live tab-bar visibility");
    let background = body
        .find("install_native_window_background(")
        .expect("every child renderer must receive the live native background");
    let plan = body
        .find("live_renderer_settings(&self.config, &self.theme, self.tab_bar_visible, origin)")
        .expect("renderer configuration must derive one typed live-settings plan");
    let exact_set_font =
        "renderer.set_font(font.family, font.size, font.line_height, font.weight_scale);";
    let set_font = body.find(exact_set_font).expect(
        "warm adoption must pass the planned family, size, line height, and weight in order",
    );
    let set_theme = body
        .find("renderer.set_theme(theme)")
        .expect("warm adoption must update the planned live theme");
    let resize = body
        .find("renderer.force_rebuild_for_scale(")
        .expect("adoption must still rebuild for the destination display scale");
    assert!(plan < subpixel && subpixel < universal && universal < set_font);
    assert!(plan < background && background < set_font);
    assert!(set_font < set_theme && set_theme < resize);

    let prepare = &SOURCE[SOURCE
        .find("fn prepare_tear_out_destination")
        .expect("destination preparation must exist")..];
    let prepare = &prepare
        [..prepare.find("fn commit_torn_out_window").expect("preparation must end before commit")];
    let warm_arm = &prepare[prepare.find("Some(mut warm) =>").expect("warm arm must exist")
        ..prepare.find("None =>").expect("cold arm must exist")];
    assert!(warm_arm.contains("ChildRendererOrigin::WarmPool"));
    assert!(!warm_arm.contains("ChildRendererOrigin::Fresh"));
    let cold_arm = &prepare[prepare.find("None =>").expect("cold arm must exist")..];
    assert!(cold_arm.contains("ChildRendererOrigin::Fresh"));
    assert!(warm_arm.contains("configure_child_renderer("));
    assert!(cold_arm.contains("configure_child_renderer("));

    let warm_create =
        &SOURCE[SOURCE.find("fn create_warm_window").expect("warm-window constructor must exist")
            ..SOURCE.find("fn take_warm_window").expect("warm-window take seam must exist")];
    assert!(warm_create.contains("ChildRendererOrigin::Fresh"));

    const MISC_SOURCE: &str = include_str!("misc.rs");
    let new_window_start =
        MISC_SOURCE.find("fn create_new_terminal_window").expect("Cmd+N constructor must exist");
    let new_window = &MISC_SOURCE[new_window_start..];
    let new_window = &new_window[..new_window
        .find("fn drain_menubar_actions")
        .expect("the Cmd+N assertion must stay bounded to its own constructor")];
    assert!(new_window.contains("ChildRendererOrigin::Fresh"));
    assert!(
        !new_window.contains("ChildRendererOrigin::WarmPool"),
        "Cmd+N builds its renderer from the current settings, so treating it as pooled would \
         rebuild the font atlas and theme it was just constructed with"
    );
    assert!(new_window.contains("configure_child_renderer("));
}

/// Every tear-out destination stays hidden until renderer adoption and window
/// registration have both succeeded.
#[test]
fn a_child_is_revealed_only_after_adoption_and_install_succeed() {
    const SOURCE: &str = include_str!("tear_out.rs");
    let prepare_start =
        SOURCE.find("fn prepare_tear_out_destination").expect("destination preparation must exist");
    let commit_start =
        SOURCE.find("fn commit_torn_out_window").expect("destination commit must exist");
    let prepare = &SOURCE[prepare_start..commit_start];
    let commit = &SOURCE[commit_start
        ..SOURCE
            .find("pub fn tear_out_apply_source_side")
            .expect("commit must end before source cleanup")];

    assert!(!prepare.contains("set_visible(true)"));
    assert!(prepare.contains(".with_visible(false)"));
    assert_eq!(commit.matches("set_visible(true)").count(), 1);
    let installed = commit.find("insert_window_registered(").expect("window registration");
    let sized = commit.find("resize_visible_panes_in_child").expect("destination pane sizing");
    let reveal = commit.find("set_visible(true)").expect("single destination reveal");
    assert!(installed < sized && sized < reveal);
}

/// Warm and fresh tear-outs share the same one-time post-commit reveal.
#[test]
fn every_destination_is_revealed_only_after_commit() {
    const SOURCE: &str = include_str!("tear_out.rs");
    let warm_create =
        &SOURCE[SOURCE.find("fn create_warm_window").expect("warm-window constructor must exist")
            ..SOURCE.find("fn take_warm_window").expect("warm-window take seam must exist")];
    assert!(warm_create.contains("with_visible(false)"));

    let prepare_start =
        SOURCE.find("fn prepare_tear_out_destination").expect("destination preparation must exist");
    let commit_start =
        SOURCE.find("fn commit_torn_out_window").expect("destination commit must exist");
    let prepare = &SOURCE[prepare_start..commit_start];
    assert!(prepare.contains("Some(mut warm) =>"));
    assert!(prepare.contains(".with_visible(false)"));
    assert!(!prepare.contains("set_visible(true)"));

    let commit = &SOURCE[commit_start
        ..SOURCE
            .find("pub fn tear_out_apply_source_side")
            .expect("commit must end before source cleanup")];
    assert_eq!(commit.matches("set_visible(true)").count(), 1);
    assert!(commit.contains("set_render_timing_label(\"child\")"));
}

fn app_with_tabs(titles: &[&str]) -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    for title in titles {
        app.__test_seed_tab(title);
    }
    app
}

#[derive(Debug, PartialEq, Eq)]
struct SourceSnapshot {
    tab_order: Vec<sonicterm_ui::tabs::TabId>,
    active_tab: Option<sonicterm_ui::tabs::TabId>,
    pane_ids: Vec<u64>,
    grid_sizes: HashMap<u64, (u16, u16)>,
    redraw_targets: HashMap<u64, Option<WindowId>>,
    pane_owners: HashMap<u64, Option<ResourceOwnerId>>,
    pane_charges: HashMap<u64, HashMap<ResourceClass, ResourceAmount>>,
    window_owner: Option<ResourceOwnerId>,
}

fn source_snapshot(app: &App, window_id: WindowId) -> SourceSnapshot {
    let window = app.windows.get(&window_id).expect("source window");
    let mut pane_ids: Vec<_> = window.panes.keys().copied().collect();
    pane_ids.sort_unstable();
    SourceSnapshot {
        tab_order: window.tabs.tabs().iter().map(|tab| tab.id).collect(),
        active_tab: window.tabs.active().map(|tab| tab.id),
        grid_sizes: pane_ids
            .iter()
            .map(|pane_id| {
                let grid = window.panes[pane_id].parser.lock();
                let grid = grid.grid();
                (*pane_id, (grid.cols, grid.rows))
            })
            .collect(),
        redraw_targets: pane_ids
            .iter()
            .map(|pane_id| (*pane_id, *window.panes[pane_id].redraw_target.lock()))
            .collect(),
        pane_owners: pane_ids
            .iter()
            .map(|pane_id| (*pane_id, window.panes[pane_id].owner.as_ref().map(|owner| owner.id())))
            .collect(),
        pane_charges: pane_ids
            .iter()
            .map(|pane_id| {
                (
                    *pane_id,
                    window.panes[pane_id]
                        .charges
                        .iter()
                        .map(|(class, held)| (*class, held.committed_amount()))
                        .collect(),
                )
            })
            .collect(),
        pane_ids,
        window_owner: window.owner.as_ref().map(|owner| owner.id()),
    }
}

fn prepare_non_vacuous_source(app: &mut App, window_id: WindowId, pane_id: u64) {
    {
        let pane = app
            .windows
            .get_mut(&window_id)
            .and_then(|window| window.panes.get_mut(&pane_id))
            .expect("source pane");
        pane.parser.lock().grid_mut().resize(73, 19);
        *pane.redraw_target.lock() = Some(window_id);
    }
    app.__test_reconcile_pane_owners();
    app.__test_charge_pane_owners();
    assert!(app.__test_pane_owner(window_id, pane_id).is_some(), "concrete pane owner");
    assert!(
        app.__test_pane_charges(window_id, pane_id)
            .expect("pane charges")
            .values()
            .any(|amount| !amount.is_zero()),
        "at least one reservation must be nonzero"
    );
}

struct CleanupDropProbe(Rc<Cell<usize>>);

// Lifecycle: dropping the cleanup-owned probe proves its one-shot action was consumed.
impl Drop for CleanupDropProbe {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

fn app_with_reducer_counts(tab_count: u32, live_window_count: u32) -> App {
    let state = sonicterm_app_core::AppState {
        tab_count,
        live_window_count,
        active_tab_idx: Some(tab_count.saturating_sub(1) as usize),
        ..Default::default()
    };
    App::new_with_proxy_and_machine(
        Theme::default(),
        Config::default(),
        Keymap::default(),
        None,
        sonicterm_app_core::AppStateMachine::new(state),
    )
}

#[test]
fn existing_window_transfer_refusal_preserves_the_complete_source() {
    // Destination readiness and charge admission are checked before any source-owned state can be lost.
    for refusal in 0..3 {
        let mut app = app_with_tabs(&["A", "B", "C"]);
        let source = app.main_window_id.unwrap();
        let pane_id = app.windows[&source].tab_states[1].active_pane;
        app.main_tabs_mut().unwrap().activate(1);
        prepare_non_vacuous_source(&mut app, source, pane_id);
        let target = app.__test_seed_child_window(&["destination"]);
        if refusal != 0 {
            app.__test_set_child_pane_viewport(
                target,
                sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
                10.0,
                10.0,
            );
        }
        if refusal == 1 {
            let window = app.windows.get_mut(&target).unwrap();
            for pane in window.panes.values_mut() {
                pane.charges.clear();
                drop(pane.owner.take());
            }
            drop(window.owner.take());
        } else if refusal == 2 {
            app.governor.begin_close(app.windows[&target].owner.as_ref().unwrap().id()).unwrap();
        }
        let before = source_snapshot(&app, source);
        let parser = app.windows[&source].panes[&pane_id].parser.clone();

        assert!(app.transfer_tab(None, 1, Some(target), 0).is_err());

        assert_eq!(source_snapshot(&app, source), before, "refusal={refusal}");
        assert!(Arc::ptr_eq(&app.windows[&source].panes[&pane_id].parser, &parser));
        assert_eq!(app.windows[&target].tabs.len(), 1);
    }
}

#[test]
fn refused_charged_transfer_cannot_reap_the_source_window() {
    // A charged final tab stays attached if the destination has no accounting owner.
    let mut app = app_with_tabs(&["destination"]);
    let target = app.main_window_id.unwrap();
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    );
    let source = app.__test_seed_child_window(&["source"]);
    let pane_id = app.windows[&source].tab_states[0].active_pane;
    prepare_non_vacuous_source(&mut app, source, pane_id);
    let source_owner = app.windows[&source].owner.as_ref().unwrap().id();
    let window = app.windows.get_mut(&target).unwrap();
    for pane in window.panes.values_mut() {
        pane.charges.clear();
        drop(pane.owner.take());
    }
    drop(window.owner.take());
    let before = source_snapshot(&app, source);

    assert!(app.transfer_tab(Some(source), 0, None, 0).is_err());

    assert_eq!(source_snapshot(&app, source), before);
    assert_eq!(
        app.governor.snapshot(source_owner).unwrap().owner_state,
        sonicterm_types::OwnerState::Open
    );
}

#[derive(Clone, Copy)]
enum FailureRoute {
    Main,
    Child,
    DeferredMain,
    DeferredChild,
}

fn run_failed_route(route: FailureRoute, stage: TearOutStage) {
    let mut app = app_with_reducer_counts(3, 2);
    let (source_window, pane_id, tab_id) = match route {
        FailureRoute::Main | FailureRoute::DeferredMain => {
            let pane_id = app.__test_seed_tab("source");
            let source = app.__test_main_window_id().expect("synthetic main");
            let tab = app.tab_id_at(source, 0).expect("source tab");
            (source, pane_id, tab)
        }
        FailureRoute::Child | FailureRoute::DeferredChild => {
            let source = app.__test_seed_child_window(&["source"]);
            let pane_id = app.__test_child_pane_ids(source).expect("source child")[0];
            let tab = app.tab_id_at(source, 0).expect("source tab");
            (source, pane_id, tab)
        }
    };
    prepare_non_vacuous_source(&mut app, source_window, pane_id);
    let before = source_snapshot(&app, source_window);
    let window_keys: HashSet<_> = app.windows.keys().copied().collect();
    let frontmost = app.frontmost_window;
    let hidden = app.__test_main_hidden();
    let reducer = {
        let state = app.machine.state();
        assert!(state.tab_count > 0 && state.live_window_count > 0);
        (state.tab_count, state.live_window_count, state.active_tab_idx)
    };
    let pending_new_window = app.pending_new_window;
    let redraws = app.redraw_request_count.load(Ordering::Relaxed);
    let reaps = app.reap_call_count.load(Ordering::Relaxed);
    let cleanup_calls = Rc::new(Cell::new(0usize));
    let cleanup_drops = Rc::new(Cell::new(0usize));
    let observed_detached = Rc::new(Cell::new(false));
    let calls = cleanup_calls.clone();
    let drops = cleanup_drops.clone();
    let observed = observed_detached.clone();
    let disposition = match stage {
        TearOutStage::CreateWindow | TearOutStage::DeviceStopped => DestinationDisposition::Nothing,
        TearOutStage::RendererInit | TearOutStage::RendererConfigure => {
            DestinationDisposition::DropFresh
        }
    };
    let installer = move |app: &mut App, transaction, _, _| {
        app.tear_out_with_destination(transaction, |_app| {
            let calls = calls.clone();
            let observed = observed.clone();
            let drop_probe = CleanupDropProbe(drops);
            Err(DestinationFailure::probe(
                stage,
                disposition,
                Box::new(move |app| {
                    let _drop_probe = drop_probe;
                    calls.set(calls.get() + 1);
                    observed.set(app.tab_index_of_id(source_window, tab_id).is_none());
                }),
            ))
        })
    };

    match route {
        FailureRoute::Main => assert!(app.tear_out_tab_with_installer(0, installer)),
        FailureRoute::Child => {
            assert!(app.tear_out_from_child_with_installer(source_window, 0, installer));
        }
        FailureRoute::DeferredMain | FailureRoute::DeferredChild => {
            app.drain_pending_tear_out_with_installer(
                crate::app::PendingTearOut {
                    source_window,
                    source_tab_idx: 0,
                    source_tab_id: Some(tab_id),
                    drop_screen_pos: Some((41, 73)),
                },
                installer,
            );
        }
    }

    assert_eq!(cleanup_calls.get(), 1);
    assert_eq!(cleanup_drops.get(), 1);
    assert!(observed_detached.get());
    assert_eq!(source_snapshot(&app, source_window), before);
    assert_eq!(app.windows.keys().copied().collect::<HashSet<_>>(), window_keys);
    assert_eq!(app.frontmost_window, frontmost);
    assert_eq!(app.__test_main_hidden(), hidden);
    let state = app.machine.state();
    assert_eq!((state.tab_count, state.live_window_count, state.active_tab_idx), reducer);
    assert_eq!(app.pending_new_window, pending_new_window);
    assert_eq!(app.redraw_request_count.load(Ordering::Relaxed), redraws);
    assert_eq!(app.reap_call_count.load(Ordering::Relaxed), reaps);
}

/// Every renderer origin and failure stage maps to one explicit cleanup policy.
#[test]
fn destination_failure_disposition_is_total() {
    assert_eq!(
        destination_disposition(ChildRendererOrigin::Fresh, TearOutStage::CreateWindow),
        DestinationDisposition::Nothing
    );
    for stage in [TearOutStage::RendererInit, TearOutStage::RendererConfigure] {
        assert_eq!(
            destination_disposition(ChildRendererOrigin::Fresh, stage),
            DestinationDisposition::DropFresh
        );
    }
    for stage in [TearOutStage::CreateWindow, TearOutStage::RendererInit] {
        assert_eq!(
            destination_disposition(ChildRendererOrigin::WarmPool, stage),
            DestinationDisposition::ReturnWarm
        );
    }
    assert_eq!(
        destination_disposition(ChildRendererOrigin::WarmPool, TearOutStage::RendererConfigure),
        DestinationDisposition::RetireWarm
    );
    for origin in [ChildRendererOrigin::Fresh, ChildRendererOrigin::WarmPool] {
        assert_eq!(
            destination_disposition(origin, TearOutStage::DeviceStopped),
            DestinationDisposition::Nothing
        );
    }
}

struct AcceptedSink;

impl crate::os_drag::OsDragSink for AcceptedSink {
    fn begin_drag(&self, _payload: &crate::os_drag::TabPayload) -> crate::os_drag::DragAck {
        crate::os_drag::DragAck::Accepted
    }
}

/// A committed OS handoff retains the reducer accounting that formerly ran
/// before every route, while failed native setup performs none.
#[test]
fn committed_handoff_accounts_only_after_the_sink_accepts() {
    let mut app = app_with_reducer_counts(3, 2);
    app.__test_seed_tab("handoff");
    app.__test_set_os_drag_sink(Arc::new(AcceptedSink));
    let before = app.machine.state();
    let before = (before.tab_count, before.live_window_count);

    assert!(app.tear_out_tab_with_installer(0, |_, _, _, _| {
        panic!("accepted handoff must not enter native installation")
    }));

    let after = app.machine.state();
    assert_eq!(after.tab_count, before.0 - 1);
    assert_eq!(after.live_window_count, before.1 + 1);
    assert!(app.pending_new_window.is_none(), "the committed route creates its own destination");
}

/// Native event-loop wrappers delegate all orchestration to the route helpers
/// exercised by the rollback matrix.
#[test]
fn native_wrappers_delegate_to_the_tested_route_helpers() {
    const TEAR_OUT: &str = include_str!("tear_out.rs");
    let main = &TEAR_OUT[TEAR_OUT.find("fn tear_out_tab(").expect("main wrapper")
        ..TEAR_OUT.find("fn tear_out_tab_with_installer").expect("main helper")];
    assert!(main.contains("tear_out_tab_with_installer"));
    assert!(main.contains("install_torn_out_window"));
    assert!(!main.contains("detach_for_tear_out"));

    let child = &TEAR_OUT[TEAR_OUT.find("fn tear_out_from_child(").expect("child wrapper")
        ..TEAR_OUT.find("fn tear_out_from_child_with_installer").expect("child helper")];
    assert!(child.contains("tear_out_from_child_with_installer"));
    assert!(child.contains("install_torn_out_window"));
    assert!(!child.contains("detach_for_tear_out"));

    const MISC: &str = include_str!("misc.rs");
    let deferred = &MISC[MISC.find("fn drain_pending_tear_out(").expect("deferred wrapper")
        ..MISC.find("fn drain_pending_tear_out_with_installer").expect("deferred helper")];
    assert!(deferred.contains("drain_pending_tear_out_with_installer"));
    assert!(deferred.contains("install_torn_out_window"));
    assert!(!deferred.contains("detach_for_tear_out"));
}

/// Main, child, and deferred production routes restore exact state at every
/// typed destination failure stage.
#[test]
fn every_tear_out_route_rolls_back_every_destination_failure_stage() {
    for route in [FailureRoute::Main, FailureRoute::Child, FailureRoute::DeferredMain] {
        for stage in [
            TearOutStage::CreateWindow,
            TearOutStage::RendererInit,
            TearOutStage::RendererConfigure,
            TearOutStage::DeviceStopped,
        ] {
            run_failed_route(route, stage);
        }
    }
    run_failed_route(FailureRoute::DeferredChild, TearOutStage::RendererConfigure);
}

fn assert_pty_marker(pty: &sonicterm_io::pty::PtyHandle, command: Vec<u8>, marker: &[u8]) {
    pty.send_input_nonblocking(command).expect("send marker through PTY");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while !output.windows(marker.len()).any(|window| window == marker) && Instant::now() < deadline
    {
        match pty.out_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("PTY output disconnected before marker")
            }
        }
    }
    assert!(
        output.windows(marker.len()).any(|window| window == marker),
        "PTY did not emit marker {:?} before the deadline; output={:?}",
        String::from_utf8_lossy(marker),
        String::from_utf8_lossy(&output)
    );
}

fn fail_without_destination(
    app: &mut App,
    transaction: DetachedTab,
    _screen_pos: Option<(i32, i32)>,
    _source: &'static str,
) -> Option<WindowId> {
    app.tear_out_with_destination(transaction, |_app| {
        Err(DestinationFailure::probe(
            TearOutStage::CreateWindow,
            DestinationDisposition::Nothing,
            Box::new(|_| {}),
        ))
    })
}

/// Failure-time source cleanup never overrides the active tab after exact
/// rollback in main, child, or deferred orchestration.
#[test]
fn rollback_skips_source_cleanup_for_every_route() {
    for route in [
        FailureRoute::Main,
        FailureRoute::Child,
        FailureRoute::DeferredMain,
        FailureRoute::DeferredChild,
    ] {
        let mut app = app_with_reducer_counts(3, 2);
        let source = match route {
            FailureRoute::Main | FailureRoute::DeferredMain => {
                for title in ["a", "b", "c"] {
                    app.__test_seed_tab(title);
                }
                let source = app.__test_main_window_id().expect("synthetic main");
                assert!(app.__test_invoke_activate_main_tab(2));
                source
            }
            FailureRoute::Child | FailureRoute::DeferredChild => {
                let source = app.__test_seed_child_window(&["a", "b", "c"]);
                assert!(app.__test_invoke_activate_tab_in_child(source, 2));
                source
            }
        };
        let tab_id = app.tab_id_at(source, 1).expect("inactive source tab");
        let before = source_snapshot(&app, source);

        match route {
            FailureRoute::Main => {
                assert!(app.tear_out_tab_with_installer(1, fail_without_destination));
            }
            FailureRoute::Child => {
                assert!(app.tear_out_from_child_with_installer(
                    source,
                    1,
                    fail_without_destination,
                ));
            }
            FailureRoute::DeferredMain | FailureRoute::DeferredChild => {
                app.drain_pending_tear_out_with_installer(
                    crate::app::PendingTearOut {
                        source_window: source,
                        source_tab_idx: 1,
                        source_tab_id: Some(tab_id),
                        drop_screen_pos: None,
                    },
                    fail_without_destination,
                );
            }
        }

        assert_eq!(source_snapshot(&app, source), before);
    }
}

/// An invalid main tear-out does not invoke installation or mutate reducer
/// accounting for a tab that never left.
#[test]
fn out_of_range_tear_out_is_a_complete_no_op() {
    let mut app = app_with_reducer_counts(3, 2);
    app.__test_seed_tab("source");
    let source = app.__test_main_window_id().expect("synthetic main");
    let before = source_snapshot(&app, source);
    let reducer = app.machine.state();
    let reducer = (reducer.tab_count, reducer.live_window_count, reducer.active_tab_idx);

    assert!(app.tear_out_tab_with_installer(99, |_, _, _, _| {
        panic!("an invalid source must not reach destination installation")
    }));

    assert_eq!(source_snapshot(&app, source), before);
    let after = app.machine.state();
    assert_eq!((after.tab_count, after.live_window_count, after.active_tab_idx), reducer);
}

/// Rollback restores the exact middle slot without stealing focus from either
/// side of the torn inactive tab.
#[test]
fn rollback_preserves_five_tab_order_and_active_identity_on_both_sides() {
    for (active_index, torn_index) in [(0, 3), (4, 1)] {
        let mut app = app_with_tabs(&["a", "b", "c", "d", "e"]);
        let window = app.__test_main_window_id().expect("synthetic main");
        assert!(app.__test_invoke_activate_main_tab(active_index));
        let before = source_snapshot(&app, window);

        assert!(app.tear_out_tab_with_installer(torn_index, |app, transaction, _, _| {
            app.tear_out_with_destination(transaction, |_app| {
                Err(DestinationFailure::probe(
                    TearOutStage::CreateWindow,
                    DestinationDisposition::Nothing,
                    Box::new(|_| {}),
                ))
            })
        }));

        assert_eq!(source_snapshot(&app, window), before);
    }
}

/// Production preparation binds every failure stage to owned cleanup while all
/// visibility and registration mutations remain in commit.
#[test]
fn production_failure_arms_own_partial_destinations_before_commit() {
    const SOURCE: &str = include_str!("tear_out.rs");
    let unwind = &SOURCE[SOURCE.find("fn drop_fresh").expect("fresh unwind")
        ..SOURCE.find("fn warm(").expect("warm unwind")];
    let drop_renderer = unwind.find("drop(renderer)").expect("renderer drop");
    let drop_window = unwind.find("drop(window)").expect("window drop");
    assert!(drop_renderer < drop_window);

    let prepare_start =
        SOURCE.find("fn prepare_tear_out_destination").expect("destination preparation");
    let commit_start = SOURCE.find("fn commit_torn_out_window").expect("destination commit");
    let prepare = &SOURCE[prepare_start..commit_start];
    for cleanup in [
        "DestinationUnwind::nothing()",
        "DestinationUnwind::drop_fresh(window.clone(), None)",
        "DestinationUnwind::drop_fresh(window, Some(renderer))",
        "DestinationUnwind::warm(warm, DestinationDisposition::RetireWarm)",
    ] {
        assert!(prepare.contains(cleanup), "missing owned cleanup: {cleanup}");
    }
    assert!(prepare.contains(".with_visible(false)"));
    for forbidden in ["set_visible(true)", "insert_window_registered", "request_redraw()"] {
        assert!(!prepare.contains(forbidden), "preparation performed commit action: {forbidden}");
    }

    let commit = &SOURCE[commit_start
        ..SOURCE.find("pub fn tear_out_apply_source_side").expect("source cleanup boundary")];
    for required in [
        "insert_window_registered",
        "resize_visible_panes_in_child",
        "set_visible(true)",
        "request_redraw()",
    ] {
        assert!(commit.contains(required), "commit omitted action: {required}");
    }
}

/// A failed tear-out keeps the same shell process and accepts input without a
/// source-side PTY resize.
#[test]
fn a_rolled_back_tear_out_keeps_the_same_live_shell_usable() {
    #[cfg(unix)]
    let (command, args, baseline_command, rollback_command) = (
        "/bin/sh",
        Vec::<String>::new(),
        b"printf 'SONICTERM_BASELINE_OK\\n'\n".to_vec(),
        b"printf 'SONICTERM_ROLLBACK_OK\\n'\n".to_vec(),
    );
    #[cfg(windows)]
    let (command, args, baseline_command, rollback_command) = (
        "cmd.exe",
        vec!["/D".to_owned(), "/Q".to_owned()],
        b"echo SONICTERM_BASELINE_OK\r\n".to_vec(),
        b"echo SONICTERM_ROLLBACK_OK\r\n".to_vec(),
    );
    let mut pty = sonicterm_io::pty::PtyHandle::spawn_with_args(command, &args, 73, 19)
        .expect("spawn long-lived shell");
    let pid = pty.pid().expect("live shell pid");
    let exit_probe = pty.child_exit_probe();
    #[cfg(windows)]
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
    assert_pty_marker(&pty, baseline_command, b"SONICTERM_BASELINE_OK");
    let resize_calls = Arc::new(AtomicUsize::new(0));
    let counter = resize_calls.clone();
    let resize = std::mem::replace(&mut pty.resize, Box::new(|_, _| Ok(())));
    pty.resize = Box::new(move |cols, rows| {
        counter.fetch_add(1, Ordering::Relaxed);
        resize(cols, rows)
    });

    let mut app = app_with_reducer_counts(1, 1);
    let pane_id = app.__test_seed_tab("live shell");
    let window = app.__test_main_window_id().expect("synthetic main");
    assert!(app.__test_set_pane_pty(pane_id, Some(pty)));
    prepare_non_vacuous_source(&mut app, window, pane_id);

    assert!(app.tear_out_tab_with_installer(0, |app, transaction, _, _| {
        app.tear_out_with_destination(transaction, |_app| {
            Err(DestinationFailure::probe(
                TearOutStage::RendererConfigure,
                DestinationDisposition::DropFresh,
                Box::new(|_| {}),
            ))
        })
    }));

    let pane = app.main().expect("main window").panes.get(&pane_id).expect("restored pane");
    let restored = pane.pty.as_ref().expect("restored PTY");
    assert_eq!(restored.pid(), Some(pid));
    assert!(!exit_probe.has_exited().expect("probe live shell"));
    assert_eq!(resize_calls.load(Ordering::Relaxed), 0);
    assert_pty_marker(restored, rollback_command, b"SONICTERM_ROLLBACK_OK");

    drop(app);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !exit_probe.has_exited().expect("probe shell cleanup") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(exit_probe.has_exited().expect("shell cleaned up after app drop"));
}

/// Failed destination setup restores the exact inactive main tab after running
/// destination cleanup while that tab is still detached.
#[test]
fn failed_main_tear_out_restores_order_focus_and_live_pane_state() {
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    assert!(app.__test_invoke_activate_main_tab(2));
    let torn = app.tab_id_at(window, 1).expect("middle tab");
    let active = app.tab_id_at(window, 2).expect("active tab");
    let pane = app.main_tab_states().expect("main tab states")[1].active_pane;
    {
        let pane_state = app.main_mut().expect("main window").panes.get_mut(&pane).expect("pane");
        pane_state.parser.lock().grid_mut().resize(73, 19);
        *pane_state.redraw_target.lock() = Some(window);
    }
    app.__test_reconcile_pane_owners();
    app.__test_charge_pane_owners();
    let owner = app.__test_pane_owner(window, pane).expect("pane owner");
    let charges = app.__test_pane_charges(window, pane).expect("pane charges");
    assert!(charges.values().any(|amount| !amount.is_zero()), "precondition: nonzero charge");
    let order: Vec<_> =
        app.main_tabs().expect("main tabs").tabs().iter().map(|tab| tab.id).collect();
    let cleanup_ran = Rc::new(Cell::new(false));
    let cleanup_observed_detached = Rc::new(Cell::new(false));
    let ran = cleanup_ran.clone();
    let observed = cleanup_observed_detached.clone();

    let handled = app.tear_out_tab_with_installer(1, move |app, transaction, _, _| {
        app.tear_out_with_destination(transaction, |_app| {
            let ran = ran.clone();
            let observed = observed.clone();
            Err(DestinationFailure::probe(
                TearOutStage::RendererConfigure,
                DestinationDisposition::DropFresh,
                Box::new(move |app| {
                    ran.set(true);
                    observed.set(app.tab_index_of_id(window, torn).is_none());
                }),
            ))
        })
    });

    assert!(handled);
    assert!(cleanup_ran.get());
    assert!(cleanup_observed_detached.get());
    assert_eq!(
        app.main_tabs().expect("main tabs").tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(),
        order
    );
    assert_eq!(app.main_tabs().and_then(|tabs| tabs.active()).map(|tab| tab.id), Some(active));
    assert_eq!(app.__test_pane_grid_size(pane), Some((73, 19)));
    assert_eq!(*app.__test_pane_redraw_target(pane).expect("redraw target").lock(), Some(window));
    assert_eq!(app.__test_pane_owner(window, pane), Some(owner));
    assert_eq!(app.__test_pane_charges(window, pane), Some(charges));
}

/// A tab closing at a lower index must not redirect a queued tear-out.
///
/// This is the whole defect: the recorded index stays *in range* when a tab
/// below it closes, so a bounds check passes while the slot now holds a
/// different tab. Asserting both halves — that the stale index misnames the
/// tab, and that the id still finds it — is what makes this a test of the fix
/// rather than of `position`.
#[test]
fn a_lower_tab_closing_moves_the_index_but_not_the_identity() {
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");

    // The user grabs the middle tab.
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");
    let last = app.tab_id_at(window, 2).expect("tab at index 2");
    assert_ne!(grabbed, last, "test setup: three distinct tabs");

    // Its neighbour below closes mid-gesture — a shell exiting, now that a
    // background thread can close a tab with no user action to serialise
    // against the drag.
    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, grabbed),
        Some(0),
        "the grabbed tab moved down one slot and must still be findable there"
    );
    // The half that shows why the id is needed: the recorded index survives
    // the close and now names the wrong tab.
    assert_eq!(
        app.tab_id_at(window, 1),
        Some(last),
        "the recorded index 1 now holds the tab that was at 2 — a build trusting the index \
         would tear out this one instead of the one the user grabbed"
    );
}

/// A tab that closed entirely must fail the tear-out, not promote a neighbour.
#[test]
fn a_tab_that_closed_resolves_to_nothing() {
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let doomed = app.tab_id_at(window, 0).expect("tab at index 0");

    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, doomed),
        None,
        "a closed tab must resolve to nothing so the tear-out fails, rather than moving \
         whichever tab inherited its slot"
    );
    // And the surviving tab is still reachable by its own id, so the failure
    // above is specific rather than the lookup being broken outright.
    let survivor = app.tab_id_at(window, 0).expect("survivor at index 0");
    assert_eq!(app.tab_index_of_id(window, survivor), Some(0));
}

/// The drain must resolve through the id, not the recorded index.
///
/// This drives `resolve_tear_out_source_index` — the method the drain calls —
/// rather than the lookup helpers underneath it. That distinction is the point
/// of the test: an earlier version exercised only the helpers, and a build that
/// ignored the recorded id entirely and trusted the stale index passed it.
#[test]
fn a_queued_tear_out_follows_its_tab_when_a_lower_tab_closes() {
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");

    // The request as the gesture recorded it.
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    // A shell exits in the tab below and closes it mid-gesture.
    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(0),
        "the drop must move the tab the user grabbed, which is now at index 0 — resolving to \
         the recorded index 1 would tear out the tab that inherited the slot"
    );
}

/// A tear-out whose tab closed must fail rather than move a neighbour.
#[test]
fn a_queued_tear_out_fails_when_its_own_tab_closed() {
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 0).expect("tab at index 0");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 0,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        None,
        "the grabbed tab is gone, so the tear-out must fail; index 0 still resolves to a live \
         tab and a build trusting it would tear out the wrong one"
    );
}

/// A request with no recorded id keeps the old index behaviour.
#[test]
fn a_tear_out_without_an_id_falls_back_to_its_index() {
    let app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: None,
        drop_screen_pos: None,
    };

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(1),
        "a request built without an id must still resolve, or the fallback path is dead"
    );
}

/// A rolled-back tear-out leaves every pane charging the app's media pool,
/// with no charge added or released.
#[test]
fn a_rolled_back_tear_out_keeps_each_pane_on_its_media_pool() {
    let pool = crate::app::media::InlineMediaPool::new();
    let mut app = app_with_reducer_counts(3, 2).with_inline_media_pool(pool.clone());
    for title in ["a", "b", "c"] {
        app.__test_seed_tab(title);
    }
    let source = app.__test_main_window_id().expect("synthetic main");
    assert_eq!(pool.live_charges(), 3);

    assert!(app.tear_out_tab_with_installer(1, fail_without_destination));

    assert_eq!(pool.live_charges(), 3, "a rolled-back tear-out neither adds nor releases a charge");
    for pane in app.windows[&source].panes.values() {
        assert!(Arc::ptr_eq(pane.inline_media_charge.lock().pool(), &pool));
    }
}

/// A tab transferred into another window keeps its pane on the app's media
/// pool, with no charge added or released.
#[test]
fn a_transferred_tab_keeps_its_pane_on_the_media_pool() {
    let pool = crate::app::media::InlineMediaPool::new();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default())
        .with_inline_media_pool(pool.clone());
    let moved = app.__test_seed_tab("moved");
    app.__test_seed_tab("remaining");
    let child = app.__test_seed_child_window(&["destination"]);
    app.windows.get_mut(&child).unwrap().test_pane_viewport =
        Some((sonicterm_ui::pane::Rect { x: 0.0, y: 0.0, w: 800.0, h: 500.0 }, 10.0, 20.0));
    assert_eq!(pool.live_charges(), 3);

    app.transfer_tab(None, 0, Some(child), 1).unwrap();

    let pane = &app.windows[&child].panes[&moved];
    assert!(
        Arc::ptr_eq(pane.inline_media_charge.lock().pool(), &pool),
        "the moved pane keeps its pool"
    );
    assert_eq!(pool.live_charges(), 3, "a transfer neither adds nor releases a charge");
}

/// A pooled spare whose device stopped is refused before it is taken, and an
/// empty pool leaves the refusal to fresh renderer construction.
#[test]
fn a_stopped_warm_spare_is_refused_before_it_is_taken() {
    assert_eq!(warm_destination_refusal(Some(false)), Some(TearOutStage::DeviceStopped));
    assert_eq!(warm_destination_refusal(Some(true)), None);
    assert_eq!(warm_destination_refusal(None), None);
}

/// Preparation refuses a stopped spare before it takes, configures, or commits
/// it, with nothing to unwind, and rechecks each destination's device after
/// sizing it.
#[test]
fn preparation_refuses_a_stopped_device_before_taking_the_spare() {
    const SOURCE: &str = include_str!("tear_out.rs");
    let start = SOURCE.find("fn prepare_tear_out_destination(").expect("destination preparation");
    let prepare = &SOURCE[start..SOURCE.find("fn commit_torn_out_window(").expect("commit")];
    let refusal = prepare.find("warm_destination_refusal(").expect("stopped-spare refusal");
    let take = prepare.find("self.take_warm_window()").expect("warm take");
    assert!(refusal < take, "the stopped spare must be refused before it is taken");
    let refused = &prepare[refusal..take];
    assert!(refused.contains("DestinationUnwind::nothing()"));
    assert!(!refused.contains("configure_child_renderer("));
    let fresh = prepare.find("None =>").expect("fresh arm");
    for arm in [&prepare[take..fresh], &prepare[fresh..]] {
        let sized = arm.rfind("configure_child_renderer(").expect("sizing");
        let recheck = arm.rfind("device_accepts_gpu_work()").expect("device recheck");
        assert!(sized < recheck, "each destination's device is rechecked after sizing");
    }
}

/// On Windows, a real pooled spare whose device stopped is refused: the
/// tear-out leaves the spare pooled and the source window with its last tab.
#[cfg(windows)]
#[test]
fn a_stopped_device_keeps_the_spare_and_the_source_tab() {
    use crate::app::pty_test_support::isolated;
    use winit::{
        application::ApplicationHandler,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    if isolated() {
        return;
    }
    struct Probe {
        ran: bool,
    }
    impl ApplicationHandler<crate::app::UserEvent> for Probe {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            run_stopped_spare_tear_out(el);
            self.ran = true;
            el.exit();
        }
        fn window_event(
            &mut self,
            _: &ActiveEventLoop,
            _: winit::window::WindowId,
            _: winit::event::WindowEvent,
        ) {
        }
    }
    let event_loop = EventLoop::<crate::app::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut probe = Probe { ran: false };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.ran);
}

#[cfg(windows)]
fn run_stopped_spare_tear_out(el: &winit::event_loop::ActiveEventLoop) {
    use sonicterm_cfg::config::{BackdropKind, ScrollbarMode, SoftwareRenderMode};
    use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
    use sonicterm_gpu::device_errors::GpuFaultKind;
    use winit::{dpi::PhysicalSize, window::Window};
    let mut app = app_with_tabs(&["last"]);
    let source = app.__test_main_window_id().expect("synthetic main");
    let window = Arc::new(
        el.create_window(
            Window::default_attributes()
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(640, 360)),
        )
        .expect("spare window"),
    );
    let settings = RendererSettings {
        font_family: &app.config.font.family,
        font_dirs: &[],
        font_size: 14.0,
        line_height_mult: 1.0,
        font_weight_scale: 1.0,
        subpixel_aa: app.config.font.subpixel_aa,
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: BackdropKind::Opaque,
            opacity: 1.0,
            scrollbar: ScrollbarMode::Never,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Force,
        },
        role: "stopped-spare-tear-out-test",
    };
    let mut renderer =
        GpuRenderer::new(window.clone(), el, &app.theme, settings).expect("spare renderer");
    // The retained-resource fault makes the next glyph-upload rebuild invalid, which stops the device.
    renderer.__inject_gpu_fault(GpuFaultKind::RetainedResourceCreation);
    renderer.force_rebuild_for_scale(renderer.scale_factor());
    assert!(!renderer.device_accepts_gpu_work(), "the spare's device must be stopped");
    let spare = window.id();
    app.warm_window_pool.push(crate::app::WarmWindow {
        window,
        renderer,
        created_at: Instant::now(),
    });
    let before = source_snapshot(&app, source);
    let windows: HashSet<_> = app.windows.keys().copied().collect();

    assert!(app.tear_out_tab(el, 0));

    assert_eq!(app.warm_window_pool.len(), 1, "the stopped spare stays pooled");
    assert_eq!(app.warm_window_pool[0].window.id(), spare);
    assert_eq!(source_snapshot(&app, source), before, "the source keeps its last tab");
    assert_eq!(app.windows.keys().copied().collect::<HashSet<_>>(), windows);
}

/// Native registration may synchronously stop the destination device. Before
/// owner transfer, commit must revoke that registration and restore the source.
#[test]
fn tear_out_rechecks_device_after_native_registration() {
    let source = include_str!("tear_out.rs");
    let body = &source[source.find("fn commit_torn_out_window(").unwrap()..];
    let register = body.find("self.register_window_with_os_drag_backend(").unwrap();
    let check = body.find("if !destination.renderer.device_accepts_gpu_work()").unwrap();
    let transfer = body.find(".transfer_pane_owners(").unwrap();
    let reveal = body.find("destination.window.set_visible(true)").unwrap();
    assert!(register < check && check < transfer && transfer < reveal);
    let stopped = &body[check..body[check..].find("let owner =").unwrap() + check];
    let revoke = stopped.find("self.release_child_window_registries(win_id)").unwrap();
    let discard = stopped.find("drop(destination)").unwrap();
    let rollback = stopped.find("self.rollback_detached_tab(transaction)").unwrap();
    assert!(revoke < discard && discard < rollback);
    assert!(stopped[rollback..].contains("return None"));
}

/// A registration double stops a usable real destination exactly during
/// registration; the last source tab and pane custody must survive rollback.
#[cfg(windows)]
#[test]
fn registration_time_device_stop_revokes_and_restores_the_last_tab() {
    use crate::app::{os_drag, pty_test_support::isolated};
    use sonicterm_gpu::device_errors::DeviceErrorState;
    use winit::{
        application::ApplicationHandler,
        event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
        platform::windows::EventLoopBuilderExtWindows,
        window::Window,
    };
    if isolated() {
        return;
    }
    type Calls = Arc<std::sync::Mutex<Vec<(&'static str, WindowId)>>>;
    struct StopOnRegistration {
        state: Arc<DeviceErrorState>,
        calls: Calls,
        registered: Option<Arc<Window>>,
    }
    impl os_drag::OsTabDragBackend for StopOnRegistration {
        fn begin_session(
            &mut self,
            _: os_drag::AppHandle,
            _: WindowId,
            _: usize,
            _: String,
            _: Vec<u8>,
        ) {
        }

        fn register_window(
            &mut self,
            _: os_drag::AppHandle,
            id: WindowId,
            window: &Arc<Window>,
        ) -> Result<(), String> {
            assert!(self.state.accepts_gpu_work(), "preparation must have reached a usable device");
            assert!(!window.is_visible().unwrap_or(false), "registration must precede reveal");
            self.calls.lock().unwrap().push(("register", id));
            self.registered = Some(Arc::clone(window));
            self.state.record_observed_validation("injected device stop during registration");
            Ok(())
        }

        fn unregister_window(&mut self, id: WindowId) -> Result<(), String> {
            let window = self.registered.take().expect("registered destination");
            assert_eq!(window.id(), id, "revoke the exact stopped destination");
            assert!(
                !window.is_visible().unwrap_or(false),
                "stopped destination must never be shown"
            );
            self.calls.lock().unwrap().push(("unregister", id));
            Ok(())
        }
    }
    struct Probe {
        proxy: EventLoopProxy<crate::app::UserEvent>,
        ran: bool,
    }
    impl ApplicationHandler<crate::app::UserEvent> for Probe {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            let mut app = app_with_tabs(&["last"]);
            app.event_loop_proxy = Some(self.proxy.clone());
            app.config.appearance.software_render_mode =
                sonicterm_cfg::config::SoftwareRenderMode::Force;
            let source = app.__test_main_window_id().unwrap();
            let pane_id = app.main_active_pane_id().unwrap();
            prepare_non_vacuous_source(&mut app, source, pane_id);
            let window = Arc::new(
                el.create_window(
                    Window::default_attributes()
                        .with_visible(false)
                        .with_inner_size(winit::dpi::PhysicalSize::new(640, 360)),
                )
                .expect("hidden destination"),
            );
            let renderer = GpuRenderer::new(
                window.clone(),
                el,
                &app.theme,
                app.tear_out_renderer_settings("registration-stop-test"),
            )
            .expect("usable renderer");
            let state = Arc::clone(renderer.device_error_state());
            let calls: Calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            app.set_os_drag_backend(Box::new(StopOnRegistration {
                state: Arc::clone(&state),
                calls: Arc::clone(&calls),
                registered: None,
            }));
            let destination = window.id();
            app.warm_window_pool.push(crate::app::WarmWindow {
                window,
                renderer,
                created_at: Instant::now(),
            });
            let before = source_snapshot(&app, source);
            let windows: HashSet<_> = app.windows.keys().copied().collect();
            let hidden = app.__test_main_hidden();
            let frontmost = app.frontmost_window;
            assert!(app.tear_out_tab(el, 0));
            assert!(!state.accepts_gpu_work(), "registration must inject the stop");
            assert_eq!(
                *calls.lock().unwrap(),
                vec![("register", destination), ("unregister", destination)]
            );
            assert_eq!(
                source_snapshot(&app, source),
                before,
                "restore the last tab and its pane custody"
            );
            assert_eq!(app.windows.keys().copied().collect::<HashSet<_>>(), windows);
            assert_eq!(app.__test_main_hidden(), hidden, "never hide the source's last tab");
            assert_eq!(app.frontmost_window, frontmost);
            assert!(
                app.warm_window_pool.is_empty(),
                "retire the destination mutated during registration"
            );
            assert!(!app.pending_exit);
            self.ran = true;
            el.exit();
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: winit::event::WindowEvent) {
        }
    }
    let event_loop = EventLoop::<crate::app::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut probe = Probe { proxy: event_loop.create_proxy(), ran: false };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.ran);
}
