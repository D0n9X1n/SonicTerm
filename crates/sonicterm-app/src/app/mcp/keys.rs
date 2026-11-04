//! Key names an agent sends with `send_keys`, and their encoding for one pane.
//!
//! A name is parsed once into a logical key with modifiers and the Win32 fields that key would
//! carry. Encoding then follows the pane's keyboard modes, read under its parser lock just before:
//! Win32 input records (a key-down and a key-up) when the pane negotiated win32-input mode on
//! Windows, otherwise the same logical encoder synthetic keyboard input uses, followed by a release
//! when the pane's Kitty flags ask for release events.

use sonicterm_vt::vt::KeyboardModes;
use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};

use crate::app::key_encoding::{
    encode_logical_release, encode_logical_with_modes, encode_win32_key, Win32KeyEvent,
};
use crate::app::keyboard_protocol::{KeyboardProtocol, KeyboardSnapshot};

/// The most keys one `send_keys` call may carry.
pub(crate) const MAX_KEYS: usize = 64;

/// Win32 `dwControlKeyState` bits for the modifiers and the enhanced-key flag.
const LEFT_ALT_PRESSED: u32 = 0x02;
/// The left Ctrl key is down.
const LEFT_CTRL_PRESSED: u32 = 0x08;
/// A Shift key is down.
const SHIFT_PRESSED: u32 = 0x10;
/// The key is on the enhanced block (arrows, Home, End, Insert, Delete, PageUp, PageDown).
const ENHANCED_KEY: u32 = 0x100;

/// One parsed key: what the logical encoder takes, and the fields a Win32 record carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeySpec {
    /// The logical key.
    pub(crate) key: Key,
    /// Held modifiers.
    pub(crate) mods: ModifiersState,
    /// Win32 virtual-key code.
    pub(crate) virtual_key: u16,
    /// Set-1 scan code for a US layout.
    pub(crate) scan_code: u16,
    /// The UTF-16 unit the key-down produces, or 0 when it produces none.
    pub(crate) unicode: u16,
    /// Win32 control-key state: modifiers and the enhanced-key flag.
    pub(crate) control_key_state: u32,
}

/// A named key: its logical key, virtual-key and scan codes, the unit it types, and whether it
/// sits on the enhanced block.
struct Named {
    /// The logical key.
    key: Key,
    /// Win32 virtual-key code.
    virtual_key: u16,
    /// Set-1 scan code.
    scan_code: u16,
    /// The UTF-16 unit the key types, or 0.
    unicode: u16,
    /// Whether the key is on the enhanced block.
    enhanced: bool,
}

/// The named keys an agent may send, by their spelling in `send_keys`.
fn named(name: &str) -> Option<Named> {
    let plain = |key, virtual_key, scan_code, unicode| Named {
        key: Key::Named(key),
        virtual_key,
        scan_code,
        unicode,
        enhanced: false,
    };
    let enhanced = |key, virtual_key, scan_code| Named {
        key: Key::Named(key),
        virtual_key,
        scan_code,
        unicode: 0,
        enhanced: true,
    };
    let found = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => plain(NamedKey::Enter, 0x0D, 0x1C, u16::from(b'\r')),
        "tab" => plain(NamedKey::Tab, 0x09, 0x0F, u16::from(b'\t')),
        "backspace" => plain(NamedKey::Backspace, 0x08, 0x0E, 0x08),
        "escape" | "esc" => plain(NamedKey::Escape, 0x1B, 0x01, 0x1B),
        "space" => plain(NamedKey::Space, 0x20, 0x39, u16::from(b' ')),
        "up" => enhanced(NamedKey::ArrowUp, 0x26, 0x48),
        "down" => enhanced(NamedKey::ArrowDown, 0x28, 0x50),
        "left" => enhanced(NamedKey::ArrowLeft, 0x25, 0x4B),
        "right" => enhanced(NamedKey::ArrowRight, 0x27, 0x4D),
        "home" => enhanced(NamedKey::Home, 0x24, 0x47),
        "end" => enhanced(NamedKey::End, 0x23, 0x4F),
        "pageup" => enhanced(NamedKey::PageUp, 0x21, 0x49),
        "pagedown" => enhanced(NamedKey::PageDown, 0x22, 0x51),
        "insert" => enhanced(NamedKey::Insert, 0x2D, 0x52),
        "delete" => enhanced(NamedKey::Delete, 0x2E, 0x53),
        function => {
            // When: the name is no table key, it may still be `F1` to `F12`.
            return function_key(function);
        }
    };
    Some(found)
}

/// `F1` to `F12`.
fn function_key(name: &str) -> Option<Named> {
    const KEYS: [NamedKey; 12] = [
        NamedKey::F1,
        NamedKey::F2,
        NamedKey::F3,
        NamedKey::F4,
        NamedKey::F5,
        NamedKey::F6,
        NamedKey::F7,
        NamedKey::F8,
        NamedKey::F9,
        NamedKey::F10,
        NamedKey::F11,
        NamedKey::F12,
    ];
    // Set-1 scan codes: F1-F10 are consecutive from 0x3B; F11 and F12 sit at 0x57 and 0x58.
    const SCANS: [u16; 12] =
        [0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58];
    let number: usize = name.strip_prefix('f')?.parse().ok()?;
    let index = number.checked_sub(1).filter(|index| *index < KEYS.len())?;
    Some(Named {
        key: Key::Named(KEYS[index]),
        virtual_key: 0x70 + index as u16,
        scan_code: SCANS[index],
        unicode: 0,
        enhanced: false,
    })
}

