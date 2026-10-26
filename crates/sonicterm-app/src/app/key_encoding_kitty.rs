//! Kitty keyboard protocol event, modifier and associated-text encoding.

use super::*;

pub(super) fn encode_kitty(
    event: KeyEventView<'_>,
    mods: ModifiersState,
    flags: u8,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    let report_all = flags & KITTY_REPORT_ALL != 0;
    let disambiguate = flags & KITTY_DISAMBIGUATE != 0 || report_all;
    let report_events = flags & KITTY_REPORT_EVENTS != 0;

    if event.state == ElementState::Released && !report_events {
        // When: the active Kitty flags omit event reporting, releases remain
        // silent just as they are in legacy terminal input.
        return None;
    }

    if !report_all && event.state == ElementState::Pressed {
        // When: report_all is off, every text-producing press or repeat remains
        // direct UTF-8, including NumLock-on keypad text.
        if let Some(text) = standalone_text(event, mods) {
            // When: standalone_text yields safe text, send its exact layout or
            // composition result instead of manufacturing a key code.
            return Some(text.as_bytes().to_vec());
        }
    }

    if let Some(keypad) = keypad_key(event) {
        // When: keypad_key recognizes this event, a negotiated disambiguate
        // flag may preserve its dedicated functional identity.
        if disambiguate {
            // When: disambiguate is active, preserve a non-text or all-key
            // keypad identity instead of its NumLock meaning.
            return Some(encode_kitty_keypad(keypad, event, mods, flags));
        }
    }

    let logical_key = if matches!(event.logical_key, Key::Dead(_)) {
        // A dead key uses its layout-resolved unmodified identity so
        // report-all can identify the physical composition key.
        event
            .unmodified_character
            .map(|character| Key::Character(character.to_string().into()))
            .unwrap_or_else(|| event.logical_key.clone())
    } else {
        // When: matches does not classify event.logical_key as Key::Dead,
        // retain winit's identity rather than fabricating a composition key.
        event.logical_key.clone()
    };
    match &logical_key {
        Key::Character(text) => {
            // When: logical_key is Character text, encode either its compatible
            // legacy form or the canonical Unicode key number.
            let primary = event.unshifted_character(text);
            let event_type = kitty_event_type(event, report_events);
            let can_use_legacy = !disambiguate
                && !report_all
                && (!report_events || !event.repeat)
                && legacy_text_modifiers(primary, mods)
                && event.state == ElementState::Pressed;
            if can_use_legacy {
                // When: can_use_legacy is true, preserve the established
                // C0/Meta representation for this text chord.
                return Some(encode_legacy_text(
                    primary,
                    event.shifted_character(),
                    event.text.unwrap_or(text),
                    mods,
                    0,
                ));
            }
            Some(encode_csi_u(
                u32::from(primary),
                alternate_codes(primary, event, mods, flags),
                mods,
                event_type,
                associated_text(event, flags),
            ))
        }
        Key::Named(named) => encode_kitty_named(*named, event, mods, flags, modes, disambiguate),
        _ if report_all => Some(encode_csi_u(
            0,
            None,
            mods,
            kitty_event_type(event, report_events),
            associated_text(event, flags),
        )),
        _ if event.state == ElementState::Released => None,
        _ => encode_legacy(event, mods, modes),
    }
}

