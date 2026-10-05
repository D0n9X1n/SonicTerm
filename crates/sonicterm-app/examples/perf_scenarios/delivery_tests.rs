//! Pins the delivery replay's classifiers and the record the comparison reads.

use super::*;
use crate::record::Status;
use crate::scenarios::{find, plan_for, Host, Workload};
use crate::workload::{fixtures, FixtureBody};

/// How much larger than the fixture ConPTY's repainted output may grow and still fit the limit.
const REPAINT_ALLOWANCE: usize = 4;

/// The sentinel the tests' streams end with.
const SENTINEL: &str = "PERF_DONE 0 0123456789abcdef";

/// One Windows plan's fixture named `name`, as bytes.
fn fixture_named(scenario: &str, variant: &str, name: &str) -> Vec<u8> {
    let plan = plan_for(scenario, variant, true, Host::Windows).unwrap();
    let file = fixtures(&plan).into_iter().find(|file| file.relative_path == name).unwrap();
    match file.body {
        FixtureBody::Bytes(bytes) => bytes,
        FixtureBody::Repeated { .. } => panic!("{name} is one block"),
    }
}

/// S10's short frames on Windows, in play order.
fn frame_files(synchronized: bool) -> Vec<Vec<u8>> {
    let variant = if synchronized { "sync" } else { "default" };
    let plan = plan_for("S10", variant, true, Host::Windows).unwrap();
    fixtures(&plan)
        .into_iter()
        .map(|file| match file.body {
            FixtureBody::Bytes(bytes) => bytes,
            FixtureBody::Repeated { .. } => panic!("a frame is one file"),
        })
        .collect()
}

#[test]
fn sync_brackets_classify_the_measured_placements() {
    // ConPTY was measured placing DEC 2026 around a frame's paint, as an empty pair just ahead of
    // it, or not at all; a frame it never painted is counted apart from all three.
    let markers: Vec<String> = (1..=4).map(|line| format!("line {line} of 99999")).collect();
    let mut stream = Vec::new();
    stream.extend_from_slice(b"\x1b[?2026h\x1b[70;1Hline 1 of 99999\x1b[?2026l");
    // conhost writes some single spaces as `CSI 1 C`; the marker still reads whole.
    stream.extend_from_slice(b"\x1b[?2026h\x1b[?2026l\x1b[70;1Hline 2\x1b[1Cof 99999");
    stream.extend_from_slice(b"\x1b[70;1Hline 3 of 99999");
    let counts = classify_sync_brackets(&stream, &markers);
    let expected =
        SyncCounts { enclosed: 1, empty_pair_ahead: 1, absent: 1, unseen: 1, brackets: 4 };
    assert_eq!(counts, expected);
    // Every real frame carries its own marker, so the fixture played back whole classifies cleanly.
    let markers = frame_markers(300);
    let synchronized = frame_files(true).concat();
    let counts = classify_sync_brackets(&synchronized, &markers);
    assert_eq!((counts.enclosed, counts.unseen, counts.brackets), (300, 0, 600));
    assert!(sync_check(counts, true).ok);
    assert_eq!(sync_check(counts, true).detail, "enclosed 300, empty pair ahead 0, absent 0");
    assert!(!sync_check(counts, false).ok, "brackets in the default variant pass");
    let plain = frame_files(false).concat();
    let counts = classify_sync_brackets(&plain, &markers);
    assert_eq!((counts.absent, counts.brackets, counts.unseen), (300, 0, 0));
    assert!(sync_check(counts, false).ok);
}

#[test]
fn completion_is_found_across_every_chunk_split() {
    // ConPTY splits its output wherever its buffer ends, so the cursor query, the sentinel and
    // the counted lines must be found whichever byte a chunk ends on.
    let mut stream = b"\x1b[6n\x1b[?25l\x1b[2J\x1b[HREADY 0\r\nalpha\r\nbeta\r\ngamma\r\n".to_vec();
    stream.extend_from_slice(b"PERF_DONE\x1b[1C0\x1b[1C0123456789abcdef\r\nperf$ ");
    let complete_at = stream.windows(4).position(|window| window == b"cdef").unwrap() + 4;
    for split in 0..=stream.len() {
        let (head, tail) = stream.split_at(split);
        let mut watch = SentinelWatch::new(SENTINEL);
        let early = watch.feed(head);
        assert_eq!(early, split >= complete_at, "split {split}");
        assert!(early || watch.feed(tail), "split {split}: the sentinel was missed");
        let mut counter = LineCounter::new("READY 0", SENTINEL);
        counter.feed(head);
        counter.feed(tail);
        assert!(counter.finished(), "split {split}");
        assert_eq!(counter.delivered(), 3, "split {split}");
        let mut query = CursorQuery::new();
        let asked = [query.feed(head), query.feed(tail)];
        assert_eq!(asked.iter().filter(|asked| **asked).count(), 1, "split {split}");
    }
}

