use super::*;
use crate::vt::vt_test_support::*;

#[test]
fn dec_private_mode_1_toggles_application_cursor_keys() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert!(!parser.application_cursor_keys());

    parser.advance(b"\x1b[?1h");
    assert!(parser.application_cursor_keys());

    parser.advance(b"\x1b[?1l");
    assert!(!parser.application_cursor_keys());
}

/// Every host-visible keyboard mode must survive compact snapshot round-tripping.
#[test]
fn keyboard_modes_track_dec_ansi_keypad_and_xterm_state() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance(b"\x1b[?1h\x1b[?67h\x1b=\x1b[20h\x1b[>4;2m");

    let modes = parser.keyboard_modes();
    assert!(modes.application_cursor_keys());
    assert!(modes.application_keypad());
    assert!(modes.backarrow_key());
    assert!(modes.newline());
    assert_eq!(modes.modify_other_keys(), 2);
    assert_eq!(super::KeyboardModes::from_bits(modes.bits()), modes);
    assert_eq!(super::KeyboardModes::from_bits(0b11_0000).modify_other_keys(), 2);

    parser.advance(b"\x1b[?1l\x1b[?67l\x1b>\x1b[20l\x1b[>4;0m");
    assert_eq!(parser.keyboard_modes(), super::KeyboardModes::default());
}

/// Adding Win32 input preserves the five-argument constructor and every existing packed mode.
#[test]
fn win32_input_mode_builder_roundtrips_without_changing_legacy_modes() {
    assert!(!super::KeyboardModes::default().win32_input());
    for bits in 0..16 {
        for modify_other_keys in 0..=2 {
            let legacy = super::KeyboardModes::new(
                bits & 1 != 0,
                bits & 2 != 0,
                bits & 4 != 0,
                bits & 8 != 0,
                modify_other_keys,
            );
            assert!(!legacy.win32_input());
            for enabled in [false, true] {
                let modes = legacy.with_win32_input(enabled);
                assert_eq!(modes.win32_input(), enabled);
                assert_eq!(super::KeyboardModes::from_bits(modes.bits()), modes);
                assert_eq!(modes.with_win32_input(false), legacy);
            }
        }
    }
}

/// DEC9001 changes eligibility only on effective toggles and never prints control bytes.
#[test]
fn win32_input_dec_mode_is_idempotent_and_ignores_unknown_modes() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert!(!parser.keyboard_modes().win32_input());
    assert_eq!(parser.keyboard_protocol_epoch(), 0);
    parser.advance(b"A");

    for (sequence, enabled, epoch) in [
        (b"\x1b[?9002h\x1b[?9002l".as_slice(), false, 0),
        (b"\x1b[?9001l", false, 0),
        (b"\x1b[?9001h", true, 1),
        (b"\x1b[?9001h\x1b[?9002h\x1b[?9002l", true, 1),
        (b"\x1b[?9001l", false, 2),
        (b"\x1b[?9001l", false, 2),
    ] {
        let (consumed, events, replies) = parser.advance_with_replies(sequence);
        assert_eq!(consumed, sequence.len());
        assert!(events.is_empty());
        assert!(replies.is_empty());
        assert_eq!(parser.keyboard_modes().win32_input(), enabled);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch);
    }

    parser.advance(b"B");
    assert_eq!(row_text(&parser, 0), "AB      ");
    assert_eq!(row_text(&parser, 1), "        ");
}

/// Equal final modes cannot hide intermediate eligibility changes from held-key cancellation.
#[test]
fn win32_input_epoch_retains_every_transition_within_one_advance() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?9001h");
    let modes = parser.keyboard_modes();
    let epoch = parser.keyboard_protocol_epoch();

    parser.advance(b"\x1b[?9001l\x1b[?9001h");
    assert_eq!(parser.keyboard_modes(), modes);
    assert_eq!(parser.keyboard_protocol_epoch(), epoch + 2);

    parser.advance(b"\x1b[>4u\x1b[<u");
    assert_eq!(parser.keyboard_modes(), modes);
    assert_eq!(parser.kitty_keyboard_flags(), 0);
    assert_eq!(parser.keyboard_protocol_epoch(), epoch + 4);
}