pub(super) fn encode_kitty_named(
    key: NamedKey,
    event: KeyEventView<'_>,
    mods: ModifiersState,
    flags: u8,
    modes: KeyboardModes,
    disambiguate: bool,
) -> Option<Vec<u8>> {
    let report_all = flags & KITTY_REPORT_ALL != 0;
    let report_events = flags & KITTY_REPORT_EVENTS != 0;
    let legacy_mode = flags & (KITTY_DISAMBIGUATE | KITTY_REPORT_EVENTS | KITTY_REPORT_ALL) == 0;
    let event_type = kitty_event_type(event, report_events);
    let text = associated_text(event, flags);

    let c0_code = match key {
        NamedKey::Enter => Some(13),
        NamedKey::Tab => Some(9),
        NamedKey::Backspace => Some(127),
        NamedKey::Escape => Some(27),
        NamedKey::Space => Some(32),
        _ => None,
    };
    if let Some(code) = c0_code {
        // When: c0_code exists, apply the reset-safe C0 exceptions before
        // falling back to canonical CSI-u.
        let reset_key = matches!(key, NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace);
        if reset_key && !report_all && mods.is_empty() && event.state == ElementState::Pressed {
            // When: reset_key is an unmodified press or repeat and report_all is
            // false, retain its reset-safe C0 byte.
            return encode_legacy_named(key, mods, modes);
        }
        if reset_key && !report_all && event.state == ElementState::Released && mods.is_empty() {
            // When: reset_key is an unmodified release while report_all is false,
            // Kitty deliberately omits it so a crashed client leaves usable input.
            return None;
        }
        if key == NamedKey::Escape
            && !disambiguate
            && !report_all
            && mods.is_empty()
            && event.state == ElementState::Pressed
            && !event.repeat
        {
            // When: key is plain Escape without disambiguate or report_all,
            // preserve the legacy ESC byte while repeat/release remain distinguishable.
            return encode_legacy_named(key, mods, modes);
        }
        if legacy_mode {
            // When: legacy_mode is active, C0 keys keep the
            // complete legacy modifier table rather than changing protocol.
            return encode_legacy_named(key, mods, modes);
        }
        let primary = (key == NamedKey::Space).then_some(' ');
        return Some(encode_csi_u(
            code,
            primary.and_then(|primary| alternate_codes(primary, event, mods, flags)),
            mods,
            event_type,
            text,
        ));
    }

    if is_modifier_key(key) && !report_all {
        // When: key is a modifier and report_all is disabled, Kitty
        // treats it as state attached to other keys rather than an event.
        return None;
    }

    if legacy_mode {
        // When: legacy_mode is active, prefer terminfo-compatible encodings for
        // functional keys that have one.
        if let Some(encoding) = named_functional_encoding(key) {
            // When: named_functional_encoding yields encoding, preserve xterm
            // functional encodings and DECCKM selection.
            return Some(encode_functional_legacy(encoding, mods, modes.application_cursor_keys()));
        }
    }

    let encoding = if let Some(encoding) = named_functional_kitty_encoding(key) {
        // A recognized Kitty functional key uses its
        // protocol-defined CSI final byte or tilde number.
        encoding
    } else {
        // When: named_functional_kitty_encoding cannot represent key, require
        // an enhancement before using its private-use code point.
        if !disambiguate && !report_events {
            // When: neither disambiguate nor report_events is active, a
            // Kitty-only PUA identity must remain silent.
            return None;
        }
        FunctionalEncoding::CsiU(kitty_functional_number(key, event.location)?)
    };
    Some(encode_functional_kitty(encoding, mods, event_type, text))
}

pub(super) fn encode_kitty_keypad(
    key: KeypadKey,
    event: KeyEventView<'_>,
    mods: ModifiersState,
    flags: u8,
) -> Vec<u8> {
    let code = match key {
        KeypadKey::Digit(digit) => 57399 + u32::from(digit),
        KeypadKey::Decimal => 57409,
        KeypadKey::Divide => 57410,
        KeypadKey::Multiply => 57411,
        KeypadKey::Subtract => 57412,
        KeypadKey::Add => 57413,
        KeypadKey::Enter => 57414,
        KeypadKey::Equal => 57415,
        KeypadKey::Separator => 57416,
        KeypadKey::Left => 57417,
        KeypadKey::Right => 57418,
        KeypadKey::Up => 57419,
        KeypadKey::Down => 57420,
        KeypadKey::PageUp => 57421,
        KeypadKey::PageDown => 57422,
        KeypadKey::Home => 57423,
        KeypadKey::End => 57424,
        KeypadKey::Insert => 57425,
        KeypadKey::Delete => 57426,
        KeypadKey::Begin => 57427,
    };
    encode_csi_u(
        code,
        None,
        mods,
        kitty_event_type(event, flags & KITTY_REPORT_EVENTS != 0),
        associated_text(event, flags),
    )
}

