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
use parking_lot::{Mutex, MutexGuard};
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
use sonicterm_vt::vt::{CommandEvent, MediaEvent, Parser, SyncState, VtEvent};
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

/// Why an OSC 52 request did not write the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Osc52Refusal {
    /// A clipboard read (`?`); reads are never answered.
    Read,
    /// A selection other than the clipboard `c`.
    Selection(char),
    /// The payload decodes past [`MAX_OSC52_CLIPBOARD_BYTES`].
    TooLarge,
    /// Empty, malformed Base64, or text that is not UTF-8.
    Invalid,
}

/// SonicTerm's decision on one OSC 52 request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Osc52Outcome {
    /// Write this decoded text to the clipboard.
    Write(String),
    /// Leave the clipboard alone.
    Refused(Osc52Refusal),
}

/// The OSC 52 requests of one VT batch, counted without their payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClipAttempts {
    /// Requests in the batch.
    pub total: u32,
    /// Requests written to the clipboard.
    pub accepted: u32,
    /// Requests refused.
    pub refused: u32,
    /// The last request's refusal; `None` when it was accepted.
    pub latest: Option<Osc52Refusal>,
}

/// Decide one OSC 52 request. Only target `c` writes are accepted. Queries (`?`), other
/// selections, malformed Base64, non-UTF-8 payloads and decoded output above the hard cap are
/// refused with their reason and can never expose or mutate the native clipboard.
pub(super) fn osc52_decision(selection: char, data: &str) -> Osc52Outcome {
    if data == "?" {
        // When: `data` is a query, the request is a clipboard read.
        return Osc52Outcome::Refused(Osc52Refusal::Read);
    }
    if selection != 'c' {
        // When: `selection` is not clipboard `c`, the selection is unsupported.
        return Osc52Outcome::Refused(Osc52Refusal::Selection(selection));
    }
    if data.is_empty() {
        // When: `data` is empty, there is nothing to write.
        return Osc52Outcome::Refused(Osc52Refusal::Invalid);
    }
    let max_encoded = MAX_OSC52_CLIPBOARD_BYTES.div_ceil(3) * 4;
    if data.len() > max_encoded {
        // When: `data` cannot encode a payload within the decoded cap, reject before allocating the output buffer.
        return Osc52Outcome::Refused(Osc52Refusal::TooLarge);
    }
    let mut decoded = vec![0u8; MAX_OSC52_CLIPBOARD_BYTES];
    let written = match base64::engine::general_purpose::STANDARD
        .decode_slice(data.as_bytes(), &mut decoded)
    {
        Ok(written) => written,
        Err(base64::DecodeSliceError::OutputSliceTooSmall) => {
            // When: OutputSliceTooSmall, the payload decodes past the cap: too large, not malformed.
            return Osc52Outcome::Refused(Osc52Refusal::TooLarge);
        }
        Err(_) => {
            // When: decode_slice returns any other Err, the Base64 is malformed.
            return Osc52Outcome::Refused(Osc52Refusal::Invalid);
        }
    };
    decoded.truncate(written);
    match String::from_utf8(decoded) {
        Ok(text) if !text.is_empty() => Osc52Outcome::Write(text),
        // Non-UTF-8 or empty decoded text is never written.
        _ => Osc52Outcome::Refused(Osc52Refusal::Invalid),
    }
}

