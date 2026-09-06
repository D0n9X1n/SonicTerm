//! Extracted from `app/mod.rs` from the monolithic app module.
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use anyhow::Context;
use base64::Engine;
use parking_lot::Mutex;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::{PtyChildExitProbe, PtyHandle};
use sonicterm_render_model::InlineImage;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::Selection;
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
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
    kitty_flags: Arc<AtomicU8>,
    keyboard_modes: Arc<AtomicU8>,
    inline_images: Arc<Mutex<Vec<InlineImage>>>,
    inline_media_charge: super::media::SharedInlineMediaCharge,
}

impl PaneVtHandles {
    /// Clone the exact pane-owned handles that its VT worker must update.
    fn from_pane_state(pane: &PaneState) -> Self {
        Self {
            parser: pane.parser.clone(),
            redraw_target: pane.redraw_target.clone(),
            command_events: pane.command_events.clone(),
            cursor_visible: pane.cursor_visible.clone(),
            kitty_flags: pane.kitty_flags.clone(),
            keyboard_modes: pane.keyboard_modes.clone(),
            inline_images: pane.inline_images.clone(),
            inline_media_charge: pane.inline_media_charge.clone(),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ReplyRejectionTotals {
    messages: u64,
    bytes: u64,
    queue_full: u64,
    too_large: u64,
    disconnected: u64,
}

fn forward_pty_replies(
    reply_rx: crossbeam_channel::Receiver<Vec<u8>>,
    mut send: impl FnMut(Vec<u8>) -> Result<(), sonicterm_io::pty::PtyInputError>,
    mut report_first: impl FnMut(sonicterm_io::pty::PtyInputError),
    mut summarize: impl FnMut(ReplyRejectionTotals),
) {
    use crossbeam_channel::RecvTimeoutError;
    use sonicterm_io::pty::PtyInputError;

    let mut reported = false;
    let mut pending = ReplyRejectionTotals::default();
    let mut deadline = None;
    loop {
        if deadline.is_some_and(|due| Instant::now() >= due) {
            summarize(std::mem::take(&mut pending));
            deadline = None;
        }
        let received = match deadline {
            Some(due) => reply_rx.recv_deadline(due),
            None => reply_rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let bytes = match received {
            Ok(bytes) => bytes,
            Err(RecvTimeoutError::Timeout) => {
                // When: `received` times out, flush the pending summary before waiting for more replies.
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: `received` disconnects, the parser is gone and no further replies can arrive.
                break;
            }
        };
        if let Err(error) = send(bytes) {
            // When: `send` refuses a reply, retain metadata counts instead of retaining payloads or flooding UI events.
            let disconnected = matches!(error, PtyInputError::WriterDisconnected(_));
            if reported {
                match &error {
                    PtyInputError::QueueFull(_) => pending.queue_full += 1,
                    PtyInputError::MessageTooLarge(_) => pending.too_large += 1,
                    PtyInputError::WriterDisconnected(_) => pending.disconnected += 1,
                }
                pending.messages += 1;
                pending.bytes += error.into_bytes().len() as u64;
                deadline.get_or_insert_with(|| Instant::now() + Duration::from_secs(1));
            } else {
                // When: `reported` is false, surface the first rejection immediately and bound this worker to one UI event.
                reported = true;
                report_first(error);
            }
            if disconnected {
                // When: `disconnected` is true, stop forwarding because the native writer cannot recover.
                break;
            }
        }
    }
    if pending.messages != 0 {
        summarize(pending);
    }
}

/// Start the reply and VT workers with handles cloned from their completed pane.
// Ordering: pty_burst_gen uses Ordering::Release to publish new PTY bytes before redraw dispatch.
pub(super) fn spawn_pane_workers(
    pane_id: u64,
    pane: &PaneState,
    reply_rx: crossbeam_channel::Receiver<Vec<u8>>,
    pty_burst_gen: Arc<AtomicU32>,
    proxy: Option<EventLoopProxy<UserEvent>>,
    reply_thread_name: &'static str,
    vt_thread_name: &'static str,
) {
    let pty = pane.pty.as_ref().expect("pane worker requires a PTY");
    let out_rx = pty.out_rx.clone();
    let exit_probe = pty.child_exit_probe();
    let in_tx_reply = pty.input_sender();
    let worker_handles = PaneVtHandles::from_pane_state(pane);
    let redraw_proxy = proxy.clone();

    std::thread::Builder::new()
        .name(reply_thread_name.into())
        .spawn(move || {
            forward_pty_replies(
                reply_rx,
                |bytes| in_tx_reply.send(bytes),
                |error| {
                    App::report_pty_input_rejection(
                        proxy.as_ref(),
                        pane_id,
                        super::PtyInputSource::TerminalReply,
                        error,
                        in_tx_reply.diagnostics(),
                    );
                },
                |totals| {
                    let diagnostics = in_tx_reply.diagnostics();
                    tracing::warn!(
                        pane_id,
                        source = ?super::PtyInputSource::TerminalReply,
                        rejected_messages = totals.messages,
                        rejected_bytes = totals.bytes,
                        queue_full = totals.queue_full,
                        message_too_large = totals.too_large,
                        writer_disconnected = totals.disconnected,
                        observation = "concurrent",
                        ?diagnostics,
                        "additional terminal replies were not queued"
                    );
                },
            );
        })
        .expect("spawn pane VT reply forwarder");

    std::thread::Builder::new()
        .name(vt_thread_name.into())
        .spawn(move || {
            let mut pending = false;
            let mut pending_since: Option<Instant> = None;
            let mut pending_bytes: usize = 0;
            let mut command_started: Option<Instant> = None;
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
                            let prev = pty_burst_gen.fetch_add(1, Ordering::Release);
                            crate::app::invariants::debug_assert_burst_gen_monotonic(
                                prev,
                                prev.wrapping_add(1),
                            );
                            pending_bytes = pending_bytes.saturating_add(bytes.len());
                            pending_since.get_or_insert_with(Instant::now);
                        }
                        process_pane_vt_batch(
                            &worker_handles,
                            bytes,
                            &mut command_started,
                            redraw_proxy.as_ref(),
                        );
                        let pending_for =
                            pending_since.map(|since| since.elapsed()).unwrap_or(Duration::ZERO);
                        if crate::app::should_flush_pending_pty_redraw(pending_bytes, pending_for) {
                            // When: should_flush_pending_pty_redraw accepts pending_bytes and pending_for, dispatch the coalesced frame.
                            if let Some(proxy) = redraw_proxy.as_ref() {
                                // When: redraw_proxy is Some(proxy), send the redraw through its current target.
                                super::redraw_target::dispatch(
                                    &worker_handles.redraw_target,
                                    |window_id| {
                                        let _ =
                                            proxy.send_event(UserEvent::RequestRedraw(window_id));
                                    },
                                );
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
                                super::redraw_target::dispatch(
                                    &worker_handles.redraw_target,
                                    |window_id| {
                                        let _ =
                                            proxy.send_event(UserEvent::RequestRedraw(window_id));
                                    },
                                );
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
                                super::redraw_target::dispatch(
                                    &worker_handles.redraw_target,
                                    |window_id| {
                                        let _ =
                                            proxy.send_event(UserEvent::RequestRedraw(window_id));
                                    },
                                );
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

/// Parse one PTY output batch and apply its app-owned side effects after unlocking.
pub(super) fn process_pane_vt_batch<Bytes: AsRef<[u8]>>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    command_started: &mut Option<Instant>,
    proxy: Option<&EventLoopProxy<UserEvent>>,
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
    );
}

// Lock order: inline_images -> inline_media_charge; parser releases before either, and command_events locks after both.
// Ordering: cursor_visible, kitty_flags, and keyboard_modes use Ordering::Relaxed; each publishes only its independent parser value.
fn process_pane_vt_batch_with<Bytes, Decode, Emit, Now>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    command_started: &mut Option<Instant>,
    mut decode_media: Decode,
    mut emit_event: Emit,
    mut now: Now,
) where
    Bytes: AsRef<[u8]>,
    Decode: FnMut(&MediaEvent) -> Option<InlineImage>,
    Emit: FnMut(UserEvent),
    Now: FnMut() -> Instant,
{
    let events = {
        let mut parser = handles.parser.lock();
        let events = parser.advance(bytes.as_ref());
        handles.kitty_flags.store(parser.kitty_keyboard_flags(), Ordering::Relaxed);
        handles.keyboard_modes.store(parser.keyboard_modes().bits(), Ordering::Relaxed);
        events
    };
    drop(bytes);

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
                    CommandEvent::PromptStart => None,
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
            super::media::trim_staged_inline_images(&mut decoded_images);
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
}

impl App {
    pub(super) fn spawn_pane(
        &self,
        pane_id: u64,
        launch: &super::pane_launch::PaneLaunch,
    ) -> PaneState {
        let (cols, rows) = self.main_renderer().map(|r| r.cells()).unwrap_or((80, 24));
        let (reply_tx, reply_rx) =
            crossbeam_channel::bounded::<Vec<u8>>(super::PTY_REPLY_QUEUE_CAPACITY);
        // Honour the user's configured scrollback depth instead of the
        // Grid's built-in 10k default.
        let mut grid = Grid::new(cols, rows);
        grid.set_scrollback_limit(self.config.terminal.scrollback);
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(grid, reply_tx)));
        // Seed theme defaults so OSC 10/11/12 `?` queries get a truthful
        // reply — without this nvim guesses (27,29,30) for bg and the
        // neo-tree icon cells visibly differ from SonicTerm's clear surface
        // . Also seeds the OSC 4 palette so CLIs like Copilot can read
        // the full colour set and enable their prompt frame.
        {
            let mut p = parser.lock();
            super::seed_parser_theme_colors(&mut p, &self.theme);
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
            Err(e) => {
                tracing::error!("failed to spawn pty: {e}");
                None
            }
        };
        let mut state = PaneState::new(parser, pty);
        state.redraw_target = redraw_target;
        if state.pty.is_some() {
            spawn_pane_workers(
                pane_id,
                &state,
                reply_rx,
                self.pty_burst_gen.clone(),
                self.event_loop_proxy.clone(),
                "sonicterm-vt-reply",
                "sonicterm-vt-loop",
            );
        }
        state
    }
}

impl App {
    pub(super) fn split_active(&mut self, dir: Direction) {
        let Some(ws) = self.main() else {
            // When: `main` is absent, no destination exists for a new pane or PTY.
            return;
        };
        let Some(tab) = ws.tab_states.get(ws.tabs.active_index()) else {
            // When: `tab_states` has no active tab, refuse before creating a speculative PTY.
            return;
        };
        if !tab.tree.leaves().contains(&tab.active_pane) || !ws.panes.contains_key(&tab.active_pane)
        {
            // When: `active_pane` is not a live leaf, preserve topology without spawning another shell.
            return;
        }
        let new_id = next_pane_id();
        let new_pane = self.spawn_pane(new_id, &super::pane_launch::PaneLaunch::default());
        let did_split = {
            let Some(ws) = self.main_mut() else {
                // When: main_mut returns None, there is no active window to split.
                return;
            };
            let i = ws.tabs.active_index();
            let split_ok = {
                let Some(st) = ws.tab_states.get_mut(i) else {
                    // When: tab_states.get_mut cannot find i, there is no active tab tree to split.
                    return;
                };
                let focus = st.active_pane;
                if st.tree.split(focus, dir, new_id) {
                    st.active_pane = new_id;
                    true
                } else {
                    // When: tree.split returns false, keep the existing pane layout and ownership.
                    false
                }
            };
            if split_ok {
                ws.panes.insert(new_id, new_pane);
            }
            split_ok
        };
        if did_split {
            // Own the new pane now rather than on the next 30-second sample:
            // until it has an owner its memory is attributed to nothing, and
            // anything reserving against it has no owner to reserve against.
            self.reconcile_pane_owners();
            self.resize_visible_panes();
            if let Some(r) = self.main_renderer_mut() {
                r.flash_pane_focus(new_id);
            }
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }
    pub(super) fn close_active_pane(&mut self) {
        let outcome = {
            let Some(ws) = self.main_mut() else {
                // When: main_mut returns None, there is no active window pane to close.
                return;
            };
            let i = ws.tabs.active_index();
            let inner = {
                let Some(st) = ws.tab_states.get_mut(i) else {
                    // When: tab_states.get_mut cannot find i, there is no active pane tree to close.
                    return;
                };
                let focus = st.active_pane;
                if matches!(st.tree, PaneTree::Leaf { id, .. } if id == focus) {
                    (Some(i), None)
                } else {
                    // When: matches finds no focused PaneTree::Leaf, close only the split pane.
                    let new_focus =
                        st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
                    if st.tree.close(focus) {
                        // A successful tree close activates its surviving sibling.
                        st.active_pane = new_focus;
                        // Same reason as the exit-driven path: the search was
                        // scanning the grid that just went away.
                        if let Some(search) = st.search.as_mut() {
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
                ws.remove_pane(focus);
            }
            inner
        };
        match outcome {
            (Some(i), _) => self.close_tab_at(i),
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
                    if let Some(r) = self.main_renderer_mut() {
                        r.flash_pane_focus(active_id);
                    }
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
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
            let Some(ws) = self.main_mut() else {
                // When: main_mut returns None, there is no pane zoom state to toggle.
                return;
            };
            let i = ws.tabs.active_index();
            let Some(st) = ws.tab_states.get_mut(i) else {
                // When: tab_states.get_mut cannot find i, there is no active pane tree to zoom.
                return;
            };
            st.tree.toggle_zoom(st.active_pane)
        };
        if toggled {
            self.resize_visible_panes();
            if let Some(w) = self.main_window() {
                w.request_redraw();
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
            let Some(ws) = self.main_mut() else {
                // When: main_mut returns None, there is no active split to resize.
                return;
            };
            let i = ws.tabs.active_index();
            let Some(st) = ws.tab_states.get_mut(i) else {
                // When: tab_states.get_mut cannot find i, there is no active split tree.
                return;
            };
            st.tree.resize_split(st.active_pane, dir, 0.05)
        };
        if resized {
            self.resize_visible_panes();
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }

    pub(super) fn resize_visible_panes(&mut self) {
        let rects = self.compute_active_pane_rects();
        let (cw, ch) = match self.test_viewport_override {
            // The test-only viewport override lets tests exercise
            // close_active_pane's resize wiring
            // without a live wgpu renderer. Production stays `None` and
            // falls through to the renderer-derived metrics below.
            Some((_, cw, ch)) => (cw, ch),
            None => {
                // When: test_viewport_override is None, derive pane metrics from main_renderer.
                match self.main_renderer() {
                    Some(r) => r.cell_size(),
                    None => {
                        // When: test_viewport_override and main_renderer are None, pane metrics are unavailable.
                        return;
                    }
                }
            }
        };
        if let Some(panes) = self.main_panes() {
            let inset = self
                .main_renderer()
                .map(|r| {
                    [
                        r.padding_left_px(),
                        r.padding_right_px(),
                        r.padding_top_px(),
                        r.padding_bottom_px(),
                    ]
                })
                .unwrap_or([0.0; 4]);
            crate::app::resize_panes_to_rects(panes, &rects, cw, ch, inset);
        }
    }
}

#[cfg(test)]
#[path = "spawn_pane_tests.rs"]
mod spawn_pane_tests;
