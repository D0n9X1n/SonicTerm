//! `osc52 = "ask"` writes: an asked write takes a consent ticket on the same gate and palette page
//! as reads, reaches the clipboard only through Apply on its own page, and is dropped by Deny,
//! Escape, expiry or any revocation. It shares the consent suite's harness and helpers.

use super::*;
use crate::app::osc52_stream::ClipboardJob;
use sonicterm_cfg::config::Osc52Policy;
use sonicterm_ui::status_line::ClipOutcome;

/// Reload `[terminal] osc52` through the production config path, keeping the read policy.
fn set_write_policy(app: &mut App, policy: Osc52Policy) {
    let mut config = app.config.clone();
    config.terminal.osc52 = policy;
    app.apply_new_config(config);
}

/// An `osc52 = "ask"` harness whose main window is frontmost and whose test clipboard holds
/// `before`, so an approved write lands in the test buffer, never the desktop clipboard.
fn asking() -> Harness {
    let mut harness = harness(Osc52ReadPolicy::Ask, FakeSource::new("unused"));
    set_write_policy(&mut harness.app, Osc52Policy::Ask);
    harness.app.__test_set_memory_clipboard("before");
    let main = main_window(&harness);
    focus(&mut harness, Some(main));
    harness
}

fn clipboard(harness: &Harness) -> Option<String> {
    harness.app.__test_memory_clipboard()
}

/// Contract: under `osc52 = "ask"` a write never reaches the clipboard on arrival; its consent
/// page opens in the source window naming only the tab and pane, with Deny highlighted first, and
/// Apply on that page writes the exact text, then frees the ticket and program slots.
#[test]
fn ask_write_waits_for_apply_then_writes() {
    let mut harness = asking();
    assert!(request(&mut harness, b"\x1b]52;c;aGk=\x07").is_empty(), "a write is never answered");
    assert_eq!(clipboard(&harness).as_deref(), Some("before"), "nothing written before Apply");
    assert!(page_open(&harness));
    let expected =
        harness.app.i18n.t_args("clipboard-write-title", Some(&[("tab", "1"), ("pane", "1")]));
    assert_ne!(expected, "clipboard-write-title", "the title is localized");
    let palette = &harness.app.interaction.command_palette;
    assert_eq!(palette.confirm_title(), expected);
    assert!(!palette.confirm_applies(), "Deny is highlighted first");
    harness.app.interaction.command_palette.move_selection_up();
    press(&mut harness, NamedKey::Enter);
    assert_eq!(clipboard(&harness).as_deref(), Some("hi"));
    let slots = (harness.staging.ticket_slots_in_use(), harness.staging.program_slots_in_use());
    assert_eq!(slots, (0, 0));
    assert_eq!(harness.delivered_count(), 0, "a write sends the program nothing");
}

/// Contract: Deny, Escape and expiry drop an asked write: the clipboard keeps its text, nothing is
/// sent to the program, every slot and lease is freed, and the clip segment says it was denied.
#[test]
fn deny_escape_and_expiry_drop_the_write() {
    for dismiss in ["deny", "escape", "expiry"] {
        let mut harness = asking();
        request(&mut harness, b"\x1b]52;c;aGk=\x07");
        assert!(page_open(&harness), "{dismiss}");
        match dismiss {
            "deny" => press(&mut harness, NamedKey::Enter),
            "escape" => press(&mut harness, NamedKey::Escape),
            _ => harness.app.pump_clipboard_reads_at(Instant::now() + TICKET_LIFETIME),
        }
        assert!(!harness.app.interaction.command_palette.is_open(), "{dismiss}: the page closes");
        assert_eq!(clipboard(&harness).as_deref(), Some("before"), "{dismiss}");
        assert_eq!(harness.delivered_count(), 0, "{dismiss}: no reply for a write");
        let slots = (harness.staging.ticket_slots_in_use(), harness.staging.program_slots_in_use());
        assert_eq!((slots, harness.staging.reserved_bytes()), ((0, 0), 0), "{dismiss}");
        let record = harness.app.status_line.clip_record().expect("a clip record");
        assert_eq!((record.pane_id, record.outcome), (harness.pane, ClipOutcome::Denied));
    }
}