/// How the pane's child exited, waiting briefly for it to become observable.
///
/// `Unknown` means the child outlived the wait, the probe failed, or no status
/// was recorded. Callers must not read that as a crash: it decides only that
/// the pane stays open, never that it closes.
pub(super) fn observe_child_exit_status(
    probe: &PtyChildExitProbe,
) -> sonicterm_types::PaneExitStatus {
    use sonicterm_types::PaneExitStatus;
    let deadline = Instant::now() + CHILD_EXIT_OBSERVE_TIMEOUT;
    loop {
        match probe.has_exited() {
            Ok(true) => {
                // When: has_exited returns Ok(true), report the child's recorded exit status.
                return probe.exit_status().unwrap_or(PaneExitStatus::Unknown);
            }
            Ok(false) => {
                // When: has_exited returns Ok(false), keep polling until the observation deadline.
            }
            Err(error) => {
                // When: has_exited returns Err(error), preserve an unknown exit classification.
                tracing::debug!(%error, "failed to observe pane child exit");
                return PaneExitStatus::Unknown;
            }
        }
        if Instant::now() >= deadline {
            // When: Instant::now reaches deadline, stop waiting and preserve an unknown result.
            return PaneExitStatus::Unknown;
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
    let status = observe_child_exit_status(probe);
    let was_clean = clean_flag(&status);
    let _ = proxy.send_event(UserEvent::PaneProcessExited { pane_id, status, was_clean });
}

/// `status` as the older clean flag: `None` only when the status is unknown.
pub(super) fn clean_flag(status: &sonicterm_types::PaneExitStatus) -> Option<bool> {
    match status {
        sonicterm_types::PaneExitStatus::Unknown => None,
        known => Some(known.is_clean()),
    }
}

/// How long one synchronized update (DEC 2026) may hold a pane's output before it is released.
pub(in crate::app) const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_millis(150);

/// The epoch bits a published word carries; epochs compare within this range.
pub(in crate::app) const SYNC_EPOCH_MASK: u64 = (1 << 31) - 1;
/// The deadline word keeps the epoch's low 16 bits above 48 bits of microseconds.
const SYNC_DEADLINE_TAG_SHIFT: u32 = 48;

/// Pack synchronized output as the pane publishes it: the reset count's low 32 bits, the epoch's
/// low 31 bits, then the set bit. One word, so a reader sees a set bit, epoch and resets together.
pub(in crate::app) fn sync_word_of(state: SyncState) -> u64 {
    (published_resets(state.resets) << 32)
        | ((state.epoch & SYNC_EPOCH_MASK) << 1)
        | u64::from(state.set)
}

/// The part of a reset count a published word carries; compare reset counts only through it.
pub(in crate::app) fn published_resets(resets: u64) -> u64 {
    resets & 0xffff_ffff
}

/// Pack the deadline of `epoch` with that epoch's tag, so a reader can tell whose deadline it is.
pub(in crate::app) fn pack_sync_deadline(epoch: u64, deadline: Instant) -> u64 {
    let micros = super::sync_clock::micros_at(deadline) & ((1 << SYNC_DEADLINE_TAG_SHIFT) - 1);
    ((epoch & 0xffff) << SYNC_DEADLINE_TAG_SHIFT) | micros
}

/// A pane's synchronized output as one lock-free read decodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct PublishedSync {
    /// The set bit of the word.
    pub(in crate::app) set: bool,
    /// The word's epoch, modulo 2^31.
    pub(in crate::app) epoch: u64,
    /// The word's reset count, modulo 2^32.
    pub(in crate::app) resets: u64,
    /// The deadline published for this epoch; `None` while the deadline word belongs to another
    /// epoch, so a reader never pairs this word with a neighbouring epoch's deadline.
    pub(in crate::app) deadline: Option<Instant>,
}

/// Decode a pane's synchronized output without the parser lock.
///
/// The deadline is matched to the word by a 16-bit epoch tag, so a reader that loads a word and
/// then a deadline published 65,536 epochs later accepts that later deadline. A later epoch's
/// deadline is never earlier, so this can only extend a hold, which the window's 150 ms cap
/// bounds; the recheck under the parser guard reads both words where the writer cannot run.
// Ordering: word loads Acquire, pairing with its Release store; deadline_word loads Relaxed,
// and its epoch tag rejects any other epoch's deadline.
pub(in crate::app) fn read_published_sync(
    word: &AtomicU64,
    deadline_word: &AtomicU64,
) -> PublishedSync {
    let packed = word.load(Ordering::Acquire);
    let epoch = (packed >> 1) & SYNC_EPOCH_MASK;
    let deadline = deadline_word.load(Ordering::Relaxed);
    let tagged = deadline >> SYNC_DEADLINE_TAG_SHIFT == epoch & 0xffff;
    PublishedSync {
        set: packed & 1 == 1,
        epoch,
        resets: packed >> 32,
        deadline: tagged.then(|| {
            super::sync_clock::instant_at_micros(deadline & ((1 << SYNC_DEADLINE_TAG_SHIFT) - 1))
        }),
    }
}

/// The deadline word a new pane starts with: tagged with its parser's epoch and already passed,
/// so an update already open when the pane is created never holds it.
pub(in crate::app) fn initial_sync_deadline(state: SyncState) -> u64 {
    pack_sync_deadline(state.epoch, super::sync_clock::origin())
}

#[cfg(test)]
thread_local! {
    /// Test-only: run between the deadline store and the word store of a publication on this thread.
    static PUBLISH_PAUSE: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}

/// Test-only: pause this thread's publications between their deadline and word stores.
#[cfg(test)]
pub(in crate::app) fn set_publish_pause(pause: Option<Box<dyn FnMut()>>) {
    PUBLISH_PAUSE.with(|slot| *slot.borrow_mut() = pause);
}

/// The worker's record of synchronized-output resets across the parser sections of a batch.
///
/// Each section only sets `reset_pending`, so a reset in any section of a batch survives later
/// sections that see none; the flush decision clears it after it has sent for the whole batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct SyncLatch {
    /// The parser reset count the worker has already seen.
    pub(in crate::app) seen_resets: u64,
    /// A reset arrived since the last flush decision sent.
    pub(in crate::app) reset_pending: bool,
    /// The parser's full epoch at the last section, read under its lock; never truncated.
    pub(in crate::app) epoch: u64,
    /// Whether an update was open at the last section.
    pub(in crate::app) set: bool,
}

