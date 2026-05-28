//! Integration tests for `sonic_ui::prefs::controls`.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in PR 5 of the
//! workspace refactor (issue #121) per CLAUDE.md §5.

use sonic_ui::prefs::controls::{
    Button, ButtonKind, ColorSwatch, Control, Dropdown, InteractionState, Rect, Slider, TextField,
    Toggle, WidgetId,
};

fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(x, y, w, h)
}

// ---- Rect ----

#[test]
fn rect_contains_is_half_open() {
    let r = r(10.0, 10.0, 20.0, 20.0);
    assert!(r.contains(10.0, 10.0));
    assert!(r.contains(29.9, 29.9));
    assert!(!r.contains(30.0, 30.0));
    assert!(!r.contains(9.9, 15.0));
}

// ---- Toggle ----

#[test]
fn toggle_hit_test_and_flip() {
    let mut t = Toggle::new(WidgetId(1), "cursor_blink", r(0.0, 0.0, 40.0, 20.0), false);
    assert!(t.hit_test(10.0, 10.0));
    assert!(!t.hit_test(100.0, 10.0));
    assert!(!t.get());
    assert!(t.toggle());
    assert!(t.get());
    t.set(false);
    assert!(!t.get());
}

// ---- Slider ----

#[test]
fn slider_clamps_constructed_value() {
    let s = Slider::new(WidgetId(2), "size", r(0.0, 0.0, 100.0, 20.0), 8.0, 32.0, 999.0);
    assert_eq!(s.get(), 32.0);
    let s2 = Slider::new(WidgetId(3), "size", r(0.0, 0.0, 100.0, 20.0), 8.0, 32.0, -10.0);
    assert_eq!(s2.get(), 8.0);
}

#[test]
fn slider_drag_maps_pixels_to_value() {
    let mut s = Slider::new(WidgetId(4), "opacity", r(100.0, 0.0, 200.0, 20.0), 0.0, 1.0, 0.0);
    s.drag_to(100.0);
    assert!((s.get() - 0.0).abs() < 1e-5);
    s.drag_to(200.0);
    assert!((s.get() - 0.5).abs() < 1e-5);
    s.drag_to(300.0);
    assert!((s.get() - 1.0).abs() < 1e-5);
    s.drag_to(-50.0);
    assert!((s.get() - 0.0).abs() < 1e-5);
    s.drag_to(1000.0);
    assert!((s.get() - 1.0).abs() < 1e-5);
}

#[test]
fn slider_step_snaps() {
    let mut s =
        Slider::new(WidgetId(5), "size", r(0.0, 0.0, 100.0, 20.0), 8.0, 32.0, 10.0).with_step(2.0);
    s.set(13.0);
    assert_eq!(s.get(), 14.0);
    s.set(12.4);
    assert_eq!(s.get(), 12.0);
}

#[test]
fn slider_fraction_is_clamped() {
    let s = Slider::new(WidgetId(6), "x", r(0.0, 0.0, 100.0, 20.0), 0.0, 10.0, 5.0);
    assert!((s.fraction() - 0.5).abs() < 1e-5);
    let s2 = Slider::new(WidgetId(7), "x", r(0.0, 0.0, 100.0, 20.0), 5.0, 5.0001, 5.0);
    let f = s2.fraction();
    assert!((0.0..=1.0).contains(&f));
}

// ---- Dropdown ----

#[test]
fn dropdown_select_and_value() {
    let mut d = Dropdown::new(
        WidgetId(8),
        "theme",
        r(0.0, 0.0, 200.0, 24.0),
        vec!["dracula".into(), "tokyo-night".into(), "solarized".into()],
        0,
    );
    assert_eq!(d.value(), Some("dracula"));
    assert!(d.select(2));
    assert_eq!(d.value(), Some("solarized"));
    assert!(!d.select(99));
    assert_eq!(d.get(), 2);
    assert!(d.select_by_name("tokyo-night"));
    assert_eq!(d.get(), 1);
    assert!(!d.select_by_name("nope"));
}

#[test]
fn dropdown_hit_option_only_when_open() {
    let mut d = Dropdown::new(
        WidgetId(9),
        "theme",
        r(10.0, 10.0, 100.0, 20.0),
        vec!["a".into(), "b".into(), "c".into()],
        0,
    );
    assert_eq!(d.hit_option(20.0, 40.0), None);
    d.toggle_open();
    assert_eq!(d.hit_option(20.0, 35.0), Some(0));
    assert_eq!(d.hit_option(20.0, 55.0), Some(1));
    assert_eq!(d.hit_option(20.0, 75.0), Some(2));
    assert_eq!(d.hit_option(20.0, 95.0), None);
    assert_eq!(d.hit_option(200.0, 35.0), None);
}

#[test]
fn dropdown_select_closes_list() {
    let mut d =
        Dropdown::new(WidgetId(10), "x", r(0.0, 0.0, 50.0, 20.0), vec!["a".into(), "b".into()], 0);
    d.toggle_open();
    d.select(1);
    assert!(!d.open);
}

// ---- ColorSwatch ----

#[test]
fn color_swatch_pick_updates_value() {
    let mut c = ColorSwatch::new(WidgetId(11), "fg", r(0.0, 0.0, 80.0, 20.0), [0, 0, 0, 255]);
    assert!(c.pick(9));
    assert_eq!(c.get(), [0xff, 0x00, 0x00, 0xff]);
    assert!(!c.pick(99));
}

#[test]
fn color_swatch_hex_roundtrip() {
    let mut c =
        ColorSwatch::new(WidgetId(12), "fg", r(0.0, 0.0, 80.0, 20.0), [0x12, 0x34, 0x56, 255]);
    assert_eq!(c.to_hex(), "#123456");
    let parsed = ColorSwatch::from_hex("#abcdef").unwrap();
    c.set(parsed);
    assert_eq!(c.to_hex(), "#abcdef");
    assert!(ColorSwatch::from_hex("xyz").is_none());
    assert!(ColorSwatch::from_hex("#12345").is_none());
}

