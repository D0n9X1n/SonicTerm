
use super::*;
use sonicterm_cfg::keymap::{ActionWrapper, Binding, Keymap, Meta};

#[test]
fn palette_defaults_do_not_expose_placeholder_parameter_actions() {
    let actions = palette_actions();
    assert!(!actions.iter().any(|a| matches!(a, Action::ApplyTheme(_))));
    assert!(!actions.iter().any(|a| matches!(a, Action::OpenSshPane(_))));
    assert!(actions.iter().any(|a| matches!(a, Action::OpenCommandPalette)));
    assert!(actions.iter().any(|a| matches!(a, Action::UpdateTabColor)));
    assert!(actions
        .iter()
        .any(|a| { matches!(a, Action::ResizePane { dir: Direction::Left, amount: 5 }) }));
    assert!(covers_every_variant_kind());
}

#[test]
fn palette_imports_concrete_keymap_theme_actions_and_shortcuts() {
    let keymap = Keymap {
        meta: Meta { name: "test".into(), version: "1.0".into() },
        bindings: vec![
            Binding {
                keys: "super+shift+y".into(),
                action: ActionWrapper(Action::ApplyTheme("wezterm".into())),
            },
            Binding {
                keys: "super+shift+s".into(),
                action: ActionWrapper(Action::OpenSshPane("alice@example.com".into())),
            },
        ],
    };
    let mut palette = CommandPalette::new();
    palette.set_keymap(&keymap);
    let visible = palette.visible();
    let theme_idx = visible
        .iter()
        .position(|a| matches!(a, Action::ApplyTheme(name) if name == "wezterm"))
        .expect("concrete keymap theme action should be visible");
    assert_eq!(palette.shortcut_hint_for_visible_index(theme_idx), Some("⌘⇧Y"));
    assert!(!visible.iter().any(|a| matches!(a, Action::OpenSshPane(_))));
}

#[test]
fn palette_hides_activate_tab_entries_beyond_current_tab_count() {
    let keymap = Keymap {
        meta: Meta { name: "test".into(), version: "1.0".into() },
        bindings: vec![
            Binding { keys: "super+1".into(), action: ActionWrapper(Action::ActivateTab(0)) },
            Binding { keys: "super+2".into(), action: ActionWrapper(Action::ActivateTab(1)) },
            Binding { keys: "super+3".into(), action: ActionWrapper(Action::ActivateTab(2)) },
        ],
    };
    let mut palette = CommandPalette::new();
    palette.set_keymap(&keymap);
    palette.set_tab_count(2);
    let visible = palette.visible();
    assert!(visible.iter().any(|a| matches!(a, Action::ActivateTab(0))));
    assert!(visible.iter().any(|a| matches!(a, Action::ActivateTab(1))));
    assert!(!visible.iter().any(|a| matches!(a, Action::ActivateTab(2))));
}

#[test]
fn palette_query_height_scales_on_large_window() {
    use crate::overlays::PaletteLayout;
    // Huge window so the window-relative clamps never bind; only the
    // SIZE terms drive the layout, so a pure SIZE field (the query-row
    // height) must double at 2x. panel_padding is held at 0 so it does
    // not enter this assertion.
    let mut palette = CommandPalette::new();
    palette.open();
    let one = PaletteLayout::compute(&mut palette, 4000.0, 2400.0, 0.0, 1.0)
        .expect("open palette yields a layout");
    let two = PaletteLayout::compute(&mut palette, 4000.0, 2400.0, 0.0, 2.0)
        .expect("open palette yields a layout");
    assert_eq!(two.query_row.h, one.query_row.h * 2.0);
}

#[test]
fn palette_text_editing_supports_space_cjk_and_caret_movement() {
    let mut palette = CommandPalette::new();
    palette.open();
    for ch in "rename".chars() {
        palette.input_char(ch);
    }
    palette.input_char(' ');
    palette.input_char('标');
    palette.input_char('题');
    assert_eq!(palette.query(), "rename 标题");
    assert_eq!(palette.cursor(), "rename 标题".len());

    palette.move_cursor_left();
    palette.move_cursor_left();
    palette.input_char('-');
    assert_eq!(palette.query(), "rename -标题");
    palette.backspace();
    assert_eq!(palette.query(), "rename 标题");
}

#[test]
fn tab_color_picker_exposes_selected_choice() {
    let mut palette = CommandPalette::new();
    palette.start_tab_color_picker(
        "#1 work",
        vec![
            TabColorChoice { name: "Reset to Default".into(), hex: None },
            TabColorChoice { name: "ANSI Red".into(), hex: Some("#fb4934".into()) },
            TabColorChoice { name: "ANSI Blue".into(), hex: Some("#83a598".into()) },
        ],
    );

    assert_eq!(palette.mode(), CommandPaletteMode::TabColor);
    assert_eq!(palette.tab_color_title(), "#1 work");
    assert_eq!(palette.len(), 3);
    assert_eq!(palette.selected_tab_color().map(|c| c.hex.as_deref()), Some(None));
    palette.move_selection_down();
    assert_eq!(palette.selected_tab_color().and_then(|c| c.hex.as_deref()), Some("#fb4934"));
    palette.move_selection_down();
    assert_eq!(palette.selected_tab_color().map(|c| c.name.as_str()), Some("ANSI Blue"));
}
