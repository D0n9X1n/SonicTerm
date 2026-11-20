//! What reaches the PTY for an Option chord on macOS, with Option as Alt and without: winit
//! delivers the unmodified character with Alt held when the key acts as Alt, and the composed
//! character otherwise.

use super::key_encoding_tests::event;
use super::*;

/// The key event winit delivers for Option+`b` with Option acting as Alt: the unmodified
/// character, the Alt modifier still held.
fn option_as_alt_b(key: &Key) -> KeyEventView<'_> {
    event(
        key,
        PhysicalKey::Code(KeyCode::KeyB),
        Some("b"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    )
}

/// Option acting as Alt sends Meta-b in legacy mode and the Alt-modified key under the kitty
/// flags herdr pushes (`CSI > 7 u`), so herdr's Alt bindings arrive as Alt.
#[test]
fn option_as_alt_sends_alt_chords() {
    let key = Key::Character("b".into());
    let modes = KeyboardModes::new(false, false, false, false, 0);
    assert_eq!(
        encode_event(option_as_alt_b(&key), ModifiersState::ALT, 0, modes),
        Some(b"\x1bb".to_vec())
    );
    assert_eq!(
        encode_event(option_as_alt_b(&key), ModifiersState::ALT, 7, modes),
        Some(b"\x1b[98;3u".to_vec())
    );
}

/// With Option composing (the default), the composed character is typed as text, unchanged.
#[test]
fn composing_option_types_the_composed_character() {
    let key = Key::Character("∫".into());
    let mut composed = event(
        &key,
        PhysicalKey::Code(KeyCode::KeyB),
        Some("∫"),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    );
    composed.unmodified_character = Some('b');
    let modes = KeyboardModes::new(false, false, false, false, 0);
    assert_eq!(
        encode_event(composed, ModifiersState::ALT, 0, modes),
        Some("∫".as_bytes().to_vec())
    );
}