pub(super) fn kitty_function_key_encoding(function_number: u8) -> FunctionalEncoding {
    match function_number {
        1 => FunctionalEncoding::Letter('P'),
        2 => FunctionalEncoding::Letter('Q'),
        3 => FunctionalEncoding::Tilde(13),
        4 => FunctionalEncoding::Letter('S'),
        5..=12 => {
            const TILDE: [u16; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            FunctionalEncoding::Tilde(TILDE[usize::from(function_number - 5)])
        }
        _ => FunctionalEncoding::CsiU(57363 + u32::from(function_number)),
    }
}

pub(super) fn encode_functional_kitty(
    encoding: FunctionalEncoding,
    mods: ModifiersState,
    event_type: Option<u8>,
    text: Option<Vec<u32>>,
) -> Vec<u8> {
    match encoding {
        FunctionalEncoding::Letter(final_byte) if mods.is_empty() && event_type.is_none() => {
            format!("\x1b[{final_byte}").into_bytes()
        }
        FunctionalEncoding::Letter(final_byte) => {
            let field = modifier_event_field(mods, event_type);
            format!("\x1b[1;{field}{final_byte}").into_bytes()
        }
        FunctionalEncoding::Tilde(code) if mods.is_empty() && event_type.is_none() => {
            format!("\x1b[{code}~").into_bytes()
        }
        FunctionalEncoding::Tilde(code) => {
            let field = modifier_event_field(mods, event_type);
            format!("\x1b[{code};{field}~").into_bytes()
        }
        FunctionalEncoding::CsiU(code) => encode_csi_u(code, None, mods, event_type, text),
    }
}

pub(super) fn named_functional_kitty_encoding(key: NamedKey) -> Option<FunctionalEncoding> {
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
        NamedKey::F1 => Some(kitty_function_key_encoding(1)),
        NamedKey::F2 => Some(kitty_function_key_encoding(2)),
        NamedKey::F3 => Some(kitty_function_key_encoding(3)),
        NamedKey::F4 => Some(kitty_function_key_encoding(4)),
        NamedKey::F5 => Some(kitty_function_key_encoding(5)),
        NamedKey::F6 => Some(kitty_function_key_encoding(6)),
        NamedKey::F7 => Some(kitty_function_key_encoding(7)),
        NamedKey::F8 => Some(kitty_function_key_encoding(8)),
        NamedKey::F9 => Some(kitty_function_key_encoding(9)),
        NamedKey::F10 => Some(kitty_function_key_encoding(10)),
        NamedKey::F11 => Some(kitty_function_key_encoding(11)),
        NamedKey::F12 => Some(kitty_function_key_encoding(12)),
        NamedKey::F13 => Some(kitty_function_key_encoding(13)),
        NamedKey::F14 => Some(kitty_function_key_encoding(14)),
        NamedKey::F15 => Some(kitty_function_key_encoding(15)),
        NamedKey::F16 => Some(kitty_function_key_encoding(16)),
        NamedKey::F17 => Some(kitty_function_key_encoding(17)),
        NamedKey::F18 => Some(kitty_function_key_encoding(18)),
        NamedKey::F19 => Some(kitty_function_key_encoding(19)),
        NamedKey::F20 => Some(kitty_function_key_encoding(20)),
        NamedKey::F21 => Some(kitty_function_key_encoding(21)),
        NamedKey::F22 => Some(kitty_function_key_encoding(22)),
        NamedKey::F23 => Some(kitty_function_key_encoding(23)),
        NamedKey::F24 => Some(kitty_function_key_encoding(24)),
        NamedKey::F25 => Some(kitty_function_key_encoding(25)),
        NamedKey::F26 => Some(kitty_function_key_encoding(26)),
        NamedKey::F27 => Some(kitty_function_key_encoding(27)),
        NamedKey::F28 => Some(kitty_function_key_encoding(28)),
        NamedKey::F29 => Some(kitty_function_key_encoding(29)),
        NamedKey::F30 => Some(kitty_function_key_encoding(30)),
        NamedKey::F31 => Some(kitty_function_key_encoding(31)),
        NamedKey::F32 => Some(kitty_function_key_encoding(32)),
        NamedKey::F33 => Some(kitty_function_key_encoding(33)),
        NamedKey::F34 => Some(kitty_function_key_encoding(34)),
        NamedKey::F35 => Some(kitty_function_key_encoding(35)),
        _ => None,
    }
}

