pub(super) use sonicterm_grid::grid::Grid;
pub(super) use sonicterm_ui::pane::Rect;
pub(super) use sonicterm_vt::vt::{CaptureStagingPool, MediaProtocol};

use super::*;

pub(super) fn app_with_unavailable_shell() -> App {
    // A child path below the test executable cannot launch a shell or read user shell profiles.
    let shell = std::env::current_exe().expect("test executable").join("unavailable-shell");
    let config = Config {
        terminal: sonicterm_cfg::config::TerminalConfig {
            shell: Some(shell.to_string_lossy().into_owned()),
            ..Default::default()
        },
        ..Config::default()
    };
    App::new(Theme::default(), config, Keymap::default())
}

/// A pane and its worker handles that stage captures and trim media in private
/// pools, so a batch that opens a capture is admitted whatever sibling tests hold.
pub(super) fn pane_and_worker_handles() -> (PaneState, PaneVtHandles) {
    let parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new());
    let pane = PaneState::new_with_media_pool(
        Arc::new(Mutex::new(parser)),
        None,
        &crate::app::media::InlineMediaPool::new(),
    );
    let worker = PaneVtHandles::from_pane_state(&pane);
    (pane, worker)
}

/// A pane worker whose pane counts into `stats`.
pub(super) fn counting_worker_handles(
    stats: &Arc<crate::app::frame_counters::VtFrameStats>,
) -> (PaneState, PaneVtHandles) {
    let parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new());
    let mut pane = PaneState::new_with_media_pool(
        Arc::new(Mutex::new(parser)),
        None,
        &crate::app::media::InlineMediaPool::new(),
    );
    pane.frame_counters =
        Some(crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(stats)));
    let worker = PaneVtHandles::from_pane_state(&pane);
    (pane, worker)
}

/// The pane's published synchronized-output word, decoded deadline and reset count.
pub(super) fn published_sync(handles: &PaneVtHandles) -> (u64, Option<Instant>, u64) {
    (
        handles.sync_word.load(Ordering::Acquire),
        read_published_sync(&handles.sync_word, &handles.sync_deadline_word).deadline,
        handles.sync_resets.load(Ordering::Relaxed),
    )
}

/// Run one batch as the VT worker does: note it, parse and publish it through the production
/// publisher with the worker's latch, then take the flush decision. One instant, `at`, is both the
/// publication's clock (the deadline) and the decision's. Each send records the
/// output generation it observed, so a test can tell whether the batch was published first.
pub(super) fn worker_step(
    handles: &PaneVtHandles,
    flush: &mut OutputFlush,
    bytes: &[u8],
    at: Instant,
    sends: &mut Vec<u64>,
) {
    flush.receive(bytes.len(), at);
    publish_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut flush.sync_latch,
        |_| None,
        |_| {},
        || at,
        |_| {},
    );
    flush
        .after_batch(handles, at, || sends.push(handles.output_generation.load(Ordering::Acquire)));
}

/// The instant the pane's published synchronized-output deadline names.
pub(super) fn published_deadline(handles: &PaneVtHandles) -> Instant {
    read_published_sync(&handles.sync_word, &handles.sync_deadline_word)
        .deadline
        .expect("the deadline belongs to the published epoch")
}

/// One captured tracing event: its target, message and integer fields.
#[derive(Clone, Debug, Default)]
pub(super) struct SyncLogEvent {
    pub(super) target: String,
    pub(super) message: String,
    pub(super) fields: Vec<(String, u64)>,
}

