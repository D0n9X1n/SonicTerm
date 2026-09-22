use super::*;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

fn snapshot(requested: bool, kitty: u8, epoch: u64) -> KeyboardSnapshot {
    KeyboardSnapshot::from_bits(
        u64::from(
            KeyboardModes::new(false, false, false, false, 0).with_win32_input(requested).bits(),
        ) | (u64::from(kitty) << 8)
            | (epoch << 16),
    )
}

fn native() -> Win32KeyEvent<'static> {
    Win32KeyEvent {
        virtual_key: 65,
        scan_code: 30,
        unicode: &[65],
        key_down: true,
        control_key_state: 16 | 32 | 128 | 256,
        repeat_count: 1,
    }
}

fn encode(
    state: KeyboardSnapshot,
    key: Option<Win32KeyEvent<'_>>,
    synthetic: bool,
    previous: Option<HeldKey>,
    repeat: bool,
) -> Option<EncodedKey> {
    encode_routed_key(state, true, key, synthetic, repeat, previous, || Some(b"other".to_vec()))
}

#[test]
fn native_selection_requires_windows_request_and_zero_kitty_flags() {
    // Even unknown or augmentation-only Kitty bits preempt native encoding.
    assert_eq!(snapshot(true, 0, 3).protocol(true), KeyboardProtocol::Win32);
    assert_eq!(snapshot(true, 0, 3).protocol(false), KeyboardProtocol::Other);
    assert_eq!(snapshot(false, 0, 3).protocol(true), KeyboardProtocol::Other);
    for flags in [1, 2, 4, 8, 16, 32, 64, 128, 255] {
        assert_eq!(snapshot(true, flags, 3).protocol(true), KeyboardProtocol::Other);
    }
}

#[test]
fn native_request_preempts_modify_other_keys_in_either_order() {
    // Outer Win32 negotiation is independent of modifyOtherKeys; only Kitty flags preempt native records.
    for bytes in [&b"\x1b[?9001h\x1b[>4;2m"[..], &b"\x1b[>4;2m\x1b[?9001h"[..]] {
        let mut parser = Parser::new(Grid::new(80, 24));
        parser.advance(bytes);
        let state = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
        assert_eq!(state.modes().modify_other_keys(), 2);
        assert_eq!(state.protocol(true), KeyboardProtocol::Win32);
        let enter = Win32KeyEvent {
            virtual_key: 13,
            scan_code: 28,
            unicode: &[13],
            control_key_state: 16,
            ..native()
        };
        assert_eq!(
            encode(state, Some(enter), false, None, false).unwrap().bytes,
            b"\x1b[13;28;13;1;16;1_"
        );
        parser.advance(b"\x1b[>10u");
        assert_eq!(
            KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot()).protocol(true),
            KeyboardProtocol::Other
        );
        parser.advance(b"\x1b[<u");
        assert_eq!(
            KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot()).protocol(true),
            KeyboardProtocol::Win32
        );
        parser.advance(b"\x1b[?9001l");
        let state = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
        assert_eq!(state.protocol(true), KeyboardProtocol::Other);
        let bytes = super::super::key_encoding::encode_logical_with_modes(
            &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter),
            winit::keyboard::ModifiersState::SHIFT,
            state.kitty_flags(),
            state.modes(),
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[27;2;13~");
    }
}

#[test]
fn missing_native_metadata_refuses_without_legacy_fallback() {
    // A real event without its native record cannot reconstruct Win32 state from logical text.
    assert!(encode(snapshot(true, 0, 3), None, false, None, false).is_none());
    assert!(encode(snapshot(false, 0, 3), None, false, None, false).is_some());
}

#[test]
fn synthetic_events_cannot_create_native_ownership_or_release_it() {
    // Focus-generated input must leave accepted native ownership for one explicit focus-loss drain.
    let state = snapshot(true, 0, 3);
    assert!(encode(state, Some(native()), true, None, false).is_none());
    assert!(encode(state, None, true, None, false).is_none());
    let held = encode(state, Some(native()), false, None, false).unwrap().held;
    assert!(encode(state, Some(native()), true, Some(held), false).is_none());
}

