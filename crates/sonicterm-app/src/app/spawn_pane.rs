//! Extracted from `app/mod.rs` from the monolithic app module.
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use base64::Engine;
use parking_lot::Mutex;
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, Direction, Keymap, ScrollAction},
    theme::Theme,
};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::{PtyChildExitProbe, PtyHandle};
use sonicterm_render_model::InlineImage;
use sonicterm_ui::{
    pane::PaneTree,
    selection::Selection,
    tabbar_view::{TabBarLayout, TabHit},
    tabs::{Tab, TabBar},
};
use sonicterm_vt::vt::{CommandEvent, MediaEvent, Parser, VtEvent};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState, UserEvent,
    WindowState,
};

/// How long the VT loop waits for an exited child to become observable.
///
/// EOF on the pty master and the child becoming reapable are two events with
/// no ordering between them, so a single probe at EOF can read "still
/// running" for a shell that has already gone. The wait is bounded because
/// the answer is only worth having promptly: past this, the pane stays open,
/// which is the same outcome as an unclean exit and costs the user nothing
/// but a stale tab they can close.
const CHILD_EXIT_OBSERVE_TIMEOUT: Duration = Duration::from_millis(250);
/// Gap between exit observations while waiting.
const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Maximum decoded UTF-8 text one OSC 52 write may place on the clipboard.
pub(super) const MAX_OSC52_CLIPBOARD_BYTES: usize = 512 * 1024;

/// Decode one ordinary OSC 52 clipboard write into a bounded app-thread event.
///
/// Only target `c` writes are accepted. Queries (`?`), unsupported selections,
/// malformed Base64, non-UTF-8 payloads, and decoded output above the hard cap
/// remain inert and can never expose or mutate the native clipboard.
pub(super) fn osc52_clipboard_write_event(selection: char, data: &str) -> Option<UserEvent> {
    if selection != 'c' || data == "?" || data.is_empty() {
        // When: `selection` is not clipboard `c`, or `data` is a query/empty, refuse the unsupported operation.
        return None;
    }
    let max_encoded = MAX_OSC52_CLIPBOARD_BYTES.div_ceil(3) * 4;
    if data.len() > max_encoded {
        // When: `data` cannot encode a payload within the decoded cap, reject before allocating the output buffer.
        return None;
    }
    let mut decoded = vec![0u8; MAX_OSC52_CLIPBOARD_BYTES];
    let written = base64::engine::general_purpose::STANDARD
        .decode_slice(data.as_bytes(), &mut decoded)
        .ok()?;
    decoded.truncate(written);
    let text = String::from_utf8(decoded).ok()?;
    (!text.is_empty()).then_some(UserEvent::ClipboardWrite { text })
}

/// Whether the pane's child exited cleanly, waiting briefly for it to become
/// observable.
///
/// `None` means unknown — the child outlived the wait, or the probe failed.
/// Callers must not read that as a crash: it decides only that the pane stays
/// open, never that it closes.
pub(super) fn observe_child_exit_cleanliness(probe: &PtyChildExitProbe) -> Option<bool> {
    let deadline = Instant::now() + CHILD_EXIT_OBSERVE_TIMEOUT;
    loop {
        match probe.has_exited() {
            Ok(true) => {
                // When: has_exited returns Ok(true), report the child's recorded exit status.
                return probe.exit_was_clean();
            }
            Ok(false) => {
                // When: has_exited returns Ok(false), keep polling until the observation deadline.
            }
            Err(error) => {
                // When: has_exited returns Err(error), preserve an unknown exit classification.
                tracing::debug!(%error, "failed to observe pane child exit");
                return None;
            }
        }
        if Instant::now() >= deadline {
            // When: Instant::now reaches deadline, stop waiting and preserve an unknown result.
            return None;
        }
        std::thread::sleep(CHILD_EXIT_POLL_INTERVAL);
    }
}

