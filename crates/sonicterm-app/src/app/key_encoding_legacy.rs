//! Legacy terminal keyboard protocol encoding without changing byte sequences.

use super::*;

pub(super) fn encode_legacy(
    event: KeyEventView<'_>,
    mods: ModifiersState,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    if event.state == ElementState::Released {
        // When: `event.state` is Released, legacy protocols have no matching
        // release representation, so the PTY receives nothing.
        return None;
    }

    let numeric_keypad = matches!(
        physical_keypad_key(event.physical_key),
        Some(KeypadKey::Digit(_))
    ) && matches!(event.logical_key, Key::Character(text) if text.len() == 1 && text.as_bytes()[0].is_ascii_digit());
    if numeric_keypad {
        // When: numeric_keypad is true, preserve OS numeric intent despite DECKPAM.
        if let Key::Character(text) = event.logical_key {
            // When: logical_key is Character, retain the existing text and modifier encoding rules.
            return Some(encode_legacy_text(
                event.unshifted_character(text),
                event.shifted_character(),
                event.text.filter(|text| !text.is_empty()).unwrap_or(text),
                mods,
                modes.modify_other_keys(),
            ));
        }
    }

    let keypad = if modes.application_keypad() {
        // Application-keypad mode gives physical identity precedence over its
        // NumLock-dependent logical meaning.
        physical_keypad_key(event.physical_key).or_else(|| keypad_key(event))
    } else {
        // When: application_keypad is inactive, use the keypad's logical key.
        keypad_key(event)
    };
    if let Some(keypad) = keypad {
        // When: keypad detection succeeds, give legacy keypad rules first
        // opportunity to encode the event.
        if let Some(encoded) = encode_legacy_keypad(keypad, event, mods, modes) {
            // When: encoded keypad bytes exist, keep
            // DECKPAM or NumLock identity instead of treating it as main-key text.
            return Some(encoded);
        }
    }

    if let Some(text) = standalone_text(event, mods) {
        // When: standalone_text yields text, preserve it unless level-two
        // modifyOtherKeys explicitly owns an unconsumed Shift chord.
        let has_command_modifier =
            mods.intersects(ModifiersState::ALT | ModifiersState::CONTROL | ModifiersState::SUPER);
        if modes.modify_other_keys() != 2
            || mods.is_empty()
            || has_command_modifier
            || text.chars().nth(1).is_some()
        {
            // When: MOK does not own the chord or text spans multiple codepoints, preserve the produced UTF-8.
            return Some(text.as_bytes().to_vec());
        }
    }

    match event.logical_key {
        Key::Character(text) => Some(encode_legacy_text(
            event.unshifted_character(text),
            event.shifted_character(),
            event.text.unwrap_or(text),
            mods,
            modes.modify_other_keys(),
        )),
        Key::Named(named) => encode_legacy_named(*named, mods, modes),
        _ => None,
    }
}