/// RIS cancels held input once even when the protocol was already inactive or Kitty-masked.
#[test]
fn win32_input_ris_always_advances_epoch_and_clears_request() {
    for setup in ["", "\x1b[?9001h", "\x1b[?9001h\x1b[>4u"] {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(setup.as_bytes());
        let epoch = parser.keyboard_protocol_epoch();

        parser.advance(b"\x1bc");
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 1, "setup={setup:?}");
        assert_eq!(parser.keyboard_modes(), super::KeyboardModes::default());
        assert_eq!(parser.kitty_keyboard_flags(), 0);

        parser.advance(b"\x1bc");
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 2, "setup={setup:?}");
    }
}

/// Any nonzero Kitty flags suppress Win32 eligibility, while an explicit zero restores it.
#[test]
fn win32_input_epoch_respects_all_kitty_flags_and_explicit_zero() {
    for flags in 1..=0x7f {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(b"\x1b[?9001h");
        let epoch = parser.keyboard_protocol_epoch();

        parser.advance(format!("\x1b[>{flags}u").as_bytes());
        assert!(parser.keyboard_modes().win32_input());
        assert_eq!(parser.kitty_keyboard_flags(), flags);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 1, "flags={flags}");

        parser.advance(b"\x1b[>0u");
        assert!(parser.keyboard_modes().win32_input());
        assert_eq!(parser.kitty_keyboard_flags(), 0);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 2, "flags={flags}");

        parser.advance(b"\x1b[<u");
        assert_eq!(parser.kitty_keyboard_flags(), flags);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 3, "flags={flags}");

        parser.advance(b"\x1b[<u");
        assert_eq!(parser.kitty_keyboard_flags(), 0);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 4, "flags={flags}");
    }
}

/// Kitty-only changes and masked DEC9001 toggles cannot create a Win32 eligibility transition.
#[test]
fn win32_input_epoch_ignores_changes_while_ineligible() {
    let mut parser = Parser::new(Grid::new(8, 2));
    for sequence in ["\x1b[>1u", "\x1b[=4u", "\x1b[>32u", "\x1b[<u", "\x1b[=0u"] {
        parser.advance(sequence.as_bytes());
        assert!(!parser.keyboard_modes().win32_input());
        assert_eq!(parser.keyboard_protocol_epoch(), 0);
    }

    parser.advance(b"\x1b[=4u\x1b[?9001h");
    assert!(parser.keyboard_modes().win32_input());
    assert_eq!(parser.keyboard_protocol_epoch(), 0);
    parser.advance(b"\x1b[=32u");
    assert_eq!(parser.kitty_keyboard_flags(), 32);
    assert_eq!(parser.keyboard_protocol_epoch(), 0);
    parser.advance(b"\x1b[?9001l\x1b[=0u");
    assert!(!parser.keyboard_modes().win32_input());
    assert_eq!(parser.kitty_keyboard_flags(), 0);
    assert_eq!(parser.keyboard_protocol_epoch(), 0);
}

/// Screen-local Kitty stacks change eligibility, but DEC9001 remains one pane-global request.
#[test]
fn win32_input_request_survives_screen_switches_with_local_kitty_precedence() {
    for mode in [47, 1047, 1049] {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(b"\x1b[?9001h\x1b[>4u");
        let epoch = parser.keyboard_protocol_epoch();
        let enter = format!("\x1b[?{mode}h");
        let leave = format!("\x1b[?{mode}l");

        parser.advance(enter.as_bytes());
        assert!(parser.keyboard_modes().win32_input());
        assert_eq!(parser.kitty_keyboard_flags(), 0);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 1);
        parser.advance(enter.as_bytes());
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 1);

        parser.advance(b"\x1b[>32u");
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 2);
        parser.advance(leave.as_bytes());
        assert!(parser.keyboard_modes().win32_input());
        assert_eq!(parser.kitty_keyboard_flags(), 4);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 2);

        parser.advance(b"\x1b[=0u");
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 3);
        parser.advance(enter.as_bytes());
        assert_eq!(parser.kitty_keyboard_flags(), 32);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 4);

        parser.advance(b"\x1b[?9001l");
        parser.advance(leave.as_bytes());
        assert!(!parser.keyboard_modes().win32_input());
        assert_eq!(parser.kitty_keyboard_flags(), 0);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 4);
    }
}

