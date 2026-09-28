//! Terminal input dispatch: key targets and writes, PTY input queues with pointer-motion
//! coalescing, and input-rejection reporting.

use super::*;

/// Wrap clipboard text for paste, applying DECSET 2004 bracketed-paste
/// guards (`ESC [ 200 ~` / `ESC [ 201 ~`) when the active pane has
/// requested bracketed paste. Pure function, exported for unit tests.
pub fn wrap_paste(text: &str, bracketed: bool) -> Vec<u8> {
    sonicterm_types::encode_payload(
        &sonicterm_types::UserPayload::Text(text.to_owned()),
        sonicterm_types::PasteTarget { bracketed, dialect: sonicterm_types::ShellDialect::Unknown },
        usize::MAX,
    )
    .expect("text and paste guards fit the address space")
}

/// Compute the absolute viewport-top row for "scroll to previous / next
/// prompt". Returns `None` if there is no prompt in the requested
/// direction. Pure function so tests can drive it without a window.
pub fn pick_prompt_target(
    grid: &sonicterm_grid::grid::Grid,
    current_top_abs: u64,
    forward: bool,
) -> Option<u64> {
    let pick = if forward {
        grid.prompt_after(current_top_abs)
    } else {
        // When: `forward` is unset, so the search runs backward from
        // `current_top_abs` toward older scrollback instead of newer output.
        grid.prompt_before(current_top_abs)
    };
    pick.map(|prompt| prompt.start_row)
}

#[derive(Debug)]
pub(super) struct PendingPointerMotion {
    bytes: [u8; 64],
    pub(super) len: usize,
    profile: Option<(MouseTracking, bool, bool)>,
}

impl Default for PendingPointerMotion {
    fn default() -> Self {
        Self { bytes: [0; 64], len: 0, profile: None }
    }
}

impl PendingPointerMotion {
    fn validate_profile(&mut self, profile: Option<(MouseTracking, bool, bool)>) {
        let Some(current) = profile else {
            // When: profile is unavailable, retain the position until a later retry can validate it.
            return;
        };
        if self.profile.is_some_and(|previous| previous != current)
            || matches!(current.0, MouseTracking::Off | MouseTracking::Button)
        {
            // A changed or disabled tracking mode cannot receive a deferred position.
            self.len = 0;
        }
        self.profile = Some(current);
    }

    fn replace(&mut self, bytes: &[u8]) {
        // SGR reports contain at most three u32 fields; both supported encodings fit this fixed slot.
        assert!(bytes.len() <= self.bytes.len());
        self.bytes[..bytes.len()].copy_from_slice(bytes);
        self.len = bytes.len();
    }

    fn take(&mut self) -> Vec<u8> {
        let bytes = self.bytes[..self.len].to_vec();
        self.len = 0;
        bytes
    }

    fn flush(
        &mut self,
        send: impl FnOnce(Vec<u8>) -> Result<(), sonicterm_io::pty::PtyInputError>,
    ) -> Result<(), sonicterm_io::pty::PtyInputError> {
        use sonicterm_io::pty::PtyInputError;
        if self.len == 0 {
            // When: self.len is zero, do not consume a writer queue slot.
            return Ok(());
        }
        match send(self.take()) {
            Err(PtyInputError::QueueFull(bytes)) => {
                // QueueFull retains the latest position for a later non-rendering wake.
                self.replace(&bytes);
                Ok(())
            }
            result => result,
        }
    }

