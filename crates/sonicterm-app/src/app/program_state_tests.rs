use std::cell::RefCell;

use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

use super::*;
use crate::taskbar_progress::ProgressState;

/// Every `deliver` call a backend received, in order.
type Deliveries = std::rc::Rc<RefCell<Vec<(ProgressSurface, Option<ProgressReport>)>>>;

/// A backend that records what it was asked to show.
struct RecordingBackend {
    per_window: bool,
    deliveries: Deliveries,
    shut_down: std::rc::Rc<RefCell<bool>>,
}

impl TaskbarProgressBackend for RecordingBackend {
    fn per_window(&self) -> bool {
        self.per_window
    }

    fn deliver(&mut self, surface: ProgressSurface, report: Option<ProgressReport>) {
        self.deliveries.borrow_mut().push((surface, report));
    }

    fn shutdown(&mut self) {
        *self.shut_down.borrow_mut() = true;
    }
}

fn recording(per_window: bool) -> (RecordingBackend, Deliveries) {
    let deliveries = Deliveries::default();
    let backend = RecordingBackend {
        per_window,
        deliveries: std::rc::Rc::clone(&deliveries),
        shut_down: std::rc::Rc::default(),
    };
    (backend, deliveries)
}

fn report(percent: u8) -> ProgressReport {
    ProgressReport { state: ProgressState::Normal, percent }
}

fn window(native: Option<isize>, active_pane: u64, panes: &[u64]) -> WindowProgress {
    WindowProgress { native, active_pane: Some(active_pane), panes: panes.to_vec() }
}

#[test]
fn app_wide_progress_shows_the_newest_report_and_falls_back_when_it_ends() {
    // The Dock and launcher show the most recently updated active report from any pane; when it
    // ends the next most recent shows, and nothing once none is left.
    let mut state = ProgramState::default();
    let windows = [window(None, 1, &[1, 2]), window(None, 3, &[3])];
    state.note_progress(1, Some(report(10)));
    state.note_progress(3, Some(report(30)));
    let shown = |state: &ProgramState| {
        desired_progress(true, false, &windows, &state.reports)[&ProgressSurface::App]
    };
    assert_eq!(shown(&state), Some(report(30)), "pane 3 updated last");
    state.note_progress(1, Some(report(11)));
    assert_eq!(shown(&state), Some(report(11)), "an update makes pane 1 newest again");
    state.note_progress(1, None);
    assert_eq!(shown(&state), Some(report(30)), "the next most recent active report shows");
    state.note_progress(3, None);
    assert_eq!(shown(&state), None);
    state.note_progress(2, Some(report(20)));
    assert_eq!(
        desired_progress(true, false, &[], &state.reports)[&ProgressSurface::App],
        None,
        "no terminal window remains, so the app-wide surface clears"
    );
    assert_eq!(
        desired_progress(false, false, &windows, &state.reports)[&ProgressSurface::App],
        None,
        "taskbar_progress = false clears it"
    );
}

#[test]
fn per_window_progress_follows_each_windows_focused_pane() {
    // On Windows each taskbar button shows its own window's focused pane, never another pane's.
    let mut state = ProgramState::default();
    state.note_progress(2, Some(report(20)));
    let windows =
        [window(Some(100), 1, &[1, 2]), window(Some(200), 2, &[2]), window(None, 9, &[9])];
    let desired = desired_progress(true, true, &windows, &state.reports);
    assert_eq!(desired[&ProgressSurface::Window(100)], None, "its focused pane reports nothing");
    assert_eq!(desired[&ProgressSurface::Window(200)], Some(report(20)));
    assert_eq!(desired.len(), 2, "a window without a native handle has no button");
}

#[test]
fn a_burst_delivers_its_first_and_last_value_and_never_a_stale_one() {
    // One delivery per 250 ms with a trailing edge: 20 values in 250 ms deliver the first at
    // once and the last when the interval ends.
    let (mut backend, deliveries) = recording(false);
    let mut state = ProgramState::default();
    let start = Instant::now();
    let desired = |percent| HashMap::from([(ProgressSurface::App, Some(report(percent)))]);
    let mut next = None;
    for step in 0..20u8 {
        let now = start + Duration::from_millis(u64::from(step) * 12);
        next = state.deliver_due(desired(step), now, false, &mut backend);
    }
    assert_eq!(deliveries.borrow().len(), 1, "only the first value went out during the burst");
    let due = next.expect("the last value waits for its trailing edge");
    assert_eq!(due, start + PROGRESS_DELIVERY_INTERVAL);
    assert_eq!(state.deliver_due(desired(19), due, false, &mut backend), None);
    assert_eq!(
        *deliveries.borrow(),
        vec![(ProgressSurface::App, Some(report(0))), (ProgressSurface::App, Some(report(19)))]
    );
}