#[test]
fn native_repeat_requires_original_accepted_press_and_preserves_utf16_count() {
    // Repeats fan out raw UTF-16 with Rc7 but cannot invent an original accepted press.
    let state = snapshot(true, 0, 3);
    let held = encode(state, Some(native()), false, None, false).unwrap().held;
    let repeated =
        Win32KeyEvent { unicode: &[0xd83d, 0xde80, 0xd800], repeat_count: 7, ..native() };
    assert!(encode(state, Some(repeated), false, None, true).is_none());
    let encoded = encode(state, Some(repeated), false, Some(held), true).unwrap();
    assert_eq!(
        encoded.bytes,
        b"\x1b[65;30;55357;1;432;7_\x1b[65;30;56960;1;432;7_\x1b[65;30;55296;1;432;7_"
    );
    assert_eq!(encoded.held, held);
}

#[test]
fn native_release_uses_actual_native_header_and_zero_character() {
    // Release metadata is taken from the release event, never press text or cached modifiers.
    let state = snapshot(true, 0, 3);
    let held = encode(state, Some(native()), false, None, false).unwrap().held;
    let released = Win32KeyEvent {
        key_down: false,
        unicode: &[65],
        control_key_state: 32,
        repeat_count: 2,
        ..native()
    };
    let encoded = encode(state, Some(released), false, Some(held), false).unwrap();
    assert_eq!(encoded.bytes, b"\x1b[65;30;0;0;32;2_");
}

#[test]
fn native_release_preserves_changed_vk_with_same_physical_scan() {
    // NumLock and Shift may change a held keypad key's VK while its physical route remains the same.
    let state = snapshot(true, 0, 3);
    let held = encode(state, Some(native()), false, None, false).unwrap().held;
    let released = Win32KeyEvent { virtual_key: 97, key_down: false, ..native() };
    let encoded = encode(state, Some(released), false, Some(held), false).unwrap();
    assert_eq!(encoded.bytes, b"\x1b[97;30;0;0;432;1_");
}

#[test]
fn native_repeat_rejects_changed_native_identity() {
    // A repeated event with another VK/scan pair cannot be attributed to the accepted native hold.
    let state = snapshot(true, 0, 3);
    let held = encode(state, Some(native()), false, None, false).unwrap().held;
    let changed = Win32KeyEvent { scan_code: 31, ..native() };
    assert!(encode(state, Some(changed), false, Some(held), true).is_none());
}

#[test]
fn native_and_other_holds_cannot_cross_protocol_family() {
    // Neither native-down/legacy-up nor legacy-down/native-up may manufacture unmatched records.
    let native_state = snapshot(true, 0, 3);
    let other_state = snapshot(false, 0, 4);
    let held_native = encode(native_state, Some(native()), false, None, false).unwrap().held;
    let held_other = encode(other_state, Some(native()), false, None, false).unwrap().held;
    assert!(encode(other_state, Some(native()), false, Some(held_native), true).is_none());
    assert!(encode(native_state, Some(native()), false, Some(held_other), true).is_none());
    let released = Win32KeyEvent { key_down: false, ..native() };
    assert!(encode(other_state, Some(released), false, Some(held_native), false).is_none());
    assert!(encode(native_state, Some(released), false, Some(held_other), false).is_none());
    assert!(encode(snapshot(true, 4, 4), Some(released), false, Some(held_native), false).is_none());
}

#[test]
fn epoch_cancels_off_on_screen_and_reset_aba() {
    // Parser transitions that return to identical flags still invalidate the original native hold.
    for transition in [
        &b"\x1b[?9001l\x1b[?9001h"[..],
        &b"\x1b[?1049h\x1b[?1049l"[..],
        &b"\x1bc\x1b[?9001h"[..],
        &b"\x1b[>4u\x1b[<u"[..],
    ] {
        let mut parser = Parser::new(Grid::new(80, 24));
        parser.advance(b"\x1b[?1049h\x1b[>4u\x1b[?1049l\x1b[?9001h");
        let before = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
        let held = encode(before, Some(native()), false, None, false).unwrap().held;
        parser.advance(transition);
        let after = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
        assert_eq!(after.protocol(true), KeyboardProtocol::Win32);
        assert!(encode(after, Some(native()), false, Some(held), true).is_none());
        assert!(held.focus_release(after, true).is_none());
    }
}