impl SyncLatch {
    /// A latch that has seen every reset and the epoch of the pane's parser as it is now.
    pub(in crate::app) fn for_pane(handles: &PaneVtHandles) -> Self {
        let state = super::frame_counters::lock_parser(&handles.parser).synchronized_output();
        Self { seen_resets: state.resets, reset_pending: false, epoch: state.epoch, set: state.set }
    }

    /// Note the parser's state after one section; a new reset sets `reset_pending`.
    fn note_section(&mut self, state: SyncState) {
        self.epoch = state.epoch;
        self.set = state.set;
        let resets = state.resets;
        if resets > self.seen_resets {
            // A reset happened in this section; it stays pending until the batch is flushed.
            self.reset_pending = true;
            self.seen_resets = resets;
        }
    }
}

/// Pane-owned state shared with its VT worker.
#[derive(Clone)]
pub(in crate::app) struct PaneVtHandles {
    parser: Arc<Mutex<Parser>>,
    redraw_target: Arc<Mutex<Option<WindowId>>>,
    command_events: Arc<Mutex<super::CommandQueue>>,
    cursor_visible: Arc<AtomicBool>,
    keyboard_input: Arc<AtomicU64>,
    pointer_input: Arc<std::sync::atomic::AtomicU8>,
    sync_word: Arc<AtomicU64>,
    sync_deadline_word: Arc<AtomicU64>,
    sync_resets: Arc<AtomicU64>,
    output_generation: Arc<AtomicU64>,
    output_bytes: Arc<AtomicU64>,
    output_counting: Arc<AtomicBool>,
    output_outstanding: Arc<AtomicBool>,
    bells: Arc<AtomicU64>,
    inline_images: Arc<Mutex<Vec<InlineImage>>>,
    inline_media_charge: super::media::SharedInlineMediaCharge,
    frame_counters: Option<super::frame_counters::PaneFrameCounters>,
    /// This worker's echo-watch target and recorded facts, so an armed watch locks its slot once
    /// per token and once per fact.
    echo_cache: std::cell::Cell<super::echo_watch::EchoCache>,
    /// Whether this worker last reported an active `OSC 9;4` progress, so a command end or RIS
    /// sends a clear only when something is shown.
    progress_shown: std::cell::Cell<bool>,
    /// The pane these handles belong to, named in the events the worker sends; 0 until set.
    pane_id: u64,
}

impl PaneVtHandles {
    /// Count `len` received PTY bytes toward the `out` segment, only while it is listed.
    // Ordering: output_counting and output_bytes are Relaxed; the sampler needs an eventual total.
    pub(in crate::app) fn count_output(&self, len: usize) {
        if self.output_counting.load(Ordering::Relaxed) {
            self.output_bytes.fetch_add(len as u64, Ordering::Relaxed);
        }
    }