/// How long a quiescent VT loop parks before looking around.
///
/// On unix the pty reader reaches EOF once the child's last slave fd closes,
/// so a loop learns of an exit by its channel disconnecting and never needs to
/// wake on its own. An hour is "effectively forever": idle panes cost no
/// wakeups at all.
///
/// Windows cannot use that signal. The ConPTY master is held open by our own
/// `PtyHandle`, whose `HPCON` is released only when that handle drops — which
/// happens when the pane closes. The reader therefore never reaches EOF while
/// the pane lives, so the disconnect that would report the exit sits
/// *downstream of the close it is supposed to cause*. Measured: the channel
/// stayed open for a full 10s after a clean exit with the handle held. Polling
/// the exit probe is the only way out of that circle, so the loop wakes
/// periodically and pays two wakeups per second per idle pane to get it.
#[cfg(not(windows))]
pub(super) const PANE_IDLE_WAIT: Duration = Duration::from_secs(3600);
#[cfg(windows)]
pub(super) const PANE_IDLE_WAIT: Duration = Duration::from_millis(500);

/// Classify a pane's child exit and report it to the event loop.
///
/// Shared by both VT loops and by both ways a loop can notice an exit: the
/// output channel disconnecting, and — where that never happens — the probe.
pub(super) fn report_pane_exit(
    proxy: Option<&EventLoopProxy<UserEvent>>,
    probe: &PtyChildExitProbe,
    pane_id: u64,
) {
    let Some(proxy) = proxy else {
        // When: proxy is None, there is no event-loop recipient for the pane exit.
        return;
    };
    let was_clean = observe_child_exit_cleanliness(probe);
    let _ = proxy.send_event(UserEvent::PaneProcessExited { pane_id, was_clean });
}

/// Pane-owned state shared with its VT worker.
#[derive(Clone)]
pub(super) struct PaneVtHandles {
    parser: Arc<Mutex<Parser>>,
    redraw_target: Arc<Mutex<Option<WindowId>>>,
    command_events: Arc<Mutex<Vec<super::PaneCommandEvent>>>,
    cursor_visible: Arc<AtomicBool>,
    keyboard_input: Arc<AtomicU64>,
    pointer_input: Arc<std::sync::atomic::AtomicU8>,
    output_generation: Arc<AtomicU64>,
    inline_images: Arc<Mutex<Vec<InlineImage>>>,
    inline_media_charge: super::media::SharedInlineMediaCharge,
    frame_counters: Option<super::frame_counters::PaneFrameCounters>,
}

impl PaneVtHandles {
    /// Clone the exact pane-owned handles that its VT worker must update.
    fn from_pane_state(pane: &PaneState) -> Self {
        Self {
            parser: pane.parser.clone(),
            redraw_target: pane.redraw_target.clone(),
            command_events: pane.command_events.clone(),
            cursor_visible: pane.cursor_visible.clone(),
            keyboard_input: pane.keyboard_input.clone(),
            pointer_input: pane.pointer_input.clone(),
            output_generation: pane.output_generation.clone(),
            inline_images: pane.inline_images.clone(),
            inline_media_charge: pane.inline_media_charge.clone(),
            frame_counters: pane.frame_counters.clone(),
        }
    }
}

