use super::*;
use crate::app::osc52_read_source::{ClipboardTextSource, ReadLocation, ReadOutcome};
use crate::app::osc52_read_ticket::{CancelReason, ClipboardTicketHandle};
use crate::app::osc52_stream::{ClipState, ClipboardStaging, Osc52Intake};
use crate::app::App;
use sonicterm_cfg::config::{Config, Osc52ReadPolicy};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::{SensitiveReply, SensitiveReplyError};
use sonicterm_vt::vt::{Parser, VtEvent};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// A clipboard holding `text`, counting native reads; a gated one blocks each read until released.
struct FakeSource {
    text: String,
    gate: Option<Mutex<crossbeam_channel::Receiver<()>>>,
    calls: AtomicUsize,
}

impl FakeSource {
    fn new(text: &str) -> Arc<Self> {
        Arc::new(Self { text: text.into(), gate: None, calls: AtomicUsize::new(0) })
    }

    /// A source whose reads block, like a wedged native call, until the returned sender sends.
    fn gated(text: &str) -> (crossbeam_channel::Sender<()>, Arc<Self>) {
        let (release, gate) = crossbeam_channel::unbounded();
        let source =
            Self { text: text.into(), gate: Some(Mutex::new(gate)), calls: AtomicUsize::new(0) };
        (release, Arc::new(source))
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ClipboardTextSource for FakeSource {
    fn read_text_bounded(&self, _location: ReadLocation, max_bytes: usize) -> ReadOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let _ = gate.lock().unwrap().recv();
        }
        match self.text.len() {
            0 => ReadOutcome::Empty,
            length if length > max_bytes => ReadOutcome::TooLarge,
            _ => ReadOutcome::Text(self.text.clone()),
        }
    }
}

/// An App reading through a fake source under `policy`, delivering to a fake writer that keeps
/// each reply (and so its custody) until the test drops it, or hands it back while `contended`.
struct Harness {
    app: App,
    pane: u64,
    staging: Arc<ClipboardStaging>,
    delivered: Arc<Mutex<Vec<(u64, SensitiveReply)>>>,
    port_calls: Arc<AtomicUsize>,
    contended: Arc<AtomicBool>,
}

fn harness(policy: Osc52ReadPolicy, source: Arc<FakeSource>) -> Harness {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    // A private staging keeps the gate and policy off the process default sibling tests share.
    let staging = ClipboardStaging::with_budget(1 << 20, 1 << 20);
    app.clip_state = ClipState::new(staging.clone());
    set_policy(&mut app, policy);
    app.clip_state.reads.source = source;
    let delivered = Arc::new(Mutex::new(Vec::new()));
    let port_calls = Arc::new(AtomicUsize::new(0));
    let contended = Arc::new(AtomicBool::new(false));
    let (sink, calls, busy) = (delivered.clone(), port_calls.clone(), contended.clone());
    app.clip_state.reads.port = Some(Box::new(move |pane_id, reply| {
        calls.fetch_add(1, Ordering::SeqCst);
        if busy.load(Ordering::SeqCst) {
            return Err(SensitiveReplyError::Contended(reply));
        }
        sink.lock().unwrap().push((pane_id, reply));
        Ok(())
    }));
    let pane = app.__test_seed_tab("reads");
    Harness { app, pane, staging, delivered, port_calls, contended }
}

/// Reload `[terminal] osc52_read` through the production config path.
fn set_policy(app: &mut App, policy: Osc52ReadPolicy) {
    let mut config = Config::default();
    config.terminal.osc52_read = policy;
    app.apply_new_config(config);
}

impl Harness {
    fn take_delivered(&self) -> Vec<(u64, SensitiveReply)> {
        std::mem::take(&mut *self.delivered.lock().unwrap())
    }

    fn delivered_count(&self) -> usize {
        self.delivered.lock().unwrap().len()
    }

    fn budget_bytes(&self) -> usize {
        self.app.clip_state.reads.budget.reserved_bytes()
    }

