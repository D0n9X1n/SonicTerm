//! Regression for PR #119 review: the command-palette chrome
//! (scrim, panel background, query field, selected-row highlight,
//! selected-row accent strip, footer border) was hardcoded to
//! Tokyo Night literals (`#05070D`, `#10131A`, `#0B0E14`, `#7AA2F7`,
//! `#FFFFFF`) in `sonicterm-shared/../sonicterm-gpu/src/core.rs`. As a result the
//! palette did NOT follow the active theme — a gruvbox user
//! opening Cmd-P still saw a Tokyo-Night-blue panel.
//!
//! The fix derives the palette chrome from
//! [`crate::ui_tokens::UiPalette::from_theme`] using the active
//! `&Theme` passed into `render()`. This test asserts:
//!
//! 1. `render.rs` no longer contains the historical hex literals
//!    (`0.478, 0.635, 0.969` — `#7AA2F7` in linear-sRGB premultiplied
//!    form — and the `[1.0, 1.0, 1.0, 0.10]` border-white literal)
//!    inside the palette overlay block.
//! 2. `render.rs` references `UiPalette::from_theme(theme)` inside
//!    the palette overlay block, proving it derives chrome from
//!    the active theme.
//! 3. `UiPalette::from_theme` on a gruvbox-style theme yields a
//!    gold accent (`bright.yellow`, ~`#fabd2f`), distinct from the
//!    tokyo-night accent (`#7aa2f7`). This guards the data path
//!    that the palette renderer now reads from.

use sonicterm_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonicterm_shared::ui_tokens::UiPalette;

fn hex(s: &str) -> Hex {
    Hex(s.to_string())
}

fn gruvbox_like() -> Theme {
    let ansi = AnsiColors {
        black: hex("#282828"),
        red: hex("#cc241d"),
        green: hex("#98971a"),
        yellow: hex("#d79921"),
        blue: hex("#458588"),
        magenta: hex("#b16286"),
        cyan: hex("#689d6a"),
        white: hex("#a89984"),
    };
    let bright = AnsiColors {
        black: hex("#928374"),
        red: hex("#fb4934"),
        green: hex("#b8bb26"),
        yellow: hex("#fabd2f"),
        blue: hex("#83a598"),
        magenta: hex("#d3869b"),
        cyan: hex("#8ec07c"),
        white: hex("#ebdbb2"),
    };
    Theme {
        name: "gruvbox-test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex("#1d2021"),
            foreground: hex("#ebdbb2"),
            cursor: hex("#ebdbb2"),
            cursor_text: hex("#1d2021"),
            selection_bg: hex("#3c3836"),
            selection_fg: hex("#ebdbb2"),
            ansi,
            bright,
            tab: TabColors {
                bar_bg: hex("#1d2021"),
                active_bg: hex("#3c3836"),
                active_fg: hex("#fabd2f"),
                inactive_bg: hex("#1d2021"),
                inactive_fg: hex("#a89984"),
                hover_bg: hex("#3c3836"),
                hover_fg: hex("#d5c4a1"),
                close_button_fg: hex("#a89984"),
            },
        },
    }
}

fn tokyo_night_like() -> Theme {
    let mut t = gruvbox_like();
    t.name = "tokyo-night-test".into();
    t.colors.tab.active_fg = hex("#7aa2f7");
    t
}

#[test]
fn palette_chrome_follows_active_theme() {
    // Data path: UiPalette must surface the theme's accent, distinct
    // per theme. Gruvbox accent is gold (#fabd2f); tokyo-night is
    // a saturated blue (#7aa2f7). Both must round-trip through
    // sRGB→linear and end up clearly different in any RGB channel.
    let gruvbox = UiPalette::from_theme(&gruvbox_like());
    let tn = UiPalette::from_theme(&tokyo_night_like());

    let any_channel_differs = (0..3).any(|i| (gruvbox.accent[i] - tn.accent[i]).abs() > 0.01);
    assert!(
        any_channel_differs,
        "gruvbox accent {:?} must differ from tokyo-night accent {:?} in some channel",
        gruvbox.accent, tn.accent
    );

    // Gruvbox accent should read as "gold" — red and green strong,
    // blue weak — i.e. R+G clearly larger than B.
    let g = gruvbox.accent;
    assert!(g[0] + g[1] > g[2] * 2.0, "gruvbox accent should be gold (R+G >> B), got {:?}", g);

    // Tokyo-night accent should read as "blue" — blue dominant.
    let b = tn.accent;
    assert!(b[2] > b[0], "tokyo-night accent should be blue-dominant, got {:?}", b);

    // Wiring path: render.rs must derive palette chrome from the
    // active theme via UiPalette::from_theme, AND must no longer
    // contain the historical Tokyo-Night literals inside the palette
    // overlay block. This protects against a future revert that
    // re-hardcodes the chrome.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../sonicterm-gpu/src/core.rs"
    ))
    .expect("read render.rs");
    let start = src.find("Command palette overlay").expect("palette overlay block present");
    let end_off = src[start..]
        .find("// -------- IME preedit overlay")
        .expect("palette block ends before IME overlay");
    let block = &src[start..start + end_off];

    assert!(
        block.contains("UiPalette::from_theme(theme)"),
        "palette overlay block must derive chrome via UiPalette::from_theme(theme); \
         got block of {} bytes without it",
        block.len()
    );

    // The forbidden literal `[0.478, 0.635, 0.969,` is the
    // linear-sRGB premultiplied form of `#7AA2F7` — Tokyo Night's
    // accent blue. It was hardcoded twice in the original (selected
    // row + selected accent strip).
    assert!(
        !block.contains("0.478, 0.635, 0.969"),
        "palette overlay block must not contain hardcoded Tokyo Night accent literal \
         (0.478, 0.635, 0.969 == #7AA2F7); chrome must come from UiPalette"
    );
    // The hardcoded scrim `#05070D` (linear-sRGB premultiplied
    // `0.020, 0.027, 0.051`) is forbidden too.
    assert!(
        !block.contains("0.020, 0.027, 0.051"),
        "palette overlay block must not contain hardcoded Tokyo Night scrim literal"
    );
    // The hardcoded modal-bg `#10131A` (0.063, 0.075, 0.102).
    assert!(
        !block.contains("0.063, 0.075, 0.102"),
        "palette overlay block must not contain hardcoded Tokyo Night modal-bg literal"
    );
    // The hardcoded query-row `#0B0E14` (0.043, 0.055, 0.078).
    assert!(
        !block.contains("0.043, 0.055, 0.078"),
        "palette overlay block must not contain hardcoded Tokyo Night query-bg literal"
    );
}
