//! Unit tests for the guard-correlation transport: the run nonce, the streamed and hashed sidecar, its
//! atomic write, each status with its window, the borrowed adapter's allocations, and the route order.

use std::sync::OnceLock;

use serde_json::Value;

use super::*;

/// Records a test serializes in place of the App's: `panes` and `spans` as fixed JSON values.
#[derive(Debug, PartialEq, Eq)]
struct FakeRecords {
    take_seq: u64,
    taken_at_ns: Option<u64>,
    panes: Value,
    spans: Value,
}

impl TakenRecords for FakeRecords {
    fn take_seq(&self) -> u64 {
        self.take_seq
    }

    fn taken_at_ns(&self) -> Option<u64> {
        self.taken_at_ns
    }

    fn serialize_panes<Out: serde::Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
        self.panes.serialize(out)
    }

    fn serialize_spans<Out: serde::Serializer>(&self, out: Out) -> Result<Out::Ok, Out::Error> {
        self.spans.serialize(out)
    }
}

/// A fresh scratch directory under the system temp dir, unique to `label` and this process.
fn scratch(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-guard-transport-{label}-{}-{}",
        std::process::id(),
        nonce_seed()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A binding for phase `phase_index` named `phase_name`, with a fixed window.
fn binding<'run>(
    nonce: &'run str,
    hash: Option<&'run str>,
    phase_index: usize,
    phase_name: &'run str,
) -> Binding<'run> {
    Binding {
        run_nonce: nonce,
        harness_hash: hash,
        pid: 4242,
        phase_index,
        phase_name,
        window: Window { start_ns: Some(100), end_ns: Some(900) },
    }
}

/// A take with one pane of `records` sections and one span.
fn taken(take_seq: u64, records: u64) -> FakeRecords {
    let sections: Vec<Value> = (1..=records)
        .map(|seq| serde_json::json!({"section_seq": seq, "before_lock_ns": seq * 10, "locked_at_ns": seq * 10 + 5}))
        .collect();
    FakeRecords {
        take_seq,
        taken_at_ns: Some(950),
        panes: serde_json::json!([{"pane_id": 7, "records": sections}]),
        spans: serde_json::json!({"spans": [{"pane_id": 7, "collection_seq": 1, "acquired_ns": 200, "released_ns": 300}]}),
    }
}

const NONCE: &str = "0123456789abcdef0123456789abcdef";

/// A′: the nonce is 32 lowercase hex from the seed and the hasher's word of that same seed.
#[test]
fn the_run_nonce_is_32_lowercase_hex_from_the_seed_and_its_hash() {
    let mut seen_seed = None;
    let nonce = mint_run_nonce(0xAB, |seed| {
        seen_seed = Some(seed);
        0x0123_4567_89AB_CDEF
    });
    assert_eq!(nonce, "00000000000000ab0123456789abcdef");
    assert_eq!(seen_seed, Some(0xAB), "the hasher is fed the seed");
    let real = run_nonce();
    assert_eq!(real.len(), 32);
    assert!(real.chars().all(|character| matches!(character, '0'..='9' | 'a'..='f')), "{real}");
}

/// A′: the nonce is minted once per cell and every later read returns it unchanged.
#[test]
fn the_run_nonce_is_minted_once() {
    let cell = OnceLock::new();
    let mut mints = 0;
    let first = run_nonce_in(&cell, || {
        mints += 1;
        "a".repeat(32)
    })
    .to_owned();
    let second = run_nonce_in(&cell, || unreachable!("minted twice")).to_owned();
    assert_eq!((first, second, mints), ("a".repeat(32), "a".repeat(32), 1));
    assert_eq!(run_nonce(), run_nonce(), "the process nonce is stable");
}

/// A′: the top-level nonce equals the clock nonce in every sidecar of a run, and the workload's sentinel
/// nonce keeps its own 16-hex form, unrelated to the run nonce.
#[test]
fn every_sidecar_carries_one_run_nonce_and_the_sentinel_is_unchanged() {
    for (index, name) in [(0_usize, "startup"), (1, "flood"), (2, "idle")] {
        let take = taken(index as u64 + 1, 2);
        let (bytes, ..) =
            stream_to(Vec::new(), &binding(NONCE, Some("hh"), index, name), &take).unwrap();
        let document: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(document["run_nonce"], NONCE);
        assert_eq!(document["clock"]["run_nonce"], NONCE, "{name}");
        assert_eq!(document["phase_index"], index, "{name}");
        assert_eq!(document["phase_name"], name);
    }
    let sentinel = crate::workload::sentinel_line(0, "00000000000000ab");
    assert!(sentinel.contains("00000000000000ab"), "{sentinel}");
}

