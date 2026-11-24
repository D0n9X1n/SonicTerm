use super::*;
use base64::engine::general_purpose::STANDARD;
use sonicterm_vt::vt::parse_kitty_clipboard_metadata;
use std::sync::Mutex;

/// A fake clipboard for reads: what a listing finds, what a text read finds, and every cap a read
/// asked with. With `forbid_reads`, a content read panics, so a test proves none happened.
struct FakeTypes {
    listing: TypeListing,
    text: ReadOutcome,
    caps: Mutex<Vec<usize>>,
    forbid_reads: bool,
}

impl FakeTypes {
    fn holding(text: &str) -> Self {
        Self {
            listing: TypeListing::Types(vec!["text/plain".into()]),
            text: ReadOutcome::Text(text.into()),
            caps: Mutex::default(),
            forbid_reads: false,
        }
    }

    fn caps(&self) -> Vec<usize> {
        self.caps.lock().unwrap().clone()
    }
}

impl ClipboardTextSource for FakeTypes {
    fn read_text_bounded(&self, _location: ReadLocation, max_bytes: usize) -> ReadOutcome {
        assert!(!self.forbid_reads, "a listing read clipboard content");
        self.caps.lock().unwrap().push(max_bytes);
        match &self.text {
            ReadOutcome::Text(text) if text.len() > max_bytes => ReadOutcome::TooLarge,
            other => other.clone(),
        }
    }

    fn list_types(&self, _location: ReadLocation) -> TypeListing {
        self.listing.clone()
    }
}

fn b64(text: &str) -> String {
    STANDARD.encode(text)
}

/// The ST-terminated read request `metadata` makes with `types` (space-separated) as its payload.
fn request(metadata: &str, types: &str) -> KittyReadRequest {
    let metadata = parse_kitty_clipboard_metadata(metadata.as_bytes()).expect("valid metadata");
    parse_read_request(&metadata, b64(types).as_bytes(), false).expect("a decodable payload")
}

/// One ST-terminated read packet whose text after `status=` is `body`.
fn packet(body: &str) -> String {
    format!("\x1b]5522;type=read:status={body}\x1b\\")
}

fn reply_text(source: &FakeTypes, request: &KittyReadRequest) -> String {
    String::from_utf8(build_read_reply(source, request)).expect("ASCII reply")
}

/// The DATA packets of `reply`, each as its metadata and decoded payload.
fn data_packets(reply: &str) -> Vec<(String, Vec<u8>)> {
    reply
        .split("\x1b\\")
        .filter(|packet| packet.contains("status=DATA"))
        .map(|packet| match packet.split_once(';').and_then(|(_, rest)| rest.split_once(';')) {
            Some((metadata, payload)) => (metadata.to_owned(), STANDARD.decode(payload).unwrap()),
            None => (packet.to_owned(), Vec::new()),
        })
        .collect()
}

/// Contract: a listing-only request answers OK, one DATA packet whose `mime` is `.` and whose
/// payload is the types space-joined plus a newline, then DONE; it never reads clipboard content.
#[test]
fn listing_only_never_reads_content() {
    let source = FakeTypes {
        listing: TypeListing::Types(vec!["text/plain".into(), "image/png".into()]),
        forbid_reads: true,
        ..FakeTypes::holding("secret")
    };
    let listing_request = request("type=read:id=l", ".");
    assert!(listing_request.listing_only());
    let listing = b64("text/plain image/png\n");
    let expected = packet("OK:id=l")
        + &packet(&format!("DATA:id=l:mime=Lg==;{listing}"))
        + &packet("DONE:id=l");
    assert_eq!(reply_text(&source, &listing_request), expected);
    assert!(source.caps().is_empty(), "no content was read");
}