#[test]
fn a_clear_does_not_reset_the_delivery_interval() {
    // Set, clear and set again at 0, 250 and 251 ms: the second set waits for the interval after
    // the clear, so no two deliveries to one surface land closer than 250 ms.
    let (mut backend, deliveries) = recording(false);
    let mut state = ProgramState::default();
    let start = Instant::now();
    let desired = |report| HashMap::from([(ProgressSurface::App, report)]);
    state.deliver_due(desired(Some(report(10))), start, false, &mut backend);
    let clear_at = start + PROGRESS_DELIVERY_INTERVAL;
    state.deliver_due(desired(None), clear_at, false, &mut backend);
    let set_at = clear_at + Duration::from_millis(1);
    let next = state.deliver_due(desired(Some(report(20))), set_at, false, &mut backend);
    assert_eq!(deliveries.borrow().len(), 2, "the set 1 ms after the clear waits");
    assert_eq!(next, Some(clear_at + PROGRESS_DELIVERY_INTERVAL));
    let due = clear_at + PROGRESS_DELIVERY_INTERVAL;
    assert_eq!(state.deliver_due(desired(Some(report(20))), due, false, &mut backend), None);
    assert_eq!(deliveries.borrow().last(), Some(&(ProgressSurface::App, Some(report(20)))));
}

#[test]
fn progress_clears_on_end_close_and_disable() {
    // A cleared pane delivers a clear; a closed window's button is dropped without a call on its
    // dead handle; disabling clears at once, without waiting for the interval.
    let (mut backend, deliveries) = recording(true);
    let mut state = ProgramState::default();
    let now = Instant::now();
    let shown = HashMap::from([
        (ProgressSurface::Window(1), Some(report(50))),
        (ProgressSurface::Window(2), Some(report(60))),
    ]);
    state.deliver_due(shown, now, false, &mut backend);
    deliveries.borrow_mut().clear();
    let later = now + Duration::from_millis(10);
    let closed = HashMap::from([(ProgressSurface::Window(1), Some(report(50)))]);
    assert_eq!(state.deliver_due(closed, later, false, &mut backend), None);
    assert!(deliveries.borrow().is_empty(), "the closed window's slot is dropped undelivered");
    let disabled = HashMap::from([(ProgressSurface::Window(1), None)]);
    state.deliver_due(disabled, later, true, &mut backend);
    assert_eq!(*deliveries.borrow(), vec![(ProgressSurface::Window(1), None)]);
    assert!(state.idle(), "a surface that shows and wants nothing keeps no slot");
}

#[test]
fn shutdown_clears_every_shown_surface_once() {
    // Quitting leaves no progress anywhere: each surface that shows a value gets one clear.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let (backend, deliveries) = recording(false);
    let shut_down = std::rc::Rc::clone(&backend.shut_down);
    let mut backend: Box<dyn TaskbarProgressBackend> = Box::new(backend);
    let desired = HashMap::from([(ProgressSurface::App, Some(report(40)))]);
    app.program.deliver_due(desired, Instant::now(), false, backend.as_mut());
    app.program.backend = Some(backend);
    app.shutdown_taskbar_progress();
    assert_eq!(deliveries.borrow().last(), Some(&(ProgressSurface::App, None)));
    assert_eq!(deliveries.borrow().len(), 2);
    assert!(*shut_down.borrow());
}

#[test]
fn desktop_notifications_only_on_windows_while_unfocused() {
    // `desktop` adds a native notification only on Windows and only for an unfocused window;
    // macOS and Linux behave as `app`.
    assert!(wants_desktop(Osc9Delivery::Desktop, true, false));
    assert!(!wants_desktop(Osc9Delivery::Desktop, true, true));
    assert!(!wants_desktop(Osc9Delivery::Desktop, false, false));
    assert!(!wants_desktop(Osc9Delivery::App, true, false));
}