/// E′ and H7: a fixed sidecar's bytes are exactly the frozen header in key order, and its digest equals a value
/// computed independently with Python's hashlib over those bytes, not with this crate's SHA-256.
#[test]
fn a_fixed_sidecar_matches_its_independent_known_answer() {
    let records = FakeRecords {
        take_seq: 1,
        taken_at_ns: Some(950),
        panes: serde_json::json!([]),
        spans: serde_json::json!({"spans": []}),
    };
    let (bytes, size, digest) =
        stream_to(Vec::new(), &binding(NONCE, Some("hh"), 0, "startup"), &records).unwrap();
    let expected = format!(
        "{{\"schema\":1,\"run_nonce\":\"{NONCE}\",\"harness_hash\":\"hh\",\"phase_index\":0,\
         \"phase_name\":\"startup\",\"take_seq\":1,\"clock\":{{\"pid\":4242,\"run_nonce\":\"{NONCE}\"}},\
         \"window\":{{\"label\":\"phase start to counter-end observation\",\"start_ns\":100,\"end_ns\":900}},\
         \"taken_at_ns\":950,\"panes\":[],\"spans\":{{\"spans\":[]}}}}"
    );
    assert_eq!(String::from_utf8(bytes).unwrap(), expected);
    assert_eq!(size, 337);
    // python3 -c "import hashlib; print(hashlib.sha256(<the bytes above>).hexdigest())"
    assert_eq!(digest, "b4bd5032392c79bf8ec12b00734437d2fc48799cdddd6f49e54419e252782c24");
}

/// B′ and H7: unknown edges and a missing harness hash are written as null, never dropped or zero.
#[test]
fn unknowns_are_null() {
    let take = taken(3, 1);
    let mut unknown = binding(NONCE, None, 1, "flood");
    unknown.window = Window { start_ns: None, end_ns: Some(900) };
    let (bytes, ..) = stream_to(Vec::new(), &unknown, &take).unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["harness_hash"], Value::Null);
    assert_eq!(document["window"]["start_ns"], Value::Null);
    assert_eq!(document["window"]["end_ns"], 900);
}