pub(super) fn kitty_functional_number(key: NamedKey, location: KeyLocation) -> Option<u32> {
    let right = location == KeyLocation::Right;
    match key {
        NamedKey::CapsLock => Some(57358),
        NamedKey::ScrollLock => Some(57359),
        NamedKey::NumLock => Some(57360),
        NamedKey::PrintScreen => Some(57361),
        NamedKey::Pause => Some(57362),
        NamedKey::ContextMenu => Some(57363),
        NamedKey::MediaPlay => Some(57428),
        NamedKey::MediaPause => Some(57429),
        NamedKey::MediaPlayPause => Some(57430),
        NamedKey::MediaRecord => Some(57437),
        NamedKey::MediaStop => Some(57432),
        NamedKey::MediaFastForward => Some(57433),
        NamedKey::MediaRewind => Some(57434),
        NamedKey::MediaTrackNext => Some(57435),
        NamedKey::MediaTrackPrevious => Some(57436),
        NamedKey::AudioVolumeDown => Some(57438),
        NamedKey::AudioVolumeUp => Some(57439),
        NamedKey::AudioVolumeMute => Some(57440),
        NamedKey::Shift => Some(if right { 57447 } else { 57441 }),
        NamedKey::Control => Some(if right { 57448 } else { 57442 }),
        NamedKey::Alt => Some(if right { 57449 } else { 57443 }),
        NamedKey::Super => Some(if right { 57450 } else { 57444 }),
        NamedKey::Hyper => Some(if right { 57451 } else { 57445 }),
        NamedKey::Meta => Some(if right { 57452 } else { 57446 }),
        NamedKey::AltGraph => Some(57453),
        _ => None,
    }
}

pub(super) fn encode_csi_u(
    code: u32,
    alternate: Option<String>,
    mods: ModifiersState,
    event_type: Option<u8>,
    text: Option<Vec<u32>>,
) -> Vec<u8> {
    let mut sequence = format!("\x1b[{code}");
    if let Some(alternate) = alternate {
        sequence.push_str(&alternate);
    }
    if !mods.is_empty() || event_type.is_some() || text.is_some() {
        sequence.push(';');
        if !mods.is_empty() || event_type.is_some() {
            sequence.push_str(&modifier_event_field(mods, event_type));
        }
    }
    if let Some(text) = text {
        sequence.push(';');
        sequence.push_str(&text.iter().map(u32::to_string).collect::<Vec<_>>().join(":"));
    }
    sequence.push('u');
    sequence.into_bytes()
}

pub(super) fn modifier_event_field(mods: ModifiersState, event_type: Option<u8>) -> String {
    let mut field = modifier_parameter(mods).to_string();
    if let Some(event_type) = event_type {
        field.push(':');
        field.push_str(&event_type.to_string());
    }
    field
}

pub(super) fn kitty_event_type(event: KeyEventView<'_>, report_events: bool) -> Option<u8> {
    if !report_events {
        // When: report_events is disabled, press and repeat retain their
        // compatibility form and releases were already filtered.
        return None;
    }
    match event.state {
        ElementState::Released => Some(3),
        ElementState::Pressed if event.repeat => Some(2),
        ElementState::Pressed => None,
    }
}

pub(super) fn associated_text(event: KeyEventView<'_>, flags: u8) -> Option<Vec<u32>> {
    if flags & (KITTY_REPORT_ALL | KITTY_REPORT_TEXT) != (KITTY_REPORT_ALL | KITTY_REPORT_TEXT)
        || event.state == ElementState::Released
    {
        // When: flags do not contain both report-all and report-text, or the
        // event is Released, no associated text field is permitted.
        return None;
    }
    if matches!(event.logical_key, Key::Named(named) if *named != NamedKey::Space) {
        // When: matches! identifies a named non-Space key, Cocoa function text has no Kitty associated text.
        return None;
    }
    let codepoints: Vec<_> = event
        .text_with_all_modifiers
        .or(event.text)
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .map(u32::from)
        .collect();
    (!codepoints.is_empty()).then_some(codepoints)
}

pub(super) fn alternate_codes(
    primary: char,
    event: KeyEventView<'_>,
    mods: ModifiersState,
    flags: u8,
) -> Option<String> {
    if flags & KITTY_REPORT_ALTERNATES == 0 {
        // When: flags omit KITTY_REPORT_ALTERNATES, do not add optional
        // shifted or PC-101 key fields to the canonical key number.
        return None;
    }
    let shifted = mods
        .shift_key()
        .then(|| event.shifted_character())
        .flatten()
        .filter(|shifted| *shifted != primary);
    let base = physical_ascii(event.physical_key).filter(|base| *base != primary);

    match (shifted, base) {
        (Some(shifted), Some(base)) => Some(format!(":{}:{}", u32::from(shifted), u32::from(base))),
        (Some(shifted), None) => Some(format!(":{}", u32::from(shifted))),
        (None, Some(base)) => Some(format!("::{}", u32::from(base))),
        (None, None) => None,
    }
}

#[cfg(test)]
#[path = "key_encoding_kitty_tests.rs"]
mod key_encoding_kitty_tests;
