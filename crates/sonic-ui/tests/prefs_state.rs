//! Integration tests for `sonic_ui::prefs::state`.

use std::path::PathBuf;

use sonic_cfg::{
    config::{Config, CursorShape},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use sonic_ui::prefs::state::{KNOWN_CURSOR_SHAPES, KNOWN_FONTS, KNOWN_THEMES};
use sonic_ui::prefs::{Category, ColorSwatch, Control, PrefsHit, PrefsState, CATEGORIES};
use tempfile::TempDir;

fn test_theme() -> Theme {
    let h = |s: &str| Hex(s.to_string());
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: h("#1d2021"),
            foreground: h("#ebdbb2"),
            cursor: h("#ebdbb2"),
            cursor_text: h("#1d2021"),
            selection_bg: h("#3c3836"),
            selection_fg: h("#ebdbb2"),
            ansi: AnsiColors {
                black: h("#000000"),
                red: h("#cc241d"),
                green: h("#98971a"),
                yellow: h("#d79921"),
                blue: h("#458588"),
                magenta: h("#b16286"),
                cyan: h("#689d6a"),
                white: h("#a89984"),
            },
            bright: AnsiColors {
                black: h("#928374"),
                red: h("#fb4934"),
                green: h("#b8bb26"),
                yellow: h("#fabd2f"),
                blue: h("#83a598"),
                magenta: h("#d3869b"),
                cyan: h("#8ec07c"),
                white: h("#ebdbb2"),
            },
            tab: TabColors {
                bar_bg: h("#1d2021"),
                active_bg: h("#3c3836"),
                active_fg: h("#fabd2f"),
                inactive_bg: h("#1d2021"),
                inactive_fg: h("#a89984"),
                hover_bg: h("#3c3836"),
                hover_fg: h("#d5c4a1"),
                close_button_fg: h("#a89984"),
            },
        },
    }
}

fn fresh() -> (PrefsState, TempDir) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sonic.toml");
    let state = PrefsState::new(Config::default(), path, test_theme());
    (state, dir)
}

#[test]
fn new_state_is_clean_and_has_font_controls() {
    let (s, _d) = fresh();
    assert!(!s.is_dirty());
    assert_eq!(s.active_category, Category::Font);
    assert_eq!(s.controls.len(), 3);
}

#[test]
fn switching_category_rebuilds_controls() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Theme);
    assert_eq!(s.controls.len(), 2);
    s.set_category(Category::Window);
    assert_eq!(s.controls.len(), 4);
    s.set_category(Category::Cursor);
    assert_eq!(s.controls.len(), 2);
    s.set_category(Category::Advanced);
    assert_eq!(s.controls.len(), 3);
}

#[test]
fn every_category_has_controls() {
    let (mut s, _d) = fresh();
    for cat in CATEGORIES {
        s.set_category(*cat);
        assert!(!s.controls.is_empty(), "{} has no controls", cat.label());
    }
}

#[test]
fn flip_window_toggle_dirties_and_writes_through() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Window);
    let toggle_id = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Toggle(t) if t.label == "Background blur" => Some(t.id),
            _ => None,
        })
        .unwrap();
    let before = s.config.window.blur;
    s.flip_toggle(toggle_id);
    assert_ne!(s.config.window.blur, before);
    assert!(s.is_dirty());
}

#[test]
fn drag_advanced_slider_updates_scrollback() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Advanced);
    let (id, rect_x, rect_w) = match &s.controls[1] {
        Control::Slider(sl) => (sl.id, sl.rect.x, sl.rect.w),
        other => panic!("expected scrollback slider, got {other:?}"),
    };
    s.drag_slider(id, rect_x + rect_w);
    assert_eq!(s.config.terminal.scrollback, 100_000);
    assert!(s.is_dirty());
}

#[test]
fn select_theme_dropdown_updates_string_field() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Theme);
    let id = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Dropdown(d) => Some(d.id),
            _ => None,
        })
        .unwrap();
    s.select_dropdown(id, 1);
    assert_ne!(s.config.theme, Config::default().theme);
    assert!(s.is_dirty());
}

#[test]
fn cancel_restores_snapshot_and_clears_dirty() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Theme);
    let id = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Dropdown(d) => Some(d.id),
            _ => None,
        })
        .unwrap();
    let before = s.config.theme.clone();
    s.select_dropdown(id, 1);
    assert_ne!(s.config.theme, before);
    s.cancel();
    assert_eq!(s.config.theme, before);
    assert!(!s.is_dirty());
}