    fn pump_until(&mut self, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(self) {
            assert!(Instant::now() < deadline, "the read pipeline did not settle");
            self.app.pump_clipboard_reads();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Run `bytes` from `pane` through a real parser and the pane's intake, post its tickets to the
/// App as the VT worker would, and return the replies the worker answered at once.
fn request_on(harness: &mut Harness, pane: u64, bytes: &[u8]) -> Vec<u8> {
    let identity = harness.app.pane_by_id(pane).expect("a live pane").read_identity.clone();
    let mut intake = Osc52Intake::new(harness.staging.clone()).with_identity(identity);
    intake.bind_pane(pane);
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = parser.advance(bytes);
    let inbox = parser.clipboard_inbox().clone();
    for event in events {
        if let VtEvent::ClipboardStream { stream, phase } = event {
            let _ = intake.handle(&inbox, stream, phase, Instant::now());
        }
    }
    for ticket in intake.take_tickets() {
        harness.app.admit_clipboard_ticket(ClipboardTicketHandle::new(ticket));
    }
    intake.take_read_replies()
}

fn request(harness: &mut Harness, bytes: &[u8]) -> Vec<u8> {
    let pane = harness.pane;
    request_on(harness, pane, bytes)
}

/// Contract: under `allow` a read delivers the clipboard's exact bytes to the source pane, ST
/// terminated for both BEL and ST requests; delivery releases the slot and budget once written.
#[test]
fn allow_read_delivers_exact_bytes_with_st() {
    for bytes in [&b"\x1b]52;c;?\x07"[..], b"\x1b]52;c;?\x1b\\"] {
        let mut harness = harness(Osc52ReadPolicy::Allow, FakeSource::new("hello"));
        assert!(request(&mut harness, bytes).is_empty(), "a ticket, not an immediate reply");
        harness.pump_until(|harness| harness.delivered_count() == 1);
        let delivered = harness.take_delivered();
        assert_eq!(delivered[0].0, harness.pane);
        assert_eq!(delivered[0].1.bytes(), b"\x1b]52;c;aGVsbG8=\x1b\\");
        drop(delivered);
        assert_eq!(harness.staging.ticket_slots_in_use(), 0);
        assert_eq!(harness.budget_bytes(), 0);
    }
}

/// Contract: `cp` is one writer message, clipboard reply then PRIMARY, whose custody holds the
/// pane's slot and its budget lease until the writer drops it after writing.
#[test]
fn cp_replies_one_message_within_budget() {
    let mut harness = harness(Osc52ReadPolicy::Allow, FakeSource::new("hi"));
    request(&mut harness, b"\x1b]52;cp;?\x07");
    harness.pump_until(|harness| harness.delivered_count() == 1);
    let delivered = harness.take_delivered();
    let primary = if cfg!(target_os = "linux") { "aGk=" } else { "" };
    let expected = format!("\x1b]52;c;aGk=\x1b\\\x1b]52;p;{primary}\x1b\\");
    assert_eq!(delivered[0].1.bytes(), expected.as_bytes());
    assert_eq!(harness.staging.ticket_slots_in_use(), 1, "the writer holds the ticket");
    assert!(harness.budget_bytes() >= delivered[0].1.len(), "and the lease covering the reply");
    drop(delivered);
    assert_eq!((harness.staging.ticket_slots_in_use(), harness.budget_bytes()), (0, 0));
}

/// Contract: under `ask` a ticket whose window is not frontmost gets no page, waits and reads
/// nothing; at its 120 s expiry it is answered as a rejected prompt is, with an empty reply.
#[test]
fn ask_waits_then_expiry_replies_empty() {
    let source = FakeSource::new("secret");
    let mut harness = harness(Osc52ReadPolicy::Ask, source.clone());
    request(&mut harness, b"\x1b]52;c;?\x07");
    harness.app.pump_clipboard_reads();
    assert_eq!(harness.delivered_count(), 0, "a waiting ticket answers nothing yet");
    assert_eq!(harness.staging.ticket_slots_in_use(), 1);
    let expired = Instant::now() + TICKET_LIFETIME + Duration::from_secs(1);
    harness.app.pump_clipboard_reads_at(expired);
    let delivered = harness.take_delivered();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].1.bytes(), b"\x1b]52;c;\x1b\\");
    assert_eq!(source.calls(), 0, "the clipboard was never read");
    drop(delivered);
    assert_eq!(harness.staging.ticket_slots_in_use(), 0);
}

/// What revokes a waiting ticket in [`each_cancellation_trigger_cancels_silently`].
#[derive(Debug, Clone, Copy)]
enum Trigger {
    PaneClosed,
    TransferAwayAndBack,
    CommandChanged,
    PtyIncarnation,
    ReadOnly,
    Policy,
}

/// Apply `trigger` to the harness's pane; returns the reason revalidation must report.
fn fire(harness: &mut Harness, trigger: Trigger) -> CancelReason {
    let main = harness.app.registry.main_window_id.expect("a main window");
    let pane = harness.pane;
    match trigger {
        Trigger::PaneClosed => {
            assert!(harness.app.__test_detach_pane(main, pane));
            CancelReason::PaneClosed
        }
        Trigger::TransferAwayAndBack => {
            let window = harness.app.registry.windows.get_mut(&main).unwrap();
            let detached = window.remove_pane(pane).expect("the pane is held");
            assert!(window.panes.attach(detached).is_ok(), "the pane returns");
            CancelReason::OwnerChanged
        }
        Trigger::CommandChanged => {
            let identity = harness.app.pane_by_id(pane).unwrap().read_identity.clone();
            identity.set_command_epoch(identity.command_epoch() + 1);
            CancelReason::CommandChanged
        }
        // Fired before the request: the ticket binds this incarnation, the pane's PTY is another.
        Trigger::PtyIncarnation => CancelReason::PtyChanged,
        Trigger::ReadOnly => {
            let window = harness.app.registry.windows.get_mut(&main).unwrap();
            window.copy_mode = Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at((0, 0)));
            CancelReason::ReadOnly
        }
        Trigger::Policy => {
            set_policy(&mut harness.app, Osc52ReadPolicy::Allow);
            CancelReason::PolicyChanged
        }
    }
}