/// Contract: an asked write is revoked silently, writing nothing, when `osc52` changes while its
/// page shows, and when its pane leaves its window while it waits; its slots are freed either way.
#[test]
fn policy_or_owner_change_revokes_the_write() {
    let mut harness = asking();
    request(&mut harness, b"\x1b]52;c;aGk=\x07");
    assert!(page_open(&harness));
    set_write_policy(&mut harness.app, Osc52Policy::Allow);
    assert!(!page_open(&harness), "a policy change closes the page");
    assert_eq!(harness.app.clip_state.reads.last_cancel, Some(CancelReason::PolicyChanged));

    let mut harness = asking();
    focus(&mut harness, None);
    request(&mut harness, b"\x1b]52;c;aGk=\x07");
    let pane = harness.app.pane_by_id(harness.pane).expect("the source pane");
    pane.read_identity.bump_owner();
    harness.app.pump_clipboard_reads();
    assert_eq!(harness.app.clip_state.reads.last_cancel, Some(CancelReason::OwnerChanged));
    assert_eq!(clipboard(&harness).as_deref(), Some("before"));
    assert_eq!(harness.staging.ticket_slots_in_use(), 0);
}

/// Contract: a write admitted under `allow` but handled after a reload to `ask` was never
/// approved, so the event loop denies it instead of writing.
#[test]
fn unapproved_write_under_ask_is_denied() {
    let mut harness = asking();
    let job = ClipboardJob::for_test(&harness.staging, "sneaky");
    let outcome = harness.app.submit_program_clipboard(harness.pane, job, false);
    assert_eq!(outcome, Some(crate::app::event_loop::ClipWriteOutcome::Denied));
    assert_eq!(clipboard(&harness).as_deref(), Some("before"));
}

/// Contract: while an asked write or read waits, the clip segment shows a waiting hint for its
/// pane, past the record's hold, and the hint ends with the ticket.
#[test]
fn waiting_ticket_shows_the_clip_hint() {
    let mut harness = asking();
    focus(&mut harness, None);
    let now = Instant::now();
    assert_eq!(harness.app.waiting_clip_record(now), None, "nothing waits yet");
    request(&mut harness, b"\x1b]52;c;aGk=\x07");
    let later = now + Duration::from_secs(60);
    let hint = harness.app.waiting_clip_record(later).expect("a hint while the write waits");
    assert_eq!((hint.pane_id, hint.outcome), (harness.pane, ClipOutcome::Waiting));
    let main = main_window(&harness);
    focus(&mut harness, Some(main));
    harness.app.pump_clipboard_reads();
    let shown = harness.app.waiting_clip_record(later).expect("a hint while its page shows");
    assert_eq!(shown.outcome, ClipOutcome::Waiting);
    press(&mut harness, NamedKey::Escape);
    assert_eq!(harness.app.waiting_clip_record(later), None, "the hint ends with the ticket");
}

/// Run one OSC 5522 write of `text` from the harness pane through a real parser and the pane's
/// transaction, admitting its `ask` ticket as the VT worker posts it.
fn kitty_write(harness: &mut Harness, text: &str) {
    kitty_write_as(harness, text, "");
}

/// [`kitty_write`] whose opening `type=write` packet carries `extra` metadata, such as
/// `:name=…:pw=…`.
fn kitty_write_as(harness: &mut Harness, text: &str, extra: &str) {
    kitty_write_admitted(harness, text, extra, Instant::now());
}