#[test]
fn color_swatch_hit_cell_is_bounded() {
    let c = ColorSwatch::new(WidgetId(13), "fg", r(10.0, 10.0, 80.0, 20.0), [0; 4]);
    assert_eq!(c.hit_cell(10.0, 34.0), Some(0));
    assert_eq!(c.hit_cell(10.0 + 18.0, 34.0), Some(1));
    assert_eq!(c.hit_cell(10.0, 34.0 + 18.0), Some(8));
    assert_eq!(c.hit_cell(10.0, 33.9), None);
    assert_eq!(c.hit_cell(10.0 + 18.0 * 8.0, 34.0), None);
}

// ---- TextField ----

#[test]
fn text_field_push_pop_respects_cap() {
    let mut tf = TextField::new(WidgetId(14), "shell", r(0.0, 0.0, 100.0, 20.0), "");
    tf.max_len = 3;
    tf.push_char('a');
    tf.push_char('b');
    tf.push_char('c');
    tf.push_char('d');
    assert_eq!(tf.get(), "abc");
    tf.pop_char();
    assert_eq!(tf.get(), "ab");
    tf.set("hellothere");
    assert_eq!(tf.get(), "hel");
}

#[test]
fn text_field_focus_blur() {
    let mut tf = TextField::new(WidgetId(15), "shell", r(0.0, 0.0, 100.0, 20.0), "x");
    assert!(!tf.focused);
    tf.focus();
    assert!(tf.focused);
    tf.blur();
    assert!(!tf.focused);
}

// ---- Control enum ----

#[test]
fn control_enum_dispatches_hit_test_and_id() {
    let t = Control::Toggle(Toggle::new(WidgetId(16), "a", r(0.0, 0.0, 10.0, 10.0), false));
    let s =
        Control::Slider(Slider::new(WidgetId(17), "b", r(20.0, 0.0, 10.0, 10.0), 0.0, 1.0, 0.5));
    assert_eq!(t.id(), WidgetId(16));
    assert_eq!(s.id(), WidgetId(17));
    assert!(t.hit_test(1.0, 1.0));
    assert!(!t.hit_test(25.0, 1.0));
    assert!(s.hit_test(25.0, 1.0));
}

#[test]
fn widget_id_displays() {
    assert_eq!(format!("{}", WidgetId(42)), "w42");
}

// ---- Issue #173 slice-2 redesign: Button + interaction state ----

#[test]
fn button_text_is_horizontally_centered_in_pill() {
    // Fixes #169: Apply button text used to be left-aligned.
    let b = Button::new(WidgetId(20), "Apply", r(100.0, 50.0, 112.0, 32.0), ButtonKind::Primary);
    let (cx, cy) = b.text_center();
    assert!((cx - (100.0 + 112.0 / 2.0)).abs() < 1e-5, "text x-center should sit in the middle");
    assert!((cy - (50.0 + 32.0 / 2.0)).abs() < 1e-5, "text y-center should sit in the middle");
}

#[test]
fn combobox_open_close_state_transitions() {
    // Fixes #166 / #168: popover opens on click, click on a row closes.
    let mut d = Dropdown::new(
        WidgetId(21),
        "theme",
        r(0.0, 0.0, 200.0, 32.0),
        vec!["a".into(), "b".into()],
        0,
    );
    assert!(!d.open, "starts closed");
    d.toggle_open();
    assert!(d.open, "opens on toggle (click)");
    d.close();
    assert!(!d.open, "closes on outside click");
    d.toggle_open();
    assert!(d.open);
    assert!(d.select(1));
    assert!(!d.open, "selecting closes the popover");
    // Visible chevron column is reserved on the right edge.
    let chev = d.chevron_rect();
    assert_eq!(chev.x, 200.0 - Dropdown::CHEVRON_W);
    assert_eq!(chev.h, 32.0);
}

#[test]
fn toggle_thumb_slides_with_value() {
    let mut t = Toggle::new(WidgetId(22), "blink", r(10.0, 10.0, 44.0, 24.0), false);
    let knob = 20.0_f32;
    let margin = 2.0_f32;
    let off_x = t.knob_x(knob, margin);
    assert!((off_x - (10.0 + margin)).abs() < 1e-5, "off → thumb on the left");
    t.set(true);
    let on_x = t.knob_x(knob, margin);
    assert!((on_x - (10.0 + 44.0 - knob - margin)).abs() < 1e-5, "on → thumb slid to the right");
    assert!(on_x > off_x, "on knob is right of off knob");
}

#[test]
fn interaction_state_hover_press_focus_independent() {
    // Hover / press / focus are independent dimensions per issue #173 spec.
    let s = InteractionState::new();
    assert!(!s.hovered && !s.pressed && !s.focused);
    let h = InteractionState { hovered: true, ..InteractionState::new() };
    assert!(h.hovered && !h.pressed && !h.focused);
    let f = InteractionState { focused: true, pressed: true, hovered: false };
    assert!(f.focused && f.pressed && !f.hovered);
}

#[test]
fn button_hit_test_respects_pill_rect() {
    let b = Button::new(WidgetId(23), "Cancel", r(50.0, 100.0, 96.0, 32.0), ButtonKind::Secondary);
    assert!(b.hit_test(50.0, 100.0));
    assert!(b.hit_test(145.9, 131.9));
    assert!(!b.hit_test(146.0, 100.0));
    assert!(!b.hit_test(49.9, 100.0));
    assert_eq!(b.kind, ButtonKind::Secondary);
}
