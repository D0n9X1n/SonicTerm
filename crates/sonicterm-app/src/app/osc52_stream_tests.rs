use super::*;
use base64::Engine as _;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{Parser, VtEvent};

/// A staging with a private budget, so no sibling test's leases or slots change what one measures.
fn staging(budget_bytes: usize, stream_limit_bytes: usize) -> Arc<ClipboardStaging> {
    ClipboardStaging::with_budget(budget_bytes, stream_limit_bytes)
}

/// Feed `bytes` through a real parser and a fresh intake; return the writes and refusals it raised.
fn run(
    staging: &Arc<ClipboardStaging>,
    bytes: &[u8],
) -> Vec<Result<ClipboardJobHandle, Osc52Refusal>> {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut intake = Osc52Intake::new(staging.clone());
    let now = Instant::now();
    let mut results = Vec::new();
    for event in parser.advance(bytes) {
        if let VtEvent::ClipboardStream { stream, phase } = event {
            if let Some(result) = intake.handle(parser.clipboard_inbox(), stream, phase, now) {
                results.push(result);
            }
        }
    }
    intake.reconcile(parser.open_clipboard_stream(), now);
    results
}

/// The text and targets of the only write `bytes` raised.
fn written(staging: &Arc<ClipboardStaging>, bytes: &[u8]) -> (String, ClipTargets) {
    let mut results = run(staging, bytes);
    assert_eq!(results.len(), 1, "{bytes:?}");
    let job = results.remove(0).expect("a write").take().expect("an untaken job");
    (job.text.clone(), job.targets)
}