/// Contract: a listing carries at most 16 names of at most 256 bytes, joined with the newline
/// into at most 4096 bytes; longer names are skipped and later ones dropped.
#[test]
fn listing_bounded_16_and_4096() {
    let mut names: Vec<String> = (0..20).map(|index| format!("type/{index}")).collect();
    names.insert(1, "x".repeat(MAX_KITTY_CLIPBOARD_MIME_BYTES + 1));
    let source = FakeTypes { listing: TypeListing::Types(names), ..FakeTypes::holding("") };
    let packets = data_packets(&reply_text(&source, &request("type=read", ".")));
    let listing = String::from_utf8(packets[0].1.clone()).unwrap();
    let kept: Vec<&str> = listing.trim_end().split(' ').collect();
    assert_eq!(kept, (0..16).map(|index| format!("type/{index}")).collect::<Vec<_>>());
    let wide: Vec<String> = (0..16).map(|index| format!("{index:0>256}")).collect();
    let source = FakeTypes { listing: TypeListing::Types(wide), ..FakeTypes::holding("") };
    let packets = data_packets(&reply_text(&source, &request("type=read", ".")));
    assert!(packets[0].1.len() <= KITTY_LISTING_CAP_BYTES, "{}", packets[0].1.len());
    assert!(packets[0].1.ends_with(b"\n"));
}

/// Contract: an empty listing still answers its DATA packet, with the payload omitted.
#[test]
fn listing_payload_omitted_when_empty() {
    let source = FakeTypes { listing: TypeListing::Types(Vec::new()), ..FakeTypes::holding("") };
    let expected = packet("OK") + &packet("DATA:mime=Lg==") + &packet("DONE");
    assert_eq!(reply_text(&source, &request("type=read", ".")), expected);
}

/// Contract: a request with no types, or one whose type holds nothing, is a successful empty
/// read: OK then DONE, both carrying the id.
#[test]
fn empty_ok_done() {
    let expected = packet("OK:id=7") + &packet("DONE:id=7");
    assert_eq!(reply_text(&FakeTypes::holding("x"), &request("type=read:id=7", "")), expected);
    let empty = FakeTypes { text: ReadOutcome::Empty, ..FakeTypes::holding("") };
    assert_eq!(reply_text(&empty, &request("type=read:id=7", "text/plain")), expected);
}

/// Contract: a payload that is not strict Base64 of UTF-8 is dropped with no reply.
#[test]
fn bad_payload_silent() {
    let metadata = parse_kitty_clipboard_metadata(b"type=read:id=b").unwrap();
    assert!(parse_read_request(&metadata, b"!!!!", false).is_none());
    assert!(parse_read_request(&metadata, b"dGV4dA", false).is_none(), "missing padding");
    assert!(parse_read_request(&metadata, b"/w==", false).is_none(), "not UTF-8");
}

/// Contract: `Unsupported` answers `ENOSYS` and a native `Failed` answers `EIO`, distinct from an
/// empty read; a listing the platform cannot enumerate answers `ENOSYS`, and one whose native
/// enumeration failed answers `EIO`.
#[test]
fn unsupported_enosys_failed_eio() {
    for (outcome, status) in [(ReadOutcome::Unsupported, "ENOSYS"), (ReadOutcome::Failed, "EIO")] {
        let source = FakeTypes { text: outcome, ..FakeTypes::holding("") };
        let reply = reply_text(&source, &request("type=read:id=u", "text/plain"));
        assert_eq!(reply, packet(&format!("{status}:id=u")));
    }
    let source = FakeTypes { listing: TypeListing::Unsupported, ..FakeTypes::holding("") };
    assert_eq!(reply_text(&source, &request("type=read", ".")), packet("ENOSYS"));
    let source = FakeTypes { listing: TypeListing::Failed, ..FakeTypes::holding("") };
    assert_eq!(reply_text(&source, &request("type=read", ".")), packet("EIO"));
}

/// Contract: at most 16 data types are accepted; the seventeenth is never read or served.
#[test]
fn seventeenth_type_never_served() {
    let types: Vec<String> = (0..17).map(|index| format!("text/plain;x={index}")).collect();
    let many = request("type=read", &types.join(" "));
    assert_eq!(many.types.len(), MAX_KITTY_READ_TYPES);
    let source = FakeTypes::holding("hi");
    let packets = data_packets(&reply_text(&source, &many));
    assert_eq!(packets.len(), MAX_KITTY_READ_TYPES);
    assert_eq!(source.caps().len(), MAX_KITTY_READ_TYPES);
    assert!(packets.iter().all(|(metadata, _)| !metadata.contains(&b64("text/plain;x=16"))));
}