#[test]
fn altered_payload_or_missing_token_fails() {
    // S11 passes only when the OSC 1337 payload arrives byte for byte, and S9 only when every
    // wide token its fixture prints arrives; the reasons say what differed.
    let fixture = fixture_named("S11", "default", "inline.osc");
    let expected = sha256_hex(osc1337_payload(&fixture).unwrap());
    let mut wrapped = b"\x1b[?25l\x1b[H".to_vec();
    wrapped.extend_from_slice(&fixture);
    assert_eq!(osc1337_intact(&wrapped, &expected), Ok(()));
    let mut altered = fixture.clone();
    let middle = altered.len() / 2;
    altered[middle] = if altered[middle] == b'A' { b'B' } else { b'A' };
    assert!(osc1337_intact(&altered, &expected).unwrap_err().contains("SHA-256"));
    let unterminated = &fixture[..fixture.len() - 2];
    assert!(osc1337_intact(unterminated, &expected).unwrap_err().contains("never ended"));
    assert!(osc1337_intact(b"no image here", &expected).unwrap_err().contains("no OSC 1337"));
    let text_fixture = fixture_named("S9", "default", "emoji-cjk.txt");
    let text = String::from_utf8(text_fixture).unwrap();
    let tokens = crate::record::wide_tokens(&text);
    let delivered = text.replace('\n', "\r\n");
    assert_eq!(wide_tokens_delivered(delivered.as_bytes(), &tokens), Vec::<&str>::new());
    let dropped = delivered.replace("漢字", "");
    assert_eq!(wide_tokens_delivered(dropped.as_bytes(), &tokens), ["漢字"]);
    let spaced = "😀\x1b[1C漢字".as_bytes();
    assert_eq!(wide_tokens_delivered(spaced, &["😀", "漢字"]), Vec::<&str>::new());
}

#[test]
fn limit_exceeds_every_replayed_plan() {
    // Every replay that keeps its output must fit DELIVERY_LIMIT_BYTES with room for ConPTY's
    // repaint; S3 keeps nothing, because it only counts lines.
    for scenario in ["S3", "S9", "S10", "S11"] {
        for variant in find(scenario).unwrap().variants {
            for short in [false, true] {
                let plan = plan_for(scenario, variant, short, Host::Windows).unwrap();
                if plan.roles.iter().any(|role| matches!(role, Workload::RowRuns { .. })) {
                    // The row-run variants run only in the counters set; their delivery is never replayed.
                    continue;
                }
                if !keeps_output(plan.roles[0]) {
                    assert_eq!(scenario, "S3", "{scenario}/{variant} keeps no output");
                    continue;
                }
                let kept: usize = fixtures(&plan).iter().map(|file| file.byte_len()).sum();
                assert!(
                    REPAINT_ALLOWANCE * kept <= DELIVERY_LIMIT_BYTES,
                    "{scenario}/{variant} short={short}: {kept} bytes"
                );
            }
        }
    }
}

#[test]
fn a_sentinel_with_missing_lines_is_incomplete() {
    // The sentinel ends the count but does not make it right: a dropped line fails the check,
    // and a count with no sentinel fails it too, saying so.
    let mut short = LineCounter::new("READY 0", SENTINEL);
    assert!(short.feed(format!("READY 0\r\none\r\ntwo\r\n{SENTINEL}\r\n").as_bytes()));
    let check = short.check(3);
    assert_eq!((check.name.as_str(), check.ok), ("delivered lines", false));
    assert_eq!(check.detail, "2 delivered, 3 planned");
    let mut unfinished = LineCounter::new("READY 0", SENTINEL);
    assert!(!unfinished.feed(b"READY 0\r\none\r\ntwo\r\nthree\r\n"));
    let check = unfinished.check(3);
    assert!(!check.ok);
    assert_eq!(check.detail, "3 delivered, 3 planned; the sentinel never arrived");
    let mut whole = LineCounter::new("READY 0", SENTINEL);
    whole.feed(format!("READY 0\r\none\r\ntwo\r\nthree\r\n{SENTINEL}\r\n").as_bytes());
    let check = whole.check(3);
    assert!(check.ok);
    assert_eq!(check.detail, "3 delivered, 3 planned");
}