fn encode(text: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn osc52(kinds: &str, payload: &str) -> Vec<u8> {
    format!("\x1b]52;{kinds};{payload}\x07").into_bytes()
}

const CLIPBOARD: ClipTargets = ClipTargets { clipboard: true, primary: false };

/// Every kinds row of the reference table: `c`, `s`, digits and an empty field write the standard
/// clipboard; `p` reaches PRIMARY only on Linux; `cp` reaches every destination there is; an
/// unknown letter beside a known one is ignored.
#[test]
fn osc52_kind_matrix() {
    let staging = staging(1 << 20, 1 << 20);
    let linux = cfg!(target_os = "linux");
    for kinds in ["c", "s", "0", "9", "", "cx", "s0"] {
        assert_eq!(written(&staging, &osc52(kinds, &encode(b"hi"))).1, CLIPBOARD, "{kinds}");
    }
    let both = ClipTargets { clipboard: true, primary: linux };
    assert_eq!(written(&staging, &osc52("cp", &encode(b"hi"))).1, both);
    let primary = run(&staging, &osc52("p", &encode(b"hi")));
    if linux {
        // When: Linux has PRIMARY, a `p` write reaches it alone.
        let job = primary[0].as_ref().expect("a write").take().expect("untaken");
        assert_eq!(job.targets, ClipTargets { clipboard: false, primary: true });
    } else {
        // When: macOS and Windows have no PRIMARY, a `p` write is dropped.
        assert_eq!(primary, [Err(Osc52Refusal::Selection('p'))]);
    }
    assert_eq!(staging.reserved_bytes(), 0, "every job above has dropped");
}

/// A kinds field with no known letter is dropped, naming its first letter.
#[test]
fn unknown_only_kind_dropped() {
    let staging = staging(1 << 20, 1 << 20);
    assert_eq!(run(&staging, &osc52("xq", &encode(b"hi"))), [Err(Osc52Refusal::Selection('x'))]);
}

/// `52;c` with no data field clears the clipboard: an empty write.
#[test]
fn bare_52_c_clears() {
    let staging = staging(1 << 20, 1 << 20);
    assert_eq!(written(&staging, b"\x1b]52;c\x07"), (String::new(), CLIPBOARD));
}

/// `52;c;` with an empty payload clears the clipboard too.
#[test]
fn empty_payload_clears() {
    let staging = staging(1 << 20, 1 << 20);
    assert_eq!(written(&staging, b"\x1b]52;c;\x1b\\"), (String::new(), CLIPBOARD));
}

/// Final padding is optional: unpadded Base64 of one and two trailing bytes decodes exactly.
#[test]
fn unpadded_b64_written() {
    let staging = staging(1 << 20, 1 << 20);
    for text in ["a", "ab", "abc", "abcd"] {
        let unpadded = encode(text.as_bytes()).trim_end_matches('=').to_string();
        assert_eq!(written(&staging, &osc52("c", &unpadded)).0, text);
        assert_eq!(written(&staging, &osc52("c", &encode(text.as_bytes()))).0, text);
    }
}

/// A byte outside the standard alphabet, a lone sextet, or data after padding discards the whole
/// write and releases its reservation.
#[test]
fn non_alphabet_discards() {
    let staging = staging(1 << 20, 1 << 20);
    for payload in ["QUJD-", "QUJD_", "QUJDR", "QQ==QQ", "QUJD\u{e9}", "QQ=A"] {
        assert_eq!(run(&staging, &osc52("c", payload)), [Err(Osc52Refusal::Invalid)], "{payload}");
        assert_eq!(staging.reserved_bytes(), 0);
    }
}

/// Non-UTF-8 bytes become U+FFFD, and the lease covers the expanded text and its native buffer
/// before the `String` is built.
#[test]
fn non_utf8_replaced_and_expansion_reserved() {
    let staging = staging(1 << 20, 1 << 20);
    let (text, _) = written(&staging, &osc52("c", &encode(b"a\xffb\xfe")));
    assert_eq!(text, "a\u{fffd}b\u{fffd}");
    let mut results = run(&staging, &osc52("c", &encode(&[0xff; 100])));
    let job = results.remove(0).expect("a write");
    let taken = job.take().expect("untaken");
    assert_eq!(taken.text.chars().count(), 100);
    assert!(taken.lease.bytes() >= taken.text.len() + native_clipboard_bytes(&taken.text));
    assert_eq!(staging.reserved_bytes(), taken.lease.bytes());
}

/// OSC 1337 `Copy=:` writes the standard clipboard like OSC 52 `c`.
#[test]
fn iterm2_copy_writes_clipboard() {
    let staging = staging(1 << 20, 1 << 20);
    let bytes = format!("\x1b]1337;Copy=:{}\x07", encode(b"copied"));
    assert_eq!(written(&staging, bytes.as_bytes()), ("copied".into(), CLIPBOARD));
}

/// A stream of several megabytes decodes whole across 64 KiB chunks within its limit.
#[test]
fn stream_4_mib_decodes_across_chunks() {
    let staging = staging(64 << 20, 32 << 20);
    let text = "x".repeat(4 << 20);
    assert_eq!(written(&staging, &osc52("c", &encode(text.as_bytes()))).0.len(), text.len());
}

/// Past the stream limit the write is refused before its output buffer grows, and everything it
/// reserved is released.
#[test]
fn limit_exceeded_frees_before_allocation() {
    let staging = staging(1 << 20, 1024);
    let mut decoder = Base64Stream::new(staging.lease(), 1024);
    assert_eq!(decoder.feed(&[b'A'; 4096]), Err(Osc52Refusal::TooLarge));
    assert_eq!(decoder.output_capacity(), 0, "nothing was allocated");
    drop(decoder);
    assert_eq!(staging.reserved_bytes(), 0);
    let exact = encode(&[b'x'; 1024]);
    assert_eq!(written(&staging, &osc52("c", &exact)).0.len(), 1024);
    let over = encode(&[b'x'; 1025]);
    assert_eq!(run(&staging, &osc52("c", &over)), [Err(Osc52Refusal::TooLarge)]);
    assert_eq!(staging.reserved_bytes(), 0);
}

/// A write the process budget cannot cover is refused busy, and nothing stays reserved.
#[test]
fn budget_exhausted_refused_busy() {
    let staging = staging(4096, 1 << 20);
    let held = staging.lease();
    let mut held = held;
    assert!(held.grow_to(4000));
    assert_eq!(run(&staging, &osc52("c", &encode(&[b'x'; 300]))), [Err(Osc52Refusal::Busy)]);
    assert_eq!(staging.reserved_bytes(), 4000);
    drop(held);
    assert_eq!(staging.reserved_bytes(), 0);
}

/// A program slot is taken before the write is posted, even for a zero-byte clear; a ninth
/// outstanding write is refused busy until one is handled.
#[test]
fn slot_reserved_before_post_for_clear() {
    let staging = staging(1 << 20, 1 << 20);
    let held: Vec<_> = (0..MAX_PROGRAM_CLIPBOARD_JOBS)
        .map(|_| run(&staging, b"\x1b]52;c\x07").remove(0))
        .collect();
    assert_eq!(staging.program_slots_in_use(), MAX_PROGRAM_CLIPBOARD_JOBS);
    assert_eq!(run(&staging, b"\x1b]52;c\x07"), [Err(Osc52Refusal::Busy)]);
    drop(held);
    assert_eq!(staging.program_slots_in_use(), 0);
    assert!(run(&staging, b"\x1b]52;c\x07")[0].is_ok());
}

/// The lease covers the decoded text and the native buffer the platform builds from it.
#[test]
fn lease_covers_decode_replacement_and_native_buffer() {
    let staging = staging(1 << 20, 1 << 20);
    let text = "日本\u{1f600}";
    let mut results = run(&staging, &osc52("c", &encode(text.as_bytes())));
    let taken = results.remove(0).expect("a write").take().expect("untaken");
    let native = if cfg!(target_os = "windows") { 2 * 4 + 2 } else { text.len() };
    assert_eq!(native_clipboard_bytes(&taken.text), native);
    assert_eq!(taken.lease.bytes(), taken.text.capacity() + native);
}

/// A clipboard read refuses as a read for every kind and writes nothing.
#[test]
fn reads_still_refused() {
    let staging = staging(1 << 20, 1 << 20);
    for kinds in ["c", "p", "", "x"] {
        assert_eq!(run(&staging, &osc52(kinds, "?")), [Err(Osc52Refusal::Read)], "{kinds}");
    }
}

/// `p` alone targets PRIMARY and never the clipboard, and only where PRIMARY exists.
#[test]
fn primary_reaches_linux_primary_only() {
    assert_eq!(clip_targets(b"p", true), Ok(ClipTargets { clipboard: false, primary: true }));
    assert_eq!(clip_targets(b"p", false), Err(Osc52Refusal::Selection('p')));
    assert_eq!(clip_targets(b"pc", false), Ok(CLIPBOARD));
}

/// An open stream expires 60 s after it opened, releasing its decoder and lease.
#[test]
fn open_stream_expires_after_60s() {
    let staging = staging(1 << 20, 1 << 20);
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut intake = Osc52Intake::new(staging.clone());
    let start = Instant::now();
    let mut payload = b"\x1b]52;c;".to_vec();
    payload.extend_from_slice(encode(&[b'x'; 100_000]).as_bytes());
    for event in parser.advance(&payload) {
        if let VtEvent::ClipboardStream { stream, phase } = event {
            assert!(intake.handle(parser.clipboard_inbox(), stream, phase, start).is_none());
        }
    }
    intake.reconcile(parser.open_clipboard_stream(), start);
    assert!(staging.reserved_bytes() > 0);
    assert_eq!(intake.deadline(), Some(start + OSC52_STREAM_EXPIRY));
    assert_eq!(intake.expire(start + Duration::from_secs(59)), None);
    let expired = intake.expire(start + OSC52_STREAM_EXPIRY);
    assert_eq!(expired, parser.open_clipboard_stream());
    assert_eq!(staging.reserved_bytes(), 0);
}

/// Windows reasserts a write only within 262,144 UTF-16 units, counting CJK as one unit and an
/// astral character as two.
#[test]
fn reassert_cutoff_in_utf16_units_with_cjk_and_astral_text() {
    let units = REASSERT_MAX_UTF16_UNITS;
    assert!(reassert_eligible(&"字".repeat(units)));
    assert!(!reassert_eligible(&"字".repeat(units + 1)));
    assert!(reassert_eligible(&"\u{1f600}".repeat(units / 2)));
    assert!(!reassert_eligible(&format!("{}a", "\u{1f600}".repeat(units / 2))));
}

/// A job cloned into a second event copies no payload, and a handle's `Debug` prints lengths only.
#[test]
fn job_handle_clone_and_debug_copy_no_payload() {
    let staging = staging(1 << 20, 1 << 20);
    let job = run(&staging, &osc52("c", &encode(b"secret text"))).remove(0).expect("a write");
    let clone = job.clone();
    assert!(!format!("{clone:?}").contains("secret"));
    assert_eq!(staging.program_slots_in_use(), 1, "a clone shares the one slot");
    assert!(clone.take().is_some());
    assert!(job.take().is_none(), "a job is taken once");
}

/// An App with an in-memory clipboard holding `previous` and a private clipboard staging.
fn app_with_clipboard(previous: &str) -> super::super::App {
    let mut app = super::super::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_set_memory_clipboard(previous);
    app.clip_state = ClipState::new(staging(1 << 20, 1 << 20));
    app
}

/// A write posted before a reload to `osc52 = "deny"` is refused at the commit point: the
/// clipboard keeps its value and the job's lease and slot are released.
#[test]
fn policy_revoked_before_commit_point_refused() {
    let staging = staging(1 << 20, 1 << 20);
    let mut app = app_with_clipboard("old");
    let job = run(&staging, &osc52("c", &encode(b"late"))).remove(0).expect("a write");
    app.config.terminal.osc52 = sonicterm_cfg::config::Osc52Policy::Deny;
    let outcome = app.submit_program_clipboard(1, job.take().expect("untaken"));
    assert_eq!(outcome, Some(super::super::event_loop::ClipWriteOutcome::Denied));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("old"));
    assert_eq!((staging.reserved_bytes(), staging.program_slots_in_use()), (0, 0));
}