/// [`kitty_write_as`] whose consent ticket is aged to `admitted_at`, as if it had been admitted
/// then; every packet arrives now, so the open write is never idle.
fn kitty_write_admitted(harness: &mut Harness, text: &str, extra: &str, admitted_at: Instant) {
    use crate::app::kitty_clipboard_tx::{KittyClipboardTx, KittyStep};
    use base64::Engine as _;
    use sonicterm_vt::vt::CaptureStagingPool;
    let identity = harness.app.pane_by_id(harness.pane).expect("a live pane").read_identity.clone();
    let mut tx = KittyClipboardTx::new(harness.staging.clone(), identity);
    // The consent fixture writes to its injected clipboard even on a headless host.
    tx.set_holds(|_, _| true);
    tx.bind_pane(harness.pane);
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new());
    let data = base64::engine::general_purpose::STANDARD.encode(text);
    let wire = format!(
        "\x1b]5522;type=write:id=k{extra}\x1b\\\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;{data}\x1b\\\
         \x1b]5522;type=wdata\x1b\\"
    );
    let events = parser.advance(wire.as_bytes());
    let inbox = parser.kitty_clipboard_inbox().clone();
    for event in events {
        if let VtEvent::KittyClipboard { stream, request, bel } = event {
            let step = tx.handle(&inbox, stream, request, bel, 0, Instant::now());
            if let KittyStep::Ticket(mut ticket) = step {
                // The commit stamps its ticket when admitted; the test ages it to `admitted_at`.
                ticket.admitted_at_for_test(admitted_at);
                harness.app.admit_clipboard_ticket(ClipboardTicketHandle::new(ticket));
            }
        }
    }
}

/// The bytes delivered so far, in order.
fn delivered_bytes(harness: &Harness) -> Vec<Vec<u8>> {
    harness.take_delivered().into_iter().map(|(_, reply)| reply.bytes().to_vec()).collect()
}

/// Contract: an asked OSC 5522 write whose consent ends without an answer, because its page was
/// lost or its pane left its window, is answered `EPERM` on the program that still waits; when
/// the pane's PTY or command changed, the program that asked is gone and nothing is sent.
#[test]
fn cancelled_kitty_write_consent_answers_eperm_to_the_same_program() {
    let eperm = b"\x1b]5522;type=write:status=EPERM:id=k\x1b\\".to_vec();
    for case in ["page-lost", "owner", "pty", "command"] {
        let mut harness = asking();
        if case == "pty" {
            // The ticket binds a PTY the pane no longer has, as after a respawn.
            let pane = harness.app.pane_by_id(harness.pane).expect("the source pane");
            pane.read_identity.set_incarnation(99);
        }
        kitty_write(&mut harness, "hi");
        // A ticket bound to another PTY is cancelled at admission, before any page opens.
        assert_eq!(page_open(&harness), case != "pty", "{case}");
        match case {
            "page-lost" => {
                let palette = &mut harness.app.interaction.command_palette;
                palette.start_confirm("other", "Apply", "Cancel", Vec::new());
            }
            "owner" => {
                let pane = harness.app.pane_by_id(harness.pane).expect("the source pane");
                pane.read_identity.bump_owner();
            }
            "command" => {
                let pane = harness.app.pane_by_id(harness.pane).expect("the source pane");
                pane.read_identity.set_command_epoch(1);
            }
            _ => {}
        }
        harness.app.pump_clipboard_reads();
        assert!(harness.app.clip_state.reads.last_cancel.is_some(), "{case}: cancelled");
        let expected =
            if matches!(case, "page-lost" | "owner") { vec![eperm.clone()] } else { vec![] };
        assert_eq!(delivered_bytes(&harness), expected, "{case}");
        assert_eq!(clipboard(&harness).as_deref(), Some("before"), "{case}: nothing written");
        let slots = (harness.staging.ticket_slots_in_use(), harness.staging.program_slots_in_use());
        assert_eq!(slots, (0, 0), "{case}");
    }
}

/// A native clipboard whose writes wait while the test holds `gate`, recording each written text.
#[derive(Clone, Default)]
struct BlockingSink {
    writes: Arc<Mutex<Vec<String>>>,
    gate: Arc<Mutex<()>>,
    /// Writes that reached the native call, including one still held at the gate.
    entered: Arc<AtomicUsize>,
}

