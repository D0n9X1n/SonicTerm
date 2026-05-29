use sonic_cfg::theme::Color;

#[test]
fn shift_toward_moves_color_by_requested_fraction() {
    let bg = Color::rgb(10, 20, 30);
    let fg = Color::rgb(110, 220, 130);

    assert_eq!(bg.shift_toward(fg, 0.18), Color::rgb(28, 56, 48));
}

#[test]
fn shift_toward_clamps_amount() {
    let bg = Color::rgb(10, 20, 30);
    let fg = Color::rgb(110, 220, 130);

    assert_eq!(bg.shift_toward(fg, -1.0), bg);
    assert_eq!(bg.shift_toward(fg, 2.0), fg);
}
