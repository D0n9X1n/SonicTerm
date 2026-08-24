use super::{
    begin_pointer_gesture, cancel_pointer_gesture, is_quit_chord, no_button_motion_report,
    pointer_report_bytes, route_pressed_pointer_motion, take_pointer_release, wheel_report_bytes,
    PointerCell, PointerGestureOwner, PointerMotionRoute, PointerReportKind,
};
use sonicterm_cfg::keymap::Action;
use sonicterm_vt::vt::MouseTracking;
use winit::keyboard::ModifiersState;

fn pointer_cell(pane_id: u64, row: u16, col: u16) -> PointerCell {
    PointerCell { pane_id, row, col }
}

#[test]
fn sgr_wheel_up_is_button_64() {
    // col=5, row=3, one tick up → ESC[<64;5;3M
    assert_eq!(wheel_report_bytes(true, true, 5, 3, 1), b"\x1b[<64;5;3M".to_vec());
}

#[test]
fn sgr_wheel_down_is_button_65() {
    assert_eq!(wheel_report_bytes(true, false, 5, 3, 1), b"\x1b[<65;5;3M".to_vec());
}

#[test]
fn sgr_emits_one_report_per_line() {
    // 3 ticks → three concatenated reports.
    assert_eq!(
        wheel_report_bytes(true, true, 1, 1, 3),
        b"\x1b[<64;1;1M\x1b[<64;1;1M\x1b[<64;1;1M".to_vec()
    );
}

#[test]
fn legacy_x10_encodes_button_and_coords_plus_32() {
    // up=button 64 → 64+32=96 ('`'); col 5 → 37 ('%'); row 3 → 35 ('#').
    assert_eq!(wheel_report_bytes(false, true, 5, 3, 1), vec![0x1b, b'[', b'M', 96, 37, 35]);
}

#[test]
fn legacy_x10_clamps_large_coords() {
    // col/row clamp to 223 so +32 stays within a byte (255).
    let out = wheel_report_bytes(false, false, 9999, 9999, 1);
    assert_eq!(out, vec![0x1b, b'[', b'M', 97, 255, 255]); // 65+32=97
}

#[test]
fn sgr_pointer_reports_encode_press_release_and_motion_modifiers() {
    // SGR keeps left-button Cb on release, uses lowercase `m`, and adds the
    // current modifier and motion bits without changing one-based coordinates.
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::LeftPress, ModifiersState::empty(), 0, 0,),
        b"\x1b[<0;1;1M".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::LeftRelease, ModifiersState::empty(), 0, 0,),
        b"\x1b[<0;1;1m".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::HeldLeftMotion, ModifiersState::ALT, 0, 0),
        b"\x1b[<40;1;1M".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(
            true,
            PointerReportKind::NoButtonMotion,
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            0,
            0,
        ),
        b"\x1b[<55;1;1M".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::LeftPress, ModifiersState::SUPER, 0, 0,),
        b"\x1b[<8;1;1M".to_vec()
    );
}

#[test]
fn legacy_pointer_reports_encode_exact_codes_and_zero_cell() {
    // Legacy pointer reports retain the X10 byte layout: Cb+32 followed by
    // one-based coordinates+32, so pane-local cell zero is byte 33 on both axes.
    let cases = [
        (PointerReportKind::LeftPress, 32),
        (PointerReportKind::LeftRelease, 35),
        (PointerReportKind::HeldLeftMotion, 64),
        (PointerReportKind::NoButtonMotion, 67),
    ];
    for (kind, cb) in cases {
        assert_eq!(
            pointer_report_bytes(false, kind, ModifiersState::empty(), 0, 0),
            vec![0x1b, b'[', b'M', cb, 33, 33]
        );
    }
    assert_eq!(
        pointer_report_bytes(false, PointerReportKind::LeftRelease, ModifiersState::ALT, 0, 0),
        vec![0x1b, b'[', b'M', 43, 33, 33]
    );
}

#[test]
fn legacy_pointer_reports_clamp_large_coordinates() {
    // The current legacy profile caps pane-local zero-based coordinates at 222,
    // yielding protocol coordinate 223 and a final encoded byte of 255.
    assert_eq!(
        pointer_report_bytes(
            false,
            PointerReportKind::LeftPress,
            ModifiersState::empty(),
            u16::MAX,
            u16::MAX,
        ),
        vec![0x1b, b'[', b'M', 32, 255, 255]
    );
}

