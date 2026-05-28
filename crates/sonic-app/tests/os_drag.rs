//! Tests for `os_drag::TabPayload` serialization + `PendingPayloadSlot`.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/os_drag.rs`.

use sonic_app::os_drag::{PendingPayloadSlot, TabPayload, PASTEBOARD_TYPE};

#[test]
fn pasteboard_type_is_stable() {
    assert_eq!(PASTEBOARD_TYPE, "com.sonic-terminal.tab.v1");
    assert!(PASTEBOARD_TYPE.chars().all(|c| c.is_ascii_lowercase() || "0123456789.-".contains(c)));
    assert!(PASTEBOARD_TYPE.contains('.'));
    assert!(PASTEBOARD_TYPE.ends_with(".v1"));
}

#[test]
fn payload_round_trip_through_json() {
    let p = TabPayload {
        pty_pid: 12345,
        tab_title: "~/Workspace/sonic — zsh".to_string(),
        scrollback_b64: TabPayload::encode_scrollback(b"hello\nworld\n"),
        cwd: "/Users/d0n9x1n/Workspace/fun-code/sonic".to_string(),
        cmd: "/bin/zsh".to_string(),
        env: vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
        ],
    };
    let json = p.to_json().expect("encode");
    let back = TabPayload::from_json(&json).expect("decode");
    assert_eq!(p, back);
}

#[test]
fn payload_tolerates_unknown_fields() {
    let json = r#"{
        "pty_pid": 1,
        "tab_title": "t",
        "scrollback_b64": "",
        "cwd": "",
        "cmd": "/bin/sh",
        "env": [],
        "future_field_we_dont_know_about": [1, 2, 3]
    }"#;
    let p = TabPayload::from_json(json).expect("tolerates unknown fields");
    assert_eq!(p.cmd, "/bin/sh");
}

#[test]
fn base64_round_trip_covers_all_pad_lengths() {
    for raw in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
        let enc = TabPayload::encode_scrollback(raw);
        let dec = TabPayload::decode_scrollback(&enc)
            .unwrap_or_else(|| panic!("decode failed for {raw:?}"));
        assert_eq!(dec.as_slice(), raw, "round-trip failed for {raw:?}");
    }
}

#[test]
fn base64_decode_rejects_garbage() {
    assert!(TabPayload::decode_scrollback("abc").is_none());
    assert!(TabPayload::decode_scrollback("!!!!").is_none());
}

#[test]
fn base64_matches_canonical_rfc_examples() {
    assert_eq!(TabPayload::encode_scrollback(b""), "");
    assert_eq!(TabPayload::encode_scrollback(b"f"), "Zg==");
    assert_eq!(TabPayload::encode_scrollback(b"fo"), "Zm8=");
    assert_eq!(TabPayload::encode_scrollback(b"foo"), "Zm9v");
    assert_eq!(TabPayload::encode_scrollback(b"foob"), "Zm9vYg==");
    assert_eq!(TabPayload::encode_scrollback(b"fooba"), "Zm9vYmE=");
    assert_eq!(TabPayload::encode_scrollback(b"foobar"), "Zm9vYmFy");
}

#[test]
fn payload_with_binary_scrollback_survives_json() {
    let nasty = b"\x00\x1b[31mred\x1b[0m\"quoted\"\n\xff\xfe";
    let p = TabPayload {
        pty_pid: 0,
        tab_title: String::new(),
        scrollback_b64: TabPayload::encode_scrollback(nasty),
        cwd: String::new(),
        cmd: String::new(),
        env: vec![],
    };
    let json = p.to_json().expect("encode");
    let back = TabPayload::from_json(&json).expect("decode");
    let decoded = TabPayload::decode_scrollback(&back.scrollback_b64).expect("b64 decode");
    assert_eq!(decoded.as_slice(), nasty);
}

#[test]
fn pending_payload_slot_put_then_take() {
    let slot = PendingPayloadSlot::new();
    assert!(slot.take().is_none(), "fresh slot is empty");
    let p = TabPayload {
        pty_pid: 7,
        tab_title: "t".to_string(),
        scrollback_b64: String::new(),
        cwd: String::new(),
        cmd: "/bin/sh".to_string(),
        env: vec![],
    };
    slot.put(p.clone());
    assert_eq!(slot.take(), Some(p));
    assert!(slot.take().is_none(), "draining is one-shot");
}

#[test]
fn pending_payload_slot_overwrites_older() {
    let slot = PendingPayloadSlot::new();
    let mk = |pid: i32| TabPayload {
        pty_pid: pid,
        tab_title: String::new(),
        scrollback_b64: String::new(),
        cwd: String::new(),
        cmd: String::new(),
        env: vec![],
    };
    slot.put(mk(1));
    slot.put(mk(2));
    slot.put(mk(3));
    assert_eq!(slot.take().map(|p| p.pty_pid), Some(3));
}

#[test]
fn pending_payload_slot_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PendingPayloadSlot>();
}