/// Contract: pane close, transfer away and back, a command boundary, a different PTY, READONLY
/// and any policy change each cancel a ticket silently: no reply, and its slot is freed.
#[test]
fn each_cancellation_trigger_cancels_silently() {
    use Trigger::*;
    for trigger in
        [PaneClosed, TransferAwayAndBack, CommandChanged, PtyIncarnation, ReadOnly, Policy]
    {
        let mut harness = harness(Osc52ReadPolicy::Ask, FakeSource::new("secret"));
        if matches!(trigger, PtyIncarnation) {
            let identity = harness.app.pane_by_id(harness.pane).unwrap().read_identity.clone();
            identity.set_incarnation(5);
        }
        assert!(request(&mut harness, b"\x1b]52;c;?\x07").is_empty(), "{trigger:?}");
        let reason = fire(&mut harness, trigger);
        harness.app.pump_clipboard_reads();
        assert_eq!(harness.delivered_count(), 0, "{trigger:?} answers nothing");
        assert_eq!(harness.staging.ticket_slots_in_use(), 0, "{trigger:?} frees the slot");
        assert_eq!(harness.app.clip_state.reads.last_cancel, Some(reason), "{trigger:?}");
    }
}

/// Contract: a policy change or pane close while the native read runs is caught by revalidation
/// before delivery: nothing reaches the writer, and the slot and lease are released.
#[test]
fn policy_change_between_worker_read_and_retry_sends_nothing() {
    for trigger in [Trigger::Policy, Trigger::PaneClosed] {
        let (release, source) = FakeSource::gated("secret");
        let mut harness = harness(Osc52ReadPolicy::Allow, source.clone());
        request(&mut harness, b"\x1b]52;c;?\x07");
        harness.pump_until(|_| source.calls() == 1);
        let reason = match trigger {
            Trigger::Policy => {
                set_policy(&mut harness.app, Osc52ReadPolicy::Ask);
                CancelReason::PolicyChanged
            }
            _ => fire(&mut harness, trigger),
        };
        release.send(()).unwrap();
        harness.pump_until(|harness| harness.app.clip_state.reads.in_flight.is_none());
        assert_eq!(harness.port_calls.load(Ordering::SeqCst), 0, "{trigger:?}");
        assert_eq!((harness.staging.ticket_slots_in_use(), harness.budget_bytes()), (0, 0));
        assert_eq!(harness.app.clip_state.reads.last_cancel, Some(reason), "{trigger:?}");
    }
}