/// Set-1 scan codes for `a` to `z` on a US layout.
const LETTER_SCANS: [u16; 26] = [
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
    0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
];

/// The US layout's shifted digit row: `SHIFTED_DIGITS[n]` is Shift with digit `n + 1`, and the
/// last entry is Shift+0.
const SHIFTED_DIGITS: [char; 10] = ['!', '@', '#', '$', '%', '^', '&', '*', '(', ')'];

/// The US layout's punctuation keys: unshifted and shifted character, Win32 `VK_OEM_*` code and
/// set-1 scan code.
const PUNCTUATION_KEYS: [(char, char, u16, u16); 11] = [
    ('-', '_', 0xBD, 0x0C),
    ('=', '+', 0xBB, 0x0D),
    ('[', '{', 0xDB, 0x1A),
    (']', '}', 0xDD, 0x1B),
    ('\\', '|', 0xDC, 0x2B),
    (';', ':', 0xBA, 0x27),
    ('\'', '"', 0xDE, 0x28),
    ('`', '~', 0xC0, 0x29),
    (',', '<', 0xBC, 0x33),
    ('.', '>', 0xBE, 0x34),
    ('/', '?', 0xBF, 0x35),
];

/// A printable ASCII character's key on a US keyboard.
struct UsKey {
    /// What the key types without Shift.
    base: char,
    /// What it types with Shift.
    shifted: char,
    /// Win32 virtual-key code.
    virtual_key: u16,
    /// Set-1 scan code.
    scan_code: u16,
    /// Whether the character named is the shifted one.
    named_shifted: bool,
}

/// Where printable ASCII `character` sits on a US keyboard.
fn us_key(character: char) -> Option<UsKey> {
    if character.is_ascii_alphabetic() {
        // When: the `character` is a letter, its key is the letter's and uppercase is shifted.
        let base = character.to_ascii_lowercase();
        let index = usize::from(base as u8 - b'a');
        return Some(UsKey {
            base,
            shifted: base.to_ascii_uppercase(),
            virtual_key: 0x41 + index as u16,
            scan_code: LETTER_SCANS[index],
            named_shifted: character.is_ascii_uppercase(),
        });
    }
    let digit_key = |digit: u8, named_shifted: bool| {
        // Scan codes run 0x02 for 1 to 0x0A for 9, and 0x0B for 0.
        let scan_code = if digit == 0 {
            0x0B
        } else {
            // When: the `digit` is 1 to 9, its scan code follows Escape's 0x01.
            0x01 + u16::from(digit)
        };
        UsKey {
            base: char::from(b'0' + digit),
            shifted: SHIFTED_DIGITS[(usize::from(digit) + 9) % 10],
            virtual_key: 0x30 + u16::from(digit),
            scan_code,
            named_shifted,
        }
    };
    if let Some(digit) = character.to_digit(10) {
        // When: the `character` is a digit, its key is on the number row.
        return Some(digit_key(digit as u8, false));
    }
    if let Some(index) = SHIFTED_DIGITS.iter().position(|shifted| *shifted == character) {
        // When: the `character` is a shifted digit, its key is that digit's, with Shift.
        return Some(digit_key(((index + 1) % 10) as u8, true));
    }
    PUNCTUATION_KEYS.iter().find_map(|&(base, shifted, virtual_key, scan_code)| {
        (character == base || character == shifted).then_some(UsKey {
            base,
            shifted,
            virtual_key,
            scan_code,
            named_shifted: character == shifted,
        })
    })
}

