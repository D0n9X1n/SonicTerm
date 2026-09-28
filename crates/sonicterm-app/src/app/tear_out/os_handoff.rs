//! Cross-process drag handoff: publish a tab to the OS drag backend or sink once the
//! cursor has left every SonicTerm window.

use super::*;

impl App {
    pub(in crate::app) fn try_os_drag_handoff(&mut self, index: usize) -> bool {
        let Some(sink) = self.os_drag_sink.clone() else {
            // When: no `os_drag_sink` is installed, so there is no cross-process route
            // for the payload; return false to fall back to the in-process tear-out.
            return false;
        };
        if self.cursor_inside_any_window() {
            // When: `cursor_inside_any_window` is true, so the drop is still over a
            // SonicTerm window; keep the gesture in-process instead of publishing to OS.
            return false;
        }
        let Some((source_window, source_tab)) = self
            .main_window_id
            .and_then(|window| self.tab_id_at(window, index).map(|tab| (window, tab)))
        else {
            // When: the requested source tab no longer exists, no OS handoff may adopt its former slot.
            return false;
        };
        let Some(payload) = self.build_payload_for_tab(index) else {
            // When: `build_payload_for_tab` found no tab at `index`, so there is nothing
            // to publish to the OS; return false and let the in-process path decide.
            return false;
        };

        // Hand the gesture to the installed OsTabDragBackend first. The backend is
        // responsible for OS cursor capture + pasteboard / OLE handoff. If
        // `handles_full_gesture()` returns true (Windows: DoDragDrop ran end-to-end
        // inside the backend) we MUST NOT also invoke `sink.begin_drag` — that would
        // re-enter DoDragDrop with no live gesture, immediately returning NONE and
        // falsely triggering `spawn_tearout_child`. The backend's DragOutcome routes
        // through `handle_os_drag_ended` (transfer_tab / cancel_drag_session); when the
        // backend owns the gesture we return true here without detaching — the
        // dispatcher will handle source-side removal via transfer_tab.
        //
        // On Mac (handles_full_gesture == false) the backend only writes the pasteboard
        // (winit intercepts mouse events, so NSDraggingSession proper isn't reachable) —
        // we still fall through to the sink, which also writes the pasteboard and
        // returns NotAcknowledged.
        if self.os_drag_backend.is_some() {
            // When: an `os_drag_backend` is installed, so it owns cursor capture and the
            // pasteboard/OLE handoff; run it before the sink to avoid a second DoDragDrop.
            let payload_json = payload.to_json().unwrap_or_default();
            let source_window = self.main_window().map(|window| window.id());
            if let Some(src_id) = source_window {
                // When: `source_window` resolves to a real id, which `begin_os_tab_drag`
                // needs to anchor the session and record the drag source.

                // Render a small PNG thumbnail for backends that support a
                // native preview. Windows OLE uses it; the current macOS
                // pasteboard-only backend records but cannot display it.
                // See `crates/sonicterm-app/src/tab_thumbnail.rs` for the
                // rationale behind the CPU-side renderer.
                let thumb_inputs =
                    crate::tab_thumbnail::tab_thumbnail_inputs_from_payload(&payload.tab_title);
                let drag_image_png = crate::tab_thumbnail::render_tab_thumbnail_png(&thumb_inputs);
                let started = self.begin_os_tab_drag(src_id, index, payload_json, drag_image_png);
                if started && self.os_drag_backend_handles_full_gesture() {
                    // When: `started` and the backend owns the whole gesture, so it already
                    // ran DoDragDrop; a second, gestureless call would return NONE.
                    tracing::info!(
                        target: "sonicterm_app::app::tear_out",
                        tab = %payload.tab_title,
                        "backend owns gesture end-to-end; legacy sink skipped"
                    );
                    return true;
                }
            }
        }

        let ack = sink.begin_drag(&payload);
        match ack {
            crate::os_drag::DragAck::Accepted => {
                // An acknowledged destination owns the payload; retire source PTYs without changing live in-process transfer behavior.
                if let Some(index) = self.tab_index_of_id(source_window, source_tab) {
                    if let Some((_, _, panes)) = self.detach_from_child(source_window, index) {
                        for pane in panes.into_values() {
                            self.retire_pane(pane);
                        }
                    }
                }
                tracing::info!(
                    target: "sonicterm_app::app::tear_out",
                    tab = %payload.tab_title,
                    "OS drag: destination acknowledged; local tab dropped"
                );
                true
            }
            crate::os_drag::DragAck::NotAcknowledged => {
                // No destination confirmed adoption. Leave the source tab
                // alive and fall back to the in-process tear-out path so
                // the user does not lose a live shell.
                tracing::warn!(
                    target: "sonicterm_app::app::tear_out",
                    tab = %payload.tab_title,
                    "OS drag: sink NotAcknowledged; keeping source tab, falling back to in-process tear-out"
                );
                false
            }
        }
    }
    pub(super) fn build_payload_for_tab(&self, index: usize) -> Option<crate::os_drag::TabPayload> {
        let tab = self.main_tabs()?.tabs().get(index)?.clone();
        // Scrollback is not carried in the payload: Grid exposes no full
        // visible+scrollback text accessor, so the buffer ships empty and
        // the destination shell starts at a fresh prompt.
        let scrollback_bytes: Vec<u8> = Vec::new();
        Some(crate::os_drag::TabPayload {
            pty_pid: 0,
            tab_title: tab.title,
            scrollback_b64: crate::os_drag::TabPayload::encode_scrollback(&scrollback_bytes),
            cwd: String::new(),
            cmd: self.config.terminal.shell.clone().unwrap_or_default(),
            env: Vec::new(),
        })
    }
    pub(super) fn cursor_inside_any_window(&self) -> bool {
        let Some(main) = self.main_window() else {
            // When: no `main_window` exists, so the cursor has no origin to be made
            // global against; report it as outside rather than guess a screen point.
            return false;
        };
        let main_origin = main
            .inner_position()
            .map(|position| (position.x, position.y))
            .unwrap_or_else(|_| (0, 0));
        let cursor_pos = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
        let global = crate::tab_drag::local_to_global(main_origin, cursor_pos);
        if crate::tab_drag::global_to_local(window_geom(main), global).is_some() {
            // When: `global_to_local` places the cursor inside main's rect, so the drop
            // is still over SonicTerm; stop before walking the child windows.
            return true;
        }
        for child in self.windows.values() {
            let Some(window) = child.window.as_ref() else {
                // When: this child holds no `window`, so it has no screen rect to test
                // the cursor against; skip it rather than treat it as a hit.
                continue;
            };
            if crate::tab_drag::global_to_local(window_geom(window), global).is_some() {
                // When: `global_to_local` places the cursor inside this child's rect, so
                // the drop is over SonicTerm; stop the walk at the first hit.
                return true;
            }
        }
        false
    }
}