/// Disabling Win32 input must leave legacy keyboard modes available for the next encoding path.
#[test]
fn win32_input_disable_preserves_other_keyboard_modes() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1h\x1b[?67h\x1b=\x1b[20h\x1b[>4;2m");
    let legacy = parser.keyboard_modes();
    assert_eq!(legacy, super::KeyboardModes::new(true, true, true, true, 2));

    parser.advance(b"\x1b[?9001h");
    assert_eq!(parser.keyboard_modes(), legacy.with_win32_input(true));
    parser.advance(b"\x1b[?9001l");
    assert_eq!(parser.keyboard_modes(), legacy);
    assert_eq!(parser.keyboard_protocol_epoch(), 2);
}

/// One packed word keeps legacy modes, the requested Win32 bit, Kitty flags, and epoch coherent.
#[test]
fn win32_input_snapshot_matches_each_parser_state() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(parser.keyboard_input_snapshot(), 0);
    assert_eq!(super::KeyboardModes::default().with_win32_input(true).bits(), 1 << 6);

    for sequence in [
        "\x1b[?1h\x1b[?67h\x1b=\x1b[20h\x1b[>4;2m",
        "\x1b[?9001h",
        "\x1b[>32u",
        "\x1b[?1049h",
        "\x1b[>4u",
        "\x1b[=0u",
        "\x1b[?1049l",
        "\x1bc",
    ] {
        parser.advance(sequence.as_bytes());
        let snapshot = parser.keyboard_input_snapshot();
        assert_eq!(snapshot & 0xff, u64::from(parser.keyboard_modes().bits()));
        assert_eq!((snapshot >> 8) & 0xff, u64::from(parser.kitty_keyboard_flags()));
        assert_eq!(snapshot >> 16, parser.keyboard_protocol_epoch());
    }
}

/// Screen roundtrips and individual DEC parameters must retain transitions hidden by final-state equality.
#[test]
fn win32_input_epoch_counts_screen_aba_and_each_private_parameter() {
    for mode in [47, 1047, 1049] {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(b"\x1b[?9001h");
        let epoch = parser.keyboard_protocol_epoch();
        parser.advance(format!("\x1b[?{mode}h\x1b[?{mode}l").as_bytes());
        assert_eq!(parser.keyboard_protocol_epoch(), epoch);

        parser.advance(b"\x1b[>4u");
        let epoch = parser.keyboard_protocol_epoch();
        parser.advance(format!("\x1b[?{mode}h\x1b[?{mode}l").as_bytes());
        assert_eq!(parser.kitty_keyboard_flags(), 4);
        assert_eq!(parser.keyboard_protocol_epoch(), epoch + 2);
    }

    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1049h\x1b[>4u\x1b[?1049l");
    assert_eq!(parser.keyboard_protocol_epoch(), 0);
    parser.advance(b"\x1b[?9001;1049h");
    assert!(parser.keyboard_modes().win32_input());
    assert_eq!(parser.kitty_keyboard_flags(), 4);
    assert_eq!(parser.keyboard_protocol_epoch(), 2);
}

/// Exhaustion stays a permanent packed sentinel instead of reusing an old held-key generation.
#[test]
fn win32_input_epoch_saturates_without_resetting_on_ris() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let exhausted = (1u64 << 48) - 1;
    parser.performer.keyboard_protocol_epoch = exhausted - 1;
    parser.advance(b"\x1b[?9001h");
    assert_eq!(parser.keyboard_protocol_epoch(), exhausted);
    assert_eq!(parser.keyboard_input_snapshot() >> 16, exhausted);
    assert!(parser.keyboard_modes().win32_input());

    for sequence in ["\x1b[?9001l\x1b[?9001h", "\x1b[>4u\x1b[<u", "\x1bc", "\x1bc"] {
        parser.advance(sequence.as_bytes());
        assert_eq!(parser.keyboard_protocol_epoch(), exhausted);
        assert_eq!(parser.keyboard_input_snapshot() >> 16, exhausted);
    }
    assert!(!parser.keyboard_modes().win32_input());
}

/// Each DECSET tracking code selects its distinct current mouse-reporting mode.
#[test]
fn decset_selects_each_mouse_tracking_mode() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(parser.mouse_tracking(), MouseTracking::Off);

    for (sequence, expected) in [
        (b"\x1b[?1000h".as_slice(), MouseTracking::Button),
        (b"\x1b[?1002h".as_slice(), MouseTracking::ButtonMotion),
        (b"\x1b[?1003h".as_slice(), MouseTracking::AnyMotion),
    ] {
        parser.advance(sequence);
        assert_eq!(parser.mouse_tracking(), expected);
    }
}

/// Selecting another tracking mode replaces the current mode without stacking it.
#[test]
fn last_mouse_tracking_decset_wins() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance(b"\x1b[?1003h\x1b[?1000h\x1b[?1002h");

    assert_eq!(parser.mouse_tracking(), MouseTracking::ButtonMotion);
}

