//! Main-thread, full-App probe: AppKit in-process synthetic sendEvent/key-equivalent/menu action.
//! Not a physical OS gesture or accessibility test; payload reads use the memory clipboard,
//! not NSPasteboard. Run only in a separately authorized desktop/build window.
//! Args: one NEW absolute scratch directory. HOME is preserved; the only config loaded is
//! scratch/config/sonicterm.toml, with a normal bundled theme/keymap. Reload/save/config-menu
//! actions are outside this probe (App's private runtime_config_path is not changed).

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("this native probe requires macOS");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    use std::{sync::mpsc, time::Duration};
    // An independent hard bound covers a wedged AppKit callback. It affects only this
    // dedicated probe process; every PTY fixture shares an absolute self-expiry deadline before 60 s.
    let fixture_expiry = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(now) => now.as_secs() + 50,
        Err(error) => {
            eprintln!("invalid clock: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (cancel, receiver) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if matches!(
            receiver.recv_timeout(Duration::from_secs(60)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "FAIL native rename probe hard deadline (60s); PTY fixtures self-expire"
            );
            std::process::abort();
        }
    });
    let result = objc2::rc::autoreleasepool(|_| native::run(fixture_expiry));
    let _ = cancel.send(());
    let _ = watchdog.join();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("FAIL native rename probe: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use anyhow::{bail, ensure, Context, Result};
    use objc2::{msg_send, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::{NSPoint, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use sonicterm_app::{
        app::{App, UserEvent, WindowState},
        menu::{PlatformMenu, Sender},
    };
    use sonicterm_cfg::{
        config::Config,
        keymap::{Action, Keymap},
        theme::Theme,
    };
    use sonicterm_io::pty::PtyChildExitProbe;
    use std::{
        collections::HashSet,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };
    use winit::{
        application::ApplicationHandler,
        event::{DeviceEvent, DeviceId, StartCause, WindowEvent},
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
        keyboard::{Key, NamedKey},
        window::{Window, WindowId},
    };

    const MARKER: &str = "I1470_NATIVE_READY";
    const PASTE: &str = "貼付";
    const SEED: &str = "draft-";

    /// All callbacks reach the real App; the wrapper only observes and sequences assertions.
    struct Probe {
        app: App,
        mtm: MainThreadMarker,
        deadline: Instant,
        windows: Vec<Arc<Window>>,
        probes: Vec<PtyChildExitProbe>,
        real_focus: Option<WindowId>,
        menu_drains: usize,
        v_keydowns: usize,
        stage: u8,
        case: usize,
        presented_before: u64,
        drained_before: usize,
        peer_title: String,
        title_expected: String,
        stable_since: Option<Instant>,
        failure: Option<String>,
        finished: bool,
    }

    /// Read the real NSWindow number corresponding to winit's own view, without foreign-process APIs.
    fn native_number(window: &Window) -> Result<isize> {
        let RawWindowHandle::AppKit(handle) = window.window_handle()?.as_raw() else {
            bail!("non-AppKit window handle");
        };
        // SAFETY: main-thread winit handle owns a live NSView; window is used synchronously and null-checked.
        unsafe {
            let view: *mut objc2::runtime::AnyObject = handle.ns_view.as_ptr().cast();
            let native: *mut objc2::runtime::AnyObject = msg_send![view, window];
            ensure!(!native.is_null(), "view has no NSWindow");
            Ok(msg_send![native, windowNumber])
        }
    }

    /// Invoke the installed production menu item, not dispatch_tag or the bridge test drain.
    fn menu_action(mtm: MainThreadMarker, menu_name: &str, item_name: &str) -> Result<()> {
        let main =
            NSApplication::sharedApplication(mtm).mainMenu().context("no production NSMenu")?;
        let submenu = main
            .itemWithTitle(&NSString::from_str(menu_name))
            .and_then(|item| item.submenu())
            .context("missing submenu")?;
        let index = submenu.indexOfItemWithTitle(&NSString::from_str(item_name));
        ensure!(index >= 0, "missing {menu_name} > {item_name}");
        submenu.performActionForItemAtIndex(index);
        Ok(())
    }

    impl Probe {
        fn active_state(&self, id: WindowId) -> Result<&WindowState> {
            ensure!(
                self.app.__test_frontmost_window() == Some(id),
                "App focus changed during case"
            );
            self.app.frontmost().context("no frontmost state")
        }

        fn frame_count(&self, id: WindowId) -> Result<u64> {
            self.active_state(id)?
                .renderer
                .as_ref()
                .map(|r| r.successful_frame_count())
                .context("no real renderer")
        }

        /// Observe real output in the live grid and a successful native present, never just a render request.
        fn ready(state: &WindowState) -> bool {
            state.renderer.as_ref().is_some_and(|r| r.successful_frame_count() > 0)
                && state.panes.values().any(|pane| {
                    pane.pty.is_some()
                        && pane.parser.try_lock().is_some_and(|parser| {
                            parser.grid().rows_iter().any(|row| {
                                row.iter().map(|cell| cell.ch).collect::<String>().contains(MARKER)
                            })
                        })
                })
        }

        fn remember_children(&mut self, id: WindowId) -> Result<()> {
            let state = self.active_state(id)?;
            let mut probes = Vec::new();
            for (pane, entry) in &state.panes {
                let pty = entry.pty.as_ref().context("pane lacks real PTY")?;
                println!("fixture window={id:?} pane={pane} pid={:?}", pty.pid());
                probes.push(pty.child_exit_probe());
            }
            ensure!(!probes.is_empty(), "no native PTY children");
            self.probes.extend(probes);
            Ok(())
        }

        fn assert_native_focus(&self, window: &Window) -> Result<bool> {
            let key = NSApplication::sharedApplication(self.mtm).keyWindow();
            Ok(self.real_focus == Some(window.id())
                && self.app.__test_frontmost_window() == Some(window.id())
                && key.is_some_and(|native| {
                    native.windowNumber() == native_number(window).unwrap_or(-1)
                }))
        }

        fn assert_underlay_and_writes(&self, id: WindowId) -> Result<()> {
            ensure!(self.probes.len() == 2, "expected both native PTY liveness probes");
            for (index, probe) in self.probes.iter().enumerate() {
                ensure!(
                    !probe.has_exited()?,
                    "PTY fixture {index} exited before paste/commit case {} completed",
                    self.case
                );
            }
            let combined = (self.case / 3) % 2 == 1;
            let state = self.active_state(id)?;
            ensure!(
                state.copy_mode.as_ref().is_some_and(|m| m.is_read_only()) == combined,
                "READONLY changed"
            );
            let search = self.app.__test_search_query_cursor(Some(id));
            ensure!(
                search.map(|(q, _)| q) == if combined { Some("underlay") } else { None },
                "underlying search consumed paste"
            );
            ensure!(
                self.app.__test_pty_write_log().is_empty(),
                "attempted PTY write from rename paste/commit"
            );
            ensure!(
                self.windows[1 - self.case / 6].title() == self.peer_title,
                "peer title changed"
            );
            Ok(())
        }

        fn tick(&mut self, el: &ActiveEventLoop) -> Result<()> {
            ensure!(
                Instant::now() < self.deadline,
                "45s total deadline, stage={} case={}",
                self.stage,
                self.case
            );
            if self.stage == 0 {
                let Some(state) = self.app.main() else {
                    return Ok(());
                };
                if !Self::ready(state) {
                    return Ok(());
                }
                let window = self.app.main_window().context("missing main window")?.clone();
                if !self.assert_native_focus(&window)? {
                    window.focus_window();
                    return Ok(());
                }
                self.remember_children(window.id())?;
                self.windows.push(window);
                // The real NSMenu -> bridge -> UserEvent route creates the live child too.
                menu_action(self.mtm, "Shell", "New Window")?;
                self.stage = 1;
            } else if self.stage == 1 {
                let Some(state) = self.app.frontmost() else {
                    return Ok(());
                };
                let Some(window) = state.window.as_ref() else {
                    return Ok(());
                };
                if window.id() == self.windows[0].id() || !Self::ready(state) {
                    return Ok(());
                }
                let child = window.clone();
                if !self.assert_native_focus(&child)? {
                    return Ok(());
                }
                self.remember_children(child.id())?;
                self.windows.push(child);
                ensure!(self.app.child_window_count() == 1, "expected one real child window");
                self.stage = 2;
            } else if self.stage == 2 {
                if self.case == 12 {
                    self.finished = true;
                    el.exit();
                    return Ok(());
                }
                self.windows[self.case / 6].focus_window();
                self.stage = 3;
            } else if self.stage == 3 {
                let target = self.windows[self.case / 6].clone();
                if !self.assert_native_focus(&target)? {
                    return Ok(());
                }
                let id = target.id();
                // Controlled underlay setup uses existing public actions/state, never a focus setter.
                let combined = (self.case / 3) % 2 == 1;
                let state = self.app.frontmost_mut().context("missing target")?;
                state.copy_mode = None;
                for tab in &mut state.tab_states {
                    tab.search = None;
                }
                if combined {
                    ensure!(
                        self.app.run_action_for_window(&Action::OpenSearch, id),
                        "open search rejected"
                    );
                    let set = if self.case / 6 == 0 {
                        self.app.__test_set_main_search_query("underlay")
                    } else {
                        self.app.__test_set_child_search_query(id, "underlay")
                    };
                    ensure!(set, "search setup failed");
                    ensure!(
                        self.app.run_action_for_window(&Action::EnterCopyMode, id),
                        "READONLY setup failed"
                    );
                }
                ensure!(
                    self.app.run_action_for_window(&Action::RenameWindow, id),
                    "rename setup rejected"
                );
                ensure!(self.app.__test_palette_open(), "rename editor did not open");
                // Main stores no attachment; only a child records its native window id.
                let expected_attachment = (self.case / 6 != 0).then_some(id);
                ensure!(
                    self.app.__test_palette_attached_window() == expected_attachment,
                    "wrong editor owner"
                );
                let seed = format!("{SEED}{}-", self.case);
                self.app.__test_set_palette_query(&seed);
                self.app.__test_set_memory_clipboard(PASTE);
                self.app.__test_enable_pty_write_log();
                self.peer_title = self.windows[1 - self.case / 6].title();
                self.assert_underlay_and_writes(id)?;
                self.presented_before = self.frame_count(id)?;
                self.drained_before = self.menu_drains;
                self.v_keydowns = 0;
                let key = self.app.window_key(id).context("no stable window key")?;
                self.title_expected = format!("#{} {seed}{PASTE}", key.raw());
                self.stage = 4;
                if self.case % 3 != 2 {
                    let menu =
                        NSApplication::sharedApplication(self.mtm).mainMenu().context("no menu")?;
                    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                        NSEventType::KeyDown, NSPoint::new(0.0, 0.0), NSEventModifierFlags::Command,
                        0.0, native_number(&target)?, None, &NSString::from_str("v"), &NSString::from_str("v"), false, 9,
                    ).context("construct synthetic Cmd+V NSEvent")?;
                    if self.case.is_multiple_of(3) {
                        // NSApp decides whether the key equivalent is intercepted before winit.
                        // This is still synthetic, in-process AppKit dispatch, never a physical OS key.
                        NSApplication::sharedApplication(self.mtm).sendEvent(&event);
                    } else {
                        // Direct menu-response control; unlike sendEvent this does not establish interception order.
                        ensure!(
                            menu.performKeyEquivalent(&event),
                            "production NSMenu did not consume Cmd+V"
                        );
                    }
                } else {
                    menu_action(self.mtm, "Edit", "Paste")?;
                }
                self.stage = 4;
            } else if self.stage == 4 {
                if self.menu_drains == self.drained_before {
                    return Ok(());
                }
                let id = self.windows[self.case / 6].id();
                ensure!(
                    self.menu_drains == self.drained_before + 1,
                    "unexpected duplicate menu wake"
                );
                if self.case.is_multiple_of(3) {
                    ensure!(
                        self.v_keydowns == 0,
                        "NSApp also delivered V to winit; interception premise not established"
                    );
                }
                let wanted = format!("{SEED}{}-{PASTE}", self.case);
                ensure!(
                    self.app.__test_palette_query() == wanted,
                    "native paste did not reach Rename Window"
                );
                ensure!(self.app.__test_palette_cursor() == wanted.len(), "rename caret mismatch");
                self.assert_underlay_and_writes(id)?;
                if self.frame_count(id)? <= self.presented_before {
                    return Ok(());
                }
                println!(
                    "paste case={} window={id:?} native_present={} route={} readonly_search={}",
                    self.case,
                    self.frame_count(id)?,
                    match self.case % 3 {
                        0 => "AppKit-NSApp-sendEvent",
                        1 => "AppKit-direct-key-equivalent-control",
                        _ => "AppKit-menu-action",
                    },
                    (self.case / 3) % 2 == 1
                );
                self.presented_before = self.frame_count(id)?;
                // Enter uses the existing public logical-key seam; this does NOT claim a native or physical Enter event.
                ensure!(
                    self.app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)),
                    "Enter not consumed"
                );
                ensure!(!self.app.__test_palette_open(), "rename editor did not commit");
                self.stable_since = None;
                self.stage = 5;
            } else if self.stage == 5 {
                let target = &self.windows[self.case / 6];
                self.assert_underlay_and_writes(target.id())?;
                if self.case.is_multiple_of(3) {
                    ensure!(
                        self.v_keydowns == 0,
                        "V reached winit after menu dispatch; interception not established"
                    );
                }
                let title = target.title();
                ensure!(title == self.title_expected, "native NSWindow title mismatch after Enter");
                if self.frame_count(target.id())? <= self.presented_before {
                    return Ok(());
                }
                let since = self.stable_since.get_or_insert_with(Instant::now);
                if since.elapsed() < Duration::from_millis(120) {
                    return Ok(());
                }
                println!("commit case={} stable_native_title={title:?}", self.case);
                self.case += 1;
                self.stage = 2;
            }
            Ok(())
        }
    }

    impl ApplicationHandler<UserEvent> for Probe {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            self.app.resumed(el);
        }
        fn new_events(&mut self, el: &ActiveEventLoop, cause: StartCause) {
            self.app.new_events(el, cause);
        }
        fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
            let menu = matches!(&event, UserEvent::MenuAction);
            self.app.user_event(el, event);
            if menu {
                self.menu_drains += 1;
                // Capture the frame counter AFTER the paste was applied but BEFORE its redraw.
                // A frame presented before the bridge drain must not satisfy the after-paste proof.
                if self.stage == 4 {
                    let id = self.windows[self.case / 6].id();
                    match self.frame_count(id) {
                        Ok(count) => self.presented_before = count,
                        Err(error) => {
                            self.failure = Some(format!("{error:#}"));
                            el.exit();
                        }
                    }
                }
            }
        }
        fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            let focus = match &event {
                WindowEvent::Focused(value) => Some(*value),
                _ => None,
            };
            if matches!(&event, WindowEvent::KeyboardInput { event, .. }
                if event.state == winit::event::ElementState::Pressed
                    && event.physical_key == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyV))
            {
                self.v_keydowns += 1;
            }
            self.app.window_event(el, id, event);
            if focus == Some(true) {
                self.real_focus = Some(id);
            }
            if focus == Some(false) && self.real_focus == Some(id) {
                self.real_focus = None;
            }
        }
        fn device_event(&mut self, el: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
            self.app.device_event(el, id, event);
        }
        fn suspended(&mut self, el: &ActiveEventLoop) {
            self.app.suspended(el);
        }
        fn memory_warning(&mut self, el: &ActiveEventLoop) {
            self.app.memory_warning(el);
        }
        fn exiting(&mut self, el: &ActiveEventLoop) {
            self.app.exiting(el);
        }
        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            self.app.about_to_wait(el);
            if self.failure.is_none() {
                if let Err(error) = self.tick(el) {
                    self.failure = Some(format!("{error:#}"));
                    el.exit();
                }
            }
            // Bounded probe progression only; App still owns every ordinary redraw and presentation.
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(15)));
        }
    }

    /// Configure only this probe, then run the real App and native menubar on the main thread.
    pub fn run(fixture_expiry: u64) -> Result<()> {
        let mtm = MainThreadMarker::new()
            .context("probe must start on process main thread (harness=false)")?;
        let home = std::env::var_os("HOME");
        // Normal color profile. No HOME, TCC, Accessibility, or user's clipboard modifications.
        std::env::remove_var("NO_COLOR");
        std::env::remove_var("BASH_ENV");
        std::env::remove_var("ENV");
        let root = PathBuf::from(
            std::env::args_os().nth(1).context("pass a NEW absolute scratch directory")?,
        );
        ensure!(root.is_absolute() && !root.exists(), "scratch root must be new and absolute");
        std::fs::create_dir_all(root.join("config"))?;
        std::fs::create_dir_all(root.join("logs"))?;
        let shell = root.join("pty-fixture");
        // Every real PTY fixture shares one absolute expiry, including a child created late.
        // A SIGKILL of the parent cannot strand an indefinite shell. read is a bash builtin;
        // the one startup date child exits immediately. No user startup files are sourced.
        let expires = fixture_expiry;
        std::fs::write(
            &shell,
            format!(
                r#"#!/bin/bash
left=$(({expires} - $(/bin/date +%s)))
[ "$left" -gt 0 ] || exit 0
printf 'I1470_NATIVE_READY\r\n'
IFS= read -r -t "$left" ignored
"#
            ),
        )?;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700))?;
        let config_text = format!("theme = \"wezterm\"\nkeymap = \"sonicterm-macos\"\nlocale = \"en\"\n[terminal]\nshell = {:?}\n", shell.to_str().context("UTF-8 scratch path")?);
        let config_path = root.join("config/sonicterm.toml");
        std::fs::write(&config_path, config_text)?;
        let config = Config::load_strict(&config_path)?;
        let _logging = sonicterm_logging::init_in(&config.logging, &root.join("logs"))?;
        sonicterm_logging::install_panic_hook(root.join("logs"));
        let assets = sonicterm_cfg::assets::asset_dir();
        let theme = Theme::load_strict(&assets.join("themes/wezterm.toml"))?;
        let keymap = Keymap::load_strict(&assets.join("keymaps/sonicterm-macos.toml"))?;
        let renderer_baseline = sonicterm_gpu::core::live_renderer_count();
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        let proxy = event_loop.create_proxy();
        sonicterm_app::menubar_bridge::install_proxy(proxy.clone());
        sonicterm_app::os_drag_bridge::install_proxy(proxy.clone());
        sonicterm_app::open_script_bridge::install_proxy(proxy.clone());
        let mut app = App::new_with_proxy(theme, config, keymap, Some(proxy));
        app.__test_set_memory_clipboard("");
        // The same production resumed-hook timing as normal main; no runtime-smoke state or auto-exit.
        app.set_on_resumed(|| {
            sonicterm_mac::menubar::MacMenu::new()
                .install(Sender::new())
                .expect("install production NSMenu");
        });
        app.set_on_window_ready(|raw| {
            if let RawWindowHandle::AppKit(handle) = raw {
                // SAFETY: main-thread winit NSView is live here; its NSWindow is null-checked before use.
                unsafe {
                    let view: *mut objc2::runtime::AnyObject = handle.ns_view.as_ptr().cast();
                    let native: *mut objc2::runtime::AnyObject = msg_send![view, window];
                    if !native.is_null() {
                        let _: () = msg_send![native, setTabbingMode: 2isize];
                    }
                }
            }
        });
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        let mut probe = Probe {
            app,
            mtm,
            deadline: Instant::now() + Duration::from_secs(45),
            windows: Vec::new(),
            probes: Vec::new(),
            real_focus: None,
            menu_drains: 0,
            v_keydowns: 0,
            stage: 0,
            case: 0,
            presented_before: 0,
            drained_before: 0,
            peer_title: String::new(),
            title_expected: String::new(),
            stable_since: None,
            failure: None,
            finished: false,
        };
        let result = event_loop.run_app(&mut probe);
        let finished = probe.finished;
        let failure = probe.failure.take();
        let exits = std::mem::take(&mut probe.probes);
        let windows = probe.windows.iter().map(|w| w.id()).collect::<HashSet<_>>();
        drop(probe); // Production App/WindowState/PtyHandle destructors own and terminate only our PTYs.
        let cleanup_deadline = Instant::now() + Duration::from_secs(3);
        for exit in exits {
            while !exit.has_exited()? && Instant::now() < cleanup_deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            ensure!(exit.has_exited()?, "owned PTY child did not exit during bounded teardown");
        }
        ensure!(
            sonicterm_gpu::core::live_renderer_count() == renderer_baseline,
            "renderer count not restored"
        );
        result?;
        ensure!(home == std::env::var_os("HOME"), "HOME changed");
        if let Some(failure) = failure {
            bail!("{failure}");
        }
        ensure!(finished && windows.len() == 2, "event loop ended before all twelve native cases");
        println!("PASS AppKit in-process synthetic sendEvent/key-equivalent/menu action: 12 cases, main+child, live PTYs, native presents and titles; memory clipboard, NOT NSPasteboard or physical keys");
        Ok(())
    }
}