    fn send_ordered(
        &mut self,
        bytes: Vec<u8>,
        send: impl FnOnce(Vec<u8>) -> Result<(), sonicterm_io::pty::PtyInputError>,
    ) -> Result<(), sonicterm_io::pty::PtyInputError> {
        use sonicterm_io::pty::{pty_input_message_allowed, PtyInputError};
        let prefix_len = self.len;
        let pending = self.take();
        if prefix_len == 0 || !pty_input_message_allowed(prefix_len.saturating_add(bytes.len())) {
            // When: prefix_len is zero or the combined length exceeds the cap, give discrete input the queue slot.
            return send(bytes);
        }
        let mut combined = Vec::with_capacity(prefix_len + bytes.len());
        combined.extend_from_slice(&pending);
        combined.extend_from_slice(&bytes);
        send(combined).map_err(|error| {
            // Rejection attribution covers only discrete input; stale motion cannot replay after it.
            let strip = |mut rejected: Vec<u8>| {
                rejected.drain(..prefix_len);
                rejected
            };
            match error {
                PtyInputError::QueueFull(bytes) => PtyInputError::QueueFull(strip(bytes)),
                PtyInputError::WriterDisconnected(bytes) => {
                    PtyInputError::WriterDisconnected(strip(bytes))
                }
                PtyInputError::MessageTooLarge(bytes) => {
                    PtyInputError::MessageTooLarge(strip(bytes))
                }
            }
        })
    }
}

/// Producer-assigned input category retained without inspecting terminal bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyInputSource {
    /// Encoded physical or logical key event.
    Keyboard,
    /// Clipboard text, including bracketed-paste framing.
    Paste,
    /// Quoted paths dropped onto a terminal.
    FileDrop,
    /// Committed input-method text.
    Ime,
    /// Terminal mouse-button press or release.
    PointerButton,
    /// Terminal pointer movement, with or without a held button.
    PointerMotion,
    /// Mouse-wheel reports or translated arrow sequences.
    Wheel,
    /// Terminal focus-in or focus-out notification.
    FocusReport,
    /// Reply generated by the terminal parser.
    TerminalReply,
    /// Initial shell draft for an opened script.
    ScriptDraft,
    /// Input supplied directly through the backend-free intent API.
    StateMachine,
}

fn pty_input_rejected_event(
    pane_id: u64,
    source: PtyInputSource,
    error: sonicterm_io::pty::PtyInputError,
    diagnostics: sonicterm_io::pty::PtyInputDiagnostics,
) -> UserEvent {
    let reason = error.to_string();
    let rejected_bytes = error.into_bytes().len();
    UserEvent::PtyInputRejected { pane_id, source, rejected_bytes, reason, diagnostics }
}