/// Contract: text served under an alias keeps the requested name, in request order, and a type
/// SonicTerm cannot read yet is omitted, which the protocol reads as unavailable.
#[test]
fn alias_served_in_order_under_requested_name() {
    let source = FakeTypes::holding("hi");
    let aliased = request("type=read", "image/png text/plain;charset=utf-8 text/plain");
    let expected = packet("OK")
        + &packet(&format!("DATA:mime={};aGk=", b64("text/plain;charset=utf-8")))
        + &packet("DATA:mime=dGV4dC9wbGFpbg==;aGk=")
        + &packet("DONE");
    assert_eq!(reply_text(&source, &aliased), expected);
}

/// Contract: a type's text is served in DATA packets of at most 4096 raw bytes, and every packet
/// repeats the opening id and the MIME type.
#[test]
fn chunks_4096_raw_id_and_mime_repeated_per_packet() {
    let source = FakeTypes::holding(&"a".repeat(2 * KITTY_READ_CHUNK_BYTES + 10));
    let packets = data_packets(&reply_text(&source, &request("type=read:id=c", "text/plain")));
    let sizes: Vec<usize> = packets.iter().map(|(_, payload)| payload.len()).collect();
    assert_eq!(sizes, [KITTY_READ_CHUNK_BYTES, KITTY_READ_CHUNK_BYTES, 10]);
    assert!(packets.iter().all(|(metadata, _)| metadata.ends_with(":id=c:mime=dGV4dC9wbGFpbg==")));
}

/// Contract: the DATA length formula is exact (a 512-byte id with `text/plain` costs 6,033 bytes
/// per full chunk), and with the largest id, sixteen 256-byte MIME types and 6 MiB of text each,
/// the whole frame stays within 12 MiB because a type that would pass it is omitted.
#[test]
fn max_id_512_mime_256_sixteen_types_frame_within_12_mib() {
    assert_eq!(data_packet_len(512, "text/plain".len(), KITTY_READ_CHUNK_BYTES, false), 6033);
    let id = "i".repeat(512);
    let types: Vec<String> = (0..16).map(|index| format!("text/plain;{index:x<245}")).collect();
    assert!(types.iter().all(|mime| mime.len() == MAX_KITTY_CLIPBOARD_MIME_BYTES));
    let source = FakeTypes::holding(&"a".repeat(6 * 1024 * 1024));
    let wide = request(&format!("type=read:id={id}"), &types.join(" "));
    let reply = build_read_reply(&source, &wide);
    assert!(reply.len() <= KITTY_READ_FRAME_CAP_BYTES, "{}", reply.len());
    assert!(reply.capacity() <= KITTY_READ_FRAME_CAP_BYTES, "{}", reply.capacity());
    let text = String::from_utf8(reply).unwrap();
    let first = text.split("\x1b\\").find(|packet| packet.contains("status=DATA")).unwrap();
    let expected =
        data_packet_len(512, MAX_KITTY_CLIPBOARD_MIME_BYTES, KITTY_READ_CHUNK_BYTES, false);
    assert_eq!(first.len() + 2, expected, "the formula is the packet's exact length");
}

/// Contract: a type whose text passes 8 MiB is refused by the bounded read before any copy and
/// omitted; every read asks with that cap.
#[test]
fn oversize_type_omitted_before_copy() {
    let source = FakeTypes::holding(&"a".repeat(KITTY_READ_TYPE_CAP_BYTES + 1));
    let reply = reply_text(&source, &request("type=read", "text/plain"));
    assert_eq!(reply, packet("OK") + &packet("DONE"));
    assert_eq!(source.caps(), [KITTY_READ_TYPE_CAP_BYTES]);
}

/// Contract: one job's peak working set (the 12 MiB frame, one 8 MiB raw type and the 4 KiB
/// listing) fits its 21 MiB reservation, and three jobs fit the 64 MiB reply budget, a fourth not.
#[test]
fn peak_working_set_within_21_mib_reservation() {
    let peak = KITTY_READ_FRAME_CAP_BYTES + KITTY_READ_TYPE_CAP_BYTES + KITTY_LISTING_CAP_BYTES;
    assert!(peak <= KITTY_READ_RESERVATION_BYTES);
    let budget = super::super::osc52_read::READ_REPLY_BUDGET_BYTES;
    assert!(3 * KITTY_READ_RESERVATION_BYTES <= budget);
    assert!(4 * KITTY_READ_RESERVATION_BYTES > budget);
}