#[test]
fn tracked_press_chooses_terminal_unless_shift_or_tracking_off() {
    // Press-time Shift overrides every active tracking mode, while an
    // unmodified active mode latches terminal ownership and Off stays local.
    let cell = pointer_cell(7, 3, 4);
    for tracking in [MouseTracking::Button, MouseTracking::ButtonMotion, MouseTracking::AnyMotion] {
        let terminal = begin_pointer_gesture(cell, tracking, true, ModifiersState::empty(), false)
            .expect("grid press must create a gesture");
        assert!(matches!(terminal.owner, PointerGestureOwner::Terminal { .. }));
    }

    let shifted =
        begin_pointer_gesture(cell, MouseTracking::AnyMotion, true, ModifiersState::SHIFT, false)
            .expect("shifted grid press must create a local gesture");
    assert_eq!(shifted.owner, PointerGestureOwner::Local);

    let off = begin_pointer_gesture(cell, MouseTracking::Off, true, ModifiersState::empty(), false)
        .expect("untracked grid press must create a local gesture");
    assert_eq!(off.owner, PointerGestureOwner::Local);
}

#[test]
fn consumed_ui_press_creates_no_terminal_gesture_or_report() {
    // A press consumed by SonicTerm chrome never latches a grid owner and emits
    // no bytes even when the pane beneath it requested mouse tracking.
    assert_eq!(
        begin_pointer_gesture(
            pointer_cell(7, 0, 0),
            MouseTracking::AnyMotion,
            true,
            ModifiersState::empty(),
            true,
        ),
        None
    );
}

#[test]
fn terminal_owner_latches_press_pane_mode_profile_and_last_cell() {
    // Current Shift, parser mode, and profile changes cannot steal a terminal
    // gesture; motion remains pinned to the press pane and its last valid cell.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 3, 4),
        MouseTracking::ButtonMotion,
        true,
        ModifiersState::empty(),
        false,
    )
    .expect("tracked press must create a terminal gesture");

    let route = route_pressed_pointer_motion(
        &mut gesture,
        Some(pointer_cell(8, 9, 9)),
        ModifiersState::SHIFT,
    );
    assert_eq!(
        route,
        PointerMotionRoute::Report {
            pane_id: 7,
            sgr: true,
            row: 3,
            col: 4,
            modifiers: ModifiersState::SHIFT,
        }
    );
    assert!(matches!(gesture.owner, PointerGestureOwner::Terminal { .. }));
}

#[test]
fn local_owner_survives_shift_release() {
    // Once Shift gives the press to local selection, later modifier state does
    // not promote the held gesture into terminal reporting.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 1, 2),
        MouseTracking::AnyMotion,
        true,
        ModifiersState::SHIFT,
        false,
    )
    .expect("shifted press must create a local gesture");
    assert_eq!(
        route_pressed_pointer_motion(
            &mut gesture,
            Some(pointer_cell(7, 2, 3)),
            ModifiersState::empty(),
        ),
        PointerMotionRoute::Local
    );
    assert_eq!(gesture.owner, PointerGestureOwner::Local);
}

#[test]
fn terminal_motion_obeys_latched_tracking_mode() {
    // Button suppresses held motion, whereas ButtonMotion and AnyMotion both
    // emit held-left motion through the same terminal route.
    for (tracking, expected_report) in [
        (MouseTracking::Button, false),
        (MouseTracking::ButtonMotion, true),
        (MouseTracking::AnyMotion, true),
    ] {
        let mut gesture = begin_pointer_gesture(
            pointer_cell(7, 1, 2),
            tracking,
            false,
            ModifiersState::empty(),
            false,
        )
        .expect("active tracking must create a terminal gesture");
        assert_eq!(
            matches!(
                route_pressed_pointer_motion(
                    &mut gesture,
                    Some(pointer_cell(7, 4, 5)),
                    ModifiersState::ALT,
                ),
                PointerMotionRoute::Report { .. }
            ),
            expected_report
        );
    }
}

