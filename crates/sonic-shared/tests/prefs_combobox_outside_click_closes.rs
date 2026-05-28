//! Regression for issue #173 slice-2b (Combobox wire-up):
//!
//! Once a `Combobox` popover is open, clicking anywhere outside both
//! the header rect and the option-row strip must close it. Before this
//! slice the popover stayed open forever — there was no outside-click
//! dismiss path at all, so once a user toggled a dropdown open they
//! could not get it back without selecting an item.
//!
//! Pins `PrefsState::close_dropdowns_outside_click` directly so the
//! invariant survives any future refactor of the host mouse handler.

use std::path::PathBuf;

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::prefs::layout::Category;
use sonic_shared::prefs::{Control, PrefsState};

fn test_theme() -> Theme {
    let h = || Hex("#7aa2f7".to_string());
    let ansi = || AnsiColors {
        black: h(),
        red: h(),
        green: h(),
        yellow: h(),
        blue: h(),
        magenta: h(),
        cyan: h(),
        white: h(),
    };
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: Hex("#1d2021".to_string()),
            foreground: Hex("#ebdbb2".to_string()),
            cursor: h(),
            cursor_text: h(),
            selection_bg: h(),
            selection_fg: h(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: h(),
                active_bg: h(),
                active_fg: h(),
                inactive_bg: h(),
                inactive_fg: h(),
                hover_bg: h(),
                hover_fg: h(),
                close_button_fg: h(),
            },
        },
    }
}

fn first_dropdown_id(s: &PrefsState) -> sonic_shared::prefs::WidgetId {
    s.controls
        .iter()
        .find_map(|c| if let Control::Dropdown(d) = c { Some(d.id) } else { None })
        .expect("Appearance category exposes a theme Dropdown")
}

#[test]
fn outside_click_far_away_closes_open_dropdown() {
    let theme = test_theme();
    let mut s =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    // Appearance category is the canonical one with a theme combobox.
    s.set_category(Category::Appearance);
    let id = first_dropdown_id(&s);

    // Open the combobox.
    assert_eq!(s.toggle_dropdown(id), Some(true));
    let header_rect = match s.controls.iter().find(|c| c.id() == id).unwrap() {
        Control::Dropdown(d) => d.rect,
        _ => unreachable!(),
    };

    // Click somewhere obviously outside both header and popover (the
    // top-left corner of the prefs window — sidebar area is at y ≈ 24,
    // x ≈ 28, far from any dropdown row).
    let closed = s.close_dropdowns_outside_click(2.0, 2.0);
    assert!(closed, "click outside the combobox should close it");

    match s.controls.iter().find(|c| c.id() == id).unwrap() {
        Control::Dropdown(d) => assert!(!d.open, "dropdown should be closed after outside click"),
        _ => unreachable!(),
    }

    // Sanity: a click inside the header rect does NOT close it (that
    // path is the "toggle" case handled by `toggle_dropdown`, not the
    // outside-click dismiss path).
    s.toggle_dropdown(id);
    let inside_x = header_rect.x + header_rect.w / 2.0;
    let inside_y = header_rect.y + header_rect.h / 2.0;
    let closed2 = s.close_dropdowns_outside_click(inside_x, inside_y);
    assert!(!closed2, "click on the header itself is not an outside click");
    match s.controls.iter().find(|c| c.id() == id).unwrap() {
        Control::Dropdown(d) => assert!(d.open, "dropdown still open after click on its header"),
        _ => unreachable!(),
    }
}

#[test]
fn outside_click_on_option_row_does_not_close() {
    // The popover's option rows are NOT "outside" — clicking one is a
    // selection, dispatched separately by classify_click. The helper
    // must therefore return false for clicks inside an option row.
    let theme = test_theme();
    let mut s =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    s.set_category(Category::Appearance);
    let id = first_dropdown_id(&s);
    s.toggle_dropdown(id);

    let (rx, ry, rw, rh) = match s.controls.iter().find(|c| c.id() == id).unwrap() {
        Control::Dropdown(d) => (d.rect.x, d.rect.y, d.rect.w, d.rect.h),
        _ => unreachable!(),
    };
    // Click in the center of the first option row (one row below the header).
    let opt_x = rx + rw / 2.0;
    let opt_y = ry + rh + rh / 2.0;
    let closed = s.close_dropdowns_outside_click(opt_x, opt_y);
    assert!(!closed, "click on option row is not an outside click");
}

#[test]
fn close_dropdowns_outside_click_is_noop_when_none_open() {
    // No dropdown open → nothing to close → returns false and does not panic.
    let theme = test_theme();
    let mut s =
        PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic-test.toml"), theme.clone());
    s.set_category(Category::Appearance);
    assert!(!s.close_dropdowns_outside_click(2.0, 2.0));
}
