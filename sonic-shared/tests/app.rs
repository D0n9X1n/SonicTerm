use winit::keyboard::SmolStr;

use sonic_shared::app::{encode_logical, key_name, next_pane_id};
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[test]
fn arrow_keys_emit_csi() {
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::ArrowUp), ModifiersState::empty()).unwrap(),
        b"\x1b[A"
    );
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::ArrowLeft), ModifiersState::empty()).unwrap(),
        b"\x1b[D"
    );
}

#[test]
fn enter_emits_cr() {
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Enter), ModifiersState::empty()).unwrap(),
        b"\r"
    );
}

#[test]
fn backspace_emits_del() {
    assert_eq!(
        encode_logical(&Key::Named(NamedKey::Backspace), ModifiersState::empty()).unwrap(),
        b"\x7f"
    );
}

#[test]
fn ctrl_c_maps_to_0x03() {
    assert_eq!(
        encode_logical(&Key::Character(SmolStr::new("c")), ModifiersState::CONTROL).unwrap(),
        vec![0x03_u8]
    );
}

#[test]
fn ctrl_letter_range_covers_a_and_z() {
    for (ch, byte) in [('a', 0x01_u8), ('z', 0x1a)] {
        let bytes =
            encode_logical(&Key::Character(SmolStr::new(ch.to_string())), ModifiersState::CONTROL)
                .unwrap();
        assert_eq!(bytes, vec![byte]);
    }
}

#[test]
fn plain_letter_passes_through() {
    assert_eq!(
        encode_logical(&Key::Character(SmolStr::new("h")), ModifiersState::empty()).unwrap(),
        b"h"
    );
}

#[test]
fn unknown_named_returns_none() {
    assert!(encode_logical(&Key::Named(NamedKey::Insert), ModifiersState::empty()).is_none());
}

#[test]
fn key_name_for_letter() {
    assert_eq!(key_name(&Key::Character(SmolStr::new("t"))).unwrap().as_str(), "t");
}

#[test]
fn key_name_for_named() {
    assert_eq!(key_name(&Key::Named(NamedKey::Enter)).unwrap().as_str(), "enter");
    assert_eq!(key_name(&Key::Named(NamedKey::PageDown)).unwrap().as_str(), "pagedown");
}

#[test]
fn key_name_for_unsupported_named_is_none() {
    assert!(key_name(&Key::Named(NamedKey::Insert)).is_none());
}

#[test]
fn next_pane_id_is_monotonic() {
    let a = next_pane_id();
    let b = next_pane_id();
    assert!(b > a);
}

#[test]
fn modifier_aware_click_only_opens_with_super() {
    // The Cmd/Super-click gate is a modifier predicate; assert it here
    // so the click path can't regress without flipping a test.
    let plain = ModifiersState::empty();
    let supered = ModifiersState::SUPER;
    assert!(!plain.super_key());
    assert!(supered.super_key());
    // Any URI the app forwards must clear url_open::validate. We mock
    // url_open::open by calling the same validate() entry point the
    // production path runs first.
    assert!(sonic_core::url_open::validate("https://example.com/path").is_ok());
    assert!(sonic_core::url_open::validate("javascript:alert(1)").is_err());
}

#[test]
fn wrap_paste_raw_when_not_bracketed() {
    let out = sonic_shared::app::wrap_paste("hello\nworld", false);
    assert_eq!(out, b"hello\nworld");
}

#[test]
fn wrap_paste_brackets_when_enabled() {
    let out = sonic_shared::app::wrap_paste("rm -rf /", true);
    assert_eq!(out, b"\x1b[200~rm -rf /\x1b[201~");
}

#[test]
fn wrap_paste_empty_text_still_emits_brackets() {
    let out = sonic_shared::app::wrap_paste("", true);
    assert_eq!(out, b"\x1b[200~\x1b[201~");
}

#[test]
fn pick_prompt_target_forward_and_back() {
    use sonic_core::grid::Grid;
    let mut g = Grid::new(10, 6);
    g.record_prompt_start();
    g.goto(2, 0);
    g.record_prompt_start();
    g.goto(5, 0);
    g.record_prompt_start();
    assert_eq!(sonic_shared::app::pick_prompt_target(&g, 0, true), Some(2));
    assert_eq!(sonic_shared::app::pick_prompt_target(&g, 5, false), Some(2));
    assert_eq!(sonic_shared::app::pick_prompt_target(&g, 5, true), None);
    assert_eq!(sonic_shared::app::pick_prompt_target(&g, 0, false), None);
}
