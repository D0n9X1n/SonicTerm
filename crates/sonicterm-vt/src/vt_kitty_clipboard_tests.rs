use super::*;

/// Parse `raw`, expecting valid metadata.
fn parsed(raw: &str) -> KittyClipboardMetadata {
    parse_kitty_clipboard_metadata(raw.as_bytes()).expect("valid metadata")
}

/// Contract: a record without `=` (empty metadata included), a missing `type` and an unknown
/// `type` are structure errors, which the host drops with no reply even while a write is open.
#[test]
fn structure_error_drops_even_mid_write() {
    let structure = Err(KittyClipboardMetadataError::Structure);
    for raw in ["", "type=wdata:mime", "mime=dGV4dA==", "type=paste", "type=write::id=a"] {
        assert_eq!(parse_kitty_clipboard_metadata(raw.as_bytes()), structure, "{raw:?}");
    }
}

/// Contract: a `mime`, `name` or `pw` that is not strict padded Base64 of UTF-8, or a `mime` or
/// `name` over its bound, is a decoded-field error naming the packet's operation, so the host can
/// finish an open write with `EINVAL` for `wdata`/`walias` and drop anything else.
#[test]
fn decoded_field_error_reports_its_operation() {
    let too_long = base64::engine::general_purpose::STANDARD.encode("m".repeat(257));
    for (raw, operation) in [
        ("type=wdata:mime=dGV4dA", KittyClipboardOperation::Wdata),
        ("type=walias:mime=dGV4\ndA==", KittyClipboardOperation::Walias),
        ("type=write:name=//4=", KittyClipboardOperation::Write),
        ("type=read:pw=!!!!", KittyClipboardOperation::Read),
        (&format!("type=wdata:mime={too_long}"), KittyClipboardOperation::Wdata),
        (&format!("type=wdata:name={too_long}"), KittyClipboardOperation::Wdata),
    ] {
        let expected = Err(KittyClipboardMetadataError::InvalidValue(operation));
        assert_eq!(parse_kitty_clipboard_metadata(raw.as_bytes()), expected, "{raw:?}");
    }
}

/// Contract: a `pw` over 128 decoded bytes is cleared, not an error, and `Debug` never prints a
/// password or name, only their lengths.
#[test]
fn pw_overflow_clears() {
    let long = base64::engine::general_purpose::STANDARD.encode("p".repeat(129));
    assert_eq!(parsed(&format!("type=read:pw={long}")).password, "");
    let metadata = parsed("type=read:pw=c2VjcmV0:name=YXBw");
    assert_eq!((metadata.password.as_str(), metadata.name.as_str()), ("secret", "app"));
    let printed = format!("{metadata:?}");
    assert!(!printed.contains("secret") && !printed.contains("app"), "{printed}");
}

/// Contract: a duplicate key keeps its last value, unknown keys are ignored, and `loc=primary`
/// selects PRIMARY while any other location is the standard clipboard.
#[test]
fn duplicate_key_last_wins() {
    let metadata = parsed("type=read:mime=YQ==:color=red:type=wdata:mime=dGV4dA==:loc=primary");
    assert_eq!(metadata.operation, KittyClipboardOperation::Wdata);
    assert_eq!(metadata.mime, "text");
    assert_eq!(metadata.location, KittyClipboardLocation::Primary);
    assert_eq!(parsed("type=write:loc=clip").location, KittyClipboardLocation::Standard);
}

/// Contract: an `id` keeps only `[A-Za-z0-9-_+.]`, at most 512 bytes, so a reply echoing it can
/// carry no separator, terminator or control byte.
#[test]
fn id_sanitized_and_bounded() {
    assert_eq!(parsed("type=write:id=a b;c\u{1b}\\d-_+.9").id, "abcd-_+.9");
    let long = "x".repeat(600);
    assert_eq!(parsed(&format!("type=write:id={long}")).id.len(), MAX_KITTY_CLIPBOARD_ID_BYTES);
}

