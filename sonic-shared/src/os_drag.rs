//! OS-level cross-process tab drag — shared payload definitions.
//!
//! When the user drags a tab off a Sonic window and the cursor leaves
//! every Sonic-owned window (i.e. they're dropping it on a *different*
//! Sonic process, not just into another window of the same process —
//! that's handled by `tab_drag`), we fall through to the platform's
//! native drag-and-drop facility. This module defines the wire format
//! the source and destination processes agree on.
//!
//! ## Why this exists
//!
//! winit cross-process drag is not a thing. We have to talk to AppKit
//! (`NSPasteboard` + `NSDraggingSession`) on macOS and OLE
//! (`IDataObject` + `DoDragDrop`) on Windows. Both expect a typed blob
//! identified by a stable, reverse-DNS-style format name. That name
//! and the payload schema must match between *whichever* two Sonic
//! processes happen to be involved — pinning v1 in the type name lets
//! us evolve the schema later without ambiguity.
//!
//! ## Live-PTY transfer is not in scope for v1
//!
//! Handing a live `pty(4)` master FD across processes is technically
//! possible on macOS (Unix-domain socket `SCM_RIGHTS`) and Windows
//! (`DuplicateHandle` across PIDs) but it's a huge correctness
//! surface — the child process is still parented to the source app,
//! the source app still has the controlling TTY, signals route to the
//! wrong place. v1 takes the pragmatic route: serialize *intent*
//! (cwd, cmd, env, scrollback as history), source kills its local
//! tab, destination spawns a fresh one with the same cwd/cmd/env and
//! shows the captured scrollback as read-only history above the new
//! shell's first prompt.
//!
//! ## Format ID stability
//!
//! [`PASTEBOARD_TYPE`] is the single source of truth. Both the macOS
//! `NSPasteboard.declareTypes:owner:` call and the Windows
//! `RegisterClipboardFormatW` call use this exact string. Tests pin
//! it so a rename never silently breaks the wire.

use serde::{Deserialize, Serialize};

/// Stable identifier for the custom pasteboard / clipboard type that
/// carries a serialized [`TabPayload`].
///
/// Reverse-DNS naming follows Apple's convention for custom UTIs and
/// is also a valid Windows clipboard format name (the latter has no
/// formal naming convention but is case-sensitive and process-global).
///
/// **Do not rename without bumping the version suffix** — both the
/// source and the destination process must agree on this exact byte
/// sequence or the drag silently no-ops.
pub const PASTEBOARD_TYPE: &str = "com.sonic-terminal.tab.v1";

/// JSON payload exchanged between Sonic processes during an
/// OS-level tab drag.
///
/// Field names are kebab-friendly snake_case to match the JSON wire
/// format directly (no `#[serde(rename)]` games). The schema is
/// versioned via the type name [`PASTEBOARD_TYPE`], so adding new
/// optional fields here is forward-compatible only if older readers
/// tolerate unknown keys — which `serde_json::from_str` does by
/// default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabPayload {
    /// PID of the PTY child in the *source* process at the moment of
    /// drag. Purely informational on the destination side — used for
    /// the history banner ("torn from PID 12345") and for log
    /// correlation. The destination MUST NOT try to act on this PID;
    /// the source kills the child before releasing the drag.
    pub pty_pid: i32,
    /// Human-readable tab title (typically the shell prompt or the
    /// last `OSC 0`/`OSC 2` title the source process saw).
    pub tab_title: String,
    /// Base64-encoded UTF-8 scrollback text. Encoded so the JSON
    /// stays single-byte-clean even if the buffer contains nulls or
    /// raw escape sequences (we strip CSI before serializing, but
    /// users sometimes paste arbitrary binary).
    pub scrollback_b64: String,
    /// Current working directory of the shell at drag time. Best
    /// effort — pulled from `OSC 7` if the shell advertised it,
    /// otherwise empty.
    pub cwd: String,
    /// Argv\[0\] (and only argv\[0\]) of the shell to respawn on the
    /// destination side. We deliberately do NOT carry argv\[1..\] —
    /// re-running an interactive shell with the source's flags is
    /// usually wrong (e.g. `--login` should only fire once per
    /// session).
    pub cmd: String,
    /// Subset of environment variables the source side believes the
    /// destination should inherit (e.g. `LANG`, `TERM`, `COLORTERM`,
    /// user-set `PROMPT_COMMAND`). Stored as `(key, value)` pairs to
    /// preserve order and allow duplicates if a wild shell ever sets
    /// them.
    pub env: Vec<(String, String)>,
}

