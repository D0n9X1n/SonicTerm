use super::wheel_report_bytes;

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

