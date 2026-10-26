use super::*;

// ---: DECCKM (application cursor keys) for arrows + Home/End ---

/// Encode an unmodified named key in the given DECCKM state.
pub(super) fn cursor_key(named: NamedKey, app_cursor: bool) -> Vec<u8> {
    encode_logical(&Key::Named(named), ModifiersState::empty(), 0, app_cursor)
        .expect("cursor key should encode")
}

pub(super) fn function_key(named: NamedKey, mods: ModifiersState) -> Vec<u8> {
    encode_logical(&Key::Named(named), mods, 0, false).expect("function key should encode")
}

pub(super) fn event<'a>(
    logical_key: &'a Key,
    physical_key: PhysicalKey,
    text: Option<&'a str>,
    location: KeyLocation,
    state: ElementState,
    repeat: bool,
) -> KeyEventView<'a> {
    KeyEventView {
        logical_key,
        unmodified_character: key_character(logical_key).map(unshift_ascii),
        physical_key,
        text,
        text_with_all_modifiers: text,
        location,
        state,
        repeat,
    }
}
