//! Pins `send_keys` naming and encoding: names parse with modifiers, unknown names and too many
//! keys refuse the whole call, arrows follow application cursor mode, Ctrl+letter types its C0
//! code, Kitty release events follow each press, and win32-input mode writes a down and an up
//! record per key.

use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{CaptureStagingPool, Parser};

use super::*;

/// The keyboard snapshot a pane publishes after it received `modes`.
fn snapshot_after(modes: &[u8]) -> KeyboardSnapshot {
    let mut parser =
        Parser::new_with_staging_pool(Grid::new(80, 24), None, CaptureStagingPool::new());
    parser.advance(modes);
    KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot())
}

/// `names` encoded for a pane that received `modes`, off Windows.
fn encode(names: &[&str], modes: &[u8]) -> Result<Vec<u8>, KeysError> {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    encode_keys(&parse_keys(&names)?, snapshot_after(modes), false)
}

/// Names parse case-insensitively with stacked modifiers; anything else names no key.
#[test]
fn key_names_parse_with_modifiers() {
    let ctrl_c = parse_key("Ctrl+C").unwrap();
    assert_eq!(ctrl_c.mods, ModifiersState::CONTROL);
    assert_eq!((ctrl_c.virtual_key, ctrl_c.unicode), (0x43, 0x03));
    let alt_x = parse_key("alt+x").unwrap();
    assert_eq!(alt_x.mods, ModifiersState::ALT);
    assert_eq!(parse_key("F12").unwrap().virtual_key, 0x7B);
    assert_eq!(parse_key("PageDown").unwrap().control_key_state, ENHANCED_KEY);
    assert_eq!(
        parse_key("Ctrl+Shift+Up").unwrap().mods,
        ModifiersState::CONTROL | ModifiersState::SHIFT
    );
    for unknown in ["F13", "F0", "Hyper+a", "ab", "Ctrl+", "é", ""] {
        assert_eq!(parse_key(unknown), None, "{unknown:?}");
    }
}

/// Up is `ESC [ A` normally and `ESC O A` in application cursor mode; Enter, Tab and Escape send
/// their control bytes; Ctrl+C sends 0x03.
#[test]
fn keys_follow_the_pane_cursor_mode() {
    assert_eq!(encode(&["Up"], b"").unwrap(), b"\x1b[A");
    assert_eq!(encode(&["Up"], b"\x1b[?1h").unwrap(), b"\x1bOA");
    assert_eq!(encode(&["Enter", "Tab", "Escape"], b"").unwrap(), b"\r\t\x1b");
    assert_eq!(encode(&["Ctrl+C"], b"").unwrap(), [0x03]);
}

/// One unknown name, or more than `MAX_KEYS` keys, refuses the call before anything is encoded.
#[test]
fn a_bad_call_is_refused_whole() {
    assert_eq!(encode(&["Enter", "Nope"], b""), Err(KeysError::Unknown("Nope".into())));
    let too_many = vec!["a"; MAX_KEYS + 1];
    assert_eq!(encode(&too_many, b""), Err(KeysError::TooMany));
    assert!(encode(&vec!["a"; MAX_KEYS], b"").is_ok());
}

/// With Kitty's report-event-types flag a release follows each press; without it only the press.
/// Up is used because Kitty reports Enter, Tab and Backspace releases only with report-all-keys.
#[test]
fn kitty_release_events_follow_each_press() {
    let plain = encode(&["Up"], b"\x1b[>1u").unwrap();
    let with_events = encode(&["Up"], b"\x1b[>3u").unwrap();
    assert!(with_events.len() > plain.len(), "a release record follows the press");
    assert!(with_events.starts_with(&plain), "the press is unchanged");
}

/// In win32-input mode on Windows each key is a key-down and a key-up record with the key's
/// virtual-key and scan codes; off Windows the same pane keeps the logical encoding.
#[test]
fn win32_input_mode_writes_down_and_up_records() {
    let names = vec!["Enter".to_owned()];
    let keys = parse_keys(&names).unwrap();
    let snapshot = snapshot_after(b"\x1b[?9001h");
    let records = String::from_utf8(encode_keys(&keys, snapshot, true).unwrap()).unwrap();
    assert_eq!(records, "\x1b[13;28;13;1;0;1_\x1b[13;28;0;0;0;1_");
    assert_eq!(encode_keys(&keys, snapshot, false).unwrap(), b"\r");
}