/// Start the VT worker with handles cloned from its completed pane.
pub(super) fn spawn_pane_workers(
    pane_id: u64,
    pane: &PaneState,
    proxy: Option<EventLoopProxy<UserEvent>>,
    vt_thread_name: &'static str,
) {
    let pty = pane.pty.as_ref().expect("pane worker requires a PTY");
    let out_rx = pty.out_rx.clone();
    let exit_probe = pty.child_exit_probe();
    let input_state = pty.input_sender();
    let in_tx_reply = pty.reply_sender();
    let worker_handles = PaneVtHandles::from_pane_state(pane);
    let redraw_proxy = proxy.clone();

    std::thread::Builder::new()
        .name(vt_thread_name.into())
        .spawn(move || {
            let mut pending = false;
            let mut pending_since: Option<Instant> = None;
            let mut pending_bytes: usize = 0;
            let mut command_started: Option<Instant> = None;
            let mut replies_failed = false;
            let mut redraw_probe = crate::app::invariants::RedrawCoalescerProbe::new();
            loop {
                match out_rx.recv_timeout(if pending {
                    crate::app::PTY_REDRAW_QUIESCENT
                } else {
                    PANE_IDLE_WAIT
                }) {
                    Ok(bytes) => {
                        // When: recv_timeout returns Ok(bytes), parse and coalesce the batch.
                        if !bytes.is_empty() {
                            pending_bytes = pending_bytes.saturating_add(bytes.len());
                            pending_since.get_or_insert_with(Instant::now);
                        }
                        process_pane_vt_batch_and_publish(
                            &worker_handles,
                            bytes,
                            &mut command_started,
                            redraw_proxy.as_ref(),
                            |reply| {
                                if replies_failed {
                                    // When: replies_failed is latched, keep consuming output and observing exit without repeated errors.
                                    return;
                                }
                                let rejected_bytes = reply.len();
                                if let Err(error) = in_tx_reply.send(reply) {
                                    // Report storage/native failure once without abandoning output or exit observation.
                                    replies_failed = true;
                                    if !input_state.is_closing() {
                                        tracing::warn!(pane_id, rejected_bytes, %error, "terminal reply delivery failed");
                                    }
                                }
                            },
                        );
                        let pending_for =
                            pending_since.map(|since| since.elapsed()).unwrap_or(Duration::ZERO);
                        if crate::app::should_flush_pending_pty_redraw(pending_bytes, pending_for) {
                            // When: should_flush_pending_pty_redraw accepts pending_bytes and pending_for, dispatch the coalesced frame.
                            if let Some(proxy) = redraw_proxy.as_ref() {
                                // When: redraw_proxy is Some(proxy), send the redraw through its current target.
                                send_output_redraw(&worker_handles.redraw_target, worker_handles.frame_counters.as_ref(), |window_id| { let _ = proxy.send_event(UserEvent::RequestRedraw(window_id)); });
                            }
                            let reason = if pending_bytes >= crate::app::PTY_REDRAW_FLUSH_BYTES {
                                crate::app::invariants::FlushReason::Buffer
                            } else {
                                crate::app::invariants::FlushReason::Interval
                            };
                            redraw_probe.note_redraw(crate::app::PTY_REDRAW_QUIESCENT, reason);
                            pending = false;
                            pending_since = None;
                            pending_bytes = 0;
                        } else {
                            // When: should_flush_pending_pty_redraw is false, retain the batch for coalescing.
                            pending = true;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        // When: recv_timeout returns Timeout, flush any trailing pending redraw.
                        if pending {
                            // When: pending is true at Timeout, dispatch the coalesced trailing frame.
                            if let Some(proxy) = redraw_proxy.as_ref() {
                                // When: redraw_proxy is Some(proxy), send the redraw through its current target.
                                send_output_redraw(&worker_handles.redraw_target, worker_handles.frame_counters.as_ref(), |window_id| { let _ = proxy.send_event(UserEvent::RequestRedraw(window_id)); });
                            }
                            redraw_probe.note_redraw(
                                crate::app::PTY_REDRAW_QUIESCENT,
                                crate::app::invariants::FlushReason::Interval,
                            );
                            pending = false;
                            pending_since = None;
                            pending_bytes = 0;
                        }
                        #[cfg(windows)]
                        if exit_probe.has_exited().unwrap_or(false) {
                            // When: exit_probe reports the child exited on Windows, report it and stop polling.
                            report_pane_exit(redraw_proxy.as_ref(), &exit_probe, pane_id);
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        // When: recv_timeout returns Disconnected, flush output and report exit.
                        if let Some(proxy) = redraw_proxy.as_ref() {
                            // When: redraw_proxy is Some(proxy), it can receive the final redraw.
                            if pending {
                                // When: pending is true at Disconnected, dispatch the shell's final output.
                                send_output_redraw(&worker_handles.redraw_target, worker_handles.frame_counters.as_ref(), |window_id| { let _ = proxy.send_event(UserEvent::RequestRedraw(window_id)); });
                            }
                        }
                        report_pane_exit(redraw_proxy.as_ref(), &exit_probe, pane_id);
                        break;
                    }
                }
            }
        })
        .expect("spawn pane VT loop");
}

/// Request a redraw of the pane's current target after output.
pub(super) fn send_output_redraw<Target: Clone>(
    redraw_target: &Mutex<Option<Target>>,
    counters: Option<&super::frame_counters::PaneFrameCounters>,
    send: impl FnOnce(Target),
) {
    let Some(counters) = counters else {
        // When: `counters` is None, the App's gate is off; the request is sent as before, uncounted.
        super::redraw_target::dispatch(redraw_target, send);
        return;
    };
    let mut targeted = false;
    super::redraw_target::dispatch(redraw_target, |target| {
        targeted = true;
        // The timestamp is published before the send, so the event loop never wakes without it.
        let now_ns = super::frame_counters::flush_clock_ns();
        super::frame_counters::publish_flush(&counters.pending_flush, now_ns, &counters.vt);
        send(target);
    });
    if !targeted {
        // the pane has no redraw target, no window can consume a timestamp; none is stored.
        super::frame_counters::note_untargeted_flush(&counters.vt);
    }
}

/// Publish one completed nonempty batch only after parser, media, and host side effects return.
// Ordering: output_generation Release pairs with the window's pre-lock Acquire snapshot.
pub(super) fn process_pane_vt_batch_and_publish<Bytes: AsRef<[u8]>>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    command_started: &mut Option<Instant>,
    proxy: Option<&EventLoopProxy<UserEvent>>,
    send_reply: impl FnMut(Vec<u8>),
) {
    let nonempty = !bytes.as_ref().is_empty();
    process_pane_vt_batch(handles, bytes, command_started, proxy, send_reply);
    if nonempty {
        handles.output_generation.fetch_add(1, Ordering::Release);
    }
}

/// Parse one PTY output batch and apply its app-owned side effects after unlocking.
pub(super) fn process_pane_vt_batch<Bytes: AsRef<[u8]>>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    command_started: &mut Option<Instant>,
    proxy: Option<&EventLoopProxy<UserEvent>>,
    send_reply: impl FnMut(Vec<u8>),
) {
    process_pane_vt_batch_with(
        handles,
        bytes,
        command_started,
        super::media::decode_inline_image,
        |event| {
            if let Some(proxy) = proxy {
                // When: proxy exists, deliver the typed host event on the app event loop.
                let _ = proxy.send_event(event);
            }
        },
        Instant::now,
        send_reply,
    )
}