/// Contract: an over-cap packet is classified only from complete, structurally valid metadata,
/// using the last `type`; metadata past 4 KiB or with a bad record classifies as nothing, so an
/// open write is left unchanged. Field values are not decoded for this.
#[test]
fn overflow_classified_only_from_complete_valid_metadata() {
    let classify = |raw: &str| kitty_clipboard_operation(raw.as_bytes());
    assert_eq!(classify("type=wdata:mime=dGV4dA=="), Some(KittyClipboardOperation::Wdata));
    assert_eq!(classify("type=read:type=walias"), Some(KittyClipboardOperation::Walias));
    assert_eq!(classify("type=wdata:mime=!!"), Some(KittyClipboardOperation::Wdata));
    assert_eq!(classify("type=wdata:broken"), None);
    let truncated = format!("type=wdata:id={}", "a".repeat(MAX_KITTY_CLIPBOARD_METADATA_BYTES));
    assert_eq!(classify(&truncated), None);
    assert_eq!(
        parse_kitty_clipboard_metadata(truncated.as_bytes()),
        Err(KittyClipboardMetadataError::Structure)
    );
}

/// Contract: a status reply names its operation and status, then `loc=primary` and the sanitized
/// id when present, and ends with the request's terminator; a read answers `EPERM`.
#[test]
fn status_reply_echoes_location_id_and_terminator() {
    let write = KittyClipboardOperation::Write;
    let standard = KittyClipboardLocation::Standard;
    let reply =
        kitty_clipboard_status_reply(write, KittyClipboardStatus::Done, standard, "", false);
    assert_eq!(reply, b"\x1b]5522;type=write:status=DONE\x1b\\");
    let primary = KittyClipboardLocation::Primary;
    let reply =
        kitty_clipboard_status_reply(write, KittyClipboardStatus::Efbig, primary, "a1", true);
    assert_eq!(reply, b"\x1b]5522;type=write:status=EFBIG:loc=primary:id=a1\x07");
    let read = KittyClipboardOperation::Read;
    let reply =
        kitty_clipboard_status_reply(read, KittyClipboardStatus::Eperm, standard, "", false);
    assert_eq!(reply, b"\x1b]5522;type=read:status=EPERM\x1b\\");
}

/// The OSC 5522 events one `advance` raised, in order.
fn kitty_events(events: Vec<VtEvent>) -> Vec<(u64, KittyClipboardRequest, bool)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::KittyClipboard { stream, request, bel } => Some((stream, request, bel)),
            _ => None,
        })
        .collect()
}

/// A parser on a private staging pool, so capture accounting is this test's alone.
fn capture_parser() -> Parser {
    Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new())
}

/// Contract: the raw collector owns a whole `5522;` packet; its encoded payload waits in the kitty
/// inbox under the event's stream id, the event carries no payload byte, and text after ST prints.
#[test]
fn packet_payload_waits_in_inbox_and_event_carries_none() {
    let mut parser = capture_parser();
    let events = parser.advance(b"\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;aGk=\x1b\\after");
    let printed = format!("{events:?}");
    let [(stream, request, bel)] = kitty_events(events).try_into().expect("one packet");
    let KittyClipboardRequest::Packet { metadata, payload: true } = request else {
        panic!("a valid packet with a payload, got {request:?}");
    };
    assert_eq!(
        (metadata.operation, metadata.mime.as_str(), bel),
        (KittyClipboardOperation::Wdata, "text/plain", false)
    );
    assert_eq!(parser.kitty_clipboard_inbox().pop(stream).as_deref(), Some(&b"aGk="[..]));
    assert!(!printed.contains("aGk="), "{printed}");
    assert!(crate::vt::vt_test_support::row_text(&parser, 0).starts_with("after"));
}

/// Contract: BEL ends a packet and is reported so the reply uses it; a packet without `;` has no
/// payload and queues nothing.
#[test]
fn bel_terminated_packet_without_payload() {
    let mut parser = capture_parser();
    let [(_, request, bel)] =
        kitty_events(parser.advance(b"\x1b]5522;type=write:id=a1\x07")).try_into().expect("one");
    let KittyClipboardRequest::Packet { metadata, payload: false } = request else {
        panic!("a packet without payload, got {request:?}");
    };
    assert_eq!(
        (metadata.operation, metadata.id.as_str(), bel),
        (KittyClipboardOperation::Write, "a1", true)
    );
    assert!(parser.kitty_clipboard_inbox().is_empty());
}

/// Contract: CAN or SUB drops the packet whole (no event, nothing queued), and a structure error
/// raises nothing, so the host never sees part of a packet.
#[test]
fn cancelled_or_malformed_packet_raises_nothing() {
    let mut parser = capture_parser();
    let mut events = parser.advance(b"\x1b]5522;type=wdata:mime=dGV4dA==;aGk=\x18x");
    events.extend(parser.advance(b"\x1b]5522;type=wdata:broken;aGk=\x1b\\"));
    events.extend(parser.advance(b"\x1b]5522;\x07"));
    assert!(kitty_events(events).is_empty());
    assert!(parser.kitty_clipboard_inbox().is_empty());
    assert!(crate::vt::vt_test_support::row_text(&parser, 0).starts_with('x'));
}