pub(super) fn encode_legacy_named(
    key: NamedKey,
    mods: ModifiersState,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    match key {
        NamedKey::Enter => Some(encode_legacy_enter(mods, modes)),
        NamedKey::Backspace => Some(encode_legacy_backspace(mods, modes)),
        NamedKey::Tab => Some(encode_legacy_tab(mods, modes)),
        NamedKey::Escape => Some(encode_legacy_escape(mods, modes)),
        NamedKey::Space => {
            Some(encode_legacy_text(' ', Some(' '), " ", mods, modes.modify_other_keys()))
        }
        NamedKey::ArrowUp => Some(encode_functional_legacy(
            FunctionalEncoding::Letter('A'),
            mods,
            modes.application_cursor_keys(),
        )),
        NamedKey::ArrowDown => Some(encode_functional_legacy(
            FunctionalEncoding::Letter('B'),
            mods,
            modes.application_cursor_keys(),
        )),
        NamedKey::ArrowRight => Some(encode_functional_legacy(
            FunctionalEncoding::Letter('C'),
            mods,
            modes.application_cursor_keys(),
        )),
        NamedKey::ArrowLeft => Some(encode_functional_legacy(
            FunctionalEncoding::Letter('D'),
            mods,
            modes.application_cursor_keys(),
        )),
        NamedKey::Home => Some(encode_functional_legacy(
            FunctionalEncoding::Letter('H'),
            mods,
            modes.application_cursor_keys(),
        )),
        NamedKey::End => Some(encode_functional_legacy(
            FunctionalEncoding::Letter('F'),
            mods,
            modes.application_cursor_keys(),
        )),
        NamedKey::Insert => {
            Some(encode_functional_legacy(FunctionalEncoding::Tilde(2), mods, false))
        }
        NamedKey::Delete => {
            Some(encode_functional_legacy(FunctionalEncoding::Tilde(3), mods, false))
        }
        NamedKey::PageUp => {
            Some(encode_functional_legacy(FunctionalEncoding::Tilde(5), mods, false))
        }
        NamedKey::PageDown => {
            Some(encode_functional_legacy(FunctionalEncoding::Tilde(6), mods, false))
        }
        NamedKey::ContextMenu => {
            Some(encode_functional_legacy(FunctionalEncoding::Tilde(29), mods, false))
        }
        NamedKey::F1 => Some(encode_function_key_legacy(1, mods)),
        NamedKey::F2 => Some(encode_function_key_legacy(2, mods)),
        NamedKey::F3 => Some(encode_function_key_legacy(3, mods)),
        NamedKey::F4 => Some(encode_function_key_legacy(4, mods)),
        NamedKey::F5 => Some(encode_function_key_legacy(5, mods)),
        NamedKey::F6 => Some(encode_function_key_legacy(6, mods)),
        NamedKey::F7 => Some(encode_function_key_legacy(7, mods)),
        NamedKey::F8 => Some(encode_function_key_legacy(8, mods)),
        NamedKey::F9 => Some(encode_function_key_legacy(9, mods)),
        NamedKey::F10 => Some(encode_function_key_legacy(10, mods)),
        NamedKey::F11 => Some(encode_function_key_legacy(11, mods)),
        NamedKey::F12 => Some(encode_function_key_legacy(12, mods)),
        NamedKey::F13 => Some(encode_function_key_legacy(13, mods)),
        NamedKey::F14 => Some(encode_function_key_legacy(14, mods)),
        NamedKey::F15 => Some(encode_function_key_legacy(15, mods)),
        NamedKey::F16 => Some(encode_function_key_legacy(16, mods)),
        NamedKey::F17 => Some(encode_function_key_legacy(17, mods)),
        NamedKey::F18 => Some(encode_function_key_legacy(18, mods)),
        NamedKey::F19 => Some(encode_function_key_legacy(19, mods)),
        NamedKey::F20 => Some(encode_function_key_legacy(20, mods)),
        NamedKey::F21 => Some(encode_function_key_legacy(21, mods)),
        NamedKey::F22 => Some(encode_function_key_legacy(22, mods)),
        NamedKey::F23 => Some(encode_function_key_legacy(23, mods)),
        NamedKey::F24 => Some(encode_function_key_legacy(24, mods)),
        NamedKey::F25 => Some(encode_function_key_legacy(25, mods)),
        NamedKey::F26 => Some(encode_function_key_legacy(26, mods)),
        NamedKey::F27 => Some(encode_function_key_legacy(27, mods)),
        NamedKey::F28 => Some(encode_function_key_legacy(28, mods)),
        NamedKey::F29 => Some(encode_function_key_legacy(29, mods)),
        NamedKey::F30 => Some(encode_function_key_legacy(30, mods)),
        NamedKey::F31 => Some(encode_function_key_legacy(31, mods)),
        NamedKey::F32 => Some(encode_function_key_legacy(32, mods)),
        NamedKey::F33 => Some(encode_function_key_legacy(33, mods)),
        NamedKey::F34 => Some(encode_function_key_legacy(34, mods)),
        NamedKey::F35 => Some(encode_function_key_legacy(35, mods)),
        _ => None,
    }
}

pub(super) fn encode_legacy_enter(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
    if should_modify_other_key(modes.modify_other_keys(), 13, mods) {
        // When: should_modify_other_key claims Return, use xterm's explicit
        // key-code form rather than collapsing the chord into carriage return.
        return encode_modify_other_key(13, mods);
    }
    if !legacy_c0_modifiers(mods) {
        // When: legacy_c0_modifiers rejects Return's modifier combination,
        // use its unambiguous CSI-u representation.
        return encode_csi_u(13, None, mods, None, None);
    }

    let mut out = Vec::with_capacity(3);
    if mods.alt_key() {
        out.push(0x1b);
    }
    out.push(b'\r');
    if modes.newline() {
        // Newline mode appends LF after Return's carriage return.
        out.push(b'\n');
    }
    out
}