#[test]
fn apply_writes_toml_and_clears_dirty() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Window);
    let tid = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Toggle(t) => Some(t.id),
            _ => None,
        })
        .unwrap();
    s.flip_toggle(tid);
    assert!(s.is_dirty());
    s.apply().unwrap();
    assert!(!s.is_dirty());
    let text = std::fs::read_to_string(&s.config_path).unwrap();
    assert!(text.contains("[window]"));
    let v = s.config.window.blur;
    s.cancel();
    assert_eq!(s.config.window.blur, v);
}

#[test]
fn hit_apply_cancel_and_reset_use_layout_rects() {
    let (s, _d) = fresh();
    let apply = s.layout.apply_button;
    let cancel = s.layout.cancel_button;
    let reset = s.layout.reset_link;
    assert!(s.hit_apply(apply.x + 1.0, apply.y + 1.0));
    assert!(!s.hit_apply(0.0, 0.0));
    assert!(s.hit_cancel(cancel.x + 1.0, cancel.y + 1.0));
    assert!(!s.hit_cancel(apply.x + 1.0, apply.y + 1.0));
    assert!(s.hit_reset(reset.x + 1.0, reset.y + 1.0));
}

#[test]
fn classify_click_resolves_reset_link() {
    let (s, _d) = fresh();
    let r = s.layout.reset_link;
    assert_eq!(s.classify_click(r.x + 1.0, r.y + 1.0), Some(PrefsHit::ResetSection));
}

#[test]
fn hit_sidebar_returns_category() {
    let (s, _d) = fresh();
    let row = s.layout.category_row(1);
    assert_eq!(s.hit_sidebar(row.x + 1.0, row.y + 1.0), Some(Category::Theme));
    assert_eq!(s.hit_sidebar(9999.0, 9999.0), None);
}

#[test]
fn preview_lines_nonempty() {
    let (s, _d) = fresh();
    assert!(!s.preview_lines().is_empty());
}

#[test]
fn hit_test_finds_widget_by_position() {
    let (s, _d) = fresh();
    let first = &s.controls[0];
    let r = match first {
        Control::Toggle(t) => t.rect,
        Control::Slider(sl) => sl.rect,
        Control::Dropdown(d) => d.rect,
        Control::ColorSwatch(c) => c.rect,
        Control::TextField(tf) => tf.rect,
    };
    assert_eq!(s.hit_test(r.x + 1.0, r.y + 1.0), Some(first.id()));
}

#[test]
fn type_into_text_field_writes_through_to_shell() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Advanced);
    let id = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::TextField(tf) => Some(tf.id),
            _ => None,
        })
        .unwrap();
    for ch in "/bin/zsh".chars() {
        s.type_into(id, ch);
    }
    assert_eq!(s.config.terminal.shell.as_deref(), Some("/bin/zsh"));
    assert!(s.is_dirty());
}

#[test]
fn reselecting_current_dropdown_option_is_not_dirty() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Theme);
    let (id, current) = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Dropdown(d) => Some((d.id, d.selected)),
            _ => None,
        })
        .unwrap();
    s.select_dropdown(id, current);
    assert!(!s.is_dirty());
}

#[test]
fn dragging_slider_to_current_value_is_not_dirty() {
    let (mut s, _d) = fresh();
    let (id, x_at_current) = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Slider(sl) => {
                let frac = sl.fraction();
                let x = sl.rect.x + frac * sl.rect.w;
                Some((sl.id, x))
            }
            _ => None,
        })
        .unwrap();
    s.drag_slider(id, x_at_current);
    assert!(!s.is_dirty());
}

#[test]
fn typing_into_textfield_at_max_len_is_not_dirty() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Advanced);
    let id = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::TextField(tf) => Some(tf.id),
            _ => None,
        })
        .unwrap();
    let max_len = match s.controls.iter_mut().find(|c| c.id() == id).unwrap() {
        Control::TextField(tf) => {
            let m = tf.max_len;
            tf.value = "x".repeat(m);
            m
        }
        _ => unreachable!(),
    };
    s.dirty = false;
    s.type_into(id, 'y');
    assert!(!s.is_dirty());
    if let Control::TextField(tf) = s.controls.iter().find(|c| c.id() == id).unwrap() {
        assert_eq!(tf.value.chars().count(), max_len);
    }
}