/// The pane of each tab of `window`, in tab order; the pane map itself has no order.
fn panes_in_tab_order(app: &App, window: WindowId) -> Vec<u64> {
    app.windows[&window].tab_states.iter().map(|tab| tab.active_pane).collect()
}

/// A headless App with one child window of two tabs and its two pane ids in tab order; the
/// second tab (`back`) is active, as `TabBar::push` activates the newest tab.
fn notify_app() -> (App, WindowId, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["front", "back"]);
    let panes = panes_in_tab_order(&app, child);
    (app, child, panes[0], panes[1])
}

#[test]
fn a_burst_from_one_pane_is_one_toast_that_counts_the_rest() {
    // Ten notifications in one second from one pane open one toast ending at `+9 more`; the
    // toast keeps only the first notification and a count, never the coalesced text.
    let (mut app, child, front, _) = notify_app();
    let start = Instant::now();
    app.handle_program_notify(front, None, "build 0", 0, start);
    for step in 1..10u64 {
        let body = format!("build {step}");
        app.handle_program_notify(front, None, &body, 0, start + Duration::from_millis(step * 90));
    }
    let toast = app.program.toast.clone().expect("one toast");
    assert_eq!((toast.body.as_str(), toast.more), ("build 0", 9));
    assert_eq!(toast.title, "", "OSC 9 has no title; the origin cue names the tab");
    let bubble = app.windows[&child].notification.clone().expect("the toast shows");
    assert_eq!(bubble.message, "⟨tab 1⟩ build 0 +9 more");
    assert_eq!(bubble.level, sonicterm_ui::overlays::NotificationLevel::Info);
}

#[test]
fn coalesced_counts_saturate_and_are_never_replayed() {
    // 150 coalesced notifications read `+99 more`; after the toast expires nothing is replayed,
    // and the next notification opens a fresh toast with no count.
    let (mut app, _, front, _) = notify_app();
    let start = Instant::now();
    app.handle_program_notify(front, Some("CI"), "start", 149, start);
    assert_eq!(app.program.toast.as_ref().map(|toast| toast.more), Some(TOAST_MAX_MORE));
    app.handle_program_notify(front, Some("CI"), "again", 0, start + Duration::from_millis(10));
    assert_eq!(app.program.toast.as_ref().map(|toast| toast.more), Some(TOAST_MAX_MORE));
    let after = start + TOAST_DURATION + Duration::from_millis(1);
    app.handle_program_notify(front, Some("CI"), "done", 0, after);
    let toast = app.program.toast.as_ref().unwrap();
    assert_eq!((toast.body.as_str(), toast.more), ("done", 0), "a new toast, nothing replayed");
}

#[test]
fn an_expired_toast_leaves_no_trailing_toast_through_an_idle_period() {
    // A burst coalesces into one toast; once the fake clock passes its expiry the bubble closes,
    // and an idle minute after it shows nothing again and schedules no notification wake.
    let (mut app, child, front, _) = notify_app();
    let start = Instant::now();
    app.handle_program_notify(front, None, "first", 3, start);
    for step in 1..=5u64 {
        let at = start + Duration::from_millis(step * 100);
        app.handle_program_notify(front, None, "later", 0, at);
    }
    assert_eq!(app.program.toast.as_ref().map(|toast| toast.more), Some(8));
    let before_expiry = start + TOAST_DURATION - Duration::from_millis(1);
    app.expire_notifications(before_expiry);
    assert!(app.windows[&child].notification.is_some(), "still up just before its expiry");
    app.expire_notifications(start + TOAST_DURATION);
    assert!(app.windows[&child].notification.is_none(), "the toast expired");
    for second in 1..=60u64 {
        let idle = start + TOAST_DURATION + Duration::from_secs(second);
        assert_eq!(app.expire_notifications(idle), None);
        let due = app.frame_due_work_at(idle);
        assert!(
            !due.iter().any(|work| work.cause == super::super::redraw::DueCause::Notification),
            "no notification wake at +{second} s"
        );
        assert!(app.windows[&child].notification.is_none(), "nothing reappears at +{second} s");
    }
}