/// Shift with a letter, digit or punctuation key types what a US keyboard types; an uppercase or
/// shifted character carries its key with Shift held in the Win32 state; Ctrl with a letter
/// ignores its case, and Ctrl with `[` types ESC.
#[test]
fn shift_and_punctuation_follow_a_us_layout() {
    let shift_a = parse_key("Shift+a").unwrap();
    assert_eq!(shift_a.key, Key::Character(SmolStr::new("A")));
    assert_eq!((shift_a.virtual_key, shift_a.unicode), (0x41, u16::from(b'A')));
    assert_ne!(shift_a.control_key_state & SHIFT_PRESSED, 0);
    let upper = parse_key("A").unwrap();
    assert_eq!(
        (upper.virtual_key, upper.scan_code, upper.mods),
        (0x41, 0x1E, ModifiersState::empty())
    );
    assert_ne!(upper.control_key_state & SHIFT_PRESSED, 0, "uppercase is typed with Shift");
    for (name, typed, virtual_key, scan_code) in
        [("!", b'!', 0x31, 0x02), ("Shift+1", b'!', 0x31, 0x02), ("Shift+/", b'?', 0xBF, 0x35)]
    {
        let spec = parse_key(name).unwrap();
        assert_eq!(spec.key, Key::Character(SmolStr::new(char::from(typed).to_string())), "{name}");
        assert_eq!(
            (spec.virtual_key, spec.scan_code, spec.unicode),
            (virtual_key, scan_code, u16::from(typed))
        );
        assert_ne!(spec.control_key_state & SHIFT_PRESSED, 0, "{name}");
    }
    let close = parse_key(")").unwrap();
    assert_eq!((close.virtual_key, close.scan_code), (0x30, 0x0B));
    let slash = parse_key("/").unwrap();
    assert_eq!((slash.virtual_key, slash.scan_code, slash.control_key_state), (0xBF, 0x35, 0));
    let (lower, upper) = (parse_key("Ctrl+c").unwrap(), parse_key("Ctrl+C").unwrap());
    assert_eq!(lower, upper, "Ctrl+C is Ctrl+c, with no Shift");
    assert_eq!((upper.unicode, upper.control_key_state), (0x03, LEFT_CTRL_PRESSED));
    let ctrl_shift_a = parse_key("Ctrl+Shift+a").unwrap();
    assert_eq!((ctrl_shift_a.virtual_key, ctrl_shift_a.unicode), (0x41, 0x01));
    let escape = parse_key("Ctrl+[").unwrap();
    assert_eq!((escape.virtual_key, escape.unicode), (0xDB, 0x1B));
    // No printable character is left without a key: none falls back to an unrelated code.
    for byte in b'!'..=b'~' {
        let spec = parse_key(&char::from(byte).to_string()).unwrap();
        assert!(spec.virtual_key != 0 && spec.scan_code != 0, "{:?}", char::from(byte));
        assert_ne!(spec.virtual_key, 0x21, "{:?} is not PageUp", char::from(byte));
    }
}

/// Shifted keys and control chords reach the pane as a US keyboard types them, both through the
/// logical encoder and in Win32 records.
#[test]
fn shifted_characters_reach_the_pane_as_typed() {
    assert_eq!(encode(&["Shift+a", "!", "Shift+1", "Shift+/"], b"").unwrap(), b"A!!?");
    assert_eq!(encode(&["Ctrl+C", "Ctrl+c", "Ctrl+["], b"").unwrap(), b"\x03\x03\x1b");
    let names: Vec<String> = ["Shift+1", "Ctrl+C"].iter().map(|name| (*name).to_owned()).collect();
    let records = encode_keys(&parse_keys(&names).unwrap(), snapshot_after(b"\x1b[?9001h"), true);
    let records = String::from_utf8(records.unwrap()).unwrap();
    assert!(records.starts_with("\x1b[49;2;33;1;16;1_"), "{records:?}");
    assert!(records.contains("\x1b[67;46;3;1;8;1_"), "{records:?}");
}

/// Ctrl with each punctuation key, a letter in either case, and Space types the same control
/// byte through the logical encoder as the Win32 record carries, so both paths agree.
#[test]
fn ctrl_chords_type_the_same_byte_on_both_paths() {
    let cases: [(&str, u8); 12] = [
        ("Ctrl+_", 0x1F),
        ("Ctrl+/", 0x1F),
        ("Ctrl+?", 0x7F),
        ("Ctrl+[", 0x1B),
        ("Ctrl+\\", 0x1C),
        ("Ctrl+]", 0x1D),
        ("Ctrl+^", 0x1E),
        ("Ctrl+@", 0x00),
        ("Ctrl+2", 0x00),
        ("Ctrl+c", 0x03),
        ("Ctrl+C", 0x03),
        ("Ctrl+Space", 0x00),
    ];
    for (name, byte) in cases {
        assert_eq!(encode(&[name], b"").unwrap(), [byte], "{name} through the encoder");
        assert_eq!(parse_key(name).unwrap().unicode, u16::from(byte), "{name} in a Win32 record");
    }
    // A punctuation key with no control byte is typed as itself.
    assert_eq!(encode(&["Ctrl+-"], b"").unwrap(), b"-");
    assert_eq!(parse_key("Ctrl+-").unwrap().unicode, u16::from(b'-'));
}