#[test]
fn apply_writes_atomically_to_nested_missing_dir() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("a/b/c/sonic.toml");
    let mut s = PrefsState::new(Config::default(), nested.clone(), test_theme());
    s.set_category(Category::Window);
    let tid = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Toggle(t) => Some(t.id),
            _ => None,
        })
        .unwrap();
    s.flip_toggle(tid);
    s.apply().unwrap();
    assert!(nested.exists());
    let text = std::fs::read_to_string(&nested).unwrap();
    assert!(text.contains("[window]"));
    let mut tmp = nested.clone();
    tmp.set_file_name("sonic.toml.tmp");
    assert!(!tmp.exists());
}

#[test]
fn classify_click_resolves_dropdown_option_when_open() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Theme);
    let id = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Dropdown(d) => Some(d.id),
            _ => None,
        })
        .unwrap();
    s.toggle_dropdown(id);
    let (rx, ry, rw, rh) = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::Dropdown(d) if d.id == id => Some((d.rect.x, d.rect.y, d.rect.w, d.rect.h)),
            _ => None,
        })
        .unwrap();
    let hit = s.classify_click(rx + rw / 2.0, ry + rh + rh + rh / 2.0);
    assert!(matches!(hit, Some(PrefsHit::DropdownOption { .. })));
}

#[test]
fn classify_click_resolves_color_swatch_cell() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Theme);
    let (id, top, left) = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::ColorSwatch(cs) => Some((cs.id, cs.rect.y + cs.rect.h + 4.0, cs.rect.x)),
            _ => None,
        })
        .unwrap();
    let hit = s.classify_click(left + ColorSwatch::CELL * 1.5, top + ColorSwatch::CELL * 0.5);
    match hit {
        Some(PrefsHit::ColorCell { id: hid, index }) => {
            assert_eq!(hid, id);
            assert_eq!(index, 1);
            assert!(s.pick_color(hid, index).unwrap());
            assert!(s.is_dirty());
        }
        other => panic!("expected ColorCell, got {other:?}"),
    }
}

#[test]
fn focus_text_field_then_type_writes_through() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Advanced);
    let id = s
        .controls
        .iter()
        .find_map(|c| match c {
            Control::TextField(tf) => Some(tf.id),
            _ => None,
        })
        .unwrap();
    assert!(s.focus_text_field(id));
    assert_eq!(s.focused_field, Some(id));
    assert!(s.type_into_focused('z'));
    assert!(s.type_into_focused('s'));
    assert!(s.type_into_focused('h'));
    assert!(s.config.terminal.shell.as_deref().unwrap().ends_with("zsh"));
    assert!(s.is_dirty());
    s.blur_text_fields();
    assert!(!s.type_into_focused('x'));
}

#[test]
fn toggle_blink_then_apply_writes_blink_false_to_disk() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Cursor);
    let id = match &s.controls[1] {
        Control::Toggle(t) => t.id,
        other => panic!("expected Toggle as second Cursor control, got {other:?}"),
    };
    assert!(s.config.terminal.cursor_blink);
    s.flip_toggle(id);
    assert!(!s.config.terminal.cursor_blink);
    s.apply().unwrap();
    let text = std::fs::read_to_string(&s.config_path).unwrap();
    assert!(text.contains("cursor_blink = false"), "missing blink=false in {text}");
}

#[test]
fn select_cursor_shape_then_apply_writes_to_disk() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Cursor);
    let id = match &s.controls[0] {
        Control::Dropdown(d) => d.id,
        other => panic!("expected cursor dropdown, got {other:?}"),
    };
    let bar_idx = KNOWN_CURSOR_SHAPES.iter().position(|s| *s == "bar").unwrap();
    s.select_dropdown(id, bar_idx);
    assert_eq!(s.config.terminal.cursor_shape, CursorShape::Bar);
    s.apply().unwrap();
    let text = std::fs::read_to_string(&s.config_path).unwrap();
    assert!(text.contains("cursor_shape = \"bar\""), "missing bar in {text}");
}

