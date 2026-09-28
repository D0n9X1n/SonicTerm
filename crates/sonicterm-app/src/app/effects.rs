//! Reducer effect and intent dispatch, and the URL-open and owner-close effect helpers.

use super::*;

/// Runs the two-phase governor close and returns any refusal to the caller.
pub(super) fn close_owner(
    governor: &ResourceGovernor,
    owner: ResourceOwnerId,
) -> Result<(), sonicterm_types::BudgetError> {
    governor.begin_close(owner).and_then(|()| governor.finish_close(owner))
}

fn open_url_effect(url: &str) -> std::io::Result<()> {
    sonicterm_cfg::url_open::open(url)
}

impl App {
    /// Execute explicit effects at live boundaries; observational variants only emit diagnostics.
    pub(crate) fn dispatch_effects(
        &mut self,
        effects: smallvec::SmallVec<[sonicterm_app_core::AppEffect; 4]>,
    ) {
        use sonicterm_app_core::AppEffect;
        for effect in effects {
            match effect {
                AppEffect::PtyWrite { .. } => {
                    self.dispatch_pty_write_effect(&effect, PtyInputSource::StateMachine);
                }
                AppEffect::ClipboardSet { text } => {
                    // When: the effect is `ClipboardSet`, so nonempty `text` is
                    // written and empty text stays a no-op contract sentinel.
                    if !text.is_empty() {
                        // When: `text` carries a payload, so it replaces the
                        // clipboard; empty text would clear what the user copied.
                        if let Some(clipboard) = self.clipboard.as_mut() {
                            // When: a `clipboard` handle exists, so the write is
                            // attempted and a backend refusal is not fatal here.
                            let _ = clipboard.set_text(text);
                        }
                    }
                    // Empty text sentinel for CopySelection:
                    // the boundary's existing `copy_selection` already
                    // resolved the selection; the sentinel exists so
                    // the Intent→Effect contract is observable in
                    // tests, and carries no text payload.
                }
                AppEffect::OpenURL { url } => {
                    if let Err(error) = open_url_effect(&url) {
                        tracing::warn!(target: "sonicterm_app::app", %error, "failed to open URL effect");
                    }
                }
                AppEffect::Quit => {
                    self.pending_exit = true;
                }
                AppEffect::Render { window, .. } | AppEffect::RenderDirtyRect { window, .. } => {
                    self.request_effect_redraw(window);
                }
                // ── PTY class ─────────────────────────────────────────
                //
                // PtyClose: the per-pane `PtyHandle::Drop` impl already
                // SIGKILLs the child (CLAUDE.md §4 land-mine). Removing
                // the pane entry from `WindowState.panes` is what
                // actually triggers the drop. We try the main window
                // first; if not found, scan child windows.
                AppEffect::PtyClose { pane } => {
                    let pane_id = pane.0;
                    let closed = self.close_pty_pane(pane_id);
                    tracing::debug!(target: "state_machine", pane = pane_id, closed, "dispatch_effects: PtyClose");
                }
                // ChildExitPropagate: observability — the renderer's
                // poll loop already noticed the child exit and updated
                // the per-pane status. Surface a structured log so the
                // session-restore layer (post-v1.0) can correlate.
                AppEffect::ChildExitPropagate { pane, status } => {
                    tracing::info!(target: "state_machine", pane = pane.0, status, "child exit observed");
                }
                // ChildSpawn: record-only at the boundary. Production
                // pane spawning flows through `App::spawn_pane` /
                // `spawn_tab_in_child`, which constructs the PTY
                // directly; the effect here is the observable contract.
                AppEffect::ChildSpawn { pane, argv0 } => {
                    tracing::debug!(target: "state_machine", pane = pane.0, %argv0, "dispatch_effects: ChildSpawn (record-only)");
                }
                // ── OS drag class ────────────────────────────────────
                //
                // The actual platform OS drag is initiated by the
                // tear-out / tab-drag path which talks directly to the
                // platform backend (NSPasteboard / OLE). The reducer
                // emits OsDragStart for observability + future
                // session-restore.
                AppEffect::OsDragStart { src_window, payload_tab } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        tab = payload_tab,
                        "dispatch_effects: OsDragStart (platform path owns the actual drag)"
                    );
                }
                // Native drag owns settlement; this observation must not commit the transfer a second time.
                AppEffect::OsDragEnd { src_window, committed } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        committed,
                        "dispatch_effects: OsDragEnd (observation-only)"
                    );
                }
                // ── Clipboard / notification side channels ───────────
                //
                // ClipboardRequest: async paste handshake. The actual
                // read happens through `clipboard.get_text()` at the
                // boundary's paste path; here we surface the request.
                AppEffect::ClipboardRequest { window, bracketed } => {
                    tracing::debug!(target: "state_machine", window = window.0, bracketed,
                        "dispatch_effects: ClipboardRequest (observation-only; native paste owns clipboard reads)");
                }
                // Notification: route through the existing
                // `notify_command_done` path (test capture friendly).
                AppEffect::Notification { title, body } => {
                    // When: the effect is `Notification`, so `title` and `body`
                    // are joined into the one line the notifier accepts.
                    let combined = if title.is_empty() { body } else { format!("{title}: {body}") };
                    notify_command_done(combined);
                }
                // ── Window ops ───────────────────────────────────────
                //
                // WindowOpen: defer to the existing pending-new-window
                // flag drained by event_loop on the next tick. The
                // platform-creation requires `&ActiveEventLoop` which
                // dispatch_effects doesn't carry — flagging keeps the
                // request observable without changing the dispatcher
                // signature.
                AppEffect::WindowOpen { role, initial_size } => {
                    self.pending_new_window = Some(self.window_request(None));
                    tracing::debug!(
                        target: "state_machine",
                        ?role,
                        ?initial_size,
                        "dispatch_effects: WindowOpen queued (drained by event_loop)"
                    );
                }
                // WindowClose is observational; only live native topology decides close and last-window exit.
                AppEffect::WindowClose { window } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        "dispatch_effects: WindowClose (platform path closes via WindowEvent::CloseRequested)"
                    );
                }
                // WindowResize: programmatic resize. winit's
                // `set_inner_size` is the API; since `LogicalSize` here
                // is f64 cells (not pixels) per the reducer's contract,
                // emit a redraw so the boundary re-measures.
                AppEffect::WindowResize { window, size } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        w = size.width,
                        h = size.height,
                        "dispatch_effects: WindowResize (observability)"
                    );
                    self.request_effect_redraw(window);
                }
                // WindowMove: record-only; OS already moved the window.
                AppEffect::WindowMove { window, pos } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        x = pos.x,
                        y = pos.y,
                        "dispatch_effects: WindowMove (record-only)"
                    );
                }
                // WindowSetTitle is observational; only explicit window naming may change native titles.
                AppEffect::WindowSetTitle { window, title } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        %title,
                        "dispatch_effects: WindowSetTitle (observation-only)"
                    );
                }
                // TimerSchedule / TimerCancel: record-only. No reducer path
                // emits them; redraw pacing sets winit's
                // `ControlFlow::WaitUntil` directly.
                AppEffect::TimerSchedule { id, at } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        ?at,
                        "dispatch_effects: TimerSchedule (record-only — winit ControlFlow drives pacing)"
                    );
                }
                AppEffect::TimerCancel { id } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        "dispatch_effects: TimerCancel (record-only)"
                    );
                }
                // ── Menubar ──────────────────────────────────────────
                //
                // MenubarUpdate: log-only on every platform. The macOS and
                // Windows menubars are built from `menu::blueprint`, and
                // `menubar_bridge` carries only menu clicks back to the app.
                AppEffect::MenubarUpdate(model) => {
                    tracing::debug!(
                        target: "state_machine",
                        items = model.items.len(),
                        "dispatch_effects: MenubarUpdate (platform path owns NSMenu/muda mutation)"
                    );
                }
                // ── Log ──────────────────────────────────────────────
                //
                // LogEvent: forward to tracing at the requested level.
                AppEffect::LogEvent { level, target, msg } => {
                    use sonicterm_app_core::LogLevel;
                    // `target` is &'static str from the reducer but
                    // tracing's `target:` slot needs a literal at the
                    // call site, so capture both as fields instead.
                    match level {
                        LogLevel::Trace => {
                            tracing::trace!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Debug => {
                            tracing::debug!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Info => {
                            tracing::info!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Warn => {
                            tracing::warn!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Error => {
                            tracing::error!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                    }
                }
                // `AppEffect` is #[non_exhaustive]; future variants
                // surface here as an unrouted log until wired.
                _ => {
                    tracing::trace!(target: "state_machine", "dispatch_effects: unrouted effect {:?}", effect);
                }
            }
        }
    }

    fn request_effect_redraw(&self, key: sonicterm_types::WindowKey) -> bool {
        let Some(window) = self.window_keys.resolve(key).and_then(|id| self.windows.get(&id))
        else {
            // When: `key` names no live window, never redirect operational work to main or frontmost.
            return false;
        };
        window.request_redraw();
        self.redraw_request_count.fetch_add(1, Ordering::SeqCst);
        true
    }

    pub(super) fn observe_intent(&mut self, intent: sonicterm_app_core::AppIntent) {
        // Compatibility reducer state is observational; its synthetic effects never mutate live topology.
        let _ = self.machine.handle(intent);
    }

    /// Execute supported explicit-target work; retain lifecycle reports as non-operational compatibility observations.
    pub fn dispatch_intent(&mut self, intent: sonicterm_app_core::AppIntent) {
        use sonicterm_app_core::{AppEffect, AppIntent};
        match intent {
            AppIntent::PtyWrite { pane, bytes } => {
                self.write_to_pane(pane.0, bytes.to_vec(), PtyInputSource::StateMachine);
            }
            AppIntent::PtyExit { pane, status } => {
                self.dispatch_effects(smallvec::smallvec![
                    AppEffect::ChildExitPropagate { pane, status },
                    AppEffect::PtyClose { pane }
                ]);
            }
            AppIntent::PtyBurst { pane, .. } | AppIntent::ForegroundProcChanged { pane, .. } => {
                if let Some(id) = self
                    .windows
                    .iter()
                    .find_map(|(id, window)| window.panes.contains_key(&pane.0).then_some(*id))
                {
                    if let Some(key) = self.window_key(id) {
                        self.request_effect_redraw(key);
                    }
                }
            }
            AppIntent::RedrawRequested { window } => {
                self.request_effect_redraw(window);
            }
            AppIntent::Key { window, pressed: true, .. }
            | AppIntent::ImeStart { window }
            | AppIntent::ImeEnd { window }
            | AppIntent::ImePreedit { window, .. }
            | AppIntent::HoverUrl { window, .. }
            | AppIntent::ScrollUp { window, .. }
            | AppIntent::ScrollDown { window, .. }
            | AppIntent::ScrollPageUp { window }
            | AppIntent::ScrollPageDown { window }
            | AppIntent::ScrollToTop { window }
            | AppIntent::ScrollToBottom { window }
            | AppIntent::ScrollToCursor { window }
            | AppIntent::MouseWheel { window, .. } => {
                self.request_effect_redraw(window);
            }
            AppIntent::ImeCommit { window, text } | AppIntent::Paste { window, text, .. } => {
                if let Some(id) = self.window_keys.resolve(window) {
                    let pane = self
                        .windows
                        .get(&id)
                        .and_then(|state| state.tab_states.get(state.tabs.active_index()))
                        .map(|tab| tab.active_pane);
                    if let Some(pane) = pane.filter(|pane| self.admits_new_user_input(*pane)) {
                        self.write_to_pane(pane, text.into_bytes(), PtyInputSource::StateMachine);
                    }
                }
            }
            AppIntent::ClickUrl { url, .. } => {
                self.dispatch_effects(smallvec::smallvec![AppEffect::OpenURL { url }])
            }
            AppIntent::Exit => self.pending_exit = true,
            other => self.observe_intent(other),
        }
    }
}

#[cfg(test)]
#[path = "effects_tests.rs"]
mod effects_tests;