pub(super) fn encode_legacy_backspace(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
    let normal = if modes.backarrow_key() { b'\x08' } else { b'\x7f' };
    let code = match (mods.control_key(), normal) {
        (true, b'\x08') => b'\x7f',
        (true, _) => b'\x08',
        (false, _) => normal,
    };
    if should_modify_other_key(modes.modify_other_keys(), u32::from(code), mods) {
        // When: should_modify_other_key includes this Backspace variant, retain
        // its modifiers in xterm's explicit form.
        return encode_modify_other_key(u32::from(code), mods);
    }
    if !legacy_c0_modifiers(mods) {
        // When: legacy_c0_modifiers rejects Backspace, avoid
        // silently discarding them by using CSI-u.
        return encode_csi_u(u32::from(code), None, mods, None, None);
    }

    let mut out = Vec::with_capacity(2);
    if mods.alt_key() {
        out.push(0x1b);
    }
    out.push(code);
    out
}

pub(super) fn encode_legacy_tab(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
    if should_modify_other_key(modes.modify_other_keys(), 9, mods) {
        // When: should_modify_other_key claims Tab, emit xterm's
        // unambiguous key-code form before the BackTab compatibility fallback.
        return encode_modify_other_key(9, mods);
    }
    if !legacy_c0_modifiers(mods) {
        // When: legacy_c0_modifiers rejects Tab, preserve
        // them with CSI-u rather than aliasing the chord to Tab or BackTab.
        return encode_csi_u(9, None, mods, None, None);
    }
    let mut out = Vec::with_capacity(4);
    if mods.alt_key() {
        // Alt prepends the legacy Meta escape byte.
        out.push(0x1b);
    }
    if mods.shift_key() {
        // Shift turns Tab into the BackTab CSI Z sequence.
        out.extend_from_slice(b"\x1b[Z");
    } else {
        // When: mods omits Shift, Tab remains the horizontal-tab C0 byte.
        out.push(b'\t');
    }
    out
}

pub(super) fn encode_legacy_escape(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
    if should_modify_other_key(modes.modify_other_keys(), 27, mods) {
        // When: should_modify_other_key claims Escape, represent it explicitly
        // rather than emitting an ambiguous escape introducer.
        return encode_modify_other_key(27, mods);
    }
    if !legacy_c0_modifiers(mods) {
        // When: legacy_c0_modifiers rejects Escape, encode
        // an explicit key event instead of emitting an ambiguous ESC prefix.
        return encode_csi_u(27, None, mods, None, None);
    }
    let mut out = Vec::with_capacity(2);
    if mods.alt_key() {
        // Alt prepends Meta's ESC before the Escape key.
        out.push(0x1b);
    }
    out.push(0x1b);
    out
}