impl App {
    /// Admit a new user gesture unless the pane's owning window is READONLY.
    pub(super) fn admits_new_user_input(&self, pane_id: u64) -> bool {
        !self.windows.values().any(|window| {
            window.panes.contains_key(&pane_id)
                && window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only)
        })
    }

    pub(super) fn terminal_key_targets(&self, source_pane: u64) -> BTreeSet<u64> {
        let mut targets = BTreeSet::from([source_pane]);
        if matches!(
            self.broadcast,
            BroadcastState::On {
                source_pane: broadcast_source,
                ..
            } if broadcast_source == source_pane
        ) {
            // A matching broadcast source includes
            // every receiver so each can negotiate its own keyboard encoding.
            targets.extend(self.broadcast_receivers());
        }
        targets
    }

    // Ordering: keyboard_input is a self-contained Relaxed snapshot; encoding and ownership share this exact loaded word.
    pub(super) fn encoded_terminal_key_writes(
        &self,
        event: &winit::event::KeyEvent,
        modifiers: ModifiersState,
        targets: &BTreeSet<u64>,
        mut previous: Option<&mut keyboard_protocol::KeyRoutes>,
        synthetic: bool,
    ) -> Vec<(u64, keyboard_protocol::EncodedKey)> {
        use keyboard_protocol::{encode_routed_key, KeyboardSnapshot};
        let native = key_encoding::native_key_event(event);
        targets
            .iter()
            .filter_map(|pane_id| {
                let (window_id, pane) = self
                    .windows
                    .iter()
                    .find_map(|(id, window)| window.panes.get(pane_id).map(|pane| (*id, pane)))?;
                let state =
                    KeyboardSnapshot::from_bits(pane.keyboard_input.load(Ordering::Relaxed));
                let held = previous.as_ref().and_then(|routes| routes.get(pane_id).copied());
                if held.is_some_and(|held| !held.compatible(state, cfg!(windows))) {
                    // When: a held route crosses a protocol boundary, remove it permanently until a fresh press.
                    if let Some(routes) = previous.as_mut() {
                        routes.remove(pane_id);
                    }
                    return None;
                }
                if let Some(reason) =
                    state.refusal_reason(cfg!(windows), native.is_some(), synthetic)
                {
                    // When: native input is unavailable, report only destination metadata and the refusal reason.
                    tracing::warn!(target: "sonicterm_app::app", ?window_id, pane_id, reason, "native keyboard input refused");
                }
                encode_routed_key(
                    state,
                    cfg!(windows),
                    native,
                    synthetic,
                    event.repeat,
                    held,
                    || {
                        key_encoding::encode_key(
                            event,
                            modifiers,
                            state.kitty_flags(),
                            state.modes(),
                            self.config.terminal.keypad_mode,
                        )
                    },
                )
                .map(|encoded| (*pane_id, encoded))
            })
            .collect()
    }

    pub(super) fn dispatch_terminal_key_writes(
        &mut self,
        writes: Vec<(u64, keyboard_protocol::EncodedKey)>,
    ) -> keyboard_protocol::KeyRoutes {
        keyboard_protocol::dispatch_key_writes(writes, |pane_id, bytes| {
            self.write_to_pane(pane_id, bytes, PtyInputSource::Keyboard)
        })
    }

    // Ordering: keyboard_input loads Relaxed; cleanup validates one complete epoch without locking parser output.
    pub(super) fn release_window_native_keys(&mut self, window_id: WindowId) {
        use keyboard_protocol::KeyboardSnapshot;
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: window_id is no longer live, no accepted key ownership remains to drain.
            return;
        };
        let pressed = std::mem::take(&mut window.pty_pressed_keys);
        let ordered: std::collections::BTreeMap<_, _> = pressed.into_iter().collect();
        let mut writes = std::collections::BTreeMap::<u64, Vec<u8>>::new();
        for (pane_id, held) in ordered.into_values().flat_map(|routes| routes.into_iter()) {
            let Some(pane) = self.pane_by_id(pane_id) else {
                // When: pane_id has closed, no live input queue can receive its cleanup.
                continue;
            };
            let snapshot = KeyboardSnapshot::from_bits(pane.keyboard_input.load(Ordering::Relaxed));
            if let Some(bytes) = held.focus_release(snapshot, cfg!(windows)) {
                // When: focus_release returns bytes, combine this pane's releases into one queue admission.
                writes.entry(pane_id).or_default().extend(bytes);
            }
        }
        for (pane_id, bytes) in writes {
            tracing::debug!(
                target: "sonicterm_app::app",
                ?window_id,
                pane_id,
                synthetic_cleanup = true,
                "native keyboard focus release"
            );
            self.write_to_pane(pane_id, bytes, PtyInputSource::Keyboard);
        }
    }

    pub(super) fn write_to_pane(
        &mut self,
        pane_id: u64,
        bytes: Vec<u8>,
        source: PtyInputSource,
    ) -> bool {
        // Test-only ledger: skipped entirely in production so we don't
        // lock+clone+push on every PTY write (— unbounded
        // growth + per-keystroke overhead over a long session).
        if self.pty_write_log_enabled {
            self.test_pty_writes.lock().push((pane_id, bytes.clone()));
        }
        let Some(pane) =
            self.windows.values_mut().find_map(|window| window.panes.get_mut(&pane_id))
        else {
            // When: find_map cannot resolve pane_id, its input has no live destination.
            return false;
        };
        let queued =
            Self::queue_pane_input(self.event_loop_proxy.as_ref(), pane, pane_id, source, bytes);
        #[cfg(windows)]
        if queued && source != PtyInputSource::PointerMotion {
            // Accepted discrete input can launch a silent command; coalesced motion must not schedule process probes.
            self.arm_foreground_probe_after_input(Instant::now());
        }
        queued
    }

    /// Deliver an explicitly targeted PTY effect through the same bounded queue as native input.
    pub(crate) fn dispatch_pty_write_effect(
        &mut self,
        effect: &sonicterm_app_core::AppEffect,
        source: PtyInputSource,
    ) -> bool {
        let sonicterm_app_core::AppEffect::PtyWrite { pane, data } = effect else {
            // When: the effect is not a PTY write, this boundary cannot accept its work.
            return false;
        };
        self.write_to_pane(pane.0, data.to_vec(), source)
    }

    pub(super) fn queue_pane_input(
        proxy: Option<&EventLoopProxy<UserEvent>>,
        pane: &mut PaneState,
        pane_id: u64,
        source: PtyInputSource,
        bytes: Vec<u8>,
    ) -> bool {
        let profile =
            if source == PtyInputSource::PointerMotion || pane.pending_pointer_motion.len != 0 {
                // Profile checks never block discrete input behind parser output.
                pane.parser.try_lock().map(|parser| {
                    (parser.mouse_tracking(), parser.mouse_sgr_enabled(), parser.grid().is_alt())
                })
            } else {
                // When: source is discrete and no motion is pending, the parser is irrelevant to admission.
                None
            };
        pane.pending_pointer_motion.validate_profile(profile);
        let Some(pty) = pane.pty.as_ref() else {
            // When: a pane has no writer, input cannot acquire delivery ownership.
            return false;
        };
        if source == PtyInputSource::PointerMotion {
            // When: source is PointerMotion, coalesce the turn's positions in fixed pane-owned storage.
            if profile.is_some_and(|current| {
                matches!(current.0, MouseTracking::Off | MouseTracking::Button)
            }) {
                // When: profile disables motion, do not retain a stale native report.
                return false;
            }
            pane.pending_pointer_motion.replace(&bytes);
            return true;
        }
        if profile.is_none() {
            // An unknown profile at a discrete barrier supersedes motion rather than delaying the key or replaying stale bytes.
            pane.pending_pointer_motion.len = 0;
        }
        match pane.pending_pointer_motion.send_ordered(bytes, |bytes| {
            #[cfg(test)]
            let submitted = mod_tests::submission_snapshot(&bytes);
            pty.send_input_nonblocking(bytes)?;
            #[cfg(test)]
            if let Some(bytes) = submitted {
                // For a scoped test, record only the bytes the PTY accepted.
                mod_tests::record_submission(pane_id, bytes);
            }
            Ok(())
        }) {
            Ok(()) => true,
            Err(error) => {
                // Refused discrete input preserves attribution without retaining its payload.
                Self::report_pty_input_rejection(
                    proxy,
                    pane_id,
                    source,
                    error,
                    pty.input_diagnostics(),
                );
                false
            }
        }
    }

    pub(super) fn flush_pointer_motion(&mut self, now: Instant) -> Option<Instant> {
        let mut pending = false;
        for window in self.windows.values_mut() {
            for (&pane_id, pane) in &mut window.panes {
                if pane.pending_pointer_motion.len == 0 {
                    // When: pending_pointer_motion.len is zero, idle panes need no parser or writer work.
                    continue;
                }
                let profile = pane.parser.try_lock().map(|parser| {
                    (parser.mouse_tracking(), parser.mouse_sgr_enabled(), parser.grid().is_alt())
                });
                pane.pending_pointer_motion.validate_profile(profile);
                let Some(pty) = pane.pty.as_ref() else {
                    // When: a pane loses its PTY, discard its unsendable position without arming a timer.
                    pane.pending_pointer_motion.len = 0;
                    continue;
                };
                if profile.is_none() {
                    // When: profile is unavailable, retry without dropping the latest position or writing unvalidated bytes.
                    pending = true;
                    continue;
                }
                if let Err(error) = pane.pending_pointer_motion.flush(|bytes| {
                    #[cfg(test)]
                    let submitted = mod_tests::submission_snapshot(&bytes);
                    pty.send_input_nonblocking(bytes)?;
                    #[cfg(test)]
                    if let Some(bytes) = submitted {
                        // For a scoped test, a successful flush is real queue admission.
                        mod_tests::record_submission(pane_id, bytes);
                    }
                    Ok(())
                }) {
                    // When: flush returns error, the disconnected writer reports once because the pending slot is consumed.
                    Self::report_pty_input_rejection(
                        self.event_loop_proxy.as_ref(),
                        pane_id,
                        PtyInputSource::PointerMotion,
                        error,
                        pty.input_diagnostics(),
                    );
                }
                pending |= pane.pending_pointer_motion.len != 0;
            }
        }
        pending.then_some(now + Duration::from_millis(10))
    }

    pub(super) fn queue_pty_input(
        proxy: Option<&EventLoopProxy<UserEvent>>,
        pty: &sonicterm_io::pty::PtyHandle,
        pane_id: u64,
        source: PtyInputSource,
        bytes: Vec<u8>,
    ) -> bool {
        #[cfg(test)]
        let submitted = mod_tests::submission_snapshot(&bytes);
        if let Err(error) = pty.send_input_nonblocking(bytes) {
            // When: `send_input_nonblocking` refuses input, report metadata rather than retaining or replaying the payload.
            Self::report_pty_input_rejection(
                proxy,
                pane_id,
                source,
                error,
                pty.input_diagnostics(),
            );
            return false;
        }
        #[cfg(test)]
        if let Some(bytes) = submitted {
            // For a scoped test, observe the standalone write only after admission.
            mod_tests::record_submission(pane_id, bytes);
        }
        true
    }

    fn report_pty_input_rejection(
        proxy: Option<&EventLoopProxy<UserEvent>>,
        pane_id: u64,
        source: PtyInputSource,
        error: sonicterm_io::pty::PtyInputError,
        diagnostics: sonicterm_io::pty::PtyInputDiagnostics,
    ) {
        let event = pty_input_rejected_event(pane_id, source, error, diagnostics);
        Self::deliver_pty_input_rejection(
            proxy.map(|proxy| |event| proxy.send_event(event).map_err(|closed| closed.0)),
            event,
        );
    }

    fn deliver_pty_input_rejection(
        send_event: Option<impl FnOnce(UserEvent) -> Result<(), UserEvent>>,
        event: UserEvent,
    ) {
        let event = match send_event {
            Some(send_event) => {
                // When: `send_event` exists, defer logging only after the event loop accepts ownership.
                match send_event(event) {
                    Ok(()) => {
                        // When: `send_event` succeeds, the event loop owns logging and current-window attribution.
                        return;
                    }
                    Err(event) => event,
                }
            }
            None => event,
        };
        if let UserEvent::PtyInputRejected {
            pane_id,
            source,
            rejected_bytes,
            reason,
            diagnostics,
        } = event
        {
            // Delivery failure preserves overload evidence without payload or stale window attribution.
            tracing::warn!(
                target: "sonicterm_app::app",
                pane_id,
                window_id = ?None::<WindowId>,
                ?source,
                rejected_bytes,
                %reason,
                observation = "concurrent",
                queued_messages = diagnostics.queued_messages,
                queued_bytes = diagnostics.queued_bytes,
                queue_capacity = diagnostics.queue_capacity,
                writer_phase = ?diagnostics.writer_phase,
                in_flight_bytes = diagnostics.in_flight_bytes,
                in_flight_millis = ?diagnostics.in_flight_millis,
                completed_messages = diagnostics.completed_messages,
                "terminal input was not queued"
            );
        }
    }
}

#[cfg(test)]
#[path = "input_dispatch_tests.rs"]
mod input_dispatch_tests;