// Lock order: inline_images -> inline_media_charge; parser releases before either, and command_events locks after both.
// Ordering: cursor_visible, keyboard_input and pointer_input use Relaxed; each word is self-contained, not a barrier for parser changes.
fn process_pane_vt_batch_with<Bytes, Decode, Emit, Now, Send>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    command_started: &mut Option<Instant>,
    mut decode_media: Decode,
    mut emit_event: Emit,
    mut now: Now,
    mut send_reply: Send,
) where
    Bytes: AsRef<[u8]>,
    Decode: FnMut(&MediaEvent) -> Option<InlineImage>,
    Emit: FnMut(UserEvent),
    Now: FnMut() -> Instant,
    Send: FnMut(Vec<u8>),
{
    let mut remaining = bytes.as_ref();
    let mut reply_batch = Vec::new();
    let counters = handles.frame_counters.as_ref();
    if let Some(counters) = counters {
        // the App's gate is on, a nonempty batch counts once however often it takes the lock.
        if !remaining.is_empty() {
            counters.vt.note_batch();
        }
    }
    loop {
        // With the gate on, one clock read precedes lock() and three are taken under the guard;
        // all arithmetic and every counter update wait until the guard has dropped.
        let before_lock = counters.map(|_| now());
        let (result, section) = {
            let mut parser = handles.parser.lock();
            let locked_at = before_lock.map(|_| now());
            let result = parser.advance_with_replies(remaining);
            let parsed_at = before_lock.map(|_| now());
            handles.keyboard_input.store(parser.keyboard_input_snapshot(), Ordering::Relaxed);
            handles.pointer_input.store(parser.pointer_input_snapshot(), Ordering::Relaxed);
            let released_at = before_lock.map(|_| now());
            (result, (locked_at, parsed_at, released_at))
        };
        let (consumed, events, replies) = result;
        if let (
            Some(counters),
            Some(before_lock),
            (Some(locked_at), Some(parsed_at), Some(released_at)),
        ) = (counters, before_lock, section)
        {
            // the gate is on, the section's four instants were taken and are recorded now.
            let times = super::frame_counters::VtSectionTimes {
                before_lock,
                locked_at,
                parsed_at,
                released_at,
            };
            counters.vt.record_section(&times, consumed as u64);
        }
        remaining = &remaining[consumed..];

        let mut clipboard_requests = Vec::new();
        let mut command_side_effects = Vec::new();
        let mut media_events = Vec::new();
        for event in events {
            match event {
                VtEvent::CursorVisibility(visible) => {
                    handles.cursor_visible.store(visible, Ordering::Relaxed);
                }
                VtEvent::Clipboard { selection, data } => {
                    clipboard_requests.push((selection, data));
                }
                VtEvent::Command(event) => {
                    let at = now();
                    let duration = match event {
                        CommandEvent::CmdStart => {
                            *command_started = Some(at);
                            None
                        }
                        CommandEvent::CmdEnd(_) => {
                            command_started.take().map(|started| at.duration_since(started))
                        }
                        CommandEvent::PromptStart | CommandEvent::PromptEnd => None,
                    };
                    command_side_effects.push(super::PaneCommandEvent { event, at, duration });
                }
                VtEvent::Media(media) => media_events.push(media),
                VtEvent::SetTitle(_) | VtEvent::Bell | VtEvent::Hyperlink { .. } => {
                    // When: event is SetTitle, Bell, or Hyperlink, parser state already owns its effect.
                }
            }
        }

        for (selection, data) in clipboard_requests {
            if let Some(event) = osc52_clipboard_write_event(selection, &data) {
                emit_event(event);
            }
        }

        let mut decoded_images = Vec::new();
        for media in media_events {
            if let Some(image) = decode_media(&media) {
                decoded_images.push(image);
                super::media::trim_staged_inline_images(
                    &mut decoded_images,
                    &handles.inline_media_charge,
                );
            }
        }
        if !decoded_images.is_empty() {
            let evicted = {
                let mut images = handles.inline_images.lock();
                images.extend(decoded_images);
                super::media::trim_inline_images_charged(&mut images, &handles.inline_media_charge)
            };
            drop(evicted);
        }

        if !command_side_effects.is_empty() {
            super::append_bounded_command_events(
                &mut handles.command_events.lock(),
                command_side_effects,
            );
        }
        if reply_batch.len() + replies.len() > 32 * 1024 {
            // Flush before appending a complete dispatch that would exceed the batch bound; never split a reply.
            send_reply(std::mem::take(&mut reply_batch));
        }
        reply_batch.extend(replies);
        if remaining.is_empty() {
            // When: remaining is empty, publish trailing replies without waiting for more child output.
            if !reply_batch.is_empty() {
                send_reply(reply_batch);
            }
            break;
        }
    }
}