#[test]
fn unchanged_screen_eligibility_and_legacy_kitty_flags_preserve_holds() {
    // Screen switches with Kitty0 on both screens are not a protocol boundary; legacy holds still use live flags.
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(b"\x1b[?9001h");
    let before = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
    let held = encode(before, Some(native()), false, None, false).unwrap().held;
    parser.advance(b"\x1b[?1049h\x1b[?1049l");
    let after = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
    assert!(encode(after, Some(native()), false, Some(held), true).is_some());
    let legacy = encode(snapshot(false, 0, 0), None, false, None, false).unwrap().held;
    assert!(encode(snapshot(false, 8, 2), None, false, Some(legacy), true).is_some());
}

#[test]
fn focus_release_retains_identity_and_lock_bits_but_clears_held_modifiers() {
    // Cleanup is a Uc0/Rc1 release with only enhanced and lock state retained from the accepted press.
    let state = snapshot(true, 0, 3);
    let held = encode(state, Some(native()), false, None, false).unwrap().held;
    assert_eq!(held.focus_release(state, true).unwrap(), b"\x1b[65;30;0;0;416;1_");
    assert!(held.focus_release(snapshot(true, 0, 5), true).is_none());
    let other = encode(snapshot(false, 0, 3), None, false, None, false).unwrap().held;
    assert!(other.focus_release(state, true).is_none());
}

#[test]
fn synthetic_release_keeps_native_route_until_focus_drain() {
    // A mixed broadcast releases non-native owners now while preserving native owners for explicit cleanup.
    let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyA);
    let native_hold =
        encode(snapshot(true, 0, 3), Some(native()), false, None, false).unwrap().held;
    let other_hold = encode(snapshot(false, 0, 3), None, false, None, false).unwrap().held;
    let mut pressed = HashMap::from([(key, BTreeMap::from([(7, native_hold), (11, other_hold)]))]);
    assert_eq!(
        take_release_routes(&mut pressed, key, true),
        Some(BTreeMap::from([(11, other_hold)]))
    );
    assert_eq!(pressed[&key], BTreeMap::from([(7, native_hold)]));
    assert_eq!(
        take_release_routes(&mut pressed, key, false),
        Some(BTreeMap::from([(7, native_hold)]))
    );
    assert!(pressed.is_empty());
}

#[test]
fn native_refusal_reason_excludes_synthetic_input_and_character_data() {
    // Saturation and missing metadata are diagnosable without logging native or logical character payloads.
    let unavailable = snapshot(true, 0, (1 << 48) - 1);
    assert_eq!(unavailable.refusal_reason(true, true, false), Some("protocol epoch exhausted"));
    assert_eq!(
        snapshot(true, 0, 3).refusal_reason(true, false, false),
        Some("native metadata unavailable")
    );
    assert_eq!(unavailable.refusal_reason(true, false, true), None);
    assert_eq!(snapshot(false, 0, 3).refusal_reason(true, false, false), None);
}

#[test]
fn exhausted_epoch_refuses_native_input() {
    // The saturated epoch is unavailable, not an identity that future held events can reuse.
    let state = snapshot(true, 0, (1 << 48) - 1);
    assert_eq!(state.protocol(true), KeyboardProtocol::Unavailable);
    assert!(encode(state, Some(native()), false, None, false).is_none());
}

#[test]
fn mixed_target_admission_keeps_only_accepted_protocol_routes() {
    // Each broadcast target owns the route encoded from its own snapshot, only after queue admission.
    let native_state = snapshot(true, 0, 3);
    let other_state = snapshot(true, 4, 8);
    let first = encode(native_state, Some(native()), false, None, false).unwrap();
    let second = encode(other_state, Some(native()), false, None, false).unwrap();
    let rejected = encode(native_state, Some(native()), false, None, false).unwrap();
    let mut writes = Vec::new();
    let accepted =
        dispatch_key_writes(vec![(7, first), (11, second), (13, rejected)], |pane, bytes| {
            writes.push((pane, bytes));
            pane != 13
        });
    assert_eq!(writes[0].1, b"\x1b[65;30;65;1;432;1_");
    assert_eq!(writes[1].1, b"other");
    assert!(accepted[&7].focus_release(native_state, true).is_some());
    assert!(accepted[&11].focus_release(other_state, true).is_none());
    assert!(!accepted.contains_key(&13));
}
