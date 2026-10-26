use sonicterm_cfg::config::KeypadMode;
use sonicterm_vt::vt::KeyboardModes;
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey},
    platform::modifier_supplement::KeyEventExtModifierSupplement,
};

/// Borrowed native Windows fields, preserving the original UTF-16 units.
#[derive(Clone, Copy)]
pub(super) struct Win32KeyEvent<'a> {
    pub virtual_key: u16,
    pub scan_code: u16,
    pub unicode: &'a [u16],
    pub key_down: bool,
    pub control_key_state: u32,
    pub repeat_count: u16,
}

/// Encode each native UTF-16 unit as an independent Win32 input record.
pub(super) fn encode_win32_key(event: Win32KeyEvent<'_>) -> Vec<u8> {
    // When: key_down has nonempty unicode, preserve its raw units; keyup and characterless keys use exactly Uc0.
    let units = if event.key_down && !event.unicode.is_empty() { event.unicode } else { &[0] };
    let mut bytes = Vec::new();
    for unit in units {
        bytes.extend_from_slice(
            format!(
                "\x1b[{};{};{};{};{};{}_",
                event.virtual_key,
                event.scan_code,
                unit,
                u8::from(event.key_down),
                event.control_key_state,
                event.repeat_count
            )
            .as_bytes(),
        );
    }
    bytes
}

/// Read only event-carried native data; synthetic events never acquire native metadata.
pub(super) fn native_key_event(event: &KeyEvent) -> Option<Win32KeyEvent<'_>> {
    #[cfg(windows)]
    {
        use winit::platform::windows::KeyEventExtWindows;
        event.native_key_event().map(|native| Win32KeyEvent {
            virtual_key: native.virtual_key,
            scan_code: native.scan_code,
            unicode: &native.unicode,
            key_down: native.key_down,
            control_key_state: native.control_key_state,
            repeat_count: native.repeat_count,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = event;
        None
    }
}

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
    keypad_mode: KeypadMode,
) -> Option<Vec<u8>> {
    let key_without_modifiers = event.key_without_modifiers();
    encode_event_with_keypad_mode(
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
        keypad_mode,
    )
}

fn encode_event_with_keypad_mode(
    event: KeyEventView<'_>,
    mods: ModifiersState,
    kitty_flags: u8,
    modes: KeyboardModes,
    keypad_mode: KeypadMode,
) -> Option<Vec<u8>> {
    encode_event(event, mods, kitty_flags, keypad_modes(modes, keypad_mode))
}

fn keypad_modes(modes: KeyboardModes, keypad_mode: KeypadMode) -> KeyboardModes {
    match keypad_mode {
        KeypadMode::Auto => modes,
        KeypadMode::Numeric => KeyboardModes::new(
            modes.application_cursor_keys(),
            false,
            modes.backarrow_key(),
            modes.newline(),
            modes.modify_other_keys(),
        )
        .with_win32_input(modes.win32_input()),
    }
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

/// Identify standalone modifier keys without classifying lock keys or modifier-produced text as modifiers.
pub(super) fn is_modifier_key(key: NamedKey) -> bool {
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

fn modifier_parameter(mods: ModifiersState) -> u16 {
    1 + u16::from(mods.shift_key())
        + u16::from(mods.alt_key()) * 2
        + u16::from(mods.control_key()) * 4
        + u16::from(mods.super_key()) * 8
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
    if matches!(event.logical_key, Key::Named(named) if *named != NamedKey::Space) {
        // When: matches! identifies a named non-Space key, preserve terminal key encoding over Cocoa function text.
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

#[doc(hidden)]
#[derive(PartialEq, Eq)]
pub enum KeyName {
    Static(&'static str),
    Owned(String),
}

#[cfg(test)]
#[path = "key_encoding_tests.rs"]
mod key_encoding_tests;

#[path = "key_encoding_legacy.rs"]
mod legacy;
use legacy::{
    encode_functional_legacy, encode_legacy, encode_legacy_named, encode_legacy_text,
    legacy_text_modifiers,
};
#[path = "key_encoding_kitty.rs"]
mod kitty;
use kitty::{encode_csi_u, encode_kitty};
#[path = "key_encoding_names.rs"]
mod names;
use names::{function_key_encoding, keypad_key, named_functional_encoding, physical_keypad_key};
pub(super) use names::{key_event_to_string, key_event_to_strings, physical_ascii, unshift_ascii};
pub use names::{key_name, key_to_string, key_to_strings};

#[cfg(test)]
use kitty::associated_text;