/// With no worker, program writes and user copies commit on the event loop in the order it
/// handles them, so a user copy after a program write wins and replaces its retained copy.
#[test]
fn user_copy_after_program_write_wins() {
    let staging = staging(1 << 20, 1 << 20);
    let mut app = app_with_clipboard("old");
    let job = run(&staging, &osc52("c", &encode(b"program"))).remove(0).expect("a write");
    let outcome = app.submit_program_clipboard(1, job.take().expect("untaken"));
    assert_eq!(outcome, Some(super::super::event_loop::ClipWriteOutcome::Written));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("program"));
    assert!(app.set_clipboard_text("user".into()));
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("user"));
    assert_eq!(app.clip_state.retained.bytes(), 0, "the user copy replaced the retained copy");
}

/// On the event-loop path the job's lease is held until the native call returns; only Linux then
/// keeps the part that covers arboard's retained copy, and the next write to the selection
/// replaces it. The worker path is pinned by `lease_held_until_worker_native_call_returns`.
#[test]
fn lease_held_until_native_return() {
    let staging = staging(1 << 20, 1 << 20);
    let mut app = app_with_clipboard("old");
    let job = run(&staging, &osc52("c", &encode(b"held"))).remove(0).expect("a write");
    let taken = job.take().expect("untaken");
    let native = native_clipboard_bytes(&taken.text);
    assert!(staging.reserved_bytes() >= taken.text.len() + native, "reserved before the call");
    let _ = app.submit_program_clipboard(1, taken);
    let retained = if cfg!(target_os = "linux") { native } else { 0 };
    assert_eq!(staging.reserved_bytes(), retained);
    let next = run(&staging, &osc52("c", &encode(b"held"))).remove(0).expect("a write");
    let _ = app.submit_program_clipboard(1, next.take().expect("untaken"));
    assert_eq!(staging.reserved_bytes(), retained, "the next write replaced the retained copy");
    assert_eq!(staging.program_slots_in_use(), 0);
}

/// The staging's leases and posted writes are charged to the process owner's
/// `ClipboardStaging` class, and a released lease clears the charge.
#[test]
fn clipboard_staging_charged_to_process_owner() {
    let mut app = app_with_clipboard("old");
    let mut lease = app.clip_state.staging.lease();
    assert!(lease.grow_to(4096));
    app.charge_clipboard_staging();
    let root = app.governor.root_owner();
    let class = sonicterm_types::ResourceClass::ClipboardStaging;
    assert_eq!(app.governor.snapshot(root).unwrap().owner_class_bytes[class], 4096);
    drop(lease);
    app.charge_clipboard_staging();
    assert_eq!(app.governor.snapshot(root).unwrap().owner_class_bytes[class], 0);
}