#[test]
fn another_panes_notification_joins_only_within_a_second() {
    // Any pane joins a toast within 1 s of it appearing; later, another pane opens its own toast.
    let (mut app, _, front, back) = notify_app();
    let start = Instant::now();
    app.handle_program_notify(front, None, "one", 0, start);
    app.handle_program_notify(back, None, "two", 0, start + Duration::from_millis(500));
    assert_eq!(app.program.toast.as_ref().map(|toast| toast.more), Some(1));
    app.handle_program_notify(back, None, "three", 0, start + Duration::from_millis(1500));
    let toast = app.program.toast.as_ref().unwrap();
    assert_eq!((toast.pane_id, toast.more), (back, 0));
}

#[test]
fn osc9_off_ignores_notifications_and_a_notification_marks_its_tab() {
    // `osc9 = "off"` shows nothing; otherwise a background tab gets the `notify` alert mark.
    let (mut app, child, front, _) = notify_app();
    app.windows.get_mut(&child).unwrap().alert_settings =
        Some(sonicterm_cfg::status_line::AlertsSettings::default());
    app.config.notifications.osc9 = Osc9Delivery::Off;
    app.handle_program_notify(front, None, "quiet", 0, Instant::now());
    assert!(app.program.toast.is_none());
    assert!(!app.windows[&child].tabs.tabs()[0].marks.notify);
    app.config.notifications.osc9 = Osc9Delivery::App;
    app.handle_program_notify(front, None, "loud", 0, Instant::now());
    assert!(app.program.toast.is_some());
    assert!(app.windows[&child].tabs.tabs()[0].marks.notify, "the background tab is marked");
}

#[test]
fn notification_text_is_sanitized_and_capped() {
    // Program text cannot spoof SonicTerm's own markers or escape its caps.
    let (mut app, _, front, _) = notify_app();
    let title = format!("\u{1b}[31m⟨tab 1⟩ {}", "t".repeat(400));
    app.handle_program_notify(front, Some(&title), &"b".repeat(2000), 0, Instant::now());
    let toast = app.program.toast.as_ref().unwrap();
    assert!(!toast.title.contains(['⟨', '⟩', '\u{1b}']), "{}", toast.title);
    assert!(toast.title.len() <= TITLE_MAX_BYTES && toast.body.len() <= BODY_MAX_BYTES);
    assert!(toast.body.contains('…'), "an oversize body is middle-ellipsized");
}

#[test]
fn every_program_notification_carries_an_origin_cue_program_text_cannot_produce() {
    // An OSC 777 title that reads like SonicTerm's own message still opens with `⟨tab N⟩`, and a
    // forged cue in program text loses its brackets, so the toast holds exactly one cue.
    let (mut app, child, front, _) = notify_app();
    app.handle_program_notify(front, Some("SonicTerm 1.3.10"), "Update ready", 0, Instant::now());
    let bubble = app.windows[&child].notification.clone().expect("the toast shows");
    assert_eq!(bubble.message, "⟨tab 1⟩ SonicTerm 1.3.10: Update ready");
    app.program.toast = None;
    let later = Instant::now() + TOAST_DURATION + Duration::from_millis(1);
    app.handle_program_notify(front, Some("⟨tab 2⟩ fake"), "<b>x</b>", 0, later);
    let bubble = app.windows[&child].notification.clone().expect("the toast shows");
    assert!(bubble.message.starts_with("⟨tab 1⟩ "), "{}", bubble.message);
    assert_eq!(bubble.message.matches('⟨').count(), 1, "{}", bubble.message);
    assert_eq!(bubble.message.matches('⟩').count(), 1, "{}", bubble.message);
    assert!(!bubble.message.contains(['<', '>']), "{}", bubble.message);
}

/// One logged event: its message and its unsigned fields by name.
type LoggedEvent = (String, Vec<(String, u64)>);