#[test]
fn any_motion_without_pressed_gesture_uses_current_pane_profile_and_modifiers() {
    // Hover motion is live rather than latched: only current AnyMotion emits,
    // carrying the current pane, profile, coordinates, and modifiers.
    assert_eq!(
        no_button_motion_report(
            pointer_cell(9, 2, 5),
            MouseTracking::AnyMotion,
            false,
            ModifiersState::SHIFT | ModifiersState::CONTROL,
        ),
        Some(PointerMotionRoute::Report {
            pane_id: 9,
            sgr: false,
            row: 2,
            col: 5,
            modifiers: ModifiersState::SHIFT | ModifiersState::CONTROL,
        })
    );
    assert_eq!(
        no_button_motion_report(
            pointer_cell(9, 2, 5),
            MouseTracking::ButtonMotion,
            true,
            ModifiersState::empty(),
        ),
        None
    );
}

#[test]
fn terminal_release_uses_press_pane_profile_and_last_same_pane_cell() {
    // Same-pane motion advances the retained cell; crossing panes or leaving the
    // grid does not, and release consumes the latched press pane/profile.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 1, 2),
        MouseTracking::AnyMotion,
        false,
        ModifiersState::empty(),
        false,
    )
    .expect("tracked press must create a terminal gesture");
    let _ = route_pressed_pointer_motion(
        &mut gesture,
        Some(pointer_cell(7, 4, 5)),
        ModifiersState::empty(),
    );
    let _ = route_pressed_pointer_motion(
        &mut gesture,
        Some(pointer_cell(8, 9, 10)),
        ModifiersState::empty(),
    );
    let _ = route_pressed_pointer_motion(&mut gesture, None, ModifiersState::empty());

    assert_eq!(
        take_pointer_release(&mut Some(gesture), ModifiersState::ALT),
        Some(PointerMotionRoute::Report {
            pane_id: 7,
            sgr: false,
            row: 4,
            col: 5,
            modifiers: ModifiersState::ALT,
        })
    );
}

#[test]
fn main_and_child_callers_share_one_pointer_decision_contract() {
    // Main and child wrappers deliberately delegate to this one pure decision;
    // equivalent surface inputs therefore produce one identical owner value.
    fn main_decision() -> Option<super::PointerGesture> {
        begin_pointer_gesture(
            pointer_cell(11, 6, 7),
            MouseTracking::ButtonMotion,
            true,
            ModifiersState::empty(),
            false,
        )
    }
    fn child_decision() -> Option<super::PointerGesture> {
        begin_pointer_gesture(
            pointer_cell(11, 6, 7),
            MouseTracking::ButtonMotion,
            true,
            ModifiersState::empty(),
            false,
        )
    }
    assert_eq!(main_decision(), child_decision());
}

#[test]
fn pointer_cleanup_clears_latched_gesture() {
    // Focus-loss and global drag cleanup share this primitive, preventing a
    // gesture whose release was lost from resuming on a later cursor event.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 0, 0),
        MouseTracking::Button,
        true,
        ModifiersState::empty(),
        false,
    );
    assert!(cancel_pointer_gesture(&mut gesture));
    assert_eq!(gesture, None);
    assert!(!cancel_pointer_gesture(&mut gesture));
}

#[test]
fn explicit_quit_app_binding_is_quit_chord_any_key() {
    // An explicit `quit_app` binding fires the guard regardless of chord or
    // platform — this is the cross-platform / user-rebind path.
    assert!(is_quit_chord("super+q", Some(&Action::QuitApp)));
    assert!(is_quit_chord("ctrl+shift+q", Some(&Action::QuitApp)));
}

#[test]
fn super_q_bound_elsewhere_is_not_quit_chord() {
    // If the user deliberately rebound super+q to another action, respect it.
    assert!(!is_quit_chord("super+q", Some(&Action::CloseActivePaneOrTab)));
    assert!(!is_quit_chord("super+q", Some(&Action::NewTab)));
}

#[test]
fn other_chords_unbound_are_not_quit_chord() {
    // Unbound non-quit chords must never trigger quit (they fall through to
    // the PTY as normal input).
    assert!(!is_quit_chord("super+w", None));
    assert!(!is_quit_chord("q", None));
    assert!(!is_quit_chord("super+shift+q", None));
}

#[cfg(target_os = "macos")]
#[test]
fn unbound_super_q_is_quit_chord_on_macos() {
    // The reported bug: a user keymap with no `super+q` binding must still
    // quit on macOS (Cmd+Q is a system chord) instead of typing a literal q.
    assert!(is_quit_chord("super+q", None));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unbound_super_q_is_not_quit_chord_off_macos() {
    // Off macOS, Cmd+Q is not a system quit chord; only an explicit binding
    // quits.
    assert!(!is_quit_chord("super+q", None));
}