/// H6 and E′: a written sidecar is renamed into place; its recorded size and digest are those of the file's
/// bytes; the phase field carries the same window as the header; no temporary file remains.
#[test]
fn a_written_sidecar_names_its_size_digest_and_window() {
    let dir = scratch("written");
    let field = transport(
        &dir,
        &binding(NONCE, Some("hh"), 2, "flood"),
        &GuardTake::Taken(taken(4, 3)),
        |from, to| std::fs::rename(from, to),
    );
    assert_eq!(field.status, TransportStatus::Written);
    assert_eq!(field.file.as_deref(), Some("guard-correlation/2-flood.json"));
    let bytes = std::fs::read(dir.join("guard-correlation/2-flood.json")).unwrap();
    assert_eq!(field.bytes, Some(bytes.len() as u64));
    assert_eq!(field.sha256.as_deref(), Some(crate::digest::sha256_hex(&bytes).as_str()));
    let header: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        serde_json::to_value(field.window).unwrap(),
        header["window"],
        "the record binds the header's window"
    );
    assert!(!dir.join("guard-correlation/2-flood.json.tmp").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// H6: a crash between the temporary write and the rename leaves no file named and no `written` status; a
/// scratch that cannot hold the directory fails the same way.
#[test]
fn a_failed_write_never_names_a_file() {
    let dir = scratch("failed");
    let take = GuardTake::Taken(taken(5, 2));
    let field = transport(&dir, &binding(NONCE, None, 0, "startup"), &take, |_, _| {
        Err(io::Error::other("crash before rename"))
    });
    assert_eq!(
        (field.status, field.file.as_deref(), field.sha256.as_deref()),
        (TransportStatus::WriteFailed, None, None)
    );
    assert_eq!(field.take_seq, Some(5), "the issued take is named");
    assert!(!dir.join("guard-correlation/0-startup.json").exists());
    assert!(
        !dir.join("guard-correlation/0-startup.json.tmp").exists(),
        "the partial file is removed"
    );
    let blocked = dir.join("not-a-dir");
    std::fs::write(&blocked, b"x").unwrap();
    let field = transport(&blocked, &binding(NONCE, None, 0, "startup"), &take, |from, to| {
        std::fs::rename(from, to)
    });
    assert_eq!(field.status, TransportStatus::WriteFailed);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A writer that accepts at most `chunk` bytes a call, can fail once with `Interrupted`, and fails for good
/// after `fail_after` bytes; it records the largest single write.
struct Recording {
    bytes: Vec<u8>,
    largest_write: usize,
    writes: usize,
    chunk: usize,
    interrupt_once: bool,
    fail_after: usize,
}

impl Recording {
    fn new(chunk: usize) -> Self {
        Self {
            bytes: Vec::new(),
            largest_write: 0,
            writes: 0,
            chunk,
            interrupt_once: false,
            fail_after: usize::MAX,
        }
    }
}

impl Write for Recording {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.interrupt_once {
            self.interrupt_once = false;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if self.bytes.len() >= self.fail_after {
            return Err(io::Error::other("disk full"));
        }
        let accepted = buf.len().min(self.chunk);
        self.largest_write = self.largest_write.max(accepted);
        self.writes += 1;
        self.bytes.extend_from_slice(&buf[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// E′: a large sidecar streams in writes of at most the 64 KiB buffer; short writes of 7 bytes and an
/// interrupted write still hash exactly the bytes accepted; a failing writer surfaces as an error.
#[test]
fn a_large_sidecar_streams_through_one_bounded_buffer() {
    let take = taken(6, 20_000);
    let run = binding(NONCE, Some("hh"), 1, "flood");
    let (sink, size, digest) = stream_to(Recording::new(usize::MAX), &run, &take).unwrap();
    assert!(sink.bytes.len() > 4 * WRITE_BUFFER_BYTES, "{} bytes", sink.bytes.len());
    assert!(sink.largest_write <= WRITE_BUFFER_BYTES && sink.writes > 4, "{} writes", sink.writes);
    assert_eq!(
        (size, digest.as_str()),
        (sink.bytes.len() as u64, crate::digest::sha256_hex(&sink.bytes).as_str())
    );
    let mut short = Recording::new(7);
    short.interrupt_once = true;
    let (short, short_size, short_digest) = stream_to(short, &run, &take).unwrap();
    assert_eq!(short.bytes, sink.bytes, "short and interrupted writes deliver the same bytes");
    assert_eq!((short_size, short_digest), (size, digest), "and hash exactly them");
    let mut failing = Recording::new(usize::MAX);
    failing.fail_after = 100_000;
    assert!(stream_to(failing, &run, &take).is_err());
}

/// Each non-take outcome records its status and the latched window, with every other key null and no file.
#[test]
fn each_status_is_named_with_its_window_and_no_file() {
    let dir = scratch("statuses");
    for (outcome, name) in [
        (GuardTake::<FakeRecords>::Unavailable, "unavailable"),
        (GuardTake::GateOff, "gate_off"),
        (GuardTake::TakeExhausted, "take_exhausted"),
    ] {
        let field =
            transport(&dir, &binding(NONCE, None, 0, "startup"), &outcome, |_, _| unreachable!());
        assert_eq!(
            serde_json::to_value(&field).unwrap(),
            serde_json::json!({"status": name, "file": null, "take_seq": null, "bytes": null, "sha256": null,
                "window": {"label": WINDOW_LABEL, "start_ns": 100, "end_ns": 900}})
        );
    }
    assert!(!dir.join(SIDECAR_DIR).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// H3 and D′: without the cfg the App is never called and the epoch converts nothing; with it, a counting-off
/// App answers `gate_off` and an instant read after the bootstrap converts as located.
#[test]
fn the_adapter_follows_the_cfg() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut app =
        sonicterm_app::app::App::new(Theme::default(), Config::default(), Keymap::default());
    api::bootstrap_epoch();
    let after = std::time::Instant::now();
    let outcome = api::take(&mut app);
    if API_ENABLED {
        assert!(matches!(outcome, GuardTake::GateOff));
        assert!(api::epoch_ns(after).is_some(), "startup's start converts as located");
    } else {
        assert!(matches!(outcome, GuardTake::Unavailable));
        assert_eq!(api::epoch_ns(after), None);
    }
}

/// The real adapter serializes the App's transferred buffers in place: streaming a take of 10 or 20,000
/// sections and spans allocates exactly the 64 KiB buffer and the 64-character digest, so no copy of the
/// records exists at the peak.
#[cfg(perf_guard_spans_api)]
#[test]
fn the_real_adapter_streams_the_apps_buffers_without_copying() {
    use sonicterm_app::app::{
        GuardCorrelationV1, LossIntervalV1, PaneSectionsV1, SectionRecordV1, SpanBatchV1,
        SpanRecordV1,
    };
    let take_of = |count: u64| GuardCorrelationV1 {
        clock_epoch: std::time::Instant::now(),
        take_seq: 1,
        taken_at_ns: Some(1),
        panes: vec![PaneSectionsV1 {
            pane_id: 7,
            closed: false,
            identities_exhausted: false,
            prev_next_section_seq: 1,
            next_section_seq: count + 1,
            carried_pending: None,
            records: (1..=count)
                .map(|seq| SectionRecordV1 {
                    section_seq: seq,
                    before_lock_ns: seq,
                    locked_at_ns: seq,
                })
                .collect(),
            pending: None,
            abandoned: Vec::new(),
            abandoned_overflow: 0,
            abandoned_unlocated: 0,
            issued_dropped_located: 0,
            issued_dropped_unlocated: 0,
            losses: vec![LossIntervalV1 { from_ns: 1, to_ns: 2 }],
            loss_events: 0,
            losses_merged: false,
            refused_closed: 0,
            refused_exhausted: 0,
            refused_unlocated: 0,
            first_refusal_ns: None,
        }],
        spans: SpanBatchV1 {
            spans: (1..=count)
                .map(|seq| SpanRecordV1 {
                    pane_id: 7,
                    collection_seq: seq,
                    acquired_ns: seq,
                    released_ns: seq,
                })
                .collect(),
            spans_issued: count,
            spans_dropped_located: 0,
            spans_dropped_unlocated: 0,
            losses: Vec::new(),
            loss_events: 0,
            losses_merged: false,
            collections_issued: count,
            collections_refused_exhausted: 0,
            prev_next_collection_seq: 1,
            next_collection_seq: count + 1,
            identities_exhausted: false,
            open_collections: 0,
        },
    };
    let run = binding(NONCE, Some("hh"), 0, "startup");
    for count in [10_u64, 20_000] {
        let take = take_of(count);
        let (streamed, allocations) = crate::test_allocator::allocations_during(|| {
            stream_to(io::sink(), &run, &take).map(|(_, size, _)| size)
        });
        assert!(streamed.unwrap() > count * 40, "count {count}");
        assert_eq!(
            allocations, 2,
            "count {count}: only the write buffer and the digest are allocated"
        );
    }
}

/// H1, H1b, H1c, H2 (source pins, beside the runtime tests in probe_tests): each route transports after its
/// endpoints are latched, and the transport time is assigned only to its own field.
#[test]
fn every_route_transports_after_its_endpoints() {
    let probe = include_str!("probe.rs").replace("\r\n", "\n");
    // Each body is found by its full signature head, so PhaseMeter's own `finish` is never taken for it.
    let body = |head: &str| {
        let start = probe.find(&format!("    fn {head}")).expect(head);
        let end =
            probe[start + 1..].find("\n    fn ").map_or(probe.len(), |offset| start + 1 + offset);
        probe[start..end].to_owned()
    };
    let order = |text: &str, marks: &[&str]| {
        let found: Vec<usize> = marks.iter().map(|mark| text.find(mark).expect(mark)).collect();
        assert!(found.windows(2).all(|pair| pair[0] < pair[1]), "{marks:?}: {found:?}");
    };
    order(
        &body("finalize_startup(&mut self"),
        &["self.finish_meter();", "self.transport_guard_correlation();", "self.record_progress();"],
    );
    order(
        &body("finalize_phase(&mut self"),
        &[
            "self.finish_meter();",
            "self.phase_ends.push(",
            "self.delivery_blocked()",
            "self.transport_guard_correlation();",
            "self.record_progress();",
        ],
    );
    order(
        &body("finalize_run(&mut self"),
        &["self.finish_meter();", "self.transport_guard_correlation();", "self.outcome = Some("],
    );
    order(
        &body("finish_meter(&mut self"),
        &[
            "self.counter_totals();",
            "let counters_end_at = self.guard_now();",
            "meter.finish(",
            "self.guard_window = Some(",
        ],
    );
    order(
        &body("transport_guard_correlation(&mut self"),
        &[
            "self.guard_window.take()",
            "let transport_started = self.guard_now();",
            "api::take(",
            "run_nonce()",
        ],
    );
    assert_eq!(
        probe.matches("guard_correlation_transport_ns =").count(),
        1,
        "assigned once, to its own field"
    );
    assert!(probe.find("api::bootstrap_epoch();").unwrap() < probe.find("\"startup\",\n").unwrap());
}