/// Contract: a reader past its deadline is answered empty, but its ticket slot and budget lease
/// stay held until the native call returns; meanwhile other reads fail fast without a native call.
#[test]
fn wedged_reader_holds_slot_and_lease_until_return() {
    let (release, source) = FakeSource::gated("secret");
    let mut harness = harness(Osc52ReadPolicy::Allow, source.clone());
    harness.app.clip_state.reads.reader_deadline = Duration::from_millis(50);
    let second = harness.app.__test_seed_tab("second");
    request(&mut harness, b"\x1b]52;c;?\x07");
    harness.pump_until(|_| source.calls() == 1);
    harness.app.pump_clipboard_reads_at(Instant::now() + Duration::from_secs(1));
    let delivered = harness.take_delivered();
    assert_eq!(delivered.len(), 1);
    assert_eq!((delivered[0].0, delivered[0].1.bytes()), (harness.pane, &b"\x1b]52;c;\x1b\\"[..]));
    drop(delivered);
    assert_eq!(harness.staging.ticket_slots_in_use(), 1, "the wedged reader keeps its ticket");
    assert!(harness.budget_bytes() > 0, "and its lease");
    request_on(&mut harness, second, b"\x1b]52;c;?\x07");
    harness.app.pump_clipboard_reads();
    let delivered = harness.take_delivered();
    assert_eq!((delivered[0].0, delivered[0].1.bytes()), (second, &b"\x1b]52;c;\x1b\\"[..]));
    assert_eq!(source.calls(), 1, "no second native read while one is wedged");
    drop(delivered);
    release.send(()).unwrap();
    harness.pump_until(|harness| harness.app.clip_state.reads.in_flight.is_none());
    assert_eq!(harness.delivered_count(), 0, "the late result is dropped");
    assert_eq!((harness.staging.ticket_slots_in_use(), harness.budget_bytes()), (0, 0));
}

/// Contract: a reader whose native call returns after its deadline never delivers its data, even
/// when its result is drained before the deadline branch runs: the read is answered empty, as an
/// overdue read is, and its slot and lease are released.
#[test]
fn reader_completed_after_deadline_never_delivered() {
    let (release, source) = FakeSource::gated("secret");
    let mut harness = harness(Osc52ReadPolicy::Allow, source.clone());
    harness.app.clip_state.reads.reader_deadline = Duration::from_millis(50);
    request(&mut harness, b"\x1b]52;c;?\x07");
    harness.pump_until(|_| source.calls() == 1);
    // The deadline passes with no pump, then the native call returns.
    std::thread::sleep(Duration::from_millis(100));
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.app.clip_state.reads.done_rx.is_empty() {
        assert!(Instant::now() < deadline, "the reader did not return");
        std::thread::sleep(Duration::from_millis(2));
    }
    harness.app.pump_clipboard_reads();
    let delivered = harness.take_delivered();
    let bytes: Vec<&[u8]> = delivered.iter().map(|(_, reply)| reply.bytes()).collect();
    assert_eq!(bytes, vec![&b"\x1b]52;c;\x1b\\"[..]], "answered empty, never the late text");
    drop(delivered);
    assert_eq!((harness.staging.ticket_slots_in_use(), harness.budget_bytes()), (0, 0));
}

