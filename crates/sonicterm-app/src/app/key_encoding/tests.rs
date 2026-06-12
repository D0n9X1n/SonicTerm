
use super::*;

#[test]
fn enter_encodes_carriage_return() {
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty(), 0),
        Some(b"\r".to_vec())
    );
}

#[test]
fn shift_enter_encodes_escape_carriage_return() {
    // Legacy (no kitty flags): Shift+Enter falls back to ESC+CR.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, 0),
        Some(b"\x1b\r".to_vec())
    );
}

#[test]
fn shift_enter_kitty_encodes_csi_u() {
    // With kitty keyboard flags active, Shift+Enter is the disambiguated
    // CSI-u form so Copilot CLI / claude insert a newline.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, 1),
        Some(b"\x1b[13;2u".to_vec())
    );
}

#[test]
fn plain_enter_stays_carriage_return_under_kitty() {
    // Plain Enter must remain CR even when kitty flags are active, so we
    // don't regress "submit" in apps that accept bare CR.
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty(), 1),
        Some(b"\r".to_vec())
    );
}