/// Contract: a decoded-field error reaches the host as the packet's operation only, so it can
/// finish an open write with `EINVAL`; its payload is not queued.
#[test]
fn invalid_value_reports_operation_without_payload() {
    let mut parser = capture_parser();
    let [(_, request, _)] =
        kitty_events(parser.advance(b"\x1b]5522;type=walias:mime=!!;eA==\x1b\\"))
            .try_into()
            .expect("one");
    assert_eq!(request, KittyClipboardRequest::InvalidValue(KittyClipboardOperation::Walias));
    assert!(parser.kitty_clipboard_inbox().is_empty());
}

/// Feed `metadata`, then `;` and enough payload to pass the 1 MiB wire cap, then ST.
fn over_cap(parser: &mut Parser, metadata: &str) -> Vec<VtEvent> {
    let mut events = parser.advance(format!("\x1b]5522;{metadata};").as_bytes());
    let filler = vec![b'A'; 64 * 1024];
    for _ in 0..(MAX_KITTY_CLIPBOARD_PACKET_BYTES / filler.len() + 1) {
        events.extend(parser.advance(&filler));
    }
    events.extend(parser.advance(b"\x1b\\after"));
    events
}

/// Contract: a packet past the 1 MiB wire cap with complete, valid `wdata` metadata becomes one
/// payload-free overflow event (the host answers `EFBIG`); nothing is queued or printed and the
/// collector releases what it held.
#[test]
fn overflow_with_complete_valid_wdata_metadata_efbig() {
    let mut parser = capture_parser();
    let idle = parser.retained_amount().bytes;
    let events = over_cap(&mut parser, "type=wdata:mime=dGV4dA==");
    let [(_, request, _)] = kitty_events(events).try_into().expect("one");
    assert_eq!(request, KittyClipboardRequest::Overflow(KittyClipboardOperation::Wdata));
    assert!(parser.kitty_clipboard_inbox().is_empty());
    assert_eq!(parser.retained_amount().bytes, idle);
    assert!(crate::vt::vt_test_support::row_text(&parser, 0).starts_with("after"));
}

/// Contract: metadata with no `;` within 4 KiB is truncated, so the packet is dropped with no
/// event and an open write is left unchanged; its tail never prints.
#[test]
fn overflow_with_truncated_metadata_dropped_write_untouched() {
    let mut parser = capture_parser();
    let mut events = parser.advance(b"\x1b]5522;type=wdata:id=");
    events.extend(parser.advance(&vec![b'a'; MAX_KITTY_CLIPBOARD_METADATA_BYTES + 10]));
    events.extend(parser.advance(b"\x1b\\after"));
    assert!(kitty_events(events).is_empty());
    assert!(crate::vt::vt_test_support::row_text(&parser, 0).starts_with("after"));
}

/// Contract: an over-cap packet is classified by its last `type`; metadata with a bad record
/// classifies as nothing and raises no event.
#[test]
fn overflow_duplicate_type_uses_last_valid() {
    let mut parser = capture_parser();
    let [(_, request, _)] =
        kitty_events(over_cap(&mut parser, "type=read:type=walias:mime=dGV4dA=="))
            .try_into()
            .expect("one");
    assert_eq!(request, KittyClipboardRequest::Overflow(KittyClipboardOperation::Walias));
    assert!(kitty_events(over_cap(&mut parser, "type=wdata:broken")).is_empty());
}

/// Contract: an open packet's held bytes count in `retained_amount`, and its end releases them.
#[test]
fn open_packet_counts_in_retained_amount() {
    let mut parser = capture_parser();
    let idle = parser.retained_amount().bytes;
    parser.advance(b"\x1b]5522;type=wdata:mime=dGV4dA==;");
    parser.advance(&[b'A'; 4096]);
    assert!(parser.retained_amount().bytes >= idle + 4096);
    parser.advance(b"\x07");
    assert_eq!(parser.retained_amount().bytes, idle);
}

/// Contract: 5522 is not advertised: DECRQM `?5522` answers 0, mode not recognised.
#[test]
fn decrqm_5522_zero() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(80, 24), tx);
    parser.advance(b"\x1b[?5522$p");
    assert_eq!(rx.try_recv().expect("a reply"), b"\x1b[?5522;0$y");
}