impl tracing::field::Visit for SyncLogEvent {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.push((field.name().to_owned(), value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

/// A layer that keeps every event it sees.
#[derive(Clone, Default)]
pub(super) struct SyncLogCapture(Arc<Mutex<Vec<SyncLogEvent>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SyncLogCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut captured =
            SyncLogEvent { target: event.metadata().target().to_owned(), ..Default::default() };
        event.record(&mut captured);
        self.0.lock().push(captured);
    }
}

/// Run `body` under a capturing subscriber and return what it logged.
pub(super) fn capture_sync_log(body: impl FnOnce()) -> Vec<SyncLogEvent> {
    use tracing_subscriber::layer::SubscriberExt;
    let capture = SyncLogCapture::default();
    let subscriber = tracing_subscriber::Registry::default().with(capture.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let events = capture.0.lock().clone();
    events
}

/// The parser sections a counting worker has timed so far.
pub(super) fn sections_parsed(stats: &crate::app::frame_counters::VtFrameStats) -> u64 {
    stats.parse.snapshot().count()
}

/// The text of the `fn name` item in `source`, through its closing brace at the same indent.
pub(super) fn item_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source.find(signature).unwrap_or_else(|| panic!("{signature} not found"));
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let indent = &source[line_start..start];
    let closing = format!("\n{indent}}}\n");
    let end = source[start..].find(&closing).expect("item end") + start;
    &source[start..end]
}

/// The deadline publication stores for a batch parsed at `at`: the bound, at microsecond precision.
pub(super) fn stored_deadline(at: Instant) -> Instant {
    crate::app::sync_clock::instant_at_micros(crate::app::sync_clock::micros_at(
        at + SYNC_OUTPUT_TIMEOUT,
    ))
}

/// A test timeline's start: a whole number of microseconds after the deadline origin, so a
/// published deadline is exactly its batch's instant plus the bound.
pub(super) fn test_base() -> Instant {
    crate::app::sync_clock::origin() + Duration::from_secs(5)
}

/// A counting worker over a `cols` x `rows` grid keeping `scrollback` history rows, targeted at
/// window 7, with its pane's echo watch.
pub(super) fn echo_worker(
    cols: u16,
    rows: u16,
    scrollback: usize,
) -> (PaneState, PaneVtHandles, Arc<crate::app::echo_watch::EchoWatch>) {
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let mut grid = Grid::new(cols, rows);
    grid.set_scrollback_limit(scrollback);
    let parser = Parser::new_with_staging_pool(grid, None, CaptureStagingPool::new());
    let mut pane = PaneState::new_with_media_pool(
        Arc::new(Mutex::new(parser)),
        None,
        &crate::app::media::InlineMediaPool::new(),
    );
    pane.frame_counters = Some(crate::app::frame_counters::PaneFrameCounters::new(stats));
    *pane.redraw_target.lock() = Some(echo_window());
    let handles = PaneVtHandles::from_pane_state(&pane);
    let watch = Arc::clone(&handles.frame_counters.as_ref().expect("counting").echo);
    (pane, handles, watch)
}

/// The window every echo worker targets.
pub(super) fn echo_window() -> WindowId {
    WindowId::from(7)
}

/// Arm `watch` with `token` for `character` at (`abs_row`, `col`) under the grid's identity now.
pub(super) fn arm_echo(
    watch: &crate::app::echo_watch::EchoWatch,
    handles: &PaneVtHandles,
    token: u64,
    (abs_row, col, character): (u64, u16, char),
    armed_at: Instant,
) {
    let identity = crate::app::echo_watch::EchoRowIdentity::of(handles.parser.lock().grid());
    let target = crate::app::echo_watch::EchoWatchTarget { abs_row, col, character, identity };
    watch.arm(crate::app::echo_watch::ArmToken::for_test(token), target, armed_at);
}

/// Parse and publish one batch with every clock read at `at`.
pub(super) fn echo_batch(handles: &PaneVtHandles, bytes: &[u8], at: Instant) {
    publish_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        || at,
        |_| {},
    );
}

/// Flush the worker's output with every new clock read at `at`, delivering through `accept`;
/// returns the windows it sent to.
pub(super) fn echo_flush(handles: &PaneVtHandles, at: Instant, accept: bool) -> Vec<WindowId> {
    let mut sent = Vec::new();
    send_output_redraw(
        &handles.redraw_target,
        &handles.output_outstanding,
        handles.frame_counters.as_ref(),
        || at,
        |window| {
            sent.push(window);
            accept
        },
    );
    sent
}

/// The watch's record, which every armed test has.
pub(super) fn echo_trace(
    watch: &crate::app::echo_watch::EchoWatch,
) -> crate::app::echo_watch::EchoTrace {
    watch.peek().expect("an armed record")
}

/// One worker step through the production flush decision, delivering through `send_output_redraw`
/// with every clock read at `at`.
pub(super) fn echo_worker_step(
    handles: &PaneVtHandles,
    flush: &mut OutputFlush,
    bytes: &[u8],
    at: Instant,
    sends: &mut Vec<WindowId>,
) {
    flush.receive(bytes.len(), at);
    publish_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut flush.sync_latch,
        |_| None,
        |_| {},
        || at,
        |_| {},
    );
    flush.after_batch(handles, at, || sends.extend(echo_flush(handles, at, true)));
}

/// Arm `watch` as a timeline owner would, with token `token`, for `X` at row 0, column 2.
pub(super) fn arm_echo_timeline(
    watch: &crate::app::echo_watch::EchoWatch,
    handles: &PaneVtHandles,
    token: u64,
    armed_at: Instant,
) {
    let identity = crate::app::echo_watch::EchoRowIdentity::of(handles.parser.lock().grid());
    let target =
        crate::app::echo_watch::EchoWatchTarget { abs_row: 0, col: 2, character: 'X', identity };
    let timeline = crate::app::echo_timeline::SlotTimeline::new(
        true,
        crate::app::echo_timeline::InitialPermit::Absent,
    );
    watch.arm_with(
        crate::app::echo_watch::ArmToken::for_test(token),
        target,
        armed_at,
        Some(timeline),
    );
}

