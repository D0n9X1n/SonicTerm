//! Round-trip test for the CF_SONIC_TAB drag payload, Windows-only.
//!
//! Most of the OLE drag-drop pipeline needs a real window + OLE thread
//! to exercise. What we *can* unit-test on Windows CI is the
//! JSON ↔ byte-buffer round-trip that the IDataObject wraps: the bytes
//! we'd put into an HGLOBAL must decode back to an equal TabPayload.
//! That's where the actual wire-compat surface lives — a renamed
//! field, a flipped serializer feature, or a stray BOM would all
//! manifest here first.
//!
//! Gated `#[ignore]` because:
//!   1. The full source/destination DoDragDrop loop needs a real
//!      desktop session (CI runners don't have one).
//!   2. Running it under a human/CI matrix with `--ignored` is the
//!      explicit gate before shipping the Windows drag feature.

#![cfg(windows)]

use sonic_shared::os_drag::TabPayload;

fn sample_payload() -> TabPayload {
    TabPayload {
        pty_pid: 4242,
        tab_title: "C:\\Users\\me — pwsh".to_string(),
        scrollback_b64: TabPayload::encode_scrollback(b"PS C:\\> ls\n"),
        cwd: "C:\\Users\\me".to_string(),
        cmd: "pwsh.exe".to_string(),
        env: vec![
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
        ],
    }
}

#[test]
fn cf_sonic_tab_json_bytes_round_trip() {
    let p = sample_payload();
    let json = p.to_json().expect("encode");
    // The bytes we'd memcpy into the HGLOBAL.
    let bytes = json.into_bytes();
    // Simulate the destination side: pull bytes out and parse.
    let s = String::from_utf8(bytes).expect("payload must be valid UTF-8");
    let back = TabPayload::from_json(&s).expect("decode");
    assert_eq!(p, back);
}

#[test]
fn cf_sonic_tab_survives_trailing_null_padding() {
    // GlobalAlloc rounds up to the allocator's chunk size; some writers
    // pad with NULs. The destination strips trailing NULs before
    // parsing — make sure that strategy survives a typical payload.
    let p = sample_payload();
    let mut bytes = p.to_json().expect("encode").into_bytes();
    bytes.extend_from_slice(&[0u8; 16]);
    // Mirror destination's strip-trailing-NULs strategy.
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = std::str::from_utf8(&bytes[..end]).expect("valid utf-8 prefix");
    let back = TabPayload::from_json(s).expect("decode after null-strip");
    assert_eq!(p, back);
}

#[test]
#[ignore = "needs OleInitialize + a real HWND; run on a Windows desktop with --ignored"]
fn full_dodragdrop_loop_smoke() {
    // Placeholder for the human-driven smoke: a developer can build
    // sonic-windows, open two windows, drag a tab from one to the
    // other, and observe the payload arrive via take_pending_payload().
    // Encoded as a test stub so it appears in `cargo test --ignored`
    // listings and reminds reviewers it exists.
}
