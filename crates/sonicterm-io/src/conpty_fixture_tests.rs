use super::*;

/// The streaming digest matches the FIPS 180-2 vectors, so an `exact` row means byte equality.
#[test]
fn sha256_matches_known_vectors() {
    let hex =
        |digest: [u8; 32]| digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    assert_eq!(
        hex(Sha256::digest(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(Sha256::digest(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(Sha256::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

/// Hashing in arbitrary pieces equals hashing whole, so a payload split across PTY reads still
/// classifies by its bytes alone.
#[test]
fn sha256_is_independent_of_chunking() {
    let payload: Vec<u8> = (0..1000u32).map(|index| (index * 7 % 251) as u8).collect();
    let mut pieces = Sha256::default();
    for piece in payload.chunks(13) {
        pieces.update(piece);
    }
    assert_eq!(pieces.finish(), Sha256::digest(&payload));
}

/// Feed `stream` to a fresh scanner `step` bytes at a time and return every event.
fn scan(stream: &[u8], step: usize) -> Vec<ScanEvent> {
    let mut scanner = Scanner::default();
    let mut events = Vec::new();
    for piece in stream.chunks(step) {
        events.extend(scanner.feed(piece));
    }
    events
}

/// The section's evidence, from the one `Closed` event the stream produced.
fn closed(events: &[ScanEvent]) -> &Evidence {
    let found: Vec<&Evidence> = events
        .iter()
        .filter_map(|event| match event {
            ScanEvent::Closed(_, evidence) => Some(evidence),
            _ => None,
        })
        .collect();
    assert_eq!(found.len(), 1, "exactly one section closes: {events:?}");
    found[0]
}

/// Sentinels are found whatever the read boundaries, including one byte at a time, and the
/// section's evidence covers exactly the bytes between them.
#[test]
fn scanner_finds_sentinels_split_across_reads() {
    let stream = b"noise\x1b[?25l@@BEGIN case/out@@\x1b]4;1;?\x07@@END case/out@@\r\n@@DONE@@";
    for step in [1, 2, 3, 7, stream.len()] {
        let events = scan(stream, step);
        assert!(events.contains(&ScanEvent::Opened("case/out".into())), "step {step}");
        let evidence = closed(&events);
        assert_eq!(evidence.raw_len, 8, "step {step}");
        assert_eq!(evidence.raw_digest, Sha256::digest(b"\x1b]4;1;?\x07"), "step {step}");
        assert_eq!(events.last(), Some(&ScanEvent::Done), "step {step}");
    }
}

/// A section that never sees its own END, because another BEGIN or DONE arrives first, is
/// abandoned rather than closed, so a lost end sentinel reads as inconclusive.
#[test]
fn scanner_abandons_a_section_without_its_end_sentinel() {
    let events = scan(b"@@BEGIN one/out@@abc@@BEGIN two/out@@x@@END two/out@@@@DONE@@", 4);
    assert!(events.contains(&ScanEvent::Abandoned("one/out".into())));
    assert_eq!(closed(&events).raw_digest, Sha256::digest(b"x"));
    let unfinished = scan(b"@@BEGIN one/out@@abc@@DONE@@", 5);
    assert!(unfinished.contains(&ScanEvent::Abandoned("one/out".into())));
}

/// Only the first and last 256 bytes of a long section are kept; the rest feeds the running
/// length and digest, so no whole-payload buffer exists.
#[test]
fn scanner_keeps_only_bounded_head_and_tail() {
    let body: Vec<u8> = (0..10_000u32).map(|index| b'a' + (index % 26) as u8).collect();
    let mut stream = b"@@BEGIN big/out@@".to_vec();
    stream.extend_from_slice(&body);
    stream.extend_from_slice(b"@@END big/out@@");
    let events = scan(&stream, 4096);
    let evidence = closed(&events);
    assert_eq!(evidence.raw_len, body.len());
    assert_eq!(evidence.head, body[..KEEP_BYTES]);
    assert_eq!(evidence.tail, body[body.len() - KEEP_BYTES..]);
    assert_eq!(evidence.raw_digest, Sha256::digest(&body));
}

/// The text view drops escape sequences and line breaks, so a console's cursor decoration of
/// the child's printable report cannot change what the report says.
#[test]
fn text_view_strips_escape_sequences_and_line_breaks() {
    let events =
        scan(b"@@BEGIN echo/in@@\x1b[?25l70\x1b[1;5H69\r\n\x1b]0;title\x076e67@@END echo/in@@", 3);
    let evidence = closed(&events);
    assert_eq!(evidence.text_len, 8);
    assert_eq!(evidence.text_digest, Sha256::digest(b"70696e67"));
}

/// `ROW-` labels are counted in the text view even when a read splits one, which is how the
/// prompt-resize row reports what a repaint repeated.
#[test]
fn text_view_counts_row_labels_across_reads() {
    let events = scan(b"@@BEGIN rs/in@@ROW-01\r\nROW-02 ROW-03@@END rs/in@@", 2);
    assert_eq!(closed(&events).row_labels, 3);
}

/// The outcome table: equal digest is `exact`; nothing but CR/LF is `dropped`; any other byte,
/// including a changed prefix, is `transformed`; no evidence is `inconclusive`.
#[test]
fn classification_follows_the_outcome_table() {
    let evidence_for = |body: &[u8]| {
        let mut stream = b"@@BEGIN row/out@@".to_vec();
        stream.extend_from_slice(body);
        stream.extend_from_slice(b"@@END row/out@@");
        closed(&scan(&stream, 5)).clone()
    };
    let wanted = Expected::of(b"\x1b]10;?\x07");
    assert_eq!(classify(Some(&evidence_for(b"\x1b]10;?\x07")), &wanted), Outcome::Exact);
    assert_eq!(classify(Some(&evidence_for(b"\r\n")), &wanted), Outcome::Dropped);
    assert_eq!(classify(Some(&evidence_for(b"")), &wanted), Outcome::Dropped);
    assert_eq!(classify(Some(&evidence_for(b"\x1b]10;?\x1b\\")), &wanted), Outcome::Transformed);
    assert_eq!(classify(Some(&evidence_for(b"\x1b]11;?\x07")), &wanted), Outcome::Transformed);
    assert_eq!(classify(None, &wanted), Outcome::Inconclusive);
}

/// A printable expectation (the control text, an input echo's hex) compares the text view, so
/// console cursor decoration around it stays `exact`; a protocol form with ESC compares raw.
#[test]
fn printable_expectations_compare_the_text_view() {
    let events = scan(b"@@BEGIN ctl/out@@\x1b[?25lCONTROL-OK\x1b[?25h@@END ctl/out@@", 4);
    assert_eq!(classify(Some(closed(&events)), &Expected::of(b"CONTROL-OK")), Outcome::Exact);
    let echoed = scan(b"@@BEGIN ctl/in@@\r\n@@END ctl/in@@", 4);
    assert_eq!(classify(Some(closed(&echoed)), &Expected::of(b"70696e67")), Outcome::Dropped);
}

/// An echoed input reply is reported as lowercase hex, and the expectation is that hex.
#[test]
fn input_echo_expectation_is_lowercase_hex() {
    assert_eq!(hex(b"\x1b[<0;12;34M"), "1b5b3c303b31323b33344d");
}

/// Control lines are single flat JSON objects whose strings are escaped, so the outer test reads
/// them back with `json_field` whatever the payload labels contain.
#[test]
fn json_fields_round_trip() {
    let events = scan(b"@@BEGIN row/out@@ab\x1b@@END row/out@@", 4);
    let line = row_line("osc \"4\"", "out", true, Outcome::Transformed, Some(closed(&events)));
    assert!(line.starts_with(LINE_PREFIX));
    assert_eq!(json_field(&line, "case").as_deref(), Some("osc \"4\""));
    assert_eq!(json_field(&line, "outcome").as_deref(), Some("transformed"));
    assert_eq!(json_field(&line, "required").as_deref(), Some("true"));
    assert_eq!(json_field(&line, "len").as_deref(), Some("3"));
    assert_eq!(json_field(&line, "head").as_deref(), Some("61621b"));
    assert_eq!(json_field(&line, "missing"), None);
}

/// A repaint has no exact form: nothing repainted is `dropped`, any repainted text is
/// `transformed` with its label count, and a missing section is `inconclusive`.
#[test]
fn repaint_classification_records_rather_than_matches() {
    assert_eq!(classify_repaint(None), Outcome::Inconclusive);
    let empty = scan(b"@@BEGIN rs/in@@\r\n@@END rs/in@@", 3);
    assert_eq!(classify_repaint(Some(closed(&empty))), Outcome::Dropped);
    let painted = scan(b"@@BEGIN rs/in@@ROW-41\r\n@@END rs/in@@", 3);
    assert_eq!(classify_repaint(Some(closed(&painted))), Outcome::Transformed);
}
