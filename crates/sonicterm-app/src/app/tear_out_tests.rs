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
pub(super) fn tear_out_constructor_uses_captured_dimensions() {
    let source = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
    let prepare = source_method(source, "prepare_tear_out_destination");
    assert!(prepare.contains(".with_inner_size(request.inner_size)"));
    assert!(prepare.contains("apply_window_request("));
    assert!(!prepare.contains("LogicalSize::new(800.0, 500.0)"));
}

pub(super) use sonicterm_cfg::{
    config::{Config, SubpixelAaMode},
    keymap::Keymap,
    theme::Theme,
};
pub(super) use sonicterm_types::{ResourceAmount, ResourceClass, ResourceOwnerId};
pub(super) use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
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
pub(super) fn pooled_renderer_adoption_applies_live_font_theme_and_tab_bar() {
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

    const SOURCE: &str = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
    let start = SOURCE
        .find("fn configure_child_renderer")
        .expect("the renderer-adoption function must exist");
    let body = &SOURCE[start..];
    let end = body.find("\n    pub(in crate::app) fn warm_window_pool_maintain").expect(
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

    let prepare = source_method(SOURCE, "prepare_tear_out_destination");
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

    const MISC_SOURCE: &str = concat!(
        include_str!("misc.rs"),
        "\n",
        include_str!("misc_clipboard.rs"),
        "\n",
        include_str!("misc_window_create.rs")
    );
    let new_window = source_method(MISC_SOURCE, "create_new_terminal_window");
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
pub(super) fn a_child_is_revealed_only_after_adoption_and_install_succeed() {
    const SOURCE: &str = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
    let prepare = source_method(SOURCE, "prepare_tear_out_destination");
    let commit = source_method(SOURCE, "commit_torn_out_window");

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
pub(super) fn every_destination_is_revealed_only_after_commit() {
    const SOURCE: &str = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
    let warm_create =
        &SOURCE[SOURCE.find("fn create_warm_window").expect("warm-window constructor must exist")
            ..SOURCE.find("fn take_warm_window").expect("warm-window take seam must exist")];
    assert!(warm_create.contains("with_visible(false)"));

    let prepare = source_method(SOURCE, "prepare_tear_out_destination");
    assert!(prepare.contains("Some(mut warm) =>"));
    assert!(prepare.contains(".with_visible(false)"));
    assert!(!prepare.contains("set_visible(true)"));

    let commit = source_method(SOURCE, "commit_torn_out_window");
    assert_eq!(commit.matches("set_visible(true)").count(), 1);
    assert!(commit.contains("set_render_timing_label(\"child\")"));
}

pub(super) fn app_with_tabs(titles: &[&str]) -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    for title in titles {
        app.__test_seed_tab(title);
    }
    app
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SourceSnapshot {
    tab_order: Vec<sonicterm_ui::tabs::TabId>,
    active_tab: Option<sonicterm_ui::tabs::TabId>,
    pane_ids: Vec<u64>,
    grid_sizes: HashMap<u64, (u16, u16)>,
    redraw_targets: HashMap<u64, Option<WindowId>>,
    pane_owners: HashMap<u64, Option<ResourceOwnerId>>,
    pane_charges: HashMap<u64, HashMap<ResourceClass, ResourceAmount>>,
    window_owner: Option<ResourceOwnerId>,
}

pub(super) fn source_snapshot(app: &App, window_id: WindowId) -> SourceSnapshot {
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

pub(super) fn prepare_non_vacuous_source(app: &mut App, window_id: WindowId, pane_id: u64) {
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

pub(super) struct CleanupDropProbe(std::rc::Rc<Cell<usize>>);

// Lifecycle: dropping the cleanup-owned probe proves its one-shot action was consumed.
impl Drop for CleanupDropProbe {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

pub(super) fn app_with_reducer_counts(tab_count: u32, live_window_count: u32) -> App {
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
pub(super) fn existing_window_transfer_refusal_preserves_the_complete_source() {
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

#[derive(Clone, Copy)]
pub(super) enum FailureRoute {
    Main,
    Child,
    DeferredMain,
    DeferredChild,
}

pub(super) fn run_failed_route(route: FailureRoute, stage: TearOutStage) {
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
    let cleanup_calls = std::rc::Rc::new(Cell::new(0usize));
    let cleanup_drops = std::rc::Rc::new(Cell::new(0usize));
    let observed_detached = std::rc::Rc::new(Cell::new(false));
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

pub(super) struct AcceptedSink;

impl crate::os_drag::OsDragSink for AcceptedSink {
    fn begin_drag(&self, _payload: &crate::os_drag::TabPayload) -> crate::os_drag::DragAck {
        crate::os_drag::DragAck::Accepted
    }
}

pub(super) fn assert_pty_marker(
    pty: &sonicterm_io::pty::PtyHandle,
    command: Vec<u8>,
    marker: &[u8],
) {
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

pub(super) fn fail_without_destination(
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

#[cfg(windows)]
pub(super) fn run_stopped_spare_tear_out(event_loop: &winit::event_loop::ActiveEventLoop) {
    use sonicterm_cfg::config::{BackdropKind, ScrollbarMode, SoftwareRenderMode};
    use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
    use sonicterm_gpu::device_errors::GpuFaultKind;
    use winit::{dpi::PhysicalSize, window::Window};
    let mut app = app_with_tabs(&["last"]);
    let source = app.__test_main_window_id().expect("synthetic main");
    let window = Arc::new(
        event_loop
            .create_window(
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
        glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
    };
    let mut renderer =
        GpuRenderer::new(window.clone(), event_loop, &app.theme, settings).expect("spare renderer");
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

    assert!(app.tear_out_tab(event_loop, 0));

    assert_eq!(app.warm_window_pool.len(), 1, "the stopped spare stays pooled");
    assert_eq!(app.warm_window_pool[0].window.id(), spare);
    assert_eq!(source_snapshot(&app, source), before, "the source keeps its last tab");
    assert_eq!(app.windows.keys().copied().collect::<HashSet<_>>(), windows);
}

/// The body of the function enclosing byte `at` in `bare`: from its nearest preceding `fn` item to
/// the brace that closes it, counted on the comment- and literal-blanked view.
pub(super) fn enclosing_function(bare: &str, at: usize) -> &str {
    let start = bare[..at]
        .match_indices("fn ")
        .filter(|(offset, _)| {
            *offset == 0
                || !crate::app::source_scan_support::is_ident_byte(bare.as_bytes()[offset - 1])
        })
        .map(|(offset, _)| offset)
        .last()
        .expect("an enclosing function");
    let open = start + bare[start..].find('{').expect("a function body");
    let mut depth = 0_usize;
    for (offset, byte) in bare.bytes().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &bare[start..=offset];
                }
            }
            _ => {}
        }
    }
    &bare[start..]
}

/// Select one inherent method through its own closing brace, regardless of its source file order.
pub(super) fn source_method<'source>(source: &'source str, name: &str) -> &'source str {
    let signature = format!("fn {name}(");
    let start = source.find(&signature).unwrap_or_else(|| panic!("missing {signature}"));
    let body = &source[start..];
    let end = body.find("\n    }\n").expect("method closing brace") + "\n    }".len();
    &body[..end]
}