#[test]
fn crlf_split_across_chunks_counts_one_line() {
    // ConPTY ends each line with CR LF, and a chunk can end between the two; that is one line.
    // A bare CR rewrites the line, and conhost's `CSI n C` reads as spaces in the sentinel.
    let mut counter = LineCounter::new("READY 0", SENTINEL);
    for chunk in [&b"READY 0\r"[..], b"\none\r", b"\ntwo\r", b"\n", b"draft\rthree\r\n"] {
        assert!(!counter.feed(chunk));
    }
    assert!(counter.feed(b"PERF_DONE\x1b[1C0\x1b[1C0123456789abcdef\r"));
    counter.feed(b"\nperf$ ");
    assert_eq!(counter.delivered(), 3);
    assert!(counter.check(3).ok);
}

#[test]
fn delivery_json_has_the_schema_the_comparison_reads() {
    // perf-compare.py reads these keys and trusts a record only when its verdict matches the
    // exit code: 0 when every check passed, Blocked's 5 otherwise.
    let detail = "enclosed 300, empty pair ahead 0, absent 0";
    let passed = DeliveryRecord::new(
        "S10",
        "sync",
        4096,
        vec![DeliveryCheck::new("sync brackets", true, detail)],
    );
    assert_eq!(
        passed.to_json(),
        r#"{"schema_version":2,"scenario":"S10","variant":"sync","bytes_kept":4096,"checks":[{"name":"sync brackets","ok":true,"detail":"enclosed 300, empty pair ahead 0, absent 0"}]}"#
    );
    assert_eq!(passed.exit_code(), 0);
    let lines = DeliveryCheck::new("delivered lines", false, "240960 delivered, 240961 planned");
    let failed = DeliveryRecord::new("S3", "default", 0, vec![lines]);
    assert_eq!(
        failed.to_json(),
        r#"{"schema_version":2,"scenario":"S3","variant":"default","bytes_kept":0,"checks":[{"name":"delivered lines","ok":false,"detail":"240960 delivered, 240961 planned"}]}"#
    );
    assert_eq!(failed.exit_code(), Status::Blocked.exit_code());
    // A record with no check proves nothing, so it never passes.
    assert_eq!(DeliveryRecord::new("S9", "default", 0, Vec::new()).exit_code(), 5);
}

