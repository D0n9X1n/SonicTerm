use super::*;
use crate::platform::windows::KeyEventExtWindows;

fn partial(
    native: Option<NativeKeyEvent>,
    unicode: Vec<u16>,
    state: ElementState,
) -> PartialKeyEventInfo {
    PartialKeyEventInfo {
        vkey: 0x41,
        key_state: state,
        is_repeat: false,
        physical_key: PhysicalKey::Code(KeyCode::KeyA),
        location: KeyLocation::Standard,
        logical_key: PartialLogicalKey::This(Key::Character("a".into())),
        key_without_modifiers: Key::Character("a".into()),
        utf16parts: unicode,
        text: PartialText::System(Vec::new()),
        native_key_event: native,
    }
}

#[test]
fn native_header_keeps_raw_vk_scan_transition_and_repeat() {
    // Native identity must not inherit winit's logical-key or extended-scancode normalization.
    let event =
        native_key_event(0x11, ((0xe0u32 << 16) | (1 << 24) | (1 << 30) | 9) as isize, &[0; 256]);
    assert_eq!(event.virtual_key, 0x11);
    assert_eq!(event.scan_code, 0xe0);
    assert_eq!(event.control_key_state, 0x100);
    assert!(event.key_down);
    assert_eq!(event.repeat_count, 9);
    assert!(event.unicode.is_empty());
    let released = native_key_event(0x11, 0xc01d0001u32 as isize, &[0; 256]);
    assert!(!released.key_down);
    assert_eq!(released.repeat_count, 1);
}

#[test]
fn native_header_uses_exact_side_and_toggle_snapshot_bits() {
    // Each Windows console flag comes from its own snapshot byte, never reconstructed logical modifiers.
    for (vk, state, flag) in [
        (VK_RMENU, 0x80, 0x0001),
        (VK_LMENU, 0x80, 0x0002),
        (VK_RCONTROL, 0x80, 0x0004),
        (VK_LCONTROL, 0x80, 0x0008),
        (VK_SHIFT, 0x80, 0x0010),
        (VK_NUMLOCK, 1, 0x0020),
        (VK_SCROLL, 1, 0x0040),
        (VK_CAPITAL, 1, 0x0080),
    ] {
        let mut snapshot = [0; 256];
        snapshot[vk as usize] = state;
        assert_eq!(native_key_event(0x41, 1, &snapshot).control_key_state, flag);
        snapshot[vk as usize] = if state == 0x80 { 1 } else { 0x80 };
        assert_eq!(native_key_event(0x41, 1, &snapshot).control_key_state, 0);
    }
}

#[test]
fn native_text_preserves_raw_utf16_before_utf8_conversion() {
    // Invalid UTF-16 remains observable even when the portable text conversion rejects it.
    for unicode in [vec![0x61], vec![0xd83d, 0xde00], vec![0xd800], vec![]] {
        let header = native_key_event(0x41, 1, &[0; 256]);
        let event = partial(Some(header), unicode.clone(), ElementState::Pressed).finalize();
        assert_eq!(event.native_key_event().unwrap().unicode, unicode);
    }
}

#[test]
fn release_and_synthetic_events_do_not_invent_native_text_or_identity() {
    // Key releases have no character sequence; focus synthesis has no native key message at all.
    let header = native_key_event(0x41, 0xc01e0001u32 as isize, &[0; 256]);
    let event = partial(Some(header), vec![0x61], ElementState::Released).finalize();
    assert!(event.native_key_event().unwrap().unicode.is_empty());
    let synthetic = partial(None, vec![0x61], ElementState::Pressed).finalize();
    assert!(synthetic.native_key_event().is_none());
}

#[test]
fn deferred_key_keeps_header_while_character_messages_arrive() {
    // A later keyboard snapshot must not replace the header waiting for this key's UTF-16 units.
    let builder = KeyEventBuilder::default();
    let mut snapshot = [0; 256];
    snapshot[VK_LCONTROL as usize] = 0x80;
    *builder.event_info.lock().unwrap() = Some(partial(
        Some(native_key_event(0x41, 4, &snapshot)),
        Vec::new(),
        ElementState::Pressed,
    ));
    snapshot[VK_LCONTROL as usize] = 0;
    snapshot[VK_RMENU as usize] = 0x80;
    assert_eq!(native_key_event(0x42, 1, &snapshot).control_key_state, 1);
    builder.event_info.lock().unwrap().as_mut().unwrap().utf16parts.extend([0xd83d, 0xde00]);
    let event = builder.event_info.lock().unwrap().take().unwrap().finalize();
    let native = event.native_key_event().unwrap();
    assert_eq!(native.virtual_key, 0x41);
    assert_eq!(native.repeat_count, 4);
    assert_eq!(native.control_key_state, 8);
    assert_eq!(native.unicode, [0xd83d, 0xde00]);
}

#[test]
fn pending_queue_keeps_metadata_with_its_key_event() {
    // Reentrant completion must retain each native header and character sequence in dispatch order.
    let queue = PendingEventQueue::default();
    let first = queue.add_pending();
    let second = queue.add_pending();
    let later =
        partial(Some(native_key_event(0x42, 7, &[0; 256])), vec![0x62], ElementState::Pressed)
            .finalize();
    assert!(queue.complete_pending(second, later).is_empty());
    let earlier =
        partial(Some(native_key_event(0x41, 3, &[0; 256])), vec![0x61], ElementState::Pressed)
            .finalize();
    let events = queue.complete_pending(first, earlier);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].native_key_event().unwrap().virtual_key, 0x41);
    assert_eq!(events[0].native_key_event().unwrap().repeat_count, 3);
    assert_eq!(events[0].native_key_event().unwrap().unicode, [0x61]);
    assert_eq!(events[1].native_key_event().unwrap().virtual_key, 0x42);
    assert_eq!(events[1].native_key_event().unwrap().repeat_count, 7);
    assert_eq!(events[1].native_key_event().unwrap().unicode, [0x62]);
}
