//! Outer dispatch forwarding and event-loop control-flow merge.

use super::*;

/// The probe callback bodies, each run inside its outer-callback record.
impl Probe {
    /// An `ApplicationHandler` callback of `kind` starts: it becomes the running outer callback, and an
    /// open sample's armed timeline records its entry. An unarmed timeline reads no clock.
    pub(super) fn outer_enter(&mut self, kind: OuterKind) {
        self.running_outer = Some(kind);
        if let Some(sample) = self.open_sample.as_mut() {
            // When: a sample is open, its timeline records the callback's entry if it is armed.
            sample.timeline.enter_outer(kind);
        }
    }
}

impl Probe {
    /// The running callback returns: an open sample's armed timeline records its return.
    pub(super) fn outer_exit(&mut self) {
        if let Some(sample) = self.open_sample.as_mut() {
            // When: a sample is open, its timeline records the callback's return if it is armed.
            sample.timeline.exit_outer();
        }
        self.running_outer = None;
    }
}

impl Probe {
    /// The body of the `resumed` callback.
    pub(super) fn resumed_turn(&mut self, event_loop: &ActiveEventLoop) {
        if self.stage != Stage::Startup || self.main_id.is_some() {
            // When: the main window already exists, a later resume must not build a second one.
            return;
        }
        self.forward(
            event_loop,
            Dispatch::Native,
            Invocation::of(InvocationKind::Resumed, None, false),
            |app, active| app.resumed(active),
        );
        let Some(window) = self.app.main_window().cloned() else {
            self.invalidate(event_loop, "the App opened no main window".to_owned());
            return;
        };
        // The floating level keeps this inactive app's window above other apps' normal windows
        // without activating it, so it renders while the user's application keeps focus.
        window.set_window_level(WindowLevel::AlwaysOnTop);
        self.main_id = Some(window.id());
        // The first-present bound counts from here, so App construction is outside it.
        self.first_present_bound.window_opened(Instant::now());
    }
}

impl Probe {
    /// The body of the `window_event` callback.
    pub(super) fn window_event_turn(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.stage == Stage::Done
            || self.cover.as_ref().is_some_and(|cover| cover.id() == window_id)
        {
            // When: the run is over, or the event is for the harness's own cover, the App must not see it.
            return;
        }
        let is_main = self.main_id == Some(window_id);
        let arrival = match &event {
            WindowEvent::RedrawRequested => Arrival::Redraw,
            WindowEvent::Focused(_) if is_main => Arrival::Focus,
            WindowEvent::Occluded(occluded) if is_main => Arrival::Occlusion(*occluded),
            // Only native events reach this handler; the probe's synthetic moves go to the App directly.
            WindowEvent::CursorMoved { position, .. }
                if is_main
                    && self.native_pointer.arrive(
                        scenarios::BUILD_HOST,
                        (position.x, position.y),
                        self.go_at.is_some(),
                    ) =>
            {
                Arrival::PointerRest
            }
            other => Arrival::Routed(classify(other, is_main)),
        };
        match arrival {
            Arrival::Redraw => {
                // An output event the worker suppressed still shows in the grid by the redraw that draws it.
                self.note_first_output();
                let kind = if is_main { Dispatch::Redraw } else { Dispatch::Native };
                let redraw = InvocationKind::WindowEvent { redraw: true };
                self.forward(
                    event_loop,
                    kind,
                    Invocation::of(redraw, Some(window_id), false),
                    |app, active| app.window_event(active, window_id, event),
                );
            }
            Arrival::Focus => {
                // When: native focus arrives, it is recorded and dropped; the App takes focus only
                // from the harness's synthetic events, so the user's application keeps keyboard focus.
                self.native_focus_dropped += 1;
            }
            Arrival::PointerRest => {
                // When: the pointer rests where the window opened under it; it is counted, not input.
            }
            Arrival::Occlusion(occluded) => self.native_occlusion(event_loop, occluded),
            Arrival::Routed(Route::Forward) => {
                let native = InvocationKind::WindowEvent { redraw: false };
                let invocation = Invocation::of(native, Some(window_id), false);
                self.forward(event_loop, Dispatch::Native, invocation, |app, active| {
                    app.window_event(active, window_id, event)
                });
            }
            Arrival::Routed(Route::Drop) => {}
            Arrival::Routed(Route::Refuse(what)) => {
                self.invalidate(
                    event_loop,
                    format!("unexpected physical input reached the window: {what}"),
                );
            }
        }
    }
}

