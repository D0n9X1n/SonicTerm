//! Warm-spare eligibility and live renderer setting contracts.

use super::super::tear_out_tests::*;
use super::*;

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
    const SOURCE: &str = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
    let prepare = source_method(SOURCE, "prepare_tear_out_destination");
    let refusal = prepare.find("warm_destination_refusal(").expect("stopped-spare refusal");
    // The call alone is matched: rustfmt can put the `self` receiver on the line before it.
    let take = prepare.find(".take_warm_window()").expect("warm take");
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
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            run_stopped_spare_tear_out(event_loop);
            self.ran = true;
            event_loop.exit();
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

/// Native registration may synchronously stop the destination device. Before
/// owner transfer, commit must revoke that registration and restore the source.
#[test]
fn tear_out_rechecks_device_after_native_registration() {
    let source = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    );
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
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let mut app = app_with_tabs(&["last"]);
            app.event_loop_proxy = Some(self.proxy.clone());
            app.config.appearance.software_render_mode =
                sonicterm_cfg::config::SoftwareRenderMode::Force;
            let source = app.__test_main_window_id().unwrap();
            let pane_id = app.main_active_pane_id().unwrap();
            prepare_non_vacuous_source(&mut app, source, pane_id);
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_visible(false)
                            .with_inner_size(winit::dpi::PhysicalSize::new(640, 360)),
                    )
                    .expect("hidden destination"),
            );
            let renderer = GpuRenderer::new(
                window.clone(),
                event_loop,
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
            assert!(app.tear_out_tab(event_loop, 0));
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
            event_loop.exit();
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

#[test]
fn every_renderer_the_app_builds_gets_the_font_fallback_waker() {
    // A fallback face published for a window's fonts must wake that window. In every non-test app
    // source file, the function that constructs a renderer installs the waker, directly or through
    // `configure_child_renderer`; that function installs it; and warm adoption reconfigures the
    // adopted renderer. Comments and literals are blanked first, so a commented-out call fails.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut pending = vec![root];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                pending.push(path);
            } else if name.ends_with(".rs")
                && !name.ends_with("_tests.rs")
                && !name.starts_with("test_hooks")
            {
                files.push(path);
            }
        }
    }
    let mut sites = 0;
    let mut configure_installs = false;
    let mut adoption_reconfigures = false;
    for path in &files {
        let (_, bare) =
            crate::app::source_scan_support::code_views(&std::fs::read_to_string(path).unwrap());
        for (at, _) in bare.match_indices("GpuRenderer::new") {
            let body = enclosing_function(&bare, at);
            assert!(
                body.contains("configure_child_renderer(")
                    || body.contains("set_font_fallback_waker("),
                "{}: the function constructing a renderer installs no fallback waker:\n{body}",
                path.display()
            );
            sites += 1;
        }
        if let Some(at) = bare.find("fn configure_child_renderer(") {
            configure_installs =
                enclosing_function(&bare, at + "fn ".len()).contains("set_font_fallback_waker(");
        }
        if let Some(at) = bare.find(".take_warm_window()") {
            let body = enclosing_function(&bare, at);
            adoption_reconfigures = body.contains("configure_child_renderer(")
                && body.contains("ChildRendererOrigin::WarmPool");
        }
    }
    assert!(sites >= 4, "found only {sites} construction sites");
    assert!(configure_installs, "configure_child_renderer installs the waker");
    assert!(
        adoption_reconfigures,
        "warm adoption reconfigures the adopted renderer for its window"
    );
}

/// A warm-pool renderer may never draw, so its glyph atlas starts at the 256 floor; a tear-out
/// child renderer takes the normal start. Both otherwise share the tear-out settings.
#[test]
fn warm_renderers_start_their_glyph_atlas_at_the_floor() {
    let app = App::new(Theme::default(), Config::default(), Keymap::default());
    let warm = app.warm_renderer_settings();
    let child = app.tear_out_renderer_settings("child");
    assert_eq!(warm.glyph_atlas_start, sonicterm_gpu::core::GlyphAtlasStart::Minimum);
    assert_eq!(child.glyph_atlas_start, sonicterm_gpu::core::GlyphAtlasStart::Normal);
    assert_eq!(warm.role, "warm");
    assert_eq!(
        sonicterm_gpu::core::start_dim(1.0, warm.glyph_atlas_start),
        sonicterm_text::glyph_atlas::MIN_ATLAS_DIM
    );
    // The warm window's construction uses these settings, not the child's.
    let source = concat!(
        include_str!("tear_out.rs"),
        "\n",
        include_str!("tear_out_destination.rs"),
        "\n",
        include_str!("tear_out_warm.rs")
    )
    .replace("\r\n", "\n");
    let create = source.split_once("fn create_warm_window(").unwrap().1;
    let create = create.split_once("\n    }\n").unwrap().0;
    assert!(create.contains("let settings = self.warm_renderer_settings();"));
}