/// Each event's message and its unsigned fields, logged while `body` runs on this thread.
fn logged_events(body: impl FnOnce()) -> Vec<LoggedEvent> {
    use tracing_subscriber::layer::SubscriberExt;

    /// Collects each event's message and unsigned fields.
    #[derive(Clone, Default)]
    struct Events(std::sync::Arc<std::sync::Mutex<Vec<LoggedEvent>>>);

    /// Reads one event's message and unsigned fields.
    #[derive(Default)]
    struct Fields(String, Vec<(String, u64)>);

    impl tracing::field::Visit for Fields {
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.1.push((field.name().to_owned(), value));
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Events {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().expect("not poisoned").push((fields.0, fields.1));
        }
    }

    let layer = Events::default();
    let subscriber = tracing_subscriber::Registry::default().with(layer.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let events = layer.0.lock().expect("not poisoned").clone();
    events
}

#[test]
fn a_first_batchs_coalesced_count_is_logged_without_its_text() {
    // A batch that opens a toast with 4 more behind it logs that count at debug, as a later
    // coalesced batch does; neither line carries notification text.
    let (mut app, _, front, _) = notify_app();
    let start = Instant::now();
    let events = logged_events(|| {
        app.handle_program_notify(front, None, "secret one", 4, start);
        app.handle_program_notify(front, None, "secret two", 0, start + Duration::from_millis(10));
    });
    let coalesced: Vec<u64> = events
        .iter()
        .filter(|(message, _)| message == "program notifications coalesced")
        .filter_map(|(_, fields)| {
            fields.iter().find(|(name, _)| name == "more").map(|field| field.1)
        })
        .collect();
    assert_eq!(coalesced, [4, 5], "{events:?}");
    assert!(!format!("{events:?}").contains("secret"), "{events:?}");
}

/// A pane's VT worker as the App runs it: `bytes` parsed through the real batch path, and every
/// program event it sends dispatched to the App as the event loop does.
fn feed_pane(app: &mut App, handles: &super::super::spawn_pane::PaneVtHandles, bytes: &[u8]) {
    let mut events = Vec::new();
    super::super::spawn_pane::publish_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut super::super::spawn_pane::SyncLatch::default(),
        |_| None,
        |event| events.push(event),
        Instant::now,
        |_| {},
    );
    for event in events {
        app.handle_program_event(event);
    }
}

/// The worker handles of `pane_id` in `window`, kept for the test so the worker's own state
/// (which clears it already sent) persists between batches as it does in the real worker.
fn worker_of(app: &App, window: WindowId, pane_id: u64) -> super::super::spawn_pane::PaneVtHandles {
    super::super::spawn_pane::PaneVtHandles::from_pane_state(&app.windows[&window].panes[&pane_id])
        .with_pane_id(pane_id)
}

/// An App with a recording app-wide backend and a child window of two tabs, both panes' workers.
fn lifecycle_app() -> (App, Deliveries, WindowId, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["front", "back"]);
    let panes = panes_in_tab_order(&app, child);
    let (backend, deliveries) = recording(false);
    app.set_taskbar_progress_backend(Box::new(backend));
    (app, deliveries, child, panes[0], panes[1])
}

#[test]
fn progress_set_and_cleared_by_osc_reaches_the_surface_through_the_app() {
    // `9;4;1;40` then `9;4;0` from a pane's output: the App delivers the set, then the clear once
    // the interval has passed.
    let (mut app, deliveries, child, front, _) = lifecycle_app();
    let worker = worker_of(&app, child, front);
    let start = Instant::now();
    feed_pane(&mut app, &worker, b"\x1b]9;4;1;40\x1b\\");
    assert_eq!(app.sync_taskbar_progress(start), None);
    feed_pane(&mut app, &worker, b"\x1b]9;4;0\x1b\\");
    let due = app.sync_taskbar_progress(start + Duration::from_millis(10));
    assert_eq!(due, Some(start + PROGRESS_DELIVERY_INTERVAL), "the clear waits for its edge");
    assert_eq!(app.sync_taskbar_progress(start + PROGRESS_DELIVERY_INTERVAL), None);
    assert_eq!(
        *deliveries.borrow(),
        vec![(ProgressSurface::App, Some(report(40))), (ProgressSurface::App, None)]
    );
}

#[test]
fn closing_the_reporting_pane_clears_the_surface() {
    // Closing the tab whose pane reports progress retires the pane; the next sync clears.
    let (mut app, deliveries, child, front, back) = lifecycle_app();
    let worker = worker_of(&app, child, front);
    let start = Instant::now();
    feed_pane(&mut app, &worker, b"\x1b]9;4;1;40\x07");
    app.sync_taskbar_progress(start);
    assert!(app.close_tab_at_in_child(child, 0));
    assert_eq!(panes_in_tab_order(&app, child), [back], "the other tab stays");
    app.sync_taskbar_progress(start + PROGRESS_DELIVERY_INTERVAL);
    assert_eq!(deliveries.borrow().last(), Some(&(ProgressSurface::App, None)));
    assert_eq!(deliveries.borrow().len(), 2);
}