/// Contract: a contended writer admission is retried once per turn, revalidating each time, at
/// most `MAX_DELIVERY_RETRIES` times, then dropped; a policy change between attempts stops it.
#[test]
fn admission_contention_retries_with_revalidation_then_drops() {
    let mut harness = harness(Osc52ReadPolicy::Allow, FakeSource::new("secret"));
    harness.contended.store(true, Ordering::SeqCst);
    request(&mut harness, b"\x1b]52;c;?\x07");
    harness.pump_until(|harness| harness.port_calls.load(Ordering::SeqCst) == 1);
    for _ in 0..6 {
        harness.app.pump_clipboard_reads();
    }
    assert_eq!(harness.port_calls.load(Ordering::SeqCst), 1 + MAX_DELIVERY_RETRIES as usize);
    assert_eq!((harness.staging.ticket_slots_in_use(), harness.budget_bytes()), (0, 0));

    let mut harness = self::harness(Osc52ReadPolicy::Allow, FakeSource::new("secret"));
    harness.contended.store(true, Ordering::SeqCst);
    request(&mut harness, b"\x1b]52;c;?\x07");
    harness.pump_until(|harness| harness.port_calls.load(Ordering::SeqCst) == 1);
    set_policy(&mut harness.app, Osc52ReadPolicy::Ask);
    for _ in 0..3 {
        harness.app.pump_clipboard_reads();
    }
    assert_eq!(harness.port_calls.load(Ordering::SeqCst), 1, "no attempt after the policy flip");
    assert_eq!(harness.staging.ticket_slots_in_use(), 0);
    assert_eq!(harness.app.clip_state.reads.last_cancel, Some(CancelReason::PolicyChanged));
}

