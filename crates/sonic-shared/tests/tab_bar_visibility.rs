//! Issue #383 — regression guard: the rendered tab-bar background MUST
//! visibly contrast with `theme.background`. Pre-fix the tab bar was
//! filled with hardcoded `tok::BG_BASE()` (`#0B0E14`, near-black in
//! linear sRGB) which, for the default Tokyo-Night theme, was pixel-
//! indistinguishable from `theme.background` (`#1a1b26` → also dark
//! near-black after sRGB→linear). PR #391 confirmed via instrumentation
//! that 6 tab-bar quads were emitted every frame at the correct NDC
//! position with alpha 1.0, but the first quad's color was
//! `[0.0033, 0.0044, 0.0070, 1.0]` — same value the cell-grid clear
//! used, so the bar drew correctly yet disappeared.
//!
//! Fix: the tab-bar `bar_bg` now sources from
//! `UiPalette::from_theme(theme).bg_base` (= `theme.background` shifted
//! -8% lightness in sRGB-space HSL) so every theme automatically gets
//! a tab-bar color that differs visibly from the cell-grid bg.
//!
//! This test parameterizes across every bundled theme and asserts the
//! perceptual L1 distance in linear-sRGB between `ui_palette.bg_base`
//! (what the tab bar paints) and `hex(theme.background)` (what the
//! cell grid clears with) exceeds a minimum contrast threshold. It
//! FAILS if a future change reverts to `tok::BG_BASE()` or otherwise
//! reintroduces the camouflage.

use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::ui_tokens::{color as tok, UiPalette};

/// Minimum L1 distance (sum of |Δr|+|Δg|+|Δb| in linear sRGB) between
/// the tab-bar background and the cell-grid background. The bug case
/// (`bar_bg == cell_bg`) scores exactly 0.0; this threshold sits well
/// above that floor but low enough to accommodate the wezterm theme
/// whose background (`#141617`) lives at the very bottom of the sRGB
/// dynamic range, where an 8% HSL-lightness shift maps to only ~0.022
/// in linear space (the perceptual ratio is still ~25× — easily
/// visible on screen — but linear-space L1 collapses near zero).
const MIN_CONTRAST_L1: f32 = 0.005;

fn hex(s: &str) -> Hex {
    Hex(s.to_string())
}

/// Construct a minimal `Theme` from just `background` + `foreground` +
/// `tab.active_fg` (the only fields `UiPalette::from_theme` reads to
/// derive `bg_base` + the tab-bar accent). Other fields are set to
/// plausible-but-irrelevant values.
fn theme_from_bg(name: &str, background: &str, foreground: &str, accent: &str) -> Theme {
    let ansi = AnsiColors {
        black: hex("#000000"),
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
        name: name.into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(background),
            foreground: hex(foreground),
            cursor: hex(foreground),
            cursor_text: hex(background),
            selection_bg: hex("#3c3836"),
            selection_fg: hex(foreground),
            ansi,
            bright,
            tab: TabColors {
                bar_bg: hex(background),
                active_bg: hex("#3c3836"),
                active_fg: hex(accent),
                inactive_bg: hex(background),
                inactive_fg: hex("#a89984"),
                hover_bg: hex("#3c3836"),
                hover_fg: hex("#d5c4a1"),
                close_button_fg: hex("#a89984"),
            },
        },
    }
}

/// Returns the L1 distance |Δr|+|Δg|+|Δb| (alpha ignored) between two
/// linear-sRGB premultiplied colors as produced by `UiPalette` / `hex`.
fn linear_l1(a: [f32; 4], b: [f32; 4]) -> f32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

/// Every bundled theme — `assets/themes/*.toml` — must produce a
/// tab-bar background that visibly contrasts with its cell-grid
/// background. Drives the #383 fix into a per-theme regression.
#[test]
fn tab_bar_background_visibly_contrasts_with_theme_background() {
    // (name, background hex, foreground hex, tab.active_fg hex)
    // Sourced from `assets/themes/*.toml`. Foreground / accent are
    // taken from the same files so the synthetic Theme produces
    // realistic palette values (`UiPalette::from_theme` reads
    // foreground for hover/border alpha overlays and accent for the
    // theme-driven highlight).
    let bundled = [
        ("tokyo-night", "#1a1b26", "#a9b1d6", "#7aa2f7"),
        ("dracula", "#282a36", "#f8f8f2", "#ff79c6"),
        ("nord", "#2e3440", "#d8dee9", "#88c0d0"),
        ("catppuccin-mocha", "#1e1e2e", "#cdd6f4", "#89b4fa"),
        ("gruvbox-dark-hard", "#1d2021", "#ebdbb2", "#fabd2f"),
        ("wezterm", "#141617", "#b4b6b4", "#7aa2f7"),
        ("monokai-pro", "#2d2a2e", "#fcfcfa", "#ffd866"),
        ("one-dark", "#282c34", "#abb2bf", "#61afef"),
        ("solarized-dark", "#002b36", "#839496", "#268bd2"),
    ];

    for (name, bg_hex, fg_hex, accent_hex) in bundled {
        let theme = theme_from_bg(name, bg_hex, fg_hex, accent_hex);
        let ui = UiPalette::from_theme(&theme);

        // Cell-grid clear color (the surface clears with this — see
        // `render::core` clear path). Tab-bar bg must NOT match it.
        let cell_bg = tok::hex(bg_hex);

        let d = linear_l1(ui.bg_base, cell_bg);
        assert!(
            d > MIN_CONTRAST_L1,
            "theme {name}: tab-bar bg {:?} too close to cell-grid bg {:?} (L1={:.4}, need > {MIN_CONTRAST_L1}). \
             This is the #383 invisibility regression.",
            ui.bg_base,
            cell_bg,
            d
        );
    }
}

/// Static guard: `render::core::core::render`'s tab-bar emit block
/// must source `bar_bg` from `ui_palette.bg_base`, not from the
/// deprecated `tok::BG_BASE()` (which was hardcoded near-black).
/// Mirrors the wiring-check pattern from
/// `palette_chrome_follows_active_theme.rs`.
#[test]
fn tab_bar_render_path_uses_theme_derived_bar_bg() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/render/core.rs"))
        .expect("read render/core.rs");

    let start = src.find("// -------- Tab bar").expect("tab bar block present");
    let end_off = src[start..]
        .find("// -------- Search highlights")
        .expect("tab bar block ends before Search highlights overlay");
    let block = &src[start..start + end_off];

    assert!(
        block.contains("ui_palette.bg_base"),
        "tab bar block must source bar_bg from ui_palette.bg_base (the #383 fix); \
         got block of {} bytes without it",
        block.len()
    );
    assert!(
        !block.contains("let bar_bg = tok::BG_BASE();"),
        "tab bar block must NOT reintroduce hardcoded tok::BG_BASE() for bar_bg \
         (the #383 invisibility regression)"
    );
}
