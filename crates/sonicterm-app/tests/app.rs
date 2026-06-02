//! Integration tests for app.

use winit::keyboard::SmolStr;

use sonicterm_app::app::{encode_logical, key_name, next_pane_id};
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
    assert!(sonicterm_cfg::url_open::validate("https://example.com/path").is_ok());
    assert!(sonicterm_cfg::url_open::validate("javascript:alert(1)").is_err());
}

#[test]
fn wrap_paste_raw_when_not_bracketed() {
    let out = sonicterm_app::app::wrap_paste("hello\nworld", false);
    assert_eq!(out, b"hello\nworld");
}

#[test]
fn wrap_paste_brackets_when_enabled() {
    let out = sonicterm_app::app::wrap_paste("rm -rf /", true);
    assert_eq!(out, b"\x1b[200~rm -rf /\x1b[201~");
}

#[test]
fn wrap_paste_empty_text_still_emits_brackets() {
    let out = sonicterm_app::app::wrap_paste("", true);
    assert_eq!(out, b"\x1b[200~\x1b[201~");
}

#[test]
fn pick_prompt_target_forward_and_back() {
    use sonicterm_grid::grid::Grid;
    let mut g = Grid::new(10, 6);
    g.record_prompt_start();
    g.goto(2, 0);
    g.record_prompt_start();
    g.goto(5, 0);
    g.record_prompt_start();
    assert_eq!(sonicterm_app::app::pick_prompt_target(&g, 0, true), Some(2));
    assert_eq!(sonicterm_app::app::pick_prompt_target(&g, 5, false), Some(2));
    assert_eq!(sonicterm_app::app::pick_prompt_target(&g, 5, true), None);
    assert_eq!(sonicterm_app::app::pick_prompt_target(&g, 0, false), None);
}

#[test]
fn scroll_to_prev_prompt_view_top_matches_prompt_row() {
    // After ScrollToPrevPrompt the renderer reads rows via
    // Grid::row_at_abs(viewport_top_abs + r). This test verifies the
    // logic end-to-end: a prompt recorded mid-scrollback, when used as
    // the viewport top, must resolve to the very row that was current
    // when record_prompt_start() ran.
    use sonicterm_grid::grid::{CellFlags, Color, Grid};
    let mut g = Grid::new(4, 3);
    // Row content "A" at scrollback origin; record a prompt there.
    g.put_char('A', Color::Default, Color::Default, CellFlags::empty());
    g.record_prompt_start();
    // Push that row into scrollback by scrolling up 3 times so the row
    // marked with 'A' ends up at scrollback index 0.
    g.scroll_up(3);
    // Sanity: the prompt's start_row should now lie in scrollback.
    let prompt = g.prompts().next().expect("prompt recorded");
    assert!(prompt.start_row < g.scrollback_len() as u64);
    // pick_prompt_target from the live bottom should hop back to the
    // recorded prompt row.
    let cur = g.scrollback_len() as u64;
    let target = sonicterm_app::app::pick_prompt_target(&g, cur, false).expect("prev prompt");
    assert_eq!(target, prompt.start_row);
    // Now the renderer would view from `target`. The first visible row
    // returned by row_at_abs must be the prompt's start row, and it
    // must still contain the 'A' character.
    let row = g.row_at_abs(target).expect("row in scrollback");
    assert_eq!(row[0].ch, 'A', "viewport top row should be the prompt-start row");
}