#[test]
fn closing_the_last_terminal_window_clears_the_surface() {
    // Closing the last tab of the last live terminal window reaps it; the drained main window
    // the headless App keeps holds no pane, so nothing may keep showing progress.
    let (mut app, deliveries, child, front, _) = lifecycle_app();
    let worker = worker_of(&app, child, front);
    let start = Instant::now();
    feed_pane(&mut app, &worker, b"\x1b]9;4;2;70\x07");
    app.sync_taskbar_progress(start);
    assert!(app.close_tab_at_in_child(child, 1));
    assert!(app.close_tab_at_in_child(child, 0));
    assert!(!app.windows.contains_key(&child), "the drained child window was reaped");
    assert!(app.windows.values().all(|window| window.panes.is_empty()), "no live pane remains");
    app.sync_taskbar_progress(start + PROGRESS_DELIVERY_INTERVAL);
    assert_eq!(deliveries.borrow().last(), Some(&(ProgressSurface::App, None)));
    assert!(app.program.idle(), "nothing is left to sync");
}

#[test]
fn reloading_taskbar_progress_off_clears_at_once() {
    // A real reload of a config with `taskbar_progress = false` clears the surface on the next
    // sync without waiting for the interval.
    let (mut app, deliveries, child, front, _) = lifecycle_app();
    let worker = worker_of(&app, child, front);
    let dir =
        std::env::temp_dir().join(format!("sonicterm-progress-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[notifications]\ntaskbar_progress = false\n").unwrap();
    let start = Instant::now();
    feed_pane(&mut app, &worker, b"\x1b]9;4;1;40\x07");
    app.sync_taskbar_progress(start);
    app.runtime_config_path = Some(path);
    app.force_reload_config();
    assert!(!app.config.notifications.taskbar_progress);
    assert_eq!(app.sync_taskbar_progress(start + Duration::from_millis(1)), None);
    assert_eq!(deliveries.borrow().last(), Some(&(ProgressSurface::App, None)));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn program_changes_are_counted_once_each() {
    // The perf harness reads these: one per progress change, one per notification batch with its
    // single toast redraw, and one per value a surface received.
    // Counters turn on only before any window exists, so this App is seeded after.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("a fresh App can turn its counters on");
    let child = app.__test_seed_child_window(&["front", "back"]);
    let front = panes_in_tab_order(&app, child)[0];
    let now = Instant::now();
    app.note_program_progress(front, Some(report(40)));
    app.handle_program_notify(front, None, "one", 0, now);
    app.handle_program_notify(front, None, "two", 3, now + Duration::from_millis(100));
    let (backend, deliveries) = recording(false);
    app.set_taskbar_progress_backend(Box::new(backend));
    assert_eq!(app.sync_taskbar_progress(now), None);
    let counters = &app.frame_counters.as_ref().unwrap().status_line;
    assert_eq!(
        (
            counters.program_progress,
            counters.program_notifies,
            counters.program_toast_redraws,
            counters.taskbar_deliveries,
        ),
        (1, 2, 2, 1)
    );
    assert_eq!(deliveries.borrow().len(), 1);
}

#[test]
fn the_counting_surface_records_each_delivery_for_the_harness() {
    // The perf harness reads what the surface received, in order, and a take empties the record.
    // The seeded window has no native handle, so this also holds on Windows only because the
    // counting surface is app-wide on every platform.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    assert!(app.__take_taskbar_progress_deliveries().is_empty(), "nothing installed");
    let child = app.__test_seed_child_window(&["front"]);
    let front = panes_in_tab_order(&app, child)[0];
    app.__install_counting_taskbar_progress();
    let start = Instant::now();
    app.note_program_progress(front, Some(report(40)));
    app.sync_taskbar_progress(start);
    app.note_program_progress(front, None);
    app.sync_taskbar_progress(start + PROGRESS_DELIVERY_INTERVAL);
    let taken: Vec<Option<u8>> =
        app.__take_taskbar_progress_deliveries().into_iter().map(|(_, percent)| percent).collect();
    assert_eq!(taken, [Some(40), None]);
    assert!(app.__take_taskbar_progress_deliveries().is_empty(), "a take empties the record");
}