/// DECRST turns tracking off when it names the currently selected mode.
#[test]
fn decrst_resets_the_active_mouse_tracking_mode() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1002h");

    parser.advance(b"\x1b[?1002l");

    assert_eq!(parser.mouse_tracking(), MouseTracking::Off);
}

/// Resetting the active mode turns tracking off without restoring its predecessor.
#[test]
fn active_mouse_tracking_reset_does_not_restore_the_prior_mode() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance(b"\x1b[?1000h\x1b[?1002h\x1b[?1002l");

    assert_eq!(parser.mouse_tracking(), MouseTracking::Off);
}

/// DECRST for an inactive tracking mode leaves the current selection unchanged.
#[test]
fn decrst_ignores_an_inactive_mouse_tracking_mode() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1003h");

    parser.advance(b"\x1b[?1000l\x1b[?1002l");

    assert_eq!(parser.mouse_tracking(), MouseTracking::AnyMotion);
}

/// RIS restores mouse tracking to Off alongside the other initial terminal modes.
#[test]
fn ris_resets_app_cursor_keys_and_mouse_tracking() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1h\x1b[?1003h");
    assert!(parser.application_cursor_keys());
    assert_eq!(parser.mouse_tracking(), MouseTracking::AnyMotion);

    parser.advance(b"\x1bc");

    assert!(!parser.application_cursor_keys());
    assert_eq!(parser.mouse_tracking(), MouseTracking::Off);
}

