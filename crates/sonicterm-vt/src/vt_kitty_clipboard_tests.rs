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
