use super::*;

#[test]
fn plain_cell_carries_no_fat_allocation() {
    let c = Cell::plain('x', Color::Indexed(1), Color::Default, CellFlags::BOLD);
    assert!(!c.has_fat());
    assert_eq!(c.hyperlink(), None);
    assert_eq!(c.extras(), None);
    // Absent fat still reports the terminal-default underline style.
    assert_eq!(c.underline_style(), UnderlineStyle::Single);
    assert_eq!(c.underline_color(), None);
}

#[test]
fn default_cell_is_a_blank_space_without_fat() {
    let c = Cell::default();
    assert_eq!(c.ch, ' ');
    assert!(!c.has_fat());
}

#[test]
fn setting_hyperlink_materializes_fat_then_clearing_collapses_it() {
    let mut c = Cell::default();
    let id = HyperlinkId(7);
    c.set_hyperlink(Some(id));
    assert!(c.has_fat(), "first rare-attr write must allocate the box");
    assert_eq!(c.hyperlink(), Some(id));

    c.set_hyperlink(None);
    assert!(!c.has_fat(), "clearing the last rare attr must drop the box");
    assert_eq!(c.hyperlink(), None);
}

#[test]
fn setting_extras_materializes_fat_then_clearing_collapses_it() {
    let mut c = Cell::default();
    c.set_extras(Some("\u{200d}\u{1f469}".to_string().into_boxed_str()));
    assert!(c.has_fat());
    assert_eq!(c.extras(), Some("\u{200d}\u{1f469}"));

    c.set_extras(None);
    assert!(!c.has_fat());
    assert_eq!(c.extras(), None);
}

#[test]
fn clearing_one_of_two_rare_attrs_keeps_fat_alive() {
    let mut c = Cell::default();
    c.set_hyperlink(Some(HyperlinkId(1)));
    c.set_extras(Some("a".to_string().into_boxed_str()));
    assert!(c.has_fat());

    // Drop only extras: hyperlink still needs the box, so it must survive.
    c.set_extras(None);
    assert!(c.has_fat());
    assert_eq!(c.hyperlink(), Some(HyperlinkId(1)));
    assert_eq!(c.extras(), None);

    // Now drop the hyperlink too: nothing left, box collapses.
    c.set_hyperlink(None);
    assert!(!c.has_fat());
}

#[test]
fn setting_non_default_underline_style_allocates_and_single_collapses() {
    let mut c = Cell::default();
    c.set_underline_style(UnderlineStyle::Curly);
    assert!(c.has_fat());
    assert_eq!(c.underline_style(), UnderlineStyle::Curly);

    // Single is the implicit default, so selecting it clears the stored
    // style and, with nothing else in the box, collapses back to no-fat.
    c.set_underline_style(UnderlineStyle::Single);
    assert!(!c.has_fat());
    assert_eq!(c.underline_style(), UnderlineStyle::Single);
}

#[test]
fn underline_color_lifecycle_is_independent_of_style() {
    let mut c = Cell::default();
    c.set_underline_color(Some(Color::Rgb(1, 2, 3)));
    assert!(c.has_fat());
    assert_eq!(c.underline_color(), Some(Color::Rgb(1, 2, 3)));

    c.set_underline_color(None);
    assert!(!c.has_fat());
    assert_eq!(c.underline_color(), None);
}

#[test]
fn take_extras_removes_cluster_but_preserves_other_rare_attrs() {
    let mut c = Cell::default();
    c.set_hyperlink(Some(HyperlinkId(42)));
    c.set_extras(Some("zz".to_string().into_boxed_str()));

    let taken = c.take_extras();
    assert_eq!(taken.as_deref(), Some("zz"));
    assert_eq!(c.extras(), None);
    // Hyperlink still present ⇒ box must remain.
    assert!(c.has_fat());
    assert_eq!(c.hyperlink(), Some(HyperlinkId(42)));
}

#[test]
fn take_extras_on_last_rare_attr_collapses_fat() {
    let mut c = Cell::default();
    c.set_extras(Some("q".to_string().into_boxed_str()));
    let taken = c.take_extras();
    assert_eq!(taken.as_deref(), Some("q"));
    assert!(!c.has_fat());
}

#[test]
fn take_extras_on_plain_cell_is_none_and_allocates_nothing() {
    let mut c = Cell::default();
    assert_eq!(c.take_extras(), None);
    assert!(!c.has_fat());
}

#[test]
fn clearing_an_already_clear_attr_never_allocates() {
    let mut c = Cell::default();
    c.set_hyperlink(None);
    c.set_extras(None);
    c.set_underline_color(None);
    assert!(!c.has_fat());
}

#[test]
fn overwriting_hyperlink_updates_value_without_extra_boxes() {
    let mut c = Cell::default();
    c.set_hyperlink(Some(HyperlinkId(1)));
    c.set_hyperlink(Some(HyperlinkId(2)));
    assert!(c.has_fat());
    assert_eq!(c.hyperlink(), Some(HyperlinkId(2)));
}

#[test]
fn toml_round_trip_preserves_inline_and_all_rare_attributes() {
    let mut cell = Cell::plain(
        '界',
        Color::Rgb(1, 2, 3),
        Color::Indexed(17),
        CellFlags::BOLD | CellFlags::ITALIC | CellFlags::UNDERLINE | CellFlags::WIDE,
    );
    cell.set_hyperlink(Some(HyperlinkId(99)));
    cell.set_extras(Some("\u{fe0f}\u{200d}".to_string().into_boxed_str()));
    cell.set_underline_style(UnderlineStyle::Curly);
    cell.set_underline_color(Some(Color::Rgb(9, 8, 7)));

    let encoded = toml::to_string(&cell).unwrap();
    let decoded: Cell = toml::from_str(&encoded).unwrap();

    assert_eq!(decoded, cell);
    assert!(decoded.has_fat());
}

#[test]
fn legacy_six_field_toml_deserializes_with_default_underline_metadata() {
    let legacy = r#"
ch = "x"
fg = { Rgb = [12, 34, 56] }
bg = "Default"
flags = 5
hyperlink = 7
extras = "́"
"#;

    let cell: Cell = toml::from_str(legacy).unwrap();

    assert_eq!(cell.ch, 'x');
    assert_eq!(cell.fg, Color::Rgb(12, 34, 56));
    assert_eq!(cell.bg, Color::Default);
    assert_eq!(cell.flags, CellFlags::BOLD | CellFlags::UNDERLINE);
    assert_eq!(cell.hyperlink(), Some(HyperlinkId(7)));
    assert_eq!(cell.extras(), Some("\u{301}"));
    assert_eq!(cell.underline_style(), UnderlineStyle::Single);
    assert_eq!(cell.underline_color(), None);
}

#[test]
fn minimal_toml_defaults_optional_colors_flags_and_rare_attributes() {
    let cell: Cell = toml::from_str("ch = 'q'").unwrap();

    assert_eq!(cell, Cell::plain('q', Color::Default, Color::Default, CellFlags::empty()));
    assert!(!cell.has_fat());
}