pub(super) fn encode_legacy_text(
    primary: char,
    shifted: Option<char>,
    produced_text: &str,
    mods: ModifiersState,
    modify_other_keys: u8,
) -> Vec<u8> {
    if produced_text.is_empty() {
        // When: produced_text is empty, avoid writing
        // an empty or fabricated terminal input record.
        return Vec::new();
    }

    let mok_character = mods.shift_key().then_some(shifted).flatten().unwrap_or(primary);
    if modify_other_keys == 1
        && mods == (ModifiersState::CONTROL | ModifiersState::SHIFT)
        && ('@'..='\u{7f}').contains(&mok_character)
    {
        // When: modify_other_keys is one and mods combines Shift+Control, keep mok_character's ASCII control alias.
        return vec![(mok_character as u8) & 0x1f];
    }
    let shifted_text = modify_other_keys == 2
        && mods == ModifiersState::SHIFT
        && primary != ' '
        && !('@'..='\u{7f}').contains(&primary);
    if shifted_text {
        // When: shifted_text consumes Shift, preserve UTF-8 without falling into ASCII-only legacy chord encoding.
        return produced_text.as_bytes().to_vec();
    }
    if should_modify_other_key(modify_other_keys, u32::from(mok_character), mods) {
        // When: should_modify_other_key claims the chord, report its layout character instead of Kitty's base.
        return encode_modify_other_key(u32::from(mok_character), mods);
    }

    let mut legacy_primary = primary;
    let mut legacy_mods = mods;
    if mods.shift_key() {
        // Fold layout-produced Shift symbols before
        // testing whether the remaining modifiers fit the legacy table.
        if let Some(shifted) = shifted.filter(|shifted| {
            *shifted != primary && (!mods.control_key() || !primary.is_ascii_alphabetic())
        }) {
            // A consumable shifted symbol incorporates Shift into the produced
            // character instead of retaining it as an independent modifier.
            legacy_primary = shifted;
            legacy_mods.remove(ModifiersState::SHIFT);
        }
    }
    if !legacy_text_modifiers(legacy_primary, legacy_mods) {
        // When: legacy_text_modifiers rejects this key and modifier set,
        // use CSI-u rather than silently dropping a modifier.
        return encode_csi_u(u32::from(primary), None, mods, None, None);
    }

    let mapped_control = legacy_mods.control_key().then(|| ctrl_mapping(legacy_primary)).flatten();
    let mut out = Vec::with_capacity(produced_text.len() + usize::from(legacy_mods.alt_key()));
    if legacy_mods.alt_key() {
        // Alt prefixes compatible legacy text with ESC.
        out.push(0x1b);
    }
    if legacy_mods.control_key() {
        // When: legacy_mods contains Control, prefer its C0/DEL mapping.
        if let Some(mapped) = mapped_control {
            // When: mapped_control contains a byte, it fully represents this chord.
            out.push(mapped);
            return out;
        }
    }
    let selected = legacy_mods.shift_key().then_some(shifted).flatten().unwrap_or(legacy_primary);
    let mut utf8 = [0; 4];
    out.extend_from_slice(selected.encode_utf8(&mut utf8).as_bytes());
    out
}

pub(super) fn encode_legacy_keypad(
    key: KeypadKey,
    event: KeyEventView<'_>,
    mods: ModifiersState,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    if modes.application_keypad() {
        // When: application_keypad is active, use DECKPAM's SS3 keypad table.
        let final_byte = match key {
            KeypadKey::Digit(digit) => char::from(b'p' + digit),
            KeypadKey::Decimal | KeypadKey::Delete => 'n',
            KeypadKey::Divide => 'o',
            KeypadKey::Multiply => 'j',
            KeypadKey::Subtract => 'm',
            KeypadKey::Add => 'k',
            KeypadKey::Enter => 'M',
            KeypadKey::Equal => 'X',
            KeypadKey::Separator => 'l',
            KeypadKey::Left => 'D',
            KeypadKey::Right => 'C',
            KeypadKey::Up => 'A',
            KeypadKey::Down => 'B',
            KeypadKey::PageUp => 'I',
            KeypadKey::PageDown => 'G',
            KeypadKey::Home => 'w',
            KeypadKey::End => 'q',
            KeypadKey::Insert => 'p',
            KeypadKey::Begin => 'E',
        };
        if mods.is_empty() {
            // When: mods is empty, emit the unprefixed DECKPAM SS3 sequence.
            return Some(format!("\x1bO{final_byte}").into_bytes());
        }
        let prefix = if mods.alt_key() { "\x1b\x1bO" } else { "\x1bO" };
        return Some(format!("{prefix}{final_byte}").into_bytes());
    }

    let fallback = match key {
        KeypadKey::Digit(digit) if matches!(event.logical_key, Key::Character(_)) => {
            char::from(b'0' + digit).to_string()
        }
        KeypadKey::Decimal if matches!(event.logical_key, Key::Character(_)) => ".".to_owned(),
        KeypadKey::Divide => "/".to_owned(),
        KeypadKey::Multiply => "*".to_owned(),
        KeypadKey::Subtract => "-".to_owned(),
        KeypadKey::Add => "+".to_owned(),
        KeypadKey::Enter => {
            // When: key is keypad Enter, reuse Return's C0 and newline-mode rules.
            return Some(encode_legacy_enter(mods, modes));
        }
        KeypadKey::Equal => "=".to_owned(),
        KeypadKey::Separator => ",".to_owned(),
        _ => {
            // When: key is a navigation keypad variant, let the named-key path
            // encode its normal-mode meaning.
            return None;
        }
    };
    let primary = fallback.chars().next().unwrap_or('\0');
    Some(encode_legacy_text(
        primary,
        event.shifted_character(),
        event.text.unwrap_or(&fallback),
        mods,
        modes.modify_other_keys(),
    ))
}