impl TabPayload {
    /// Encode the payload as a JSON string ready to drop on a
    /// pasteboard / clipboard.
    ///
    /// Returns a `Result` rather than panicking so a future change to
    /// the struct (e.g. adding a non-string-serializable field) is
    /// caught at the call site instead of crashing a UI handler.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Decode a JSON string read off the pasteboard / clipboard back
    /// into a [`TabPayload`].
    ///
    /// Tolerant of unknown trailing fields (default `serde_json`
    /// behavior) so a newer source can drop on an older destination
    /// without the destination outright rejecting the drag.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Convenience: encode arbitrary bytes (e.g. raw scrollback) as
    /// base64 suitable for [`Self::scrollback_b64`].
    ///
    /// Hand-rolled to avoid pulling in a `base64` crate just for this
    /// one use site — the algorithm is RFC 4648 standard alphabet
    /// (`A-Z a-z 0-9 + /`) with `=` padding.
    pub fn encode_scrollback(raw: &[u8]) -> String {
        const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(raw.len().div_ceil(3) * 4);
        let mut i = 0;
        while i + 3 <= raw.len() {
            let n = ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8) | (raw[i + 2] as u32);
            out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 6) & 0x3F) as usize] as char);
            out.push(ALPH[(n & 0x3F) as usize] as char);
            i += 3;
        }
        let rem = raw.len() - i;
        if rem == 1 {
            let n = (raw[i] as u32) << 16;
            out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        } else if rem == 2 {
            let n = ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8);
            out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        out
    }

    /// Convenience: decode the [`Self::scrollback_b64`] field back
    /// into raw bytes. Returns `None` on malformed input rather than
    /// erroring so the destination can gracefully fall back to "no
    /// scrollback" without aborting the drop.
    pub fn decode_scrollback(b64: &str) -> Option<Vec<u8>> {
        // Lookup table for the RFC-4648 alphabet. 0xFF = invalid.
        let mut table = [0xFFu8; 256];
        for (i, &c) in
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate()
        {
            table[c as usize] = i as u8;
        }
        let bytes = b64.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        let mut i = 0;
        while i < bytes.len() {
            let c = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
            let v0 = table[c[0] as usize];
            let v1 = table[c[1] as usize];
            if v0 == 0xFF || v1 == 0xFF {
                return None;
            }
            let n0 = v0 as u32;
            let n1 = v1 as u32;
            if c[2] == b'=' && c[3] == b'=' {
                out.push(((n0 << 2) | (n1 >> 4)) as u8);
            } else if c[3] == b'=' {
                let v2 = table[c[2] as usize];
                if v2 == 0xFF {
                    return None;
                }
                let n2 = v2 as u32;
                out.push(((n0 << 2) | (n1 >> 4)) as u8);
                out.push((((n1 & 0xF) << 4) | (n2 >> 2)) as u8);
            } else {
                let v2 = table[c[2] as usize];
                let v3 = table[c[3] as usize];
                if v2 == 0xFF || v3 == 0xFF {
                    return None;
                }
                let n2 = v2 as u32;
                let n3 = v3 as u32;
                out.push(((n0 << 2) | (n1 >> 4)) as u8);
                out.push((((n1 & 0xF) << 4) | (n2 >> 2)) as u8);
                out.push((((n2 & 0x3) << 6) | n3) as u8);
            }
            i += 4;
        }
        Some(out)
    }
}

/// Trait implemented by platform-specific OS-drag senders.
///
/// `sonic-shared` knows when a tab has been dragged outside every
/// Sonic-owned window — that's the trigger for an OS-level handoff.
/// What it does NOT know is how to actually start an NSDragging
/// session or a `DoDragDrop` call; those live in `sonic-mac` and
/// `sonic-windows` respectively. The platform binary installs an
/// `OsDragSink` impl at startup; the app dispatches into it.
///
/// The sink is fire-and-forget: success / failure is logged inside
/// the impl; the caller treats both as "tab is gone now" (the source
/// side has already serialized + killed its local copy).
pub trait OsDragSink: Send + Sync {
    /// Hand the payload off to the OS. On macOS this writes it to
    /// the general `NSPasteboard` under [`PASTEBOARD_TYPE`] and
    /// starts a drag session. On Windows it builds an `IDataObject`
    /// and calls `DoDragDrop`. On unsupported platforms it logs a
    /// warning and returns.
    fn begin_drag(&self, payload: &TabPayload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasteboard_type_is_stable() {
        // This test exists specifically to fail loudly if someone
        // edits the constant — the wire format is a coordination
        // surface between two independently-built Sonic processes,
        // so a silent rename would manifest as "drag does nothing"
        // in production with no compiler help.
        assert_eq!(PASTEBOARD_TYPE, "com.sonic-terminal.tab.v1");
        // Must be a valid reverse-DNS-ish UTI (no spaces, no upper
        // case, dot-separated). NSPasteboard accepts most strings
        // but Apple's convention is required for system-wide UTI
        // declaration we may add later.
        assert!(PASTEBOARD_TYPE
            .chars()
            .all(|c| c.is_ascii_lowercase() || "0123456789.-".contains(c)));
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
        // Forward-compat: a newer source might drop on us with extra
        // keys. We must not reject the whole drag.
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
        // Wrong length.
        assert!(TabPayload::decode_scrollback("abc").is_none());
        // Out-of-alphabet character.
        assert!(TabPayload::decode_scrollback("!!!!").is_none());
    }

    #[test]
    fn base64_matches_canonical_rfc_examples() {
        // RFC 4648 §10 test vectors.
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
        // The whole point of base64-encoding the scrollback is that
        // it must survive being embedded in a JSON string regardless
        // of contents — including the kinds of bytes (`\0`, `"`,
        // raw CSI) that would break a naive JSON-string approach.
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
}