/// Parse and publish one batch whose chunk was read at `read_at`, every parse clock read at `at`.
pub(super) fn echo_batch_read(
    handles: &PaneVtHandles,
    bytes: &[u8],
    read_at: Option<Instant>,
    at: Instant,
) {
    publish_pane_vt_batch_with(
        handles,
        bytes,
        read_at,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        || at,
        |_| {},
    );
}

/// Freeze and transfer token `token`'s timeline from `watch` as the owner's take would.
pub(super) fn echo_timeline_of(
    watch: &crate::app::echo_watch::EchoWatch,
    token: u64,
) -> crate::app::EchoTimelineV1 {
    let token = crate::app::echo_watch::ArmToken::for_test(token);
    watch.take_with(token, Some(crate::app::echo_timeline::FrozenFlood::empty()));
    match watch.transfer_timeline(token) {
        crate::app::EchoTimelineTakeV1::Timeline(timeline) => timeline,
        other => panic!("timeline transfer returned {other:?}"),
    }
}

/// The recorded `LatestChunkRead` times, in record order.
pub(super) fn chunk_reads(timeline: &crate::app::EchoTimelineV1) -> Vec<u64> {
    timeline
        .events
        .iter()
        .filter(|event| event.kind == crate::app::EchoTimelineKindV1::LatestChunkRead)
        .map(|event| event.at_ns)
        .collect()
}

/// Worker handles whose counters carry a section log, and that log.
pub(super) fn counting_worker_with_log(
) -> (PaneState, PaneVtHandles, Arc<crate::app::guard_correlation::PaneSectionLog>) {
    let (mut pane, _) = pane_and_worker_handles();
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let mut counters = crate::app::frame_counters::PaneFrameCounters::new(stats);
    let log = Arc::new(crate::app::guard_correlation::PaneSectionLog::new(11));
    counters.sections = Some(Arc::clone(&log));
    pane.frame_counters = Some(counters);
    let handles = PaneVtHandles::from_pane_state(&pane);
    (pane, handles, log)
}

/// Run one VT batch of `bytes` through pane 7's worker handles and return what it emitted.
pub(super) fn osc52_batch(bytes: &[u8]) -> Vec<UserEvent> {
    let (_pane, handles) = pane_and_worker_handles();
    let handles = handles.with_pane_id(7);
    let mut emitted = Vec::new();
    process_pane_vt_batch_with(
        &handles,
        bytes,
        None,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |event| emitted.push(event),
        Instant::now,
        |_| {},
    );
    emitted
}

/// The events one batch of `bytes` emits through `handles`.
pub(super) fn program_batch(handles: &PaneVtHandles, bytes: &str) -> Vec<UserEvent> {
    let mut emitted = Vec::new();
    process_pane_vt_batch_with(
        handles,
        bytes.as_bytes(),
        None,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |event| emitted.push(event),
        Instant::now,
        |_| {},
    );
    emitted
}

/// The progress reports `events` carry, in order.
pub(super) fn progress_reports(
    events: &[UserEvent],
) -> Vec<Option<sonicterm_vt::vt::ProgressReport>> {
    events
        .iter()
        .filter_map(|event| match event {
            UserEvent::ProgramProgress { report, .. } => Some(*report),
            _ => None,
        })
        .collect()
}

/// Names the test whose re-run, as a ConPTY child, writes program-state OSCs instead of testing.
#[cfg(windows)]
pub(super) const PROGRAM_OSC_CHILD: &str = "SONICTERM_PROGRAM_OSC_CHILD";

/// Turn VT output on for this process's console, so the OSC bytes reach ConPTY as sequences.
#[cfg(windows)]
pub(super) fn enable_vt_output() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{
        GetConsoleMode, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };
    let handle = HANDLE(std::io::stdout().as_raw_handle());
    let mut mode = CONSOLE_MODE(0);
    let read =
        // SAFETY: `handle` is this process's live stdout console handle and `mode` outlives the call.
        unsafe { GetConsoleMode(handle, &mut mode) };
    if read.is_ok() {
        let _ =
            // SAFETY: the same live handle; a failure leaves the console mode as it was.
            unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) };
    }
}

/// Write an `OSC 9;4` set, an OSC 9 and an OSC 777 notification, then the clear, 300 ms apart so
/// each change is its own read.
#[cfg(windows)]
pub(super) fn write_program_oscs() {
    use std::io::Write;
    #[cfg(windows)]
    enable_vt_output();
    let mut stdout = std::io::stdout().lock();
    let writes: [&[u8]; 3] = [
        b"\x1b]9;4;1;40\x07",
        b"\x1b]9;build done\x07\x1b]777;notify;CI;green\x07",
        b"\x1b]9;4;0\x07",
    ];
    for bytes in writes {
        let _ = stdout.write_all(bytes);
        let _ = stdout.flush();
        std::thread::sleep(Duration::from_millis(300));
    }
}