/// RIS restores all keyboard-affecting terminal modes, not only DECCKM.
#[test]
fn ris_resets_every_keyboard_mode() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1h\x1b[?67h\x1b=\x1b[20h\x1b[>4;2m");

    parser.advance(b"\x1bc");

    assert_eq!(parser.keyboard_modes(), super::KeyboardModes::default());
}

/// SGR report encoding can toggle independently without selecting a tracking mode.
#[test]
fn sgr_mouse_encoding_does_not_enable_tracking() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance(b"\x1b[?1006h");
    assert!(parser.mouse_sgr_enabled());
    assert_eq!(parser.mouse_tracking(), MouseTracking::Off);

    parser.advance(b"\x1b[?1002h\x1b[?1006l");
    assert!(!parser.mouse_sgr_enabled());
    assert_eq!(parser.mouse_tracking(), MouseTracking::ButtonMotion);
}

/// Every combination of the five pointer bits survives a pack and decode unchanged,
/// so a reader of the published byte sees exactly the modes the worker stored.
#[test]
fn pointer_modes_round_trip_every_combination() {
    let trackings = [
        MouseTracking::Off,
        MouseTracking::Button,
        MouseTracking::ButtonMotion,
        MouseTracking::AnyMotion,
    ];
    for tracking in trackings {
        for flag_bits in 0u8..8 {
            let modes = PointerModes::new(
                tracking,
                flag_bits & 1 != 0,
                flag_bits & 2 != 0,
                flag_bits & 4 != 0,
            );
            let decoded = PointerModes::from_bits(modes.bits());
            assert_eq!(decoded, modes);
            assert_eq!(decoded.tracking(), tracking);
            assert_eq!(decoded.sgr(), flag_bits & 1 != 0);
            assert_eq!(decoded.is_alt(), flag_bits & 2 != 0);
            assert_eq!(decoded.application_cursor(), flag_bits & 4 != 0);
        }
    }
    // The documented layout: tracking in bits 0-1, SGR bit 2, alt bit 3, app cursor bit 4.
    let all = PointerModes::new(MouseTracking::AnyMotion, true, true, true);
    assert_eq!(all.bits(), 0b1_1111);
    assert_eq!(PointerModes::new(MouseTracking::Button, false, false, false).bits(), 0b01);
}

/// The parser's pointer snapshot follows tracking, SGR, alternate-screen and DECCKM
/// changes, and RIS clears all of them, so publishing it after a batch is complete.
#[test]
fn pointer_input_snapshot_follows_mode_changes() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(parser.pointer_input_snapshot(), 0);

    parser.advance(b"\x1b[?1003h\x1b[?1006h\x1b[?1049h\x1b[?1h");
    let modes = PointerModes::from_bits(parser.pointer_input_snapshot());
    assert_eq!(modes.tracking(), MouseTracking::AnyMotion);
    assert!(modes.sgr() && modes.is_alt() && modes.application_cursor());

    parser.advance(b"\x1b[?1049l\x1b[?1006l");
    let modes = PointerModes::from_bits(parser.pointer_input_snapshot());
    assert_eq!(modes.tracking(), MouseTracking::AnyMotion);
    assert!(!modes.sgr() && !modes.is_alt() && modes.application_cursor());

    parser.advance(b"\x1bc");
    assert_eq!(parser.pointer_input_snapshot(), 0);
}

#[test]
fn kitty_keyboard_push_sets_flags() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(parser.kitty_keyboard_flags(), 0);

    // CSI > 1 u — push flags = 1 (disambiguate escape codes).
    parser.advance(b"\x1b[>1u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);
}

