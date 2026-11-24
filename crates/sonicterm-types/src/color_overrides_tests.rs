use super::*;

/// A fresh store holds no override, so a pane that never set a color keeps the theme everywhere.
#[test]
fn a_new_store_holds_no_override() {
    let overrides = ColorOverrides::default();
    assert!(overrides.is_empty());
    assert_eq!(overrides.dynamic(DynamicColor::Foreground), None);
    assert_eq!(overrides.palette(200), None);
}

/// Setting a color changes the generation once; setting the same color again stores nothing new,
/// so a program that repeats one color every batch does not repaint every frame.
#[test]
fn only_a_stored_change_moves_the_generation() {
    let mut overrides = ColorOverrides::default();
    let start = overrides.generation();
    overrides.set_dynamic(DynamicColor::Background, Some((1, 2, 3)));
    let after_set = overrides.generation();
    assert_ne!(after_set, start);
    overrides.set_dynamic(DynamicColor::Background, Some((1, 2, 3)));
    assert_eq!(overrides.generation(), after_set, "the same color stores nothing new");
    overrides.set_dynamic(DynamicColor::Cursor, None);
    assert_eq!(overrides.generation(), after_set, "resetting an absent override changes nothing");
    assert_eq!(overrides.dynamic(DynamicColor::Background), Some((1, 2, 3)));
}

/// Palette overrides are recorded per index; resetting the whole palette clears every entry and
/// leaves the dynamic colors alone, which is what RIS and a bare OSC 104 need.
#[test]
fn palette_entries_are_set_and_cleared_without_touching_dynamic_colors() {
    let mut overrides = ColorOverrides::default();
    overrides.set_dynamic(DynamicColor::Foreground, Some((9, 9, 9)));
    overrides.set_palette(0, Some((1, 1, 1)));
    overrides.set_palette(255, Some((2, 2, 2)));
    assert_eq!(overrides.palette(0), Some((1, 1, 1)));
    assert_eq!(overrides.palette(255), Some((2, 2, 2)));
    assert_eq!(overrides.palette(1), None);
    overrides.set_palette(0, None);
    assert_eq!(overrides.palette(0), None);
    let before_clear = overrides.generation();
    overrides.clear_palette();
    assert_eq!(overrides.palette(255), None);
    assert_ne!(overrides.generation(), before_clear);
    assert_eq!(overrides.dynamic(DynamicColor::Foreground), Some((9, 9, 9)));
    assert!(!overrides.is_empty());
    let after_clear = overrides.generation();
    overrides.clear_palette();
    assert_eq!(overrides.generation(), after_clear, "clearing an empty palette changes nothing");
}

/// Two stores never share a generation once either changed, so a renderer cache salted with it
/// cannot replay one pane's colors for another pane with different overrides.
#[test]
fn generations_are_unique_across_stores() {
    let mut first = ColorOverrides::default();
    let mut second = ColorOverrides::default();
    first.set_palette(3, Some((3, 3, 3)));
    second.set_palette(3, Some((4, 4, 4)));
    assert_ne!(first.generation(), second.generation());
}

/// The store reports empty again once every override is reset, so a pane returns to the
/// theme-only path.
#[test]
fn a_store_is_empty_again_after_every_reset() {
    let mut overrides = ColorOverrides::default();
    overrides.set_dynamic(DynamicColor::Cursor, Some((5, 5, 5)));
    overrides.set_palette(7, Some((6, 6, 6)));
    overrides.set_dynamic(DynamicColor::Cursor, None);
    overrides.set_palette(7, None);
    assert!(overrides.is_empty());
}

/// The selection background, selection foreground and cursor text are held like the other dynamic
/// colors: each set makes the store non-empty and moves the generation, a palette clear (RIS)
/// keeps them, and a reset removes them.
#[test]
fn selection_and_cursor_text_are_held_dynamic_colors() {
    for target in [
        DynamicColor::SelectionBackground,
        DynamicColor::SelectionForeground,
        DynamicColor::CursorText,
    ] {
        let mut overrides = ColorOverrides::default();
        let start = overrides.generation();
        overrides.set_dynamic(target, Some((9, 8, 7)));
        assert!(!overrides.is_empty(), "{target:?}");
        assert_ne!(overrides.generation(), start, "{target:?}");
        overrides.clear_palette();
        assert_eq!(overrides.dynamic(target), Some((9, 8, 7)), "{target:?} survives RIS");
        overrides.set_dynamic(target, None);
        assert!(overrides.is_empty(), "{target:?}");
    }
}