#[test]
fn sha256_matches_published_vectors() {
    // The S11 payload check is only as good as this hasher, so it is checked against FIPS 180-4.
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(sha256_hex(b""), empty);
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(sha256_hex(b"abc"), abc);
    let two_blocks = "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";
    assert_eq!(sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"), two_blocks);
    let million_a = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
    assert_eq!(sha256_hex(&vec![b'a'; 1_000_000]), million_a);
}

#[test]
fn a_sync_replay_with_mixed_placements_and_every_frame_seen_passes() {
    // Enclosed, an empty pair ahead and absent are the placements ConPTY was measured making, so a
    // sync replay states them and passes once every frame arrived, whichever placements it saw.
    let markers: Vec<String> = (1..=3).map(|line| format!("line {line} of 99999")).collect();
    let mut stream = b"[?2026h[70;1Hline 1 of 99999[?2026l".to_vec();
    stream.extend_from_slice(b"[?2026h[?2026l[70;1Hline 2 of 99999");
    stream.extend_from_slice(b"[70;1Hline 3 of 99999");
    let counts = classify_sync_brackets(&stream, &markers);
    assert_eq!(
        (counts.enclosed, counts.empty_pair_ahead, counts.absent, counts.unseen),
        (1, 1, 1, 0)
    );
    let check = sync_check(counts, true);
    assert!(check.ok, "{}", check.detail);
    assert_eq!(check.detail, "enclosed 1, empty pair ahead 1, absent 1");
    // The counts this host's replay measured pass too.
    let measured =
        SyncCounts { enclosed: 13, empty_pair_ahead: 137, absent: 150, unseen: 0, brackets: 300 };
    assert!(sync_check(measured, true).ok);
    assert_eq!(sync_check(measured, true).detail, "enclosed 13, empty pair ahead 137, absent 150");
}

#[test]
fn a_replay_with_an_unseen_frame_fails() {
    // A frame ConPTY never painted cannot be classified, so it fails both variants, and the
    // detail names how many.
    let unseen = SyncCounts { enclosed: 2, empty_pair_ahead: 0, absent: 0, unseen: 1, brackets: 4 };
    let check = sync_check(unseen, true);
    assert!(!check.ok);
    assert_eq!(check.detail, "enclosed 2, empty pair ahead 0, absent 0, never painted 1");
    let plain = SyncCounts { enclosed: 0, empty_pair_ahead: 0, absent: 2, unseen: 1, brackets: 0 };
    assert!(!sync_check(plain, false).ok, "a default replay with an unpainted frame passed");
}

/// S10's frame count and synchronized flag in the Windows plan for `variant` at `short` length.
fn frames_workload(variant: &str, short: bool) -> (u32, bool) {
    let plan = plan_for("S10", variant, short, Host::Windows).unwrap();
    match plan.roles[0] {
        Workload::Frames { count, synchronized } => (count, synchronized),
        other => panic!("S10 plays frames, not {other:?}"),
    }
}

#[test]
fn the_sync_check_record_names_its_unseen_frames() {
    // The comparison retries only a missing-frame failure it can prove from structured fields, so the
    // record carries the unseen count, every bracket delivered and the first eight missing markers.
    let markers: Vec<String> = (1..=12).map(|line| format!("line {line} of 99999")).collect();
    let mut stream = b"\x1b[?2026h\x1b[70;1Hline 1 of 99999\x1b[?2026l".to_vec();
    stream.extend_from_slice(b"\x1b[70;1Hline 12 of 99999");
    let check = frames_check(&stream, &markers, true);
    assert!(!check.ok);
    assert_eq!(check.detail, "enclosed 1, empty pair ahead 0, absent 1, never painted 10");
    let missing: Vec<String> = (2..=9).map(|line| format!("line {line} of 99999")).collect();
    assert_eq!(
        check.frames,
        Some(FrameEvidence { unseen: 10, brackets: 2, unseen_markers: missing.clone() })
    );
    let json: serde_json::Value = serde_json::from_str(
        &DeliveryRecord::new("S10", "sync", stream.len() as u64, vec![check]).to_json(),
    )
    .unwrap();
    let written = &json["checks"][0];
    assert_eq!(json["schema_version"], 2);
    assert_eq!((written["unseen"].as_u64(), written["brackets"].as_u64()), (Some(10), Some(2)));
    assert_eq!(written["unseen_markers"], serde_json::json!(missing));
    // A check about anything but frames writes none of those fields.
    let lines = DeliveryCheck::new("delivered lines", true, "3 delivered, 3 planned");
    let plain = DeliveryRecord::new("S3", "default", 0, vec![lines]).to_json();
    assert!(!plain.contains("unseen") && !plain.contains("brackets"), "{plain}");
}

#[test]
fn frame_evidence_leaves_every_verdict_unchanged() {
    // The structured fields are evidence only: for every S10 fixture, and for the hand-built streams the
    // classifier tests use, the check passes or fails with exactly the detail sync_check gives.
    for variant in ["default", "sync"] {
        for short in [false, true] {
            let (count, synchronized) = frames_workload(variant, short);
            let plan = plan_for("S10", variant, short, Host::Windows).unwrap();
            let played: Vec<u8> = fixtures(&plan)
                .into_iter()
                .flat_map(|file| match file.body {
                    FixtureBody::Bytes(bytes) => bytes,
                    FixtureBody::Repeated { .. } => panic!("a frame is one file"),
                })
                .collect();
            let markers = frame_markers(count);
            let check = frames_check(&played, &markers, synchronized);
            let expected = sync_check(classify_sync_brackets(&played, &markers), synchronized);
            let label = format!("{variant} short={short}");
            assert_eq!((check.ok, &check.detail), (expected.ok, &expected.detail), "{label}");
            assert!(check.ok, "{label}: {}", check.detail);
            let placement = if synchronized { "enclosed" } else { "absent" };
            assert!(
                check.detail.contains(&format!("{placement} {count}")),
                "{label}: {}",
                check.detail
            );
            let brackets = if synchronized { 2 * count as usize } else { 0 };
            let evidence = FrameEvidence { unseen: 0, brackets, unseen_markers: Vec::new() };
            assert_eq!(check.frames, Some(evidence), "{label}");
        }
    }
    let markers: Vec<String> = (1..=4).map(|line| format!("line {line} of 99999")).collect();
    let mut stream = b"\x1b[?2026h\x1b[70;1Hline 1 of 99999\x1b[?2026l".to_vec();
    stream.extend_from_slice(b"\x1b[?2026h\x1b[?2026l\x1b[70;1Hline 2\x1b[1Cof 99999");
    stream.extend_from_slice(b"\x1b[70;1Hline 3 of 99999");
    for synchronized in [true, false] {
        let check = frames_check(&stream, &markers, synchronized);
        let expected = sync_check(classify_sync_brackets(&stream, &markers), synchronized);
        assert_eq!((check.ok, check.detail), (expected.ok, expected.detail));
    }
    let unseen = frames_check(&stream, &markers, true).frames.unwrap();
    assert_eq!(unseen.unseen_markers, ["line 4 of 99999"]);
}