impl App {
    // Lock order: test_pane_launches releases before parser; neither guard survives PTY or worker creation.
    pub(super) fn spawn_pane(
        &self,
        pane_id: u64,
        launch: &super::pane_launch::PaneLaunch,
    ) -> PaneState {
        #[cfg(test)]
        self.test_pane_launches.borrow_mut().push((pane_id, launch.clone()));
        let (cols, rows) =
            self.main_renderer().map(|renderer| renderer.cells()).unwrap_or((80, 24));
        // Honour the user's configured scrollback depth instead of the
        // Grid's built-in 10k default.
        let mut grid = Grid::new(cols, rows);
        grid.set_scrollback_limit(self.config.terminal.scrollback);
        let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
            grid,
            None,
            Arc::clone(&self.capture_staging_pool),
        )));
        // Seed theme defaults so OSC 10/11/12 `?` queries get a truthful
        // reply — without this nvim guesses (27,29,30) for bg and the
        // neo-tree icon cells visibly differ from SonicTerm's clear surface
        // . Also seeds the OSC 4 palette so CLIs like Copilot can read
        // the full colour set and enable their prompt frame.
        {
            let mut parser_guard = crate::app::frame_counters::lock_parser(&parser);
            super::seed_parser_theme_colors(&mut parser_guard, &self.theme);
        }
        let redraw_target = Arc::new(Mutex::new(self.main_window_id));
        let mut shell_opts = launch.shell_spawn_opts(
            self.config.terminal.term_program.clone(),
            self.config.terminal.shell.clone(),
        );
        shell_opts.clean_e2e = self.runtime_smoke.is_some();
        let pty = match PtyHandle::spawn_default_shell(cols, rows, shell_opts) {
            Ok(pty) => {
                // When: spawn_default_shell returns Ok(pty), stage any launch draft before the worker starts.
                match launch.draft_for_shell(pty.shell_program_path()) {
                    Ok(Some(draft)) => {
                        Self::queue_pty_input(
                            self.event_loop_proxy.as_ref(),
                            &pty,
                            pane_id,
                            super::PtyInputSource::ScriptDraft,
                            draft.into_bytes(),
                        );
                    }
                    Ok(None) => {
                        // When: draft_for_shell returns Ok(None), start the shell without staged script input.
                    }
                    Err(rejection) => {
                        // When: draft_for_shell returns Err(rejection), warn and notify the event loop.
                        let message = launch.draft_rejection_message(rejection);
                        tracing::warn!(%message);
                        if let Some(proxy) = self.event_loop_proxy.as_ref() {
                            // When: event_loop_proxy is Some(proxy), surface the draft rejection to the UI.
                            let _ = proxy.send_event(UserEvent::ScriptDraftRejected { message });
                        }
                    }
                }
                Some(pty)
            }
            Err(error) => {
                tracing::error!("failed to spawn pty: {error}");
                None
            }
        };
        let mut state = PaneState::new_with_media_pool(parser, pty, &self.inline_media_pool);
        self.reserve_pane_teardown(&mut state);
        state.frame_counters = self.pane_frame_counters();
        state.redraw_target = redraw_target;
        if state.pty.is_some() {
            spawn_pane_workers(pane_id, &state, self.event_loop_proxy.clone(), "sonicterm-vt-loop");
        }
        state
    }
}

