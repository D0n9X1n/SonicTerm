use sonicterm_vt::vt::KeyboardModes;
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey},
    platform::modifier_supplement::KeyEventExtModifierSupplement,
};

const KITTY_DISAMBIGUATE: u8 = 1;
const KITTY_REPORT_EVENTS: u8 = 1 << 1;
const KITTY_REPORT_ALTERNATES: u8 = 1 << 2;
const KITTY_REPORT_ALL: u8 = 1 << 3;
const KITTY_REPORT_TEXT: u8 = 1 << 4;

#[derive(Clone, Copy)]
struct KeyEventView<'a> {
    logical_key: &'a Key,
    unmodified_character: Option<char>,
    physical_key: PhysicalKey,
    text: Option<&'a str>,
    text_with_all_modifiers: Option<&'a str>,
    location: KeyLocation,
    state: ElementState,
    repeat: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeypadKey {
    Digit(u8),
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
    Enter,
    Equal,
    Separator,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
    Delete,
    Begin,
}

#[derive(Clone, Copy)]
enum FunctionalEncoding {
    Letter(char),
    Tilde(u16),
    CsiU(u32),
}

/// Encode one winit key event using the terminal's negotiated keyboard state.
pub(crate) fn encode_key(
    event: &KeyEvent,
    mods: ModifiersState,
    kitty_flags: u8,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    let key_without_modifiers = event.key_without_modifiers();
    encode_event(
        KeyEventView {
            logical_key: &event.logical_key,
            unmodified_character: key_character(&key_without_modifiers),
            physical_key: event.physical_key,
            text: event.text.as_deref(),
            text_with_all_modifiers: event.text_with_all_modifiers(),
            location: event.location,
            state: event.state,
            repeat: event.repeat,
        },
        mods,
        kitty_flags,
        modes,
    )
}

/// Backwards-compatible logical-key entry point retained for focused unit tests.
#[doc(hidden)]
pub fn encode_logical(
    key: &Key,
    mods: ModifiersState,
    kitty_flags: u8,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    encode_logical_with_modes(
        key,
        mods,
        kitty_flags,
        KeyboardModes::new(app_cursor, false, false, false, 0),
    )
}

/// Encode a synthetic logical key with a complete terminal-mode snapshot.
#[doc(hidden)]
pub fn encode_logical_with_modes(
    key: &Key,
    mods: ModifiersState,
    kitty_flags: u8,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    let text = match key {
        Key::Character(text) => Some(text.as_str()),
        Key::Named(NamedKey::Space) => Some(" "),
        _ => None,
    };
    encode_event(
        KeyEventView {
            logical_key: key,
            unmodified_character: key_character(key),
            physical_key: PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
            text,
            text_with_all_modifiers: text,
            location: KeyLocation::Standard,
            state: ElementState::Pressed,
            repeat: false,
        },
        mods,
        kitty_flags,
        modes,
    )
}

fn encode_event(
    event: KeyEventView<'_>,
    mods: ModifiersState,
    kitty_flags: u8,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    let reports_events = kitty_flags & KITTY_REPORT_EVENTS != 0;
    if event.state == ElementState::Released && !reports_events {
        // When: a Released event has no reports_events support, no terminal
        // protocol record can represent it.
        return None;
    }

    let kitty_behavior_flags = KITTY_DISAMBIGUATE | KITTY_REPORT_EVENTS | KITTY_REPORT_ALL;
    if kitty_flags & kitty_behavior_flags != 0 {
        // When: kitty_flags has a behavior bit, encode the event with Kitty rules.
        return encode_kitty(event, mods, kitty_flags, modes);
    }

    let legacy = encode_legacy(event, mods, modes);
    if kitty_flags & KITTY_REPORT_ALTERNATES != 0
        && legacy.as_deref().is_some_and(is_csi_u_encoding)
    {
        // Alternate reporting rebuilds an existing CSI-u record with optional
        // shifted and base-layout fields; other legacy encodings stay unchanged.
        encode_kitty(event, mods, kitty_flags, modes)
    } else {
        // When: no behavior flag or augmentable CSI-u record exists, retain legacy bytes.
        legacy
    }
}