/// A log sink the capture subscriber writes into.
#[derive(Clone, Default)]
struct LogSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Contract: no log line at any level carries clipboard text or its Base64, through a delivered
/// read and a read dropped after contended retries; only lengths and outcomes are logged.
#[test]
fn logs_never_contain_clipboard_text() {
    let sink = LogSink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter("trace")
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, || {
        let mut harness = harness(Osc52ReadPolicy::Allow, FakeSource::new("secret-clip"));
        request(&mut harness, b"\x1b]52;c;?\x07");
        harness.pump_until(|harness| harness.delivered_count() == 1);
        harness.contended.store(true, Ordering::SeqCst);
        harness.take_delivered();
        request(&mut harness, b"\x1b]52;c;?\x07");
        for _ in 0..200 {
            harness.app.pump_clipboard_reads();
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let logs = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains("secret-clip"), "{logs}");
    assert!(!logs.contains("c2VjcmV0LWNsaXA"), "{logs}");
}

#[cfg(test)]
#[path = "osc52_read_pipeline_tests_consent_tests.rs"]
mod osc52_read_pipeline_tests_consent_tests;

/// Admit an OSC 5522 read of `types` from the harness pane, as the VT worker does.
fn kitty_read(harness: &mut Harness, types: &[&str]) {
    kitty_read_as(harness, types, None);
}

/// [`kitty_read`] carrying `credential`, the request's `name` and `pw`.
fn kitty_read_as(
    harness: &mut Harness,
    types: &[&str],
    credential: Option<crate::app::osc52_read_grants::GrantCredential>,
) {
    use crate::app::kitty_clipboard_read::KittyReadRequest;
    use crate::app::osc52_read_ticket::admit_kitty_read;
    let identity = harness.app.pane_by_id(harness.pane).expect("a live pane").read_identity.clone();
    let request = KittyReadRequest {
        id: "k".into(),
        location: sonicterm_vt::vt::KittyClipboardLocation::Standard,
        listing: false,
        types: types.iter().map(|mime| (*mime).to_owned()).collect(),
        bel: false,
        credential,
    };
    let ticket =
        admit_kitty_read(&harness.staging, &identity, harness.pane, 0, request, Instant::now())
            .expect("an admitted read");
    harness.app.admit_clipboard_ticket(ClipboardTicketHandle::new(ticket));
}

/// Contract: an OSC 5522 read under `allow` reaches only the source pane's sensitive slot as one
/// OK/DATA/DONE message, holding its 21 MiB budget lease until the writer drops the reply.
#[test]
fn kitty_read_delivered_with_lease_custody() {
    let mut harness = harness(Osc52ReadPolicy::Allow, FakeSource::new("secret"));
    kitty_read(&mut harness, &["text/plain"]);
    harness.pump_until(|harness| harness.delivered_count() == 1);
    assert!(harness.budget_bytes() > 0, "the lease rides the reply");
    let delivered = harness.take_delivered();
    let expected = "\x1b]5522;type=read:status=OK:id=k\x1b\\\
                    \x1b]5522;type=read:status=DATA:id=k:mime=dGV4dC9wbGFpbg==;c2VjcmV0\x1b\\\
                    \x1b]5522;type=read:status=DONE:id=k\x1b\\";
    assert_eq!(delivered[0].0, harness.pane);
    assert_eq!(delivered[0].1.bytes(), expected.as_bytes());
    drop(delivered);
    assert_eq!(harness.budget_bytes(), 0);
}

/// Contract: three OSC 5522 read jobs fit the 64 MiB reply budget; a fourth gets one `EBUSY`
/// packet and never a partial OK sequence.
#[test]
fn fourth_job_ebusy() {
    use crate::app::kitty_clipboard_read::KITTY_READ_RESERVATION_BYTES;
    let mut harness = harness(Osc52ReadPolicy::Allow, FakeSource::new("secret"));
    let mut leases: Vec<_> = (0..3).map(|_| harness.app.clip_state.reads.budget.lease()).collect();
    assert!(leases.iter_mut().all(|lease| lease.grow_to(KITTY_READ_RESERVATION_BYTES)));
    kitty_read(&mut harness, &["text/plain"]);
    harness.pump_until(|harness| harness.delivered_count() == 1);
    let delivered = harness.take_delivered();
    assert_eq!(delivered[0].1.bytes(), b"\x1b]5522;type=read:status=EBUSY:id=k\x1b\\");
}

/// Contract: an OSC 5522 reply whose writer admission stays contended through every retry is
/// dropped with its custody, and the program then hears exactly one `EBUSY` packet, never a
/// partial OK sequence.
#[test]
fn queue_pressure_single_ebusy_no_partial_ok() {
    let mut harness = harness(Osc52ReadPolicy::Allow, FakeSource::new("secret"));
    let refusals = 1 + MAX_DELIVERY_RETRIES as usize;
    let (sink, calls) = (harness.delivered.clone(), harness.port_calls.clone());
    harness.app.clip_state.reads.port = Some(Box::new(move |pane_id, reply| {
        // The first attempt and every retry are contended; the next admission succeeds.
        if calls.fetch_add(1, Ordering::SeqCst) < refusals {
            return Err(SensitiveReplyError::Contended(reply));
        }
        sink.lock().unwrap().push((pane_id, reply));
        Ok(())
    }));
    kitty_read(&mut harness, &["text/plain"]);
    harness.pump_until(|harness| harness.port_calls.load(Ordering::SeqCst) >= refusals);
    for _ in 0..4 {
        harness.app.pump_clipboard_reads();
    }
    let delivered = harness.take_delivered();
    let bytes: Vec<&[u8]> = delivered.iter().map(|(_, reply)| reply.bytes()).collect();
    assert_eq!(bytes, vec![&b"\x1b]5522;type=read:status=EBUSY:id=k\x1b\\"[..]]);
    assert_eq!(harness.budget_bytes(), 0, "the dropped reply released its lease");
    assert_eq!(harness.staging.ticket_slots_in_use(), 0);
}