#[test]
fn theme_keymap_font_opacity_scrollback_all_reach_disk() {
    let (mut s, _d) = fresh();
    s.set_category(Category::Theme);
    let theme_id = match &s.controls[0] {
        Control::Dropdown(d) => d.id,
        _ => unreachable!(),
    };
    s.select_dropdown(theme_id, 1);

    s.set_category(Category::Window);
    let (op_id, rect_x) = match &s.controls[0] {
        Control::Slider(sl) => (sl.id, sl.rect.x),
        _ => unreachable!(),
    };
    s.drag_slider(op_id, rect_x);

    s.set_category(Category::Font);
    let font_id = match &s.controls[0] {
        Control::Dropdown(d) => d.id,
        _ => unreachable!(),
    };
    s.select_dropdown(font_id, 1);
    let (size_id, sx, sw) = match &s.controls[1] {
        Control::Slider(sl) => (sl.id, sl.rect.x, sl.rect.w),
        _ => unreachable!(),
    };
    s.drag_slider(size_id, sx + sw);

    s.set_category(Category::Advanced);
    let (sb_id, sbx, sbw) = match &s.controls[1] {
        Control::Slider(sl) => (sl.id, sl.rect.x, sl.rect.w),
        _ => unreachable!(),
    };
    s.drag_slider(sb_id, sbx + sbw);
    s.set_category(Category::Keymap);
    s.apply().unwrap();

    let cfg = Config::load_or_default(&s.config_path).unwrap();
    assert_eq!(cfg.theme, KNOWN_THEMES[1]);
    assert_eq!(cfg.keymap, "wezterm");
    assert_eq!(cfg.font.family, KNOWN_FONTS[1]);
    assert!((cfg.font.size - 32.0).abs() < 1e-3);
    assert!((cfg.window.opacity - 0.3).abs() < 1e-3);
    assert_eq!(cfg.terminal.scrollback, 100_000);
}

#[test]
fn reset_font_section_restores_defaults() {
    let (mut s, _d) = fresh();
    s.config.font.family = "Fira Code".into();
    s.config.font.size = 22.0;
    s.config.font.line_height = 1.5;
    s.set_category(Category::Font);
    s.reset_active_section_to_default();
    assert_eq!(s.config.font, Config::default().font);
    assert!(s.is_dirty());
}

#[test]
fn reset_each_section_only_restores_that_subset() {
    let (mut s, _d) = fresh();
    s.config.font.size = 20.0;
    s.config.theme = "dracula".into();
    s.config.keymap = "custom".into();
    s.config.window.opacity = 0.5;
    s.config.terminal.cursor_blink = false;
    s.config.terminal.cursor_shape = CursorShape::Underline;
    s.config.terminal.shell = Some("pwsh".into());
    s.config.terminal.scrollback = 42_000;
    s.config.locale = "ja".into();

    s.set_category(Category::Theme);
    s.reset_active_section_to_default();
    assert_eq!(s.config.theme, Config::default().theme);
    assert_eq!(s.config.font.size, 20.0);

    s.set_category(Category::Keymap);
    s.reset_active_section_to_default();
    assert_eq!(s.config.keymap, Config::default().keymap);
    assert!((s.config.window.opacity - 0.5).abs() < f32::EPSILON);

    s.set_category(Category::Window);
    s.reset_active_section_to_default();
    assert_eq!(s.config.window, Config::default().window);
    assert!(!s.config.terminal.cursor_blink);

    s.set_category(Category::Cursor);
    s.reset_active_section_to_default();
    assert_eq!(s.config.terminal.cursor_blink, Config::default().terminal.cursor_blink);
    assert_eq!(s.config.terminal.cursor_shape, Config::default().terminal.cursor_shape);
    assert_eq!(s.config.terminal.shell.as_deref(), Some("pwsh"));

    s.set_category(Category::Advanced);
    s.reset_active_section_to_default();
    assert_eq!(s.config.terminal.shell, Config::default().terminal.shell);
    assert_eq!(s.config.terminal.scrollback, Config::default().terminal.scrollback);
    assert_eq!(s.config.locale, Config::default().locale);
}

#[test]
fn apply_uses_config_save_atomic_no_tmp_left_behind() {
    let (mut s, _d) = fresh();
    s.dirty = true;
    s.apply().unwrap();
    let mut tmp = s.config_path.clone();
    tmp.set_file_name("sonic.toml.tmp");
    assert!(!tmp.exists());
    assert!(s.config_path.exists());
}

#[test]
fn temporary_path_constructor_compiles_on_windows() {
    let _ = PathBuf::from("sonic-test.toml");
}