    /// Clone the exact pane-owned handles that its VT worker must update.
    pub(in crate::app) fn from_pane_state(pane: &PaneState) -> Self {
        Self {
            parser: pane.parser.clone(),
            redraw_target: pane.redraw_target.clone(),
            command_events: pane.command_events.clone(),
            cursor_visible: pane.cursor_visible.clone(),
            keyboard_input: pane.keyboard_input.clone(),
            pointer_input: pane.pointer_input.clone(),
            sync_word: pane.sync_word.clone(),
            sync_deadline_word: pane.sync_deadline_word.clone(),
            sync_resets: pane.sync_resets.clone(),
            output_generation: pane.output_generation.clone(),
            output_bytes: pane.output_bytes.clone(),
            output_counting: pane.output_counting.clone(),
            output_outstanding: pane.output_outstanding.clone(),
            bells: pane.bells.clone(),
            inline_images: pane.inline_images.clone(),
            inline_media_charge: pane.inline_media_charge.clone(),
            frame_counters: pane.frame_counters.clone(),
            echo_cache: std::cell::Cell::new(super::echo_watch::EchoCache::default()),
            progress_shown: std::cell::Cell::new(false),
            pane_id: 0,
        }
    }

    /// These handles, naming `pane_id` in the events the worker sends.
    pub(in crate::app) fn with_pane_id(mut self, pane_id: u64) -> Self {
        self.pane_id = pane_id;
        self
    }
}

/// One batch's program notifications: the first one whole, the rest only counted, so a burst
/// reaches the App as one event and no coalesced text is kept.
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct ProgramNotices {
    first: Option<(Option<String>, String)>,
    more: u32,
}

impl ProgramNotices {
    /// Record one notification; one that sanitizes to no text is neither shown nor counted.
    pub(in crate::app) fn push(&mut self, title: Option<String>, body: String) {
        if !super::program_state::has_visible_text(title.as_deref(), &body) {
            // When: has_visible_text is false, the App would show nothing, so the notification
            // must not take the batch's first place or its alert mark.
            return;
        }
        if self.first.is_some() {
            // A later notification only counts; its text is never kept.
            self.more = self.more.saturating_add(1);
        } else {
            // When: first is None, this is the batch's first notification, kept whole.
            self.first = Some((title, body));
        }
    }