impl App {
    pub(super) fn split_active(&mut self, dir: Direction) {
        let Some(window) = self.main() else {
            // When: `main` is absent, no destination exists for a new pane or PTY.
            return;
        };
        let Some(tab) = window.tab_states.get(window.tabs.active_index()) else {
            // When: `tab_states` has no active tab, refuse before creating a speculative PTY.
            return;
        };
        if !tab.tree.leaves().contains(&tab.active_pane)
            || !window.panes.contains_key(&tab.active_pane)
        {
            // When: `active_pane` is not a live leaf, preserve topology without spawning another shell.
            return;
        }
        let launch =
            super::pane_launch::PaneLaunch::from_window(Some(window), &self.local_hostname);
        let new_id = next_pane_id();
        let mut new_pane = Some(self.spawn_pane(new_id, &launch));
        let did_split = 'install: {
            let Some(window) = self.main_mut() else {
                // When: main_mut has no destination, leave the spawned pane outside the window for retirement.
                break 'install false;
            };
            let tab_index = window.tabs.active_index();
            let split_ok = {
                let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                    // When: tab_states lacks tab_index, retain the uninstalled pane for the common retirement path.
                    break 'install false;
                };
                let focus = tab_state.active_pane;
                if tab_state.tree.split(focus, dir, new_id) {
                    tab_state.active_pane = new_id;
                    true
                } else {
                    // When: tree.split returns false, keep the existing pane layout and ownership.
                    false
                }
            };
            if split_ok {
                window.panes.insert(new_id, new_pane.take().expect("uninstalled split pane"));
            }
            split_ok
        };
        if let Some(pane) = new_pane {
            self.retire_pane(pane);
        }
        if did_split {
            // Own the new pane now rather than on the next 30-second sample:
            // until it has an owner its memory is attributed to nothing, and
            // anything reserving against it has no owner to reserve against.
            self.reconcile_pane_owners();
            self.resize_visible_panes();
            if let Some(renderer) = self.main_renderer_mut() {
                renderer.flash_pane_focus(new_id);
            }
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
    }
    pub(super) fn close_active_pane(&mut self) {
        let mut retired = None;
        let outcome = {
            let Some(window) = self.main_mut() else {
                // When: main_mut returns None, there is no active window pane to close.
                return;
            };
            let tab_index = window.tabs.active_index();
            let inner = {
                let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                    // When: tab_states.get_mut cannot find tab_index, there is no active pane tree to close.
                    return;
                };
                let focus = tab_state.active_pane;
                if matches!(tab_state.tree, PaneTree::Leaf { id, .. } if id == focus) {
                    (Some(tab_index), None)
                } else {
                    // When: matches finds no focused PaneTree::Leaf, close only the split pane.
                    let new_focus = tab_state
                        .tree
                        .leaves()
                        .into_iter()
                        .find(|id| *id != focus)
                        .unwrap_or(focus);
                    if tab_state.tree.close(focus) {
                        // A successful tree close activates its surviving sibling.
                        tab_state.active_pane = new_focus;
                        // Same reason as the exit-driven path: the search was
                        // scanning the grid that just went away.
                        if let Some(search) = tab_state.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                        (None, Some(focus))
                    } else {
                        // When: tree.close returns false, preserve the pane and tab unchanged.
                        (None, None)
                    }
                }
            };
            if let (_, Some(focus)) = inner {
                retired = window.remove_pane(focus);
            }
            inner
        };
        if let Some(pane) = retired {
            self.retire_pane(pane);
        }
        match outcome {
            (Some(tab_index), _) => self.close_tab_at(tab_index),
            (_, Some(_focus)) => {
                // the surviving sibling's PaneRect just grew to cover
                // the closed pane's area. Push the new layout into its Grid
                // + PtyHandle (matches split / zoom / resize-split paths and
                // mirrors `close_active_pane_in_child`). Without this the
                // survivor keeps its narrow split-time column count and
                // shell output wraps at the old width until the OS window
                // is resized. The actual resize is delegated to
                // `resize_visible_panes` which routes through the pure
                // helper `resize_panes_to_rects` — the path tested by
                // `close_sibling_pane_resizes_survivor_to_full_width` in
                // `crates/sonicterm-app/tests/per_pane_resize.rs`.
                self.resize_visible_panes();
                if let Some(active_id) = self.active_pane_id() {
                    if let Some(renderer) = self.main_renderer_mut() {
                        renderer.flash_pane_focus(active_id);
                    }
                }
                if let Some(main_window) = self.main_window() {
                    crate::app::frame_counters::request_native_redraw(main_window);
                }
            }
            _ => {
                // When: outcome closes neither a tab nor a pane, no layout update is required.
            }
        }
    }
    pub(super) fn focus_pane_dir(&mut self, dir: Direction) {
        let Some(window) = self.main_mut() else {
            // When: main_mut returns None, there is no pane focus to move.
            return;
        };
        let tab_idx = window.tabs.active_index();
        let Some(next) = window
            .tab_states
            .get(tab_idx)
            .and_then(|tab| tab.tree.focus_neighbor(tab.active_pane, dir))
        else {
            // When: the active tab has no pane in `dir`, focus stays unchanged.
            return;
        };
        if let Some(change) = window.begin_pane_focus_change(next) {
            window.finish_pane_focus_change(change);
        }
    }

    pub(super) fn toggle_active_pane_zoom(&mut self) {
        let toggled = {
            let Some(window) = self.main_mut() else {
                // When: main_mut returns None, there is no pane zoom state to toggle.
                return;
            };
            let tab_index = window.tabs.active_index();
            let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                // When: tab_states.get_mut cannot find tab_index, there is no active pane tree to zoom.
                return;
            };
            tab_state.tree.toggle_zoom(tab_state.active_pane)
        };
        if toggled {
            self.resize_visible_panes();
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
    }

    pub(super) fn toggle_broadcast(&mut self, scope: sonicterm_cfg::keymap::BroadcastScope) {
        self.toggle_broadcast_for(self.frontmost_kind(), scope);
    }

    pub(super) fn toggle_broadcast_for(
        &mut self,
        kind: FrontmostKind,
        scope: sonicterm_cfg::keymap::BroadcastScope,
    ) {
        let Some(source_pane) = self.active_pane_id_for_kind(kind) else {
            // When: active_pane_id_for_kind returns None, no pane can source the broadcast.
            return;
        };
        self.broadcast = self.broadcast.toggled(scope, source_pane);
        self.request_redraw_all_terminal_windows();
    }

    pub(super) fn resize_active_split(&mut self, dir: Direction) {
        let resized = {
            let Some(window) = self.main_mut() else {
                // When: main_mut returns None, there is no active split to resize.
                return;
            };
            let tab_index = window.tabs.active_index();
            let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                // When: tab_states.get_mut cannot find tab_index, there is no active split tree.
                return;
            };
            tab_state.tree.resize_split(tab_state.active_pane, dir, 0.05)
        };
        if resized {
            self.resize_visible_panes();
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
    }

    pub(super) fn resize_visible_panes(&mut self) {
        let viewport = self.test_viewport_override;
        if let Some(window) = self.main_mut() {
            window.complete_topology_change(
                super::TopologyChange {
                    resize_visible: true,
                    focus_feedback: None,
                    dirt: super::window_state::TopologyDirt::Window,
                },
                viewport,
            );
        }
    }
}

#[cfg(test)]
#[path = "spawn_pane_tests.rs"]
mod spawn_pane_tests;