impl Probe {
    /// The body of the `user_event` callback.
    pub(super) fn user_event_turn(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if self.stage == Stage::Done {
            return;
        }
        if matches!(event, UserEvent::PaneOutput { .. }) {
            // A batch was published; a role's first one after GO is the phase's output diagnostic.
            self.note_first_output();
        }
        let unexpected = match &event {
            UserEvent::MenuAction => Some("a menu action"),
            UserEvent::OpenScripts => Some("a launch request"),
            UserEvent::OsDrag | UserEvent::DragMoved | UserEvent::DragEnded => {
                Some("an OS drag event")
            }
            _ => None,
        };
        if let Some(what) = unexpected {
            // When: no bridge is installed, such an event came from outside the harness and could
            // reach settings paths outside the scratch directory.
            self.invalidate(event_loop, format!("unexpected user event before dispatch: {what}"));
            return;
        }
        if let UserEvent::PaneProcessExited { pane_id, was_clean, .. } = &event {
            if scenarios::BUILD_HOST == Host::Windows && self.outcome.is_none() {
                // When: on Windows each exit is kept, so a pane that becomes a role pane later is still judged.
                self.exited_panes.push((*pane_id, *was_clean));
                // The first role pane is recorded as startup ends, so an empty list means startup is running.
                let startup_ended = !self.role_panes.is_empty();
                let reason = waits::role_exit_reason(
                    *pane_id,
                    *was_clean,
                    &self.role_panes,
                    startup_ended,
                    false,
                );
                if let Some(reason) = reason {
                    // When: a role's program exited mid-run; the App keeps the pane, so a Hold phase would pass.
                    self.invalidate(event_loop, reason);
                }
            }
        }
        let invocation = Invocation::of(InvocationKind::UserEvent, None, false);
        self.forward(event_loop, Dispatch::Native, invocation, |app, active| {
            app.user_event(active, event)
        });
    }
}

impl Probe {
    /// The body of the `new_events` callback.
    pub(super) fn new_events_turn(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if self.stage == Stage::Done {
            return;
        }
        let invocation = Invocation::of(InvocationKind::NewEvents, None, false);
        self.forward(event_loop, Dispatch::Harness, invocation, |app, active| {
            app.new_events(active, cause)
        });
    }
}