    /// The first notification's title and body and how many followed, if any arrived.
    pub(in crate::app) fn into_parts(self) -> Option<(Option<String>, String, u32)> {
        self.first.map(|(title, body)| (title, body, self.more))
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
    let worker_handles = PaneVtHandles::from_pane_state(pane).with_pane_id(pane_id);
    let redraw_proxy = proxy.clone();

    std::thread::Builder::new()
        .name(vt_thread_name.into())
        .spawn(move || {
            let mut command_started: Option<Instant> = None;
            let mut replies_failed = false;
            let mut flush = OutputFlush::new(pane_id, &worker_handles);
            let send_output = || {
                if let Some(proxy) = redraw_proxy.as_ref() {
                    // redraw_proxy is Some(proxy): send the output event through its current target.
                    send_pane_output(&worker_handles, proxy, pane_id);
                }
            };
            loop {
                match out_rx.recv_timeout(flush.wait(Instant::now())) {
                    Ok(bytes) => {
                        // When: recv_timeout returns Ok(bytes), parse and coalesce the batch.
                        // The chunk's read stamp is read before the chunk moves into the batch.
                        let read_at = bytes.read_at();
                        worker_handles.count_output(bytes.len());
                        flush.receive(bytes.len(), Instant::now());
                        process_pane_vt_batch_and_publish(
                            &worker_handles,
                            bytes,
                            read_at,
                            &mut command_started,
                            &mut flush.sync_latch,
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
                        flush.after_batch(&worker_handles, Instant::now(), send_output);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        // When: recv_timeout returns Timeout, flush any trailing pending redraw.
                        flush.on_quiet(&worker_handles, Instant::now(), send_output);
                        #[cfg(windows)]
                        if exit_probe.has_exited().unwrap_or(false) {
                            // When: exit_probe reports the child exited on Windows, report it and stop polling.
                            report_pane_exit(redraw_proxy.as_ref(), &exit_probe, pane_id);
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        // When: recv_timeout returns Disconnected, flush output and report exit.
                        flush.on_disconnect(send_output);
                        report_pane_exit(redraw_proxy.as_ref(), &exit_probe, pane_id);
                        break;
                    }
                }
            }
        })
        .expect("spawn pane VT loop");
}

/// The VT worker's flush decision: coalesce parsed output and send one output event per flush.
pub(in crate::app) struct OutputFlush {
    /// The pane whose worker owns this decision.
    pane_id: u64,
    /// Parsed output no output event has covered yet.
    pending: bool,
    /// When the oldest uncovered nonempty batch arrived.
    pending_since: Option<Instant>,
    /// Bytes received since the last flush.
    pending_bytes: usize,
    /// Synchronized-output resets seen across the current batch's parser sections.
    pub(in crate::app) sync_latch: SyncLatch,
    /// The open synchronized update holding this pane's output: its epoch and deadline.
    held: Option<(u64, Instant)>,
    /// The newest epoch already released, by its deadline or as inherited; it never holds again.
    released_epoch: u64,
    /// Debug check that interval flushes respect the quiescent spacing.
    redraw_probe: crate::app::invariants::RedrawCoalescerProbe,
}

impl App {
    /// Start the PTY for a new pane: the launch's command run directly, or the shell.
    ///
    /// A command failure starts nothing else. Its sanitized reason is written
    /// into the pane's own grid, logged at `warn`, and posted as a
    /// notification; it never falls back to a shell. Logs name the launch
    /// kind, the program's file name, the argument count and byte lengths,
    /// the cwd source and `hold`, never an argument value, a path or a title.
    pub(super) fn spawn_launch_pty(
        &self,
        pane_id: u64,
        parser: &Arc<Mutex<Parser>>,
        launch: &super::pane_launch::PaneLaunch,
        cols: u16,
        rows: u16,
        shell_opts: &sonicterm_io::pty::ShellSpawnOpts,
    ) -> Result<PtyHandle, ()> {
        let cwd_source = launch.cwd_source.map_or("default", sonicterm_types::CwdSource::as_str);
        let Some(command) = launch.command.as_ref() else {
            // When: `command` is absent, the pane runs the configured shell exactly as before.
            return match PtyHandle::spawn_default_shell(cols, rows, shell_opts.clone()) {
                Ok(pty) => {
                    tracing::info!(
                        target: "sonicterm_app::launch",
                        pane = pane_id,
                        kind = launch.kind(),
                        cwd_source,
                        hold = launch.hold,
                        "pane launched"
                    );
                    Ok(pty)
                }
                Err(error) => {
                    tracing::error!("failed to spawn pty: {error}");
                    Err(())
                }
            };
        };
        let program = command.program_file_name();
        if !launch.executable_still_matches() {
            // When: `executable_still_matches` is false, the opened file was replaced after classification.
            let message = format!("Not run: {program} changed before it could start.");
            tracing::warn!(
                target: "sonicterm_app::launch",
                pane = pane_id,
                kind = launch.kind(),
                reason = "identity_changed",
                "pane command did not start"
            );
            self.report_launch_failure(parser, message);
            return Err(());
        }
        let scheme = launch.scheme().unwrap_or("none");
        let spawn = sonicterm_io::pty::CommandSpawn {
            program: command.program.clone(),
            args: command.args.clone(),
            cwd: launch.cwd.clone().or_else(crate::launch_cli::home_dir).unwrap_or_default(),
        };
        match PtyHandle::spawn_command(&spawn, cols, rows, shell_opts) {
            Ok(pty) => {
                tracing::info!(
                    target: "sonicterm_app::launch",
                    pane = pane_id,
                    kind = launch.kind(),
                    scheme,
                    program = %program,
                    arg_count = command.args.len(),
                    cwd_source,
                    hold = launch.hold,
                    "pane launched"
                );
                let arg_bytes: Vec<usize> = command.args.iter().map(|arg| arg.len()).collect();
                tracing::debug!(
                    target: "sonicterm_app::launch",
                    pane = pane_id,
                    ?arg_bytes,
                    "pane command argument byte lengths"
                );
                Ok(pty)
            }
            Err(error) => {
                // The error's text names only the program's file name and the rule it broke.
                let message = error.to_string();
                tracing::warn!(
                    target: "sonicterm_app::launch",
                    pane = pane_id,
                    program = %program,
                    reason = %message,
                    "pane command did not start"
                );
                self.report_launch_failure(parser, message);
                Err(())
            }
        }
    }

    /// Write a command's failure `message` into its pane's grid and post it as a notification.
    fn report_launch_failure(&self, parser: &Arc<Mutex<Parser>>, message: String) {
        super::pane_exit::write_local_line(parser, &message);
        if let Some(proxy) = self.event_loop_proxy.as_ref() {
            // When: `event_loop_proxy` is set, surface the failure in the notification bubble.
            let _ = proxy.send_event(UserEvent::LaunchFailed { message });
        }
    }

    /// The shell spawn options for a new pane, main or child: the launch's own options, clean
    /// startup under the runtime smoke, and output read timestamps exactly when the pane's frame
    /// counters are on. The gate is fixed by then, so the reader's flag is fixed for its life.
    pub(super) fn pane_spawn_opts(
        &self,
        launch: &super::pane_launch::PaneLaunch,
        frame_counters_on: bool,
    ) -> sonicterm_io::pty::ShellSpawnOpts {
        sonicterm_io::pty::ShellSpawnOpts {
            clean_e2e: self.runtime_smoke.is_some(),
            diagnostic_timestamps: frame_counters_on,
            ..launch.shell_spawn_opts(
                self.config.terminal.term_program.clone(),
                self.config.terminal.shell.clone(),
            )
        }
    }

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
        // The gate is read before the PTY exists, so its reader stamps reads from the first one.
        let frame_counters = self.pane_frame_counters();
        let shell_opts = self.pane_spawn_opts(launch, frame_counters.is_some());
        let pty = match self.spawn_launch_pty(pane_id, &parser, launch, cols, rows, &shell_opts) {
            Ok(pty) => {
                // When: spawn_launch_pty returns Ok(pty), stage any launch draft before the worker starts.
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
            Err(()) => None,
        };
        let mut state = PaneState::new_with_media_pool(parser, pty, &self.inline_media_pool);
        state.hold_on_exit = launch.hold;
        self.reserve_pane_teardown(&mut state);
        let mut frame_counters = frame_counters;
        state.guard_correlation = self.attach_guard_correlation(pane_id, &mut frame_counters);
        state.frame_counters = frame_counters;
        state.redraw_target = redraw_target;
        if state.pty.is_some() {
            spawn_pane_workers(pane_id, &state, self.event_loop_proxy.clone(), "sonicterm-vt-loop");
        }
        state
    }
}

#[cfg(test)]
#[path = "spawn_pane_tests.rs"]
mod spawn_pane_tests;

#[path = "spawn_pane_output.rs"]
mod output;
pub(in crate::app) use output::send_output_redraw;
use output::{publish_sync_output, send_pane_output};
#[path = "spawn_pane_vt.rs"]
mod vt;
pub(in crate::app) use vt::{process_pane_vt_batch_and_publish, publish_pane_vt_batch_with};

#[path = "spawn_pane_commands.rs"]
mod commands;

#[path = "spawn_pane_media.rs"]
mod media_batch;

// Lock order: parser -> echo slot (pre_read, post_read); inline_images -> inline_media_charge after parser releases,
// command_events after both.
// Ordering: cursor_visible, keyboard_input, pointer_input, bells, output_generation load are Relaxed;
// self-contained words, this worker the sole writer.
#[allow(clippy::too_many_arguments)]
fn process_pane_vt_batch_with<Bytes, Decode, Emit, Now, Send>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    read_at: Option<Instant>,
    command_started: &mut Option<Instant>,
    sync_latch: &mut SyncLatch,
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
        // Registered before before_lock and outside the parser-guard scope: a section that unwinds is
        // abandoned after its guard drops. Lock order: a parser guard is never held while a log mutex is taken.
        let registration = counters
            .and_then(|counters| counters.sections.as_deref())
            .and_then(|log| log.register());
        // Gate on: one clock read before lock(), three under the guard (four when a new sync epoch opens);
        // an armed echo watch reads the target cell around the parse and may lock its slot, all before the guard drops.
        let before_lock = counters.map(|_| now());
        let (result, section) = {
            let mut parser = handles.parser.lock();
            let locked_at = before_lock.map(|_| now());
            let echo_before = counters
                .and_then(|counters| counters.echo.pre_read(&handles.echo_cache, parser.grid()));
            let result = parser.advance_with_replies(remaining);
            let parsed_at = before_lock.map(|_| now());
            if let (Some(counters), Some(before), Some(parsed_at)) =
                (counters, echo_before, parsed_at)
            {
                // the watch was armed at the pre-read, so this section's facts are derived now.
                // The worker is output_generation's sole writer, so the batch publishes this load plus one.
                let section = super::echo_watch::SectionFacts {
                    consumed: result.0,
                    parsed_at,
                    generation: handles.output_generation.load(Ordering::Relaxed) + 1,
                    sync_open: parser.synchronized_output().set,
                    // Every section of this batch consumed the same chunk, so each carries its stamp.
                    latest_read_at: read_at,
                };
                counters.echo.post_read(&handles.echo_cache, before, parser.grid(), section);
            }
            handles.keyboard_input.store(parser.keyboard_input_snapshot(), Ordering::Relaxed);
            handles.pointer_input.store(parser.pointer_input_snapshot(), Ordering::Relaxed);
            let sync_state = parser.synchronized_output();
            publish_sync_output(handles, sync_state, &mut now);
            sync_latch.note_section(sync_state);
            let released_at = before_lock.map(|_| now());
            // A fair release hands the parser to a selected parked waiter, such as a frame collection,
            // before this worker can relock. On unwind the guard still drops through ordinary RAII release.
            MutexGuard::unlock_fair(parser);
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
            if let Some(registration) = registration {
                // The section is published with D2.1a's own before_lock and locked_at reads.
                registration.publish(before_lock, locked_at);
            }
        }
        remaining = &remaining[consumed..];

        let mut clipboard_requests = Vec::new();
        let mut command_side_effects = Vec::new();
        let mut media_events = Vec::new();
        // The batch's last progress change, latest wins; `Some(None)` clears.
        let mut progress: Option<Option<sonicterm_vt::vt::ProgressReport>> = None;
        let mut notices = ProgramNotices::default();
        // A batch that rings any number of times is one bell, so a ringing loop marks its tab once.
        if events.iter().any(|event| matches!(event, VtEvent::Bell)) {
            handles.bells.fetch_add(1, Ordering::Relaxed);
        }
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
                            // A finished command leaves no progress bar behind.
                            progress = Some(None);
                            command_started.take().map(|started| at.duration_since(started))
                        }
                        CommandEvent::PromptStart | CommandEvent::PromptEnd => None,
                    };
                    command_side_effects.push(super::PaneCommandEvent { event, at, duration });
                }
                VtEvent::Media(media) => media_events.push(media),
                VtEvent::Progress(report) => progress = Some(report),
                VtEvent::ProgramStateReset => progress = Some(None),
                VtEvent::Notify { title, body } => notices.push(title, body),
                VtEvent::SetTitle(_) | VtEvent::Bell | VtEvent::Hyperlink { .. } => {
                    // When: event is SetTitle, Bell, or Hyperlink, parser state already owns its effect.
                }
            }
        }

        if let Some(report) = progress {
            if report.is_some() || handles.progress_shown.get() {
                // The App hears a set, or a clear of progress this pane showed; other clears are no-ops.
                handles.progress_shown.set(report.is_some());
                emit_event(UserEvent::ProgramProgress { pane_id: handles.pane_id, report });
            }
        }
        if let Some((title, body, more)) = notices.into_parts() {
            // One event per batch: the first notification and how many followed it.
            emit_event(UserEvent::ProgramNotify { pane_id: handles.pane_id, title, body, more });
        }

        // Every accepted write is its own event, in order; refusals only count toward one summary.
        let mut attempts = ClipAttempts::default();
        for (selection, data) in clipboard_requests {
            attempts.total = attempts.total.saturating_add(1);
            match osc52_decision(selection, &data) {
                Osc52Outcome::Write(text) => {
                    attempts.accepted = attempts.accepted.saturating_add(1);
                    attempts.latest = None;
                    emit_event(UserEvent::ClipboardWrite { pane_id: handles.pane_id, text });
                }
                Osc52Outcome::Refused(reason) => {
                    attempts.refused = attempts.refused.saturating_add(1);
                    attempts.latest = Some(reason);
                }
            }
        }
        if attempts.total > 0 {
            emit_event(UserEvent::ClipboardAttempts { pane_id: handles.pane_id, attempts });
        }

        let kitty_replies =
            media_batch::apply_batch_media(handles, media_events, &mut decode_media);

        if !command_side_effects.is_empty() {
            super::append_bounded_command_events(
                &mut handles.command_events.lock(),
                command_side_effects,
            );
        }
        if reply_batch.len() + replies.len() + kitty_replies.len() > 32 * 1024 {
            // Flush before appending a complete dispatch that would exceed the batch bound; never split a reply.
            send_reply(std::mem::take(&mut reply_batch));
        }
        reply_batch.extend(replies);
        // A kitty reply ends its advance prefix, so it follows every VT reply of that prefix.
        reply_batch.extend(kitty_replies);
        if remaining.is_empty() {
            // When: remaining is empty, publish trailing replies without waiting for more child output.
            if !reply_batch.is_empty() {
                send_reply(reply_batch);
            }
            break;
        }
    }
}