impl BlockingSink {
    /// Wait until `count` writes have reached the native call.
    fn wait_entered(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.entered.load(Ordering::SeqCst) < count {
            assert!(Instant::now() < deadline, "the worker never reached the native call");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl crate::app::clipboard_worker::ClipboardSink for BlockingSink {
    fn write(
        &mut self,
        request: crate::app::clipboard_worker::WriteRequest<'_>,
    ) -> crate::app::clipboard_worker::WriteResult {
        use crate::app::clipboard_worker::{TargetOutcome, WriteResult};
        // The commit gate is checked first, as a native sink checks it before its call.
        if !(request.gate)() {
            return WriteResult::stale();
        }
        self.entered.fetch_add(1, Ordering::SeqCst);
        let _held = self.gate.lock().unwrap();
        self.writes.lock().unwrap().push(request.text.to_owned());
        WriteResult::each(request.targets, TargetOutcome::Taken)
    }

    fn retains_copies(&self) -> bool {
        false
    }
}

/// Contract: an approved write keeps the identity that authorized it until the clipboard worker
/// commits it. With the worker busy, Apply, then the pane closing, leaving its window, its command
/// changing, its window entering READONLY, or `osc52_read` changing drops the write: nothing
/// reaches the clipboard and the program hears no status. Untouched, it lands and answers `DONE`.
#[test]
fn approved_write_rebound_while_worker_busy_is_dropped_silently() {
    let done = b"\x1b]5522;type=write:status=DONE:id=k\x1b\\".to_vec();
    let mut failures = Vec::new();
    for case in ["unchanged", "close", "transfer", "command", "readonly", "osc52_read"] {
        let mut harness = asking();
        let sink = BlockingSink::default();
        let domain = &mut harness.app.clip_state.domain;
        assert!(domain.start_worker(Box::new(sink.clone()), None, Box::new(|| {})), "{case}");
        let held = sink.gate.lock().unwrap();
        // A user copy past the inline limit goes to the worker and holds it in its native call.
        let blocker = "b".repeat(crate::app::clipboard_worker::INLINE_WRITE_LIMIT_BYTES + 1);
        assert!(harness.app.set_clipboard_text(blocker), "{case}");
        kitty_write(&mut harness, "hi");
        assert!(page_open(&harness), "{case}");
        harness.app.interaction.command_palette.move_selection_up();
        press(&mut harness, NamedKey::Enter);
        assert!(!page_open(&harness), "{case}: Apply answered the page");
        let main = main_window(&harness);
        match case {
            "close" => {
                // The production close path: the pane leaves its window and is retired.
                let window = harness.app.registry.windows.get_mut(&main).expect("main");
                let pane = window.panes.detach(harness.pane).expect("the source pane");
                harness.app.retire_pane(pane);
            }
            "transfer" => {
                // A transfer detaches the pane and attaches it to its new window.
                let window = harness.app.registry.windows.get_mut(&main).expect("main");
                let pane = window.panes.detach(harness.pane).expect("the source pane");
                assert!(window.panes.attach(pane).is_ok(), "{case}");
            }
            "command" => {
                // The VT worker publishes a prompt or command boundary.
                let pane = harness.app.pane_by_id(harness.pane).expect("the source pane");
                pane.read_identity.set_command_epoch(1);
            }
            "readonly" => harness.app.enter_copy_mode_for_kind(crate::app::FrontmostKind::Main),
            "osc52_read" => {
                let mut config = harness.app.config.clone();
                config.terminal.osc52_read = Osc52ReadPolicy::Allow;
                harness.app.apply_new_config(config);
            }
            _ => {}
        }
        drop(held);
        let deadline = Instant::now() + Duration::from_secs(10);
        while harness.app.clip_state.domain.in_flight() > 0 {
            assert!(Instant::now() < deadline, "{case}: the clipboard worker did not settle");
            harness.app.drain_clipboard_completions();
            std::thread::sleep(Duration::from_millis(1));
        }
        let writes = sink.writes.lock().unwrap().clone();
        let program_writes: Vec<&str> = writes[1..].iter().map(String::as_str).collect();
        let (expected_writes, expected_replies) =
            if case == "unchanged" { (vec!["hi"], vec![done.clone()]) } else { (vec![], vec![]) };
        let replies = delivered_bytes(&harness);
        let slots = (harness.staging.ticket_slots_in_use(), harness.staging.program_slots_in_use());
        // Every case is checked before the test fails, so one run names each broken case.
        if program_writes != expected_writes || replies != expected_replies || slots != (0, 0) {
            failures.push(format!("{case}: writes {program_writes:?}, replies {replies:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Contract: an approved write keeps its ticket's 120 s deadline through the clipboard worker's
/// commit. Applied just inside the deadline while the worker is busy, it is dropped at the commit
/// point once the deadline passes: nothing reaches the clipboard and the program hears no status.
#[test]
fn approved_write_past_its_ticket_deadline_is_dropped_silently() {
    let mut harness = asking();
    let sink = BlockingSink::default();
    let domain = &mut harness.app.clip_state.domain;
    assert!(domain.start_worker(Box::new(sink.clone()), None, Box::new(|| {})));
    let held = sink.gate.lock().unwrap();
    // A user copy past the inline limit goes to the worker and holds it in its native call.
    let blocker = "b".repeat(crate::app::clipboard_worker::INLINE_WRITE_LIMIT_BYTES + 1);
    assert!(harness.app.set_clipboard_text(blocker));
    // The ticket was admitted 1.5 s before its lifetime ends, so Apply below is still in time.
    let margin = Duration::from_millis(1500);
    let admitted_at = Instant::now().checked_sub(TICKET_LIFETIME - margin).expect("a past instant");
    kitty_write_admitted(&mut harness, "hi", "", admitted_at);
    assert!(page_open(&harness));
    harness.app.interaction.command_palette.move_selection_up();
    press(&mut harness, NamedKey::Enter);
    assert_eq!(harness.app.clip_state.domain.in_flight(), 2, "Apply posted the write");
    // The worker stays busy until the ticket's deadline has passed.
    std::thread::sleep(margin + Duration::from_millis(200));
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.app.clip_state.domain.in_flight() > 0 {
        assert!(Instant::now() < deadline, "the clipboard worker did not settle");
        harness.app.drain_clipboard_completions();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(sink.writes.lock().unwrap().len(), 1, "only the blocking copy landed");
    assert_eq!(delivered_bytes(&harness), Vec::<Vec<u8>>::new(), "no status");
}

/// Contract: a write's status goes only to the program that asked for it. A write the worker
/// committed while the pane's command changed lands, but its `DONE` is not sent, because the next
/// command would read it as input.
#[test]
fn committed_write_status_is_not_routed_to_a_later_command() {
    let mut harness = asking();
    let sink = BlockingSink::default();
    let domain = &mut harness.app.clip_state.domain;
    assert!(domain.start_worker(Box::new(sink.clone()), None, Box::new(|| {})));
    let held = sink.gate.lock().unwrap();
    kitty_write(&mut harness, "hi");
    harness.app.interaction.command_palette.move_selection_up();
    press(&mut harness, NamedKey::Enter);
    // The worker is inside the native call: the write is past its commit point.
    sink.wait_entered(1);
    let pane = harness.app.pane_by_id(harness.pane).expect("the source pane");
    pane.read_identity.set_command_epoch(1);
    drop(held);
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.app.clip_state.domain.in_flight() > 0 {
        assert!(Instant::now() < deadline, "the clipboard worker did not settle");
        harness.app.drain_clipboard_completions();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(*sink.writes.lock().unwrap(), ["hi"], "the committed write landed");
    assert_eq!(delivered_bytes(&harness), Vec::<Vec<u8>>::new(), "no status for a later command");
}

#[cfg(test)]
#[path = "osc52_read_pipeline_tests_consent_tests_write_tests_grants_tests.rs"]
mod osc52_read_pipeline_tests_consent_tests_write_tests_grants_tests;