fn is_csi_u_encoding(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[") && bytes.ends_with(b"u")
}

fn encode_legacy(
    event: KeyEventView<'_>,
    mods: ModifiersState,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    if event.state == ElementState::Released {
        // When: `event.state` is Released, legacy protocols have no matching
        // release representation, so the PTY receives nothing.
        return None;
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
        if modes.modify_other_keys() != 2 || mods.is_empty() || has_command_modifier {
            // When: modify_other_keys is not level two, mods is empty, or
            // has_command_modifier was consumed, preserve the OS-produced UTF-8.
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

fn encode_kitty(
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
            .map(|ch| Key::Character(ch.to_string().into()))
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

fn encode_legacy_named(
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

fn encode_kitty_named(
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

fn encode_legacy_enter(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
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

fn encode_legacy_backspace(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
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

fn encode_legacy_tab(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
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

fn encode_legacy_escape(mods: ModifiersState, modes: KeyboardModes) -> Vec<u8> {
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

fn encode_legacy_text(
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

    if should_modify_other_key(modify_other_keys, u32::from(primary), mods) {
        // When: should_modify_other_key claims this chord, use xterm's sequence
        // instead of the otherwise compatible legacy representation.
        return encode_modify_other_key(u32::from(primary), mods);
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

fn encode_legacy_keypad(
    key: KeypadKey,
    event: KeyEventView<'_>,
    mods: ModifiersState,
    modes: KeyboardModes,
) -> Option<Vec<u8>> {
    if modes.application_keypad() {
        // When: application_keypad is active, use DECKPAM's SS3 keypad table.
        let final_byte = match key {
            KeypadKey::Digit(n) => char::from(b'p' + n),
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
        KeypadKey::Digit(n) if matches!(event.logical_key, Key::Character(_)) => {
            char::from(b'0' + n).to_string()
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

fn encode_kitty_keypad(
    key: KeypadKey,
    event: KeyEventView<'_>,
    mods: ModifiersState,
    flags: u8,
) -> Vec<u8> {
    let code = match key {
        KeypadKey::Digit(n) => 57399 + u32::from(n),
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

fn encode_function_key_legacy(n: u8, mods: ModifiersState) -> Vec<u8> {
    encode_functional_legacy(function_key_encoding(n), mods, false)
}

fn function_key_encoding(n: u8) -> FunctionalEncoding {
    match n {
        1 => FunctionalEncoding::Letter('P'),
        2 => FunctionalEncoding::Letter('Q'),
        3 => FunctionalEncoding::Letter('R'),
        4 => FunctionalEncoding::Letter('S'),
        5..=12 => {
            const TILDE: [u16; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            FunctionalEncoding::Tilde(TILDE[usize::from(n - 5)])
        }
        13..=24 => {
            const TILDE: [u16; 12] = [25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45];
            FunctionalEncoding::Tilde(TILDE[usize::from(n - 13)])
        }
        _ => FunctionalEncoding::CsiU(57363 + u32::from(n)),
    }
}

fn kitty_function_key_encoding(n: u8) -> FunctionalEncoding {
    match n {
        1 => FunctionalEncoding::Letter('P'),
        2 => FunctionalEncoding::Letter('Q'),
        3 => FunctionalEncoding::Tilde(13),
        4 => FunctionalEncoding::Letter('S'),
        5..=12 => {
            const TILDE: [u16; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            FunctionalEncoding::Tilde(TILDE[usize::from(n - 5)])
        }
        _ => FunctionalEncoding::CsiU(57363 + u32::from(n)),
    }
}

fn encode_functional_legacy(
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

fn encode_functional_kitty(
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

fn named_functional_encoding(key: NamedKey) -> Option<FunctionalEncoding> {
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

fn named_functional_kitty_encoding(key: NamedKey) -> Option<FunctionalEncoding> {
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

fn kitty_functional_number(key: NamedKey, location: KeyLocation) -> Option<u32> {
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

fn is_modifier_key(key: NamedKey) -> bool {
    matches!(
        key,
        NamedKey::Shift
            | NamedKey::Control
            | NamedKey::Alt
            | NamedKey::Super
            | NamedKey::Hyper
            | NamedKey::Meta
            | NamedKey::AltGraph
    )
}

fn encode_csi_u(
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

fn modifier_event_field(mods: ModifiersState, event_type: Option<u8>) -> String {
    let mut field = modifier_parameter(mods).to_string();
    if let Some(event_type) = event_type {
        field.push(':');
        field.push_str(&event_type.to_string());
    }
    field
}

fn modifier_parameter(mods: ModifiersState) -> u16 {
    1 + u16::from(mods.shift_key())
        + u16::from(mods.alt_key()) * 2
        + u16::from(mods.control_key()) * 4
        + u16::from(mods.super_key()) * 8
}

fn kitty_event_type(event: KeyEventView<'_>, report_events: bool) -> Option<u8> {
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

fn associated_text(event: KeyEventView<'_>, flags: u8) -> Option<Vec<u32>> {
    if flags & (KITTY_REPORT_ALL | KITTY_REPORT_TEXT) != (KITTY_REPORT_ALL | KITTY_REPORT_TEXT)
        || event.state == ElementState::Released
    {
        // When: flags do not contain both report-all and report-text, or the
        // event is Released, no associated text field is permitted.
        return None;
    }
    let codepoints: Vec<_> = event
        .text_with_all_modifiers
        .or(event.text)
        .unwrap_or_default()
        .chars()
        .filter(|ch| !ch.is_control())
        .map(u32::from)
        .collect();
    (!codepoints.is_empty()).then_some(codepoints)
}

fn alternate_codes(
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

impl KeyEventView<'_> {
    fn unshifted_character(self, fallback: &str) -> char {
        self.unmodified_character
            .or_else(|| fallback.chars().next().map(unshift_ascii))
            .unwrap_or('\0')
    }

    fn shifted_character(self) -> Option<char> {
        key_character(self.logical_key).or_else(|| self.text.and_then(|text| text.chars().next()))
    }
}

fn key_character(key: &Key) -> Option<char> {
    match key {
        Key::Character(text) => text.chars().next(),
        Key::Named(NamedKey::Space) => Some(' '),
        _ => None,
    }
}

fn standalone_text<'a>(event: KeyEventView<'a>, mods: ModifiersState) -> Option<&'a str> {
    if event.state == ElementState::Released {
        // When: event.state is Released, winit text is not a new text input
        // event and must not be replayed into the PTY.
        return None;
    }
    let text = event.text_with_all_modifiers.or(event.text)?;
    if text.is_empty() || text.chars().any(char::is_control) {
        // When: text is empty or contains a control, it represents a
        // terminal chord rather than safe standalone UTF-8.
        return None;
    }
    if !mods.intersects(ModifiersState::ALT | ModifiersState::CONTROL | ModifiersState::SUPER) {
        // When: mods intersects no terminal command modifier, OS-produced text
        // is the authoritative layout and composition result.
        return Some(text);
    }
    if mods.super_key() || !mods.alt_key() {
        // When: mods has Super or lacks Alt, this is a command chord rather
        // than AltGr/Option-produced text and must retain its modifiers.
        return None;
    }

    let produced = text.chars().next()?;
    let unmodified = event.unmodified_character?;
    let consumed_modifier = if mods.shift_key() {
        // Compare both ASCII-unshifted and Unicode
        // lowercase forms so Shift alone is not mistaken for AltGr.
        unshift_ascii(produced) != unmodified
            && produced.to_lowercase().next() != unmodified.to_lowercase().next()
    } else {
        // When: mods omits Shift, any produced/unmodified difference came from
        // AltGr or Option rather than case selection.
        produced != unmodified
    };
    consumed_modifier.then_some(text)
}

fn legacy_text_modifiers(primary: char, mods: ModifiersState) -> bool {
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

fn legacy_c0_modifiers(mods: ModifiersState) -> bool {
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

fn is_legacy_ascii_key(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
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

pub(super) fn unshift_ascii(ch: char) -> char {
    match ch {
        'A'..='Z' => ch.to_ascii_lowercase(),
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
        _ if ch.is_uppercase() => ch.to_lowercase().next().unwrap_or(ch),
        _ => ch,
    }
}

pub(super) fn physical_ascii(key: PhysicalKey) -> Option<char> {
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

fn ctrl_mapping(ch: char) -> Option<u8> {
    match ch {
        ' ' | '@' | '2' => Some(0),
        'a'..='z' => Some((ch as u8) - b'a' + 1),
        'A'..='Z' => Some((ch as u8) - b'A' + 1),
        '[' | '3' => Some(27),
        '\\' | '4' => Some(28),
        ']' | '5' => Some(29),
        '^' | '~' | '6' => Some(30),
        '_' | '/' | '7' => Some(31),
        '?' | '8' => Some(127),
        _ if is_legacy_ascii_key(ch) && ch.is_ascii() => Some(ch as u8),
        _ => None,
    }
}

fn encode_modify_other_key(code: u32, mods: ModifiersState) -> Vec<u8> {
    format!("\x1b[27;{};{code}~", modifier_parameter(mods)).into_bytes()
}

fn should_modify_other_key(level: u8, code: u32, mods: ModifiersState) -> bool {
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

fn keypad_key(event: KeyEventView<'_>) -> Option<KeypadKey> {
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

fn physical_keypad_key(physical_key: PhysicalKey) -> Option<KeypadKey> {
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

pub(super) fn key_event_to_string(event: &KeyEvent, mods: ModifiersState) -> Option<String> {
    key_event_to_strings(event, mods).into_iter().next()
}

pub(super) fn key_event_to_strings(event: &KeyEvent, mods: ModifiersState) -> Vec<String> {
    key_strings_for_parts(&event.key_without_modifiers(), &event.logical_key, mods)
}

fn key_strings_for_parts(
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

fn chord_string(key_name: &str, mods: ModifiersState) -> String {
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

fn key_candidates(key: &Key, mods: ModifiersState) -> Option<Vec<KeyName>> {
    let primary = key_name(key)?;
    let mut candidates = Vec::new();
    if let Key::Character(text) = key {
        if let Some(ch) = text.chars().next().filter(|_| text.chars().count() == 1) {
            let normalized = unshift_ascii(ch);
            if (mods.shift_key() || ch.is_uppercase()) && normalized != ch {
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

#[doc(hidden)]
#[derive(PartialEq, Eq)]
pub enum KeyName {
    Static(&'static str),
    Owned(String),
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
#[path = "key_encoding_tests.rs"]
mod key_encoding_tests;
