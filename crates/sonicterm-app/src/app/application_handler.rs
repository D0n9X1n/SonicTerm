//! Native ApplicationHandler callbacks and dispatch-scoped forwarding.

use super::*;

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _dispatch = self.frame_dispatch_scope();
        self.do_resumed(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let _dispatch = self.frame_dispatch_scope();
        self.note_frame_user_wake();
        let started = self.frame_clock_start();
        self.do_user_event(event_loop, event);
        self.acknowledge_tab_activations();
        self.note_frame_dispatch(frame_counters::DispatchKind::UserEvent, started);
        self.emit_frame_lines(None);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        let _dispatch = self.frame_dispatch_scope();
        let started = self.frame_clock_start();
        let redraw = started.is_some() && matches!(event, WindowEvent::RedrawRequested);
        if redraw {
            // the gate is on, a watched window's dispatch entry is recorded first.
            self.note_dispatch_entry(win_id);
        }
        // The dispatch may close the window, so whether it counts is read before it runs.
        let counted = started.is_some() && self.begin_window_handler(win_id);
        if redraw {
            // the gate is on, a redraw is counted and takes its panes' pending flushes.
            self.note_redraw_requested(win_id);
        }
        self.do_window_event(event_loop, win_id, event);
        self.acknowledge_tab_activations();
        if let Some(started) = started {
            // the gate is on, the window's handler time is recorded.
            self.note_window_handler(win_id, started, counted);
            self.emit_frame_lines(Some((win_id, redraw)));
            if redraw {
                // A watched dispatch's return is recorded after its bookkeeping.
                self.note_dispatch_return(win_id);
            }
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        let _dispatch = self.frame_dispatch_scope();
        self.note_frame_wake(&cause);
        let started = self.frame_clock_start();
        self.do_new_events(event_loop, cause);
        self.note_frame_dispatch(frame_counters::DispatchKind::NewEvents, started);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _dispatch = self.frame_dispatch_scope();
        let started = self.frame_clock_start();
        self.do_about_to_wait(event_loop);
        self.acknowledge_tab_activations();
        self.note_frame_dispatch(frame_counters::DispatchKind::AboutToWait, started);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        let _dispatch = self.frame_dispatch_scope();
        // Forward to sonicterm-logging so every Cmd+Q / WM_CLOSE /
        // last-window exit lands in sonicterm.log. See
        // `crates/sonicterm-logging/src/exit_trace.rs`.
        self.finish_frame_lines();
        // Quitting leaves no progress on any platform surface.
        self.shutdown_taskbar_progress();
        sonicterm_logging::record_loop_exiting();
    }
}
