use super::*;

/// One `GetProperty` reply of `kind`.
fn reply(kind: PropertyKind, format: u8, bytes_after: u32, value: &[u8]) -> PropertyReply {
    PropertyReply { kind, format, bytes_after, value: value.to_vec() }
}

/// Contract: a text reply must be format 8 of `UTF8_STRING`, `STRING` or
/// `text/plain;charset=utf-8` with nothing left after it: at the cap it is read, one byte over
/// it (or with bytes after) is `TooLarge`, and the fetch asks one unit past the cap so the extra
/// byte is visible.
#[test]
fn x11_text_reply_one_unit_over_cap_too_large() {
    let max = 8;
    assert!(fetch_length_units(max) as usize * 4 > max);
    let done = |outcome| FirstReply::Done(outcome);
    let text = |value: &str| done(ReadOutcome::Text(value.into()));
    assert_eq!(
        classify_first(&reply(PropertyKind::Utf8, 8, 0, b"12345678"), max),
        text("12345678")
    );
    let over = classify_first(&reply(PropertyKind::Utf8, 8, 0, b"123456789"), max);
    assert_eq!(over, done(ReadOutcome::TooLarge));
    let after = classify_first(&reply(PropertyKind::Utf8, 8, 4, b"12345678"), max);
    assert_eq!(after, done(ReadOutcome::TooLarge));
    let wide = classify_first(&reply(PropertyKind::Utf8, 32, 0, b"1234"), max);
    assert_eq!(wide, done(ReadOutcome::Failed));
    let other = classify_first(&reply(PropertyKind::Other, 8, 0, b"1234"), max);
    assert_eq!(other, done(ReadOutcome::Failed));
    assert_eq!(classify_first(&reply(PropertyKind::Latin1, 8, 0, &[0xE9]), max), text("é"));
    let empty = classify_first(&reply(PropertyKind::Utf8, 8, 0, b""), max);
    assert_eq!(empty, done(ReadOutcome::Empty));
}

/// Contract: an `INCR` announcement is format 32 holding the lower-bound size (ICCCM 2.7.2):
/// within the cap the transfer starts, above it is `TooLarge`, and any other shape fails.
#[test]
fn x11_incr_format32_announcement_accepted() {
    let announce =
        |format, value: &[u8]| classify_first(&reply(PropertyKind::Incr, format, 0, value), 8);
    assert_eq!(announce(32, &8u32.to_ne_bytes()), FirstReply::Incr);
    assert_eq!(announce(32, &9u32.to_ne_bytes()), FirstReply::Done(ReadOutcome::TooLarge));
    assert_eq!(announce(8, &8u32.to_ne_bytes()), FirstReply::Done(ReadOutcome::Failed));
    assert_eq!(announce(32, &[1, 2]), FirstReply::Done(ReadOutcome::Failed));
}

/// Contract: every `INCR` chunk is format 8 of one text type; chunks accumulate until a
/// zero-length chunk ends the transfer, and passing the cap stops it `TooLarge`.
#[test]
fn x11_incr_chunk_format8_required() {
    let start = Instant::now();
    let mut transfer = IncrTransfer::new(8, start);
    assert_eq!(transfer.push(reply(PropertyKind::Utf8, 8, 0, b"1234")), None);
    assert_eq!(transfer.remaining(), 4);
    let done = transfer.push(reply(PropertyKind::Utf8, 8, 0, b""));
    assert_eq!(done, Some(ReadOutcome::Text("1234".into())));

    let mut wide = IncrTransfer::new(8, start);
    assert_eq!(wide.push(reply(PropertyKind::Utf8, 32, 0, b"1234")), Some(ReadOutcome::Failed));
    let mut mixed = IncrTransfer::new(8, start);
    assert_eq!(mixed.push(reply(PropertyKind::Utf8, 8, 0, b"12")), None);
    assert_eq!(mixed.push(reply(PropertyKind::Latin1, 8, 0, b"3")), Some(ReadOutcome::Failed));
    let mut over = IncrTransfer::new(8, start);
    assert_eq!(over.push(reply(PropertyKind::Utf8, 8, 0, b"12345")), None);
    assert_eq!(over.push(reply(PropertyKind::Utf8, 8, 0, b"6789")), Some(ReadOutcome::TooLarge));
    let mut after = IncrTransfer::new(8, start);
    let truncated = after.push(reply(PropertyKind::Utf8, 8, 1, b"12"));
    assert_eq!(truncated, Some(ReadOutcome::TooLarge), "a chunk longer than the room left");
}

/// Contract: each `INCR` chunk waits at most 2 s, and never past 5 s from the transfer's start.
#[test]
fn incr_per_chunk_deadline() {
    let start = Instant::now();
    let transfer = IncrTransfer::new(8, start);
    assert_eq!(transfer.chunk_deadline(start), start + CHUNK_WAIT);
    assert_eq!(transfer.chunk_deadline(start + Duration::from_secs(4)), start + INCR_TOTAL_WAIT);
}
