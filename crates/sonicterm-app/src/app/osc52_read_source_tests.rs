use super::*;
use crate::app::osc52_read::READ_TEXT_CAP;
use std::sync::Mutex;

/// A source with fixed outcomes per destination that records each `(location, max)` it was asked.
struct Recording {
    clipboard: ReadOutcome,
    primary: ReadOutcome,
    asked: Mutex<Vec<(ReadLocation, usize)>>,
}

impl Recording {
    fn new(clipboard: ReadOutcome, primary: ReadOutcome) -> Self {
        Self { clipboard, primary, asked: Mutex::new(Vec::new()) }
    }
}

impl ClipboardTextSource for Recording {
    fn read_text_bounded(&self, location: ReadLocation, max_bytes: usize) -> ReadOutcome {
        self.asked.lock().unwrap().push((location, max_bytes));
        match location {
            ReadLocation::Clipboard => self.clipboard.clone(),
            ReadLocation::Primary => self.primary.clone(),
        }
    }
}

/// Contract: Windows decides from `GlobalSize` before `GlobalLock`: a block holding more UTF-16
/// units than the cap plus a terminator is refused, since each unit is at least one UTF-8 byte.
#[test]
fn utf16_block_admission_is_decided_from_its_size() {
    assert!(utf16_block_admits(10, 4), "4 units and a terminator");
    assert!(!utf16_block_admits(12, 4), "5 units cannot fit 4 bytes");
}

/// Contract: UTF-16 text converts within the UTF-8 cap; a lone surrogate becomes U+FFFD and counts
/// its 3 bytes; one byte over the cap is `TooLarge`; no units is `Empty`.
#[test]
fn bounded_utf16_cases() {
    let units: Vec<u16> = "héllo".encode_utf16().collect();
    assert_eq!(bounded_utf16(&units, 6), ReadOutcome::Text("héllo".into()));
    assert_eq!(bounded_utf16(&units, 5), ReadOutcome::TooLarge);
    assert_eq!(bounded_utf16(&[0xD800, u16::from(b'a')], 4), ReadOutcome::Text("\u{FFFD}a".into()));
    assert_eq!(bounded_utf16(&[0xD800], 2), ReadOutcome::TooLarge);
    assert_eq!(bounded_utf16(&[], 0), ReadOutcome::Empty);
}

/// Contract: non-UTF-8 bytes become U+FFFD before the cap applies, and `STRING` (Latin-1) bytes
/// above 0x7F take two UTF-8 bytes each.
#[test]
fn bounded_utf8_and_latin1_cases() {
    assert_eq!(bounded_utf8(b"ok".to_vec(), 2), ReadOutcome::Text("ok".into()));
    assert_eq!(bounded_utf8(vec![b'a', 0xFF], 4), ReadOutcome::Text("a\u{FFFD}".into()));
    assert_eq!(bounded_utf8(vec![b'a', 0xFF], 3), ReadOutcome::TooLarge);
    assert_eq!(bounded_utf8(Vec::new(), 0), ReadOutcome::Empty);
    assert_eq!(bounded_latin1(&[b'a', 0xE9], 3), ReadOutcome::Text("aé".into()));
    assert_eq!(bounded_latin1(&[b'a', 0xE9], 2), ReadOutcome::TooLarge);
}

/// Contract: one destination reads with the 8 MiB cap and `cp` reads each with half of it; text
/// frames as Base64 and every other outcome frames empty, so a reply never says why it is empty.
#[test]
fn read_replies_frame_each_destination() {
    let source = Recording::new(ReadOutcome::Text("clip".into()), ReadOutcome::TooLarge);
    assert_eq!(read_replies(&source, b"c"), b"\x1b]52;c;Y2xpcA==\x1b\\");
    assert_eq!(read_replies(&source, b"s"), b"\x1b]52;s;Y2xpcA==\x1b\\");
    assert_eq!(read_replies(&source, b"cp"), b"\x1b]52;c;Y2xpcA==\x1b\\\x1b]52;p;\x1b\\");
    let asked = source.asked.lock().unwrap().clone();
    assert_eq!(asked[0], (ReadLocation::Clipboard, READ_TEXT_CAP));
    assert_eq!(asked[2], (ReadLocation::Clipboard, READ_TEXT_CAP / 2));
    for outcome in [ReadOutcome::Empty, ReadOutcome::Unsupported, ReadOutcome::Failed] {
        let source = Recording::new(outcome, ReadOutcome::Empty);
        assert_eq!(read_replies(&source, b"c"), b"\x1b]52;c;\x1b\\");
    }
}

/// Contract: `p` reads PRIMARY only on Linux; elsewhere its reply is empty and no native read runs.
#[test]
fn p_primary_linux_empty_elsewhere() {
    let source = Recording::new(ReadOutcome::Empty, ReadOutcome::Text("prim".into()));
    let replies = read_replies(&source, b"p");
    if cfg!(target_os = "linux") {
        assert_eq!(replies, b"\x1b]52;p;cHJpbQ==\x1b\\");
        assert_eq!(*source.asked.lock().unwrap(), [(ReadLocation::Primary, READ_TEXT_CAP)]);
    } else {
        assert_eq!(replies, b"\x1b]52;p;\x1b\\");
        assert!(source.asked.lock().unwrap().is_empty());
    }
}

/// Contract: `Debug` of a read outcome prints only the text's length, never the clipboard text.
#[test]
fn outcome_debug_never_prints_text() {
    let printed = format!("{:?}", ReadOutcome::Text("secret-value".into()));
    assert!(!printed.contains("secret"), "{printed}");
    assert!(printed.contains("12"), "{printed}");
}