/// Kitty flag storage keeps the protocol's seven data bits without integer wraparound.
#[test]
fn kitty_keyboard_flags_mask_the_reserved_stack_bit() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance(b"\x1b[>255u");
    assert_eq!(parser.kitty_keyboard_flags(), 0x7f);

    parser.advance(b"\x1b[=128;1u");
    assert_eq!(parser.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_keyboard_pop_restores_previous_flags() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[>1u");
    parser.advance(b"\x1b[>5u");
    assert_eq!(parser.kitty_keyboard_flags(), 5);

    // CSI < u — pop one entry (default count 1).
    parser.advance(b"\x1b[<u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);

    // Pop the last entry back to legacy (0).
    parser.advance(b"\x1b[<u");
    assert_eq!(parser.kitty_keyboard_flags(), 0);

    // Popping an empty stack is a no-op, not a panic.
    parser.advance(b"\x1b[<u");
    assert_eq!(parser.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_keyboard_pop_count_pops_multiple() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[>1u");
    parser.advance(b"\x1b[>2u");
    parser.advance(b"\x1b[>4u");
    assert_eq!(parser.kitty_keyboard_flags(), 4);

    // CSI < 2 u — pop two entries.
    parser.advance(b"\x1b[<2u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);
}

#[test]
fn kitty_keyboard_set_replaces_top() {
    let mut parser = Parser::new(Grid::new(8, 2));
    // CSI = flags u with an empty stack pushes the active set.
    parser.advance(b"\x1b[=3u");
    assert_eq!(parser.kitty_keyboard_flags(), 3);

    // CSI = 5 ; 1 u — mode 1 (default) replaces the top.
    parser.advance(b"\x1b[=5;1u");
    assert_eq!(parser.kitty_keyboard_flags(), 5);

    // CSI = 2 ; 2 u — mode 2 ORs in the new bits.
    parser.advance(b"\x1b[=2;2u");
    assert_eq!(parser.kitty_keyboard_flags(), 7);

    // CSI = 1 ; 3 u — mode 3 clears the given bits.
    parser.advance(b"\x1b[=1;3u");
    assert_eq!(parser.kitty_keyboard_flags(), 6);

    // Unsupported application modes leave the active flags unchanged.
    parser.advance(b"\x1b[=31;9u");
    assert_eq!(parser.kitty_keyboard_flags(), 6);
    parser.advance(b"\x1b[=31;0u");
    assert_eq!(parser.kitty_keyboard_flags(), 6);
}

#[test]
fn kitty_keyboard_stack_depth_is_capped() {
    let mut parser = Parser::new(Grid::new(8, 2));
    // Push far more than the cap; flags must stay valid and the stack must
    // not grow without bound.
    for _ in 0..100 {
        parser.advance(b"\x1b[>1u");
    }
    assert_eq!(parser.kitty_keyboard_flags(), 1);
}

/// A full Kitty stack evicts its oldest entry so the newest push still becomes active.
#[test]
fn kitty_keyboard_full_stack_evicts_oldest_entry() {
    let mut parser = Parser::new(Grid::new(8, 2));
    for flags in 1..=32 {
        parser.advance(format!("\x1b[>{flags}u").as_bytes());
    }

    parser.advance(b"\x1b[>99u\x1b[<31u");

    assert_eq!(parser.kitty_keyboard_flags(), 2);
}

/// Main and alternate screens retain independent Kitty keyboard stacks.
#[test]
fn kitty_keyboard_flags_are_screen_local() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[>1u");

    parser.advance(b"\x1b[?1049h");
    assert_eq!(parser.kitty_keyboard_flags(), 0);
    parser.advance(b"\x1b[>2u");
    assert_eq!(parser.kitty_keyboard_flags(), 2);

    parser.advance(b"\x1b[?1049l");
    assert_eq!(parser.kitty_keyboard_flags(), 1);
    parser.advance(b"\x1b[?1049h");
    assert_eq!(parser.kitty_keyboard_flags(), 2);
}

#[test]
fn kitty_keyboard_query_reports_current_flags() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);

    // Query with no flags pushed → reply CSI ? 0 u.
    parser.advance(b"\x1b[?u");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b[?0u".to_vec());

    // Push flags = 1, then query → reply CSI ? 1 u.
    parser.advance(b"\x1b[>1u");
    parser.advance(b"\x1b[?u");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b[?1u".to_vec());
}

/// XTVERSION (`CSI > q`) answers `DCS > | kitty SonicTerm <version> ST` and sends exactly one
/// reply. The leading `kitty ` word is what Claude Code and similar clients test before sending
/// kitty graphics; it names no kitty version, and the SonicTerm release follows it.
#[test]
fn xtversion_reports_kitty_graphics_and_the_release_version() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);

    parser.advance(b"\x1b[>q");

    let expected = format!("\x1bP>|kitty SonicTerm {}\x1b\\", env!("CARGO_PKG_VERSION"));
    assert_eq!(rx.try_recv().unwrap(), expected.into_bytes());
    assert!(rx.try_recv().is_err(), "XTVERSION sends exactly one reply");
}

#[test]
fn ris_resets_kitty_keyboard_flags() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[>1u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);

    parser.advance(b"\x1bc");
    assert_eq!(parser.kitty_keyboard_flags(), 0);
}

/// DEC 2026 is stored: `h` sets it, `l` clears it, the epoch moves only on a reset-to-set
/// transition (a repeated `h` cannot renew a hold), and every set-to-reset transition is
/// counted, including an `l` followed by an `h` within one `advance`.
#[test]
fn synchronized_output_tracks_mode_epoch_and_resets() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(parser.synchronized_output(), SyncState { set: false, epoch: 0, resets: 0 });

    parser.advance(b"\x1b[?2026h");
    assert_eq!(parser.synchronized_output(), SyncState { set: true, epoch: 1, resets: 0 });
    parser.advance(b"\x1b[?2026h");
    assert_eq!(
        parser.synchronized_output(),
        SyncState { set: true, epoch: 1, resets: 0 },
        "a repeated set keeps its epoch"
    );

    parser.advance(b"\x1b[?2026l");
    assert_eq!(parser.synchronized_output(), SyncState { set: false, epoch: 1, resets: 1 });
    parser.advance(b"\x1b[?2026l");
    assert_eq!(
        parser.synchronized_output(),
        SyncState { set: false, epoch: 1, resets: 1 },
        "a reset while already reset is not a transition"
    );

    parser.advance(b"\x1b[?2026hpaint\x1b[?2026l\x1b[?2026h");
    assert_eq!(
        parser.synchronized_output(),
        SyncState { set: true, epoch: 3, resets: 2 },
        "a reset inside one advance is counted even when the mode is set again"
    );
}

/// RIS clears a set mode and counts the reset; leaving the alternate screen clears no mode,
/// as it clears no other mode; a fresh parser starts reset with no resets counted.
#[test]
fn synchronized_output_resets_only_on_ris() {
    let fresh = Parser::new(Grid::new(8, 2));
    assert_eq!(fresh.synchronized_output(), SyncState { set: false, epoch: 0, resets: 0 });

    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1049h\x1b[?2026h\x1b[?1049l");
    assert_eq!(
        parser.synchronized_output(),
        SyncState { set: true, epoch: 1, resets: 0 },
        "?1049l leaves the mode set"
    );

    parser.advance(b"\x1bc");
    assert_eq!(parser.synchronized_output(), SyncState { set: false, epoch: 1, resets: 1 });
    parser.advance(b"\x1bc");
    assert_eq!(
        parser.synchronized_output(),
        SyncState { set: false, epoch: 1, resets: 1 },
        "RIS while reset counts nothing"
    );
}

/// DECRQM for mode 2026 answers `CSI ? 2026 ; Ps $ y`, with Ps 2 while reset and 1 while set,
/// through the captured reply path, and prints nothing.
#[test]
fn decrqm_reports_synchronized_output_state() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let (_, _, reply) = parser.advance_with_replies(b"\x1b[?2026$p");
    assert_eq!(reply, b"\x1b[?2026;2$y");

    parser.advance(b"\x1b[?2026h");
    let (_, _, reply) = parser.advance_with_replies(b"\x1b[?2026$p");
    assert_eq!(reply, b"\x1b[?2026;1$y");

    parser.advance(b"\x1b[?2026l");
    let (_, _, reply) = parser.advance_with_replies(b"\x1b[?2026$p");
    assert_eq!(reply, b"\x1b[?2026;2$y");
    assert_eq!(parser.grid().cursor.col, 0, "a query prints nothing");
}

/// Only mode 2026 is answered: DECRQM for any other private mode, and an unhandled `?` final,
/// stay silent and change no mode, keyboard, pointer or cursor state, as before 2026 was answered.
#[test]
fn other_private_mode_queries_stay_unanswered() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1h\x1b[?2026h");
    let before = (
        parser.synchronized_output(),
        parser.keyboard_input_snapshot(),
        parser.pointer_input_snapshot(),
        parser.grid().cursor.row,
        parser.grid().cursor.col,
    );
    for query in
        [&b"\x1b[?1$p"[..], b"\x1b[?25$p", b"\x1b[?2026;1$p", b"\x1b[?2026z", b"\x1b[?2026$q"]
    {
        let (consumed, events, reply) = parser.advance_with_replies(query);
        assert_eq!(consumed, query.len(), "{query:?}");
        assert!(events.is_empty(), "{query:?}");
        assert!(reply.is_empty(), "{query:?} must not be answered");
        let after = (
            parser.synchronized_output(),
            parser.keyboard_input_snapshot(),
            parser.pointer_input_snapshot(),
            parser.grid().cursor.row,
            parser.grid().cursor.col,
        );
        assert_eq!(after, before, "{query:?} changed state");
    }
}