impl Probe {
    /// The body of the `about_to_wait` callback.
    pub(super) fn about_to_wait_turn(&mut self, event_loop: &ActiveEventLoop) {
        if self.stage == Stage::Done {
            return;
        }
        let now = Instant::now();
        if now >= self.run_deadline {
            let reason = format!(
                "the {} s harness deadline expired in {:?}",
                self.plan.timeout_s, self.stage
            );
            self.finish(event_loop, Status::Timeout, Some(reason));
            return;
        }
        if self.stage == Stage::Startup && self.first_present_bound.expired(now, self.first_present)
        {
            // When: no frame presented within the bound, the run cannot be exercised; it ends now
            // as a suspected occlusion, which the comparison retries, not at the run's deadline.
            let redraws = self.meter.as_ref().map_or(0, |meter| meter.redraw_requested);
            let reason = waits::first_present_missing_reason(
                FIRST_PRESENT_WAIT,
                redraws,
                self.occlusion.delivered,
                scenarios::BUILD_HOST,
            );
            self.invalidate(event_loop, reason);
            return;
        }
        self.deliver_overdue_occlusion(event_loop, now);
        self.service_trim(false);
        if matches!(self.stage, Stage::Steps(_)) {
            self.maybe_scan(Instant::now(), ScanTrigger::Harness);
        }
        self.advance(event_loop);
        if self.stage == Stage::Done {
            return;
        }
        let invocation = Invocation::of(InvocationKind::AboutToWait, None, false);
        self.forward(event_loop, Dispatch::Harness, invocation, |app, active| {
            app.about_to_wait(active)
        });
        if self.stage != Stage::Done && event_loop.exiting() {
            // When: the App asked to exit, for example after a last-window close, the run cannot complete.
            self.invalidate(event_loop, "the App requested exit".to_owned());
            return;
        }
        if self.stage == Stage::Startup && self.first_present {
            // The end of the first about_to_wait after a present is the warm-pool barrier; its instant
            // is latched before the startup meter finishes and the progress file is written.
            self.startup_endpoint.get_or_insert(Instant::now());
            self.end_startup(event_loop);
        }
        let flow =
            merge_control_flow(event_loop.control_flow(), self.next_deadline(Instant::now()));
        event_loop.set_control_flow(flow);
    }
}

/// How an ordinary native window event is routed. Physical input could reach settings paths
/// outside the scratch directory, so it voids the run before the App sees it.
pub(super) fn classify(event: &WindowEvent, is_main: bool) -> Route {
    match event {
        WindowEvent::KeyboardInput { .. } => Route::Refuse("keyboard input"),
        // Nothing held means no input; anything held is a physical key.
        WindowEvent::ModifiersChanged(modifiers) if modifiers.state().is_empty() => Route::Drop,
        WindowEvent::ModifiersChanged(_) => Route::Refuse("a modifier key"),
        // IME enable and disable follow the App's own `set_ime_allowed`, not the user.
        WindowEvent::Ime(Ime::Enabled | Ime::Disabled) => Route::Forward,
        // An empty preedit is an IME reset, not typed text.
        WindowEvent::Ime(Ime::Preedit(text, _)) if text.is_empty() => Route::Drop,
        WindowEvent::Ime(_) => Route::Refuse("IME text"),
        WindowEvent::CursorMoved { .. } => Route::Refuse("pointer motion"),
        // Crossing the window edge carries no position or button the App needs.
        WindowEvent::CursorEntered { .. } | WindowEvent::CursorLeft { .. } => Route::Drop,
        WindowEvent::MouseWheel { .. } => Route::Refuse("a wheel scroll"),
        WindowEvent::MouseInput { .. } => Route::Refuse("a mouse button"),
        WindowEvent::Touch(_)
        | WindowEvent::TouchpadPressure { .. }
        | WindowEvent::AxisMotion { .. }
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::DoubleTapGesture { .. }
        | WindowEvent::RotationGesture { .. } => Route::Refuse("a touch or gesture"),
        WindowEvent::DroppedFile(_)
        | WindowEvent::HoveredFile(_)
        | WindowEvent::HoveredFileCancelled => Route::Refuse("a file drag"),
        WindowEvent::CloseRequested => Route::Refuse("a close request"),
        WindowEvent::Destroyed if is_main => Route::Refuse("the measurement window's destruction"),
        _ => Route::Forward,
    }
}

/// The App's `ControlFlow`, shortened to the probe's next wake so neither starves the other.
pub(super) fn merge_control_flow(app_flow: ControlFlow, probe: Option<Instant>) -> ControlFlow {
    match (app_flow, probe) {
        (ControlFlow::Poll, _) => ControlFlow::Poll,
        (flow, None) => flow,
        (ControlFlow::Wait, Some(at)) => ControlFlow::WaitUntil(at),
        (ControlFlow::WaitUntil(app_at), Some(at)) => ControlFlow::WaitUntil(app_at.min(at)),
    }
}