pub(super) fn encode_function_key_legacy(function_number: u8, mods: ModifiersState) -> Vec<u8> {
    encode_functional_legacy(function_key_encoding(function_number), mods, false)
}

pub(super) fn encode_functional_legacy(
    encoding: FunctionalEncoding,
    mods: ModifiersState,
    application_mode: bool,
) -> Vec<u8> {
    match encoding {
        FunctionalEncoding::Letter(final_byte) if mods.is_empty() => {
            let prefix = if application_mode || matches!(final_byte, 'P' | 'Q' | 'R' | 'S') {
                "\x1bO"
            } else {
                "\x1b["
            };
            format!("{prefix}{final_byte}").into_bytes()
        }
        FunctionalEncoding::Letter(final_byte) => {
            format!("\x1b[1;{}{final_byte}", modifier_parameter(mods)).into_bytes()
        }
        FunctionalEncoding::Tilde(code) if mods.is_empty() => format!("\x1b[{code}~").into_bytes(),
        FunctionalEncoding::Tilde(code) => {
            format!("\x1b[{code};{}~", modifier_parameter(mods)).into_bytes()
        }
        FunctionalEncoding::CsiU(code) => encode_csi_u(code, None, mods, None, None),
    }
}

pub(super) fn legacy_text_modifiers(primary: char, mods: ModifiersState) -> bool {
    is_legacy_ascii_key(primary)
        && !mods.super_key()
        && (mods.is_empty()
            || mods == ModifiersState::SHIFT
            || mods == ModifiersState::ALT
            || mods == ModifiersState::CONTROL
            || mods == (ModifiersState::SHIFT | ModifiersState::ALT)
            || mods == (ModifiersState::CONTROL | ModifiersState::ALT)
            || (primary == ' ' && mods == (ModifiersState::CONTROL | ModifiersState::SHIFT)))
}

pub(super) fn legacy_c0_modifiers(mods: ModifiersState) -> bool {
    [
        ModifiersState::empty(),
        ModifiersState::SHIFT,
        ModifiersState::ALT,
        ModifiersState::CONTROL,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        ModifiersState::ALT | ModifiersState::SHIFT,
        ModifiersState::CONTROL | ModifiersState::ALT,
    ]
    .contains(&mods)
}

pub(super) fn is_legacy_ascii_key(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '`' | '~'
                | '-'
                | '_'
                | '='
                | '+'
                | '['
                | '{'
                | ']'
                | '}'
                | '\\'
                | '|'
                | ';'
                | ':'
                | '\''
                | '"'
                | ','
                | '<'
                | '.'
                | '>'
                | '/'
                | '?'
                | ' '
                | '!'
                | '@'
                | '#'
                | '$'
                | '%'
                | '^'
                | '&'
                | '*'
                | '('
                | ')'
        )
}

pub(super) fn ctrl_mapping(character: char) -> Option<u8> {
    match character {
        ' ' | '@' | '2' => Some(0),
        'a'..='z' => Some((character as u8) - b'a' + 1),
        'A'..='Z' => Some((character as u8) - b'A' + 1),
        '[' | '3' => Some(27),
        '\\' | '4' => Some(28),
        ']' | '5' => Some(29),
        '^' | '~' | '6' => Some(30),
        '_' | '/' | '7' => Some(31),
        '?' | '8' => Some(127),
        _ if is_legacy_ascii_key(character) && character.is_ascii() => Some(character as u8),
        _ => None,
    }
}

pub(super) fn encode_modify_other_key(code: u32, mods: ModifiersState) -> Vec<u8> {
    format!("\x1b[27;{};{code}~", modifier_parameter(mods)).into_bytes()
}

pub(super) fn should_modify_other_key(level: u8, code: u32, mods: ModifiersState) -> bool {
    !mods.is_empty()
        && match level {
            1 => match code {
                8 | 127 => false,
                9 => mods != ModifiersState::SHIFT,
                13 => true,
                _ if mods == ModifiersState::SHIFT => false,
                _ if mods == ModifiersState::CONTROL => char::from_u32(code)
                    .and_then(ctrl_mapping)
                    .is_none_or(|mapped| mapped > 31 && mapped != 127),
                _ => true,
            },
            2 => true,
            _ => false,
        }
}

#[cfg(test)]
#[path = "key_encoding_legacy_tests.rs"]
mod key_encoding_legacy_tests;