/// The control byte Ctrl with `character` types, as the logical encoder sends it in legacy mode:
/// a letter's C0 code, NUL for `@`, `2` and Space, ESC for `[`, `\\`, `]`, `^`/`6`, US for `_`
/// and `/`, and DEL for `?`. Any other character has none and is typed as itself.
fn control_code(character: char) -> Option<u8> {
    match character.to_ascii_lowercase() {
        letter @ 'a'..='z' => Some(letter as u8 & 0x1f),
        '@' | '2' | ' ' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

/// Parse one key name: a named key, or `Ctrl+`, `Alt+` and `Shift+` prefixes on a named key or a
/// single printable character. `None` names no key this tool sends.
pub(crate) fn parse_key(name: &str) -> Option<KeySpec> {
    let mut mods = ModifiersState::empty();
    let mut rest = name;
    while let Some((modifier, tail)) = rest.split_once('+').filter(|(_, tail)| !tail.is_empty()) {
        mods |= match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ModifiersState::CONTROL,
            "alt" | "option" => ModifiersState::ALT,
            "shift" => ModifiersState::SHIFT,
            _ => {
                // When: the `modifier` is not Ctrl, Alt or Shift, the name is not a key.
                return None;
            }
        };
        rest = tail;
    }
    let control_key_state = |enhanced: bool| {
        let mut state = if enhanced { ENHANCED_KEY } else { 0 };
        if mods.control_key() {
            state |= LEFT_CTRL_PRESSED;
        }
        if mods.alt_key() {
            state |= LEFT_ALT_PRESSED;
        }
        if mods.shift_key() {
            state |= SHIFT_PRESSED;
        }
        state
    };
    if let Some(found) = named(rest) {
        // When: `rest` spells a `named` key, its fields come from the table.
        let space = found.key == Key::Named(NamedKey::Space);
        // Ctrl+Space types NUL, as the logical encoder sends it.
        let unicode = if space && mods.control_key() { 0 } else { found.unicode };
        return Some(KeySpec {
            key: found.key,
            mods,
            virtual_key: found.virtual_key,
            scan_code: found.scan_code,
            unicode,
            control_key_state: control_key_state(found.enhanced),
        });
    }
    let mut chars = rest.chars();
    let (Some(character), None) = (chars.next(), chars.next()) else {
        // When: `rest` is neither a named key nor one character, no key has that name.
        return None;
    };
    let key = us_key(character)?;
    let letter = character.is_ascii_alphabetic();
    let ctrl = mods.control_key();
    // Shift is held when asked for, or when the character named is a shifted one; Ctrl with a
    // letter ignores the letter's case, so `Ctrl+C` is `Ctrl+c`.
    let shift = mods.shift_key() || (key.named_shifted && !(ctrl && letter));
    let typed_char = if shift { key.shifted } else { key.base };
    // Ctrl types the control byte of the character named, the same byte the encoder sends.
    let control = ctrl.then(|| control_code(typed_char)).flatten();
    let typed = control.map_or(u16::from(typed_char as u8), u16::from);
    // The encoder receives the character named; Ctrl takes a letter in lowercase, as a keyboard
    // reports it, so `Ctrl+C` and `Ctrl+c` encode alike.
    let logical = if ctrl && letter { key.base } else { typed_char };
    // A Shift implied by the character rather than `mods` is held only in the Win32 record.
    let shift_state = SHIFT_PRESSED * u32::from(shift && !mods.shift_key());
    Some(KeySpec {
        key: Key::Character(SmolStr::new(logical.to_string())),
        mods,
        virtual_key: key.virtual_key,
        scan_code: key.scan_code,
        unicode: typed,
        control_key_state: control_key_state(false) | shift_state,
    })
}

/// The UTF-16 units a key-down carries: none for a key that types nothing.
fn unicode_units(unit: &[u16; 1]) -> &[u16] {
    if unit[0] == 0 {
        // When: the key's `unit` is 0, it types nothing and the record carries no units.
        return &[];
    }
    unit
}

/// Why `send_keys` refused before writing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeysError {
    /// More than [`MAX_KEYS`] keys.
    TooMany,
    /// A key name this tool does not send.
    Unknown(String),
    /// The pane's keyboard mode cannot encode this key.
    Unencodable(String),
}

/// Parse every name first, so one unknown name refuses the whole call.
pub(crate) fn parse_keys(names: &[String]) -> Result<Vec<KeySpec>, KeysError> {
    if names.len() > MAX_KEYS {
        // When: more than `MAX_KEYS` names were given, the call is refused whole.
        return Err(KeysError::TooMany);
    }
    names
        .iter()
        .map(|name| parse_key(name).ok_or_else(|| KeysError::Unknown(name.clone())))
        .collect()
}

/// Encode `keys` for a pane in `snapshot`'s keyboard state; `windows` says whether Win32 input
/// records are possible. Every key is encoded before any byte is returned.
pub(crate) fn encode_keys(
    keys: &[KeySpec],
    snapshot: KeyboardSnapshot,
    windows: bool,
) -> Result<Vec<u8>, KeysError> {
    let modes: KeyboardModes = snapshot.modes();
    let kitty_flags = snapshot.kitty_flags();
    let win32 = snapshot.protocol(windows) == KeyboardProtocol::Win32;
    let mut bytes = Vec::new();
    for spec in keys {
        if win32 {
            // When: the pane negotiated `win32` input, each key is a key-down and a key-up record.
            for key_down in [true, false] {
                let unicode = [spec.unicode];
                bytes.extend(encode_win32_key(Win32KeyEvent {
                    virtual_key: spec.virtual_key,
                    scan_code: spec.scan_code,
                    unicode: unicode_units(&unicode),
                    key_down,
                    control_key_state: spec.control_key_state,
                    repeat_count: 1,
                }));
            }
            continue;
        }
        let press = encode_logical_with_modes(&spec.key, spec.mods, kitty_flags, modes)
            .filter(|press| !press.is_empty())
            .ok_or_else(|| KeysError::Unencodable(format!("{:?}", spec.key)))?;
        bytes.extend(press);
        // A release follows only when the Kitty flags ask for release events.
        bytes.extend(
            encode_logical_release(&spec.key, spec.mods, kitty_flags, modes).unwrap_or_default(),
        );
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod keys_tests;
