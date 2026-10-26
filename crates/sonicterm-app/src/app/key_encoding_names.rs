//! Stable keymap spellings, candidate chords and physical key lookup tables.

use super::*;

pub(super) fn function_key_encoding(function_number: u8) -> FunctionalEncoding {
    match function_number {
        1 => FunctionalEncoding::Letter('P'),
        2 => FunctionalEncoding::Letter('Q'),
        3 => FunctionalEncoding::Letter('R'),
        4 => FunctionalEncoding::Letter('S'),
        5..=12 => {
            const TILDE: [u16; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            FunctionalEncoding::Tilde(TILDE[usize::from(function_number - 5)])
        }
        13..=24 => {
            const TILDE: [u16; 12] = [25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45];
            FunctionalEncoding::Tilde(TILDE[usize::from(function_number - 13)])
        }
        _ => FunctionalEncoding::CsiU(57363 + u32::from(function_number)),
    }
}

pub(super) fn named_functional_encoding(key: NamedKey) -> Option<FunctionalEncoding> {
    match key {
        NamedKey::ArrowUp => Some(FunctionalEncoding::Letter('A')),
        NamedKey::ArrowDown => Some(FunctionalEncoding::Letter('B')),
        NamedKey::ArrowRight => Some(FunctionalEncoding::Letter('C')),
        NamedKey::ArrowLeft => Some(FunctionalEncoding::Letter('D')),
        NamedKey::Home => Some(FunctionalEncoding::Letter('H')),
        NamedKey::End => Some(FunctionalEncoding::Letter('F')),
        NamedKey::Insert => Some(FunctionalEncoding::Tilde(2)),
        NamedKey::Delete => Some(FunctionalEncoding::Tilde(3)),
        NamedKey::PageUp => Some(FunctionalEncoding::Tilde(5)),
        NamedKey::PageDown => Some(FunctionalEncoding::Tilde(6)),
        NamedKey::ContextMenu => Some(FunctionalEncoding::Tilde(29)),
        NamedKey::F1 => Some(function_key_encoding(1)),
        NamedKey::F2 => Some(function_key_encoding(2)),
        NamedKey::F3 => Some(function_key_encoding(3)),
        NamedKey::F4 => Some(function_key_encoding(4)),
        NamedKey::F5 => Some(function_key_encoding(5)),
        NamedKey::F6 => Some(function_key_encoding(6)),
        NamedKey::F7 => Some(function_key_encoding(7)),
        NamedKey::F8 => Some(function_key_encoding(8)),
        NamedKey::F9 => Some(function_key_encoding(9)),
        NamedKey::F10 => Some(function_key_encoding(10)),
        NamedKey::F11 => Some(function_key_encoding(11)),
        NamedKey::F12 => Some(function_key_encoding(12)),
        NamedKey::F13 => Some(function_key_encoding(13)),
        NamedKey::F14 => Some(function_key_encoding(14)),
        NamedKey::F15 => Some(function_key_encoding(15)),
        NamedKey::F16 => Some(function_key_encoding(16)),
        NamedKey::F17 => Some(function_key_encoding(17)),
        NamedKey::F18 => Some(function_key_encoding(18)),
        NamedKey::F19 => Some(function_key_encoding(19)),
        NamedKey::F20 => Some(function_key_encoding(20)),
        NamedKey::F21 => Some(function_key_encoding(21)),
        NamedKey::F22 => Some(function_key_encoding(22)),
        NamedKey::F23 => Some(function_key_encoding(23)),
        NamedKey::F24 => Some(function_key_encoding(24)),
        NamedKey::F25 => Some(function_key_encoding(25)),
        NamedKey::F26 => Some(function_key_encoding(26)),
        NamedKey::F27 => Some(function_key_encoding(27)),
        NamedKey::F28 => Some(function_key_encoding(28)),
        NamedKey::F29 => Some(function_key_encoding(29)),
        NamedKey::F30 => Some(function_key_encoding(30)),
        NamedKey::F31 => Some(function_key_encoding(31)),
        NamedKey::F32 => Some(function_key_encoding(32)),
        NamedKey::F33 => Some(function_key_encoding(33)),
        NamedKey::F34 => Some(function_key_encoding(34)),
        NamedKey::F35 => Some(function_key_encoding(35)),
        _ => None,
    }
}

pub(in crate::app) fn unshift_ascii(character: char) -> char {
    match character {
        'A'..='Z' => character.to_ascii_lowercase(),
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '~' => '`',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ if character.is_uppercase() => character.to_lowercase().next().unwrap_or(character),
        _ => character,
    }
}

pub(in crate::app) fn physical_ascii(key: PhysicalKey) -> Option<char> {
    let PhysicalKey::Code(code) = key else {
        // When: key is not a PhysicalKey::Code, no stable PC-101 alternate
        // position is available.
        return None;
    };
    match code {
        KeyCode::KeyA => Some('a'),
        KeyCode::KeyB => Some('b'),
        KeyCode::KeyC => Some('c'),
        KeyCode::KeyD => Some('d'),
        KeyCode::KeyE => Some('e'),
        KeyCode::KeyF => Some('f'),
        KeyCode::KeyG => Some('g'),
        KeyCode::KeyH => Some('h'),
        KeyCode::KeyI => Some('i'),
        KeyCode::KeyJ => Some('j'),
        KeyCode::KeyK => Some('k'),
        KeyCode::KeyL => Some('l'),
        KeyCode::KeyM => Some('m'),
        KeyCode::KeyN => Some('n'),
        KeyCode::KeyO => Some('o'),
        KeyCode::KeyP => Some('p'),
        KeyCode::KeyQ => Some('q'),
        KeyCode::KeyR => Some('r'),
        KeyCode::KeyS => Some('s'),
        KeyCode::KeyT => Some('t'),
        KeyCode::KeyU => Some('u'),
        KeyCode::KeyV => Some('v'),
        KeyCode::KeyW => Some('w'),
        KeyCode::KeyX => Some('x'),
        KeyCode::KeyY => Some('y'),
        KeyCode::KeyZ => Some('z'),
        KeyCode::Digit0 => Some('0'),
        KeyCode::Digit1 => Some('1'),
        KeyCode::Digit2 => Some('2'),
        KeyCode::Digit3 => Some('3'),
        KeyCode::Digit4 => Some('4'),
        KeyCode::Digit5 => Some('5'),
        KeyCode::Digit6 => Some('6'),
        KeyCode::Digit7 => Some('7'),
        KeyCode::Digit8 => Some('8'),
        KeyCode::Digit9 => Some('9'),
        KeyCode::Backquote => Some('`'),
        KeyCode::Minus => Some('-'),
        KeyCode::Equal => Some('='),
        KeyCode::BracketLeft => Some('['),
        KeyCode::BracketRight => Some(']'),
        KeyCode::Backslash => Some('\\'),
        KeyCode::Semicolon => Some(';'),
        KeyCode::Quote => Some('\''),
        KeyCode::Comma => Some(','),
        KeyCode::Period => Some('.'),
        KeyCode::Slash => Some('/'),
        KeyCode::Space => Some(' '),
        _ => None,
    }
}

pub(super) fn keypad_key(event: KeyEventView<'_>) -> Option<KeypadKey> {
    if event.location == KeyLocation::Numpad {
        // When: event.location is Numpad, preserve NumLock-off navigation
        // identities before consulting the physical digit position.
        let navigation = match event.logical_key {
            Key::Named(NamedKey::ArrowLeft) => Some(KeypadKey::Left),
            Key::Named(NamedKey::ArrowRight) => Some(KeypadKey::Right),
            Key::Named(NamedKey::ArrowUp) => Some(KeypadKey::Up),
            Key::Named(NamedKey::ArrowDown) => Some(KeypadKey::Down),
            Key::Named(NamedKey::PageUp) => Some(KeypadKey::PageUp),
            Key::Named(NamedKey::PageDown) => Some(KeypadKey::PageDown),
            Key::Named(NamedKey::Home) => Some(KeypadKey::Home),
            Key::Named(NamedKey::End) => Some(KeypadKey::End),
            Key::Named(NamedKey::Insert) => Some(KeypadKey::Insert),
            Key::Named(NamedKey::Delete) => Some(KeypadKey::Delete),
            Key::Named(NamedKey::Clear) => Some(KeypadKey::Begin),
            _ => None,
        };
        if navigation.is_some() {
            // When: navigation contains a NumLock-off meaning, it determines
            // the keypad protocol number.
            return navigation;
        }
    }
    if let Some(physical) = physical_keypad_key(event.physical_key) {
        // When: physical_keypad_key recognizes the position, use its stable
        // keypad identity across platform logical-key differences.
        return Some(physical);
    }
    if event.location != KeyLocation::Numpad {
        // When: event.location is not Numpad and no physical keypad key matched,
        // this event belongs to the main keyboard.
        return None;
    }
    match event.logical_key {
        Key::Character(text) => match text.as_str() {
            "0" => Some(KeypadKey::Digit(0)),
            "1" => Some(KeypadKey::Digit(1)),
            "2" => Some(KeypadKey::Digit(2)),
            "3" => Some(KeypadKey::Digit(3)),
            "4" => Some(KeypadKey::Digit(4)),
            "5" => Some(KeypadKey::Digit(5)),
            "6" => Some(KeypadKey::Digit(6)),
            "7" => Some(KeypadKey::Digit(7)),
            "8" => Some(KeypadKey::Digit(8)),
            "9" => Some(KeypadKey::Digit(9)),
            "." => Some(KeypadKey::Decimal),
            "/" => Some(KeypadKey::Divide),
            "*" => Some(KeypadKey::Multiply),
            "-" => Some(KeypadKey::Subtract),
            "+" => Some(KeypadKey::Add),
            "=" => Some(KeypadKey::Equal),
            "," => Some(KeypadKey::Separator),
            _ => None,
        },
        Key::Named(NamedKey::Enter) => Some(KeypadKey::Enter),
        _ => None,
    }
}

pub(super) fn physical_keypad_key(physical_key: PhysicalKey) -> Option<KeypadKey> {
    match physical_key {
        PhysicalKey::Code(KeyCode::Numpad0) => Some(KeypadKey::Digit(0)),
        PhysicalKey::Code(KeyCode::Numpad1) => Some(KeypadKey::Digit(1)),
        PhysicalKey::Code(KeyCode::Numpad2) => Some(KeypadKey::Digit(2)),
        PhysicalKey::Code(KeyCode::Numpad3) => Some(KeypadKey::Digit(3)),
        PhysicalKey::Code(KeyCode::Numpad4) => Some(KeypadKey::Digit(4)),
        PhysicalKey::Code(KeyCode::Numpad5) => Some(KeypadKey::Digit(5)),
        PhysicalKey::Code(KeyCode::Numpad6) => Some(KeypadKey::Digit(6)),
        PhysicalKey::Code(KeyCode::Numpad7) => Some(KeypadKey::Digit(7)),
        PhysicalKey::Code(KeyCode::Numpad8) => Some(KeypadKey::Digit(8)),
        PhysicalKey::Code(KeyCode::Numpad9) => Some(KeypadKey::Digit(9)),
        PhysicalKey::Code(KeyCode::NumpadDecimal) => Some(KeypadKey::Decimal),
        PhysicalKey::Code(KeyCode::NumpadDivide) => Some(KeypadKey::Divide),
        PhysicalKey::Code(KeyCode::NumpadMultiply) => Some(KeypadKey::Multiply),
        PhysicalKey::Code(KeyCode::NumpadSubtract) => Some(KeypadKey::Subtract),
        PhysicalKey::Code(KeyCode::NumpadAdd) => Some(KeypadKey::Add),
        PhysicalKey::Code(KeyCode::NumpadEnter) => Some(KeypadKey::Enter),
        PhysicalKey::Code(KeyCode::NumpadEqual) => Some(KeypadKey::Equal),
        PhysicalKey::Code(KeyCode::NumpadComma) => Some(KeypadKey::Separator),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Keymap bridge
// ---------------------------------------------------------------------------

pub(in crate::app) fn key_event_to_string(
    event: &KeyEvent,
    mods: ModifiersState,
) -> Option<String> {
    key_event_to_strings(event, mods).into_iter().next()
}

pub(in crate::app) fn key_event_to_strings(event: &KeyEvent, mods: ModifiersState) -> Vec<String> {
    key_strings_for_parts(&event.key_without_modifiers(), &event.logical_key, mods)
}

pub(super) fn key_strings_for_parts(
    key_without_modifiers: &Key,
    logical_key: &Key,
    mods: ModifiersState,
) -> Vec<String> {
    let mut result = key_to_strings(key_without_modifiers, mods);
    for candidate in key_to_strings(logical_key, mods) {
        if !result.contains(&candidate) {
            result.push(candidate);
        }
    }
    result
}

/// Canonical chord string for a key press, such as `ctrl+shift+p`.
#[doc(hidden)]
pub fn key_to_string(key: &Key, mods: ModifiersState) -> Option<String> {
    let mut candidates = key_candidates(key, mods)?;
    candidates.dedup();
    let candidate = candidates.into_iter().next()?;
    Some(chord_string(candidate.as_str(), mods))
}

/// Every chord string a key press can match, normalized alias first.
#[doc(hidden)]
pub fn key_to_strings(key: &Key, mods: ModifiersState) -> Vec<String> {
    let Some(mut candidates) = key_candidates(key, mods) else {
        // When: key_candidates cannot name key, no configured chord can match it.
        return Vec::new();
    };
    candidates.dedup();
    candidates.into_iter().map(|candidate| chord_string(candidate.as_str(), mods)).collect()
}

pub(super) fn chord_string(key_name: &str, mods: ModifiersState) -> String {
    let mut parts: Vec<String> = Vec::new();
    if mods.super_key() {
        parts.push("super".into());
    }
    if mods.control_key() {
        parts.push("ctrl".into());
    }
    if mods.alt_key() {
        parts.push("alt".into());
    }
    if mods.shift_key() {
        parts.push("shift".into());
    }
    parts.push(key_name.to_string());
    parts.join("+").to_ascii_lowercase()
}

pub(super) fn key_candidates(key: &Key, mods: ModifiersState) -> Option<Vec<KeyName>> {
    let primary = key_name(key)?;
    let mut candidates = Vec::new();
    if let Key::Character(text) = key {
        if let Some(character) = text.chars().next().filter(|_| text.chars().count() == 1) {
            let normalized = unshift_ascii(character);
            if (mods.shift_key() || character.is_uppercase()) && normalized != character {
                candidates.push(KeyName::Owned(normalized.to_string()));
            }
            let lower = text.to_ascii_lowercase();
            if lower != *text {
                candidates.push(KeyName::Owned(lower));
            }
        }
    }
    candidates.push(primary);
    Some(candidates)
}

/// Map a key to the spelling accepted by the keymap parser.
#[doc(hidden)]
pub fn key_name(key: &Key) -> Option<KeyName> {
    Some(match key {
        Key::Named(named) => {
            // When: named selects a supported keymap identity, return its
            // canonical spelling; unsupported variants stop at the fallback arm.
            KeyName::Static(match named {
                NamedKey::Enter => "enter",
                NamedKey::Backspace => "backspace",
                NamedKey::Tab => "tab",
                NamedKey::Escape => "escape",
                NamedKey::Space => "space",
                NamedKey::ArrowUp => "up",
                NamedKey::ArrowDown => "down",
                NamedKey::ArrowRight => "right",
                NamedKey::ArrowLeft => "left",
                NamedKey::Home => "home",
                NamedKey::End => "end",
                NamedKey::PageUp => "pageup",
                NamedKey::PageDown => "pagedown",
                NamedKey::Insert => "insert",
                NamedKey::Delete => "delete",
                NamedKey::ContextMenu => "menu",
                NamedKey::Pause => "pause",
                NamedKey::PrintScreen => "printscreen",
                NamedKey::ScrollLock => "scrolllock",
                NamedKey::NumLock => "numlock",
                NamedKey::CapsLock => "capslock",
                NamedKey::F1 => "f1",
                NamedKey::F2 => "f2",
                NamedKey::F3 => "f3",
                NamedKey::F4 => "f4",
                NamedKey::F5 => "f5",
                NamedKey::F6 => "f6",
                NamedKey::F7 => "f7",
                NamedKey::F8 => "f8",
                NamedKey::F9 => "f9",
                NamedKey::F10 => "f10",
                NamedKey::F11 => "f11",
                NamedKey::F12 => "f12",
                NamedKey::F13 => "f13",
                NamedKey::F14 => "f14",
                NamedKey::F15 => "f15",
                NamedKey::F16 => "f16",
                NamedKey::F17 => "f17",
                NamedKey::F18 => "f18",
                NamedKey::F19 => "f19",
                NamedKey::F20 => "f20",
                NamedKey::F21 => "f21",
                NamedKey::F22 => "f22",
                NamedKey::F23 => "f23",
                NamedKey::F24 => "f24",
                NamedKey::F25 => "f25",
                NamedKey::F26 => "f26",
                NamedKey::F27 => "f27",
                NamedKey::F28 => "f28",
                NamedKey::F29 => "f29",
                NamedKey::F30 => "f30",
                NamedKey::F31 => "f31",
                NamedKey::F32 => "f32",
                NamedKey::F33 => "f33",
                NamedKey::F34 => "f34",
                NamedKey::F35 => "f35",
                _ => {
                    // When: named has no canonical keymap spelling, it is not bindable.
                    return None;
                }
            })
        }
        Key::Character(text) => KeyName::Owned(text.to_string()),
        _ => {
            // When: key is neither Named nor Character, it has no stable chord spelling.
            return None;
        }
    })
}

impl KeyName {
    /// Borrow the keymap spelling without exposing its storage.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Static(name) => name,
            Self::Owned(name) => name.as_str(),
        }
    }
}

#[cfg(test)]
#[path = "key_encoding_names_tests.rs"]
mod key_encoding_names_tests;
