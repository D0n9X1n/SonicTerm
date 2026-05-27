//! Regression tests for 5 user-reported bugs in Sonic.
//!
//! These tests pair with a forthcoming fix PR (TBD). Some of them are
//! expected to FAIL on `main` today — that is intentional TDD: they go
//! red against the current bug, then green once the fix lands.
//!
//! Bugs covered:
//! 1. Command palette selection highlight must vertically enclose the
//!    selected row's text region.
//! 2. Preferences window must render content on the FIRST frame (not
//!    blank-until-click). Strengthens the existing
//!    `prefs_first_frame_has_nonzero_draw_commands` test.
//! 3. Preferences window must not freeze after rapid sidebar/toggle
//!    clicks (no deadlock). Implemented as a source-lint test that
//!    forbids unconditional `mutex.lock()` calls inside the prefs
//!    event handlers — they must use `try_lock` or no lock at all.
//! 4. Tab title font must equal the configured font family.
//! 5. Global font is user-configurable via `sonic.toml [font] family`.
//!    (Lives in `sonic-core/tests/user_font_regression.rs`.)

use glyphon::{Attrs, Color as GColor, Family};
use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::command_palette::CommandPalette;
use sonic_shared::overlays::{PaletteLayout, PALETTE_ROW_HEIGHT};
use sonic_shared::prefs::PrefsState;
use sonic_shared::prefs_renderer::build_draw_list;
use sonic_shared::render::{build_tab_title_rich_text_spans, build_tab_title_spans, TabSpanInput};
use std::path::PathBuf;

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

// =====================================================================
// Bug 1 — palette highlight must vertically enclose selected row text
// =====================================================================
//
// We assume an approximate font size of 14 px (renderer default) and
// require that the selected-row rectangle vertically contains the
// expected text band — i.e. the rect top is at-or-above the text top
// (row.y + (row.h - font_size)/2), and the rect bottom is at-or-below
// the text bottom. The exact ascent/descent is unknowable without GPU,
// so we use font_size as a conservative proxy for the line box.
#[test]
fn palette_highlight_y_encloses_text_y() {
    let mut p = CommandPalette::new();
    p.open();
    // Move selection to index 2 (the third row).
    p.move_selection_down();
    p.move_selection_down();
    let layout = PaletteLayout::compute(&mut p, 1920.0, 1080.0).expect("palette open");
    assert!(layout.rows.len() >= 5, "need ≥5 rows in default 1080p layout");

    let sel_idx = layout.selected_row.expect("a row is selected");
    let row = &layout.rows[sel_idx];

    // Renderer centers `font_size`-tall text inside `PALETTE_ROW_HEIGHT`.
    let font_size: f32 = 14.0;
    let text_top = row.rect.y + ((PALETTE_ROW_HEIGHT - font_size) * 0.5).max(0.0);
    let text_bottom = text_top + font_size;

    // The "highlight rect" that the renderer actually draws is the
    // row's own rect.
    let hl_top = row.rect.y;
    let hl_bottom = row.rect.y + row.rect.h;

    assert!(
        hl_top <= text_top + 0.001,
        "highlight top ({}) must enclose text top ({})",
        hl_top,
        text_top,
    );
    assert!(
        hl_bottom + 0.001 >= text_bottom,
        "highlight bottom ({}) must enclose text bottom ({})",
        hl_bottom,
        text_bottom,
    );

    // Row height must be >= font_size — otherwise no vertical
    // enclosure is possible regardless of centering.
    assert!(
        row.rect.h >= font_size,
        "row height {} must be ≥ font_size {} to enclose text",
        row.rect.h,
        font_size,
    );

    // Selection accent must also sit within the row vertically.
    let acc = layout.selected_accent.expect("accent present when row is");
    assert!(
        acc.y >= row.rect.y - 0.001 && acc.y + acc.h <= row.rect.y + row.rect.h + 0.001,
        "accent ({:?}) must be inside row ({:?}) vertically",
        acc,
        row.rect,
    );
}

// =====================================================================
// Bug 2 — prefs first frame must not be blank (strengthened)
// =====================================================================
//
// PR #123 added `prefs_first_frame_has_nonzero_draw_commands` which
// only requires ≥4 quads + ≥1 text. A genuinely populated prefs window
// emits well over 10 commands on the first frame (sidebar bg, divider,
// per-category row backgrounds, form card, title, body labels, …).
// Bump the threshold so a future regression that drops to "barely any
// content" still trips.
#[test]
fn prefs_first_frame_emits_more_than_ten_commands() {
    let theme = test_theme();
    let state = PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic.toml"), theme.clone());
    let dl = build_draw_list(&state, &theme);
    let total = dl.quads.len() + dl.texts.len();
    assert!(
        total > 10,
        "prefs first frame must emit >10 draw commands (quads+texts) to count \
         as 'non-blank' — got {} (quads={}, texts={})",
        total,
        dl.quads.len(),
        dl.texts.len(),
    );
    // Body content (not just chrome): at least one *non-title* text.
    let non_title_texts = dl.texts.iter().filter(|t| !t.text.contains("Preferences")).count();
    assert!(
        non_title_texts > 0,
        "prefs first frame must include body text beyond the 'Preferences' title — got 0"
    );
}

// =====================================================================
// Bug 3 — prefs click handlers must not deadlock with the renderer
// =====================================================================
//
// Hard to test by simulation alone — we instead lint the source of the
// prefs event handler functions for unconditional `.lock()` calls on
// any parking_lot Mutex (or std Mutex). Such locks would block the
// main thread if the renderer were holding the same lock during a
// redraw triggered by the click, reproducing the reported freeze.
//
// The handler MAY use `try_lock` (non-blocking) or hold no lock at all.
#[test]
fn prefs_event_handlers_use_no_blocking_locks() {
    let app_src = std::fs::read_to_string("../sonic-app/src/app/prefs_window.rs")
        .expect("read sonic-app/src/app/mod.rs");
    let prefs_renderer_src =
        std::fs::read_to_string("src/prefs_renderer.rs").expect("read prefs_renderer.rs");

    // Extract the body of `fn handle_prefs_event` by line-counted brace
    // matching — simple enough for this single-function lint.
    let handler = extract_fn_body(&app_src, "fn handle_prefs_event")
        .expect("handle_prefs_event present in app.rs");

    // Forbidden: any call of the form `.lock()` that is NOT preceded
    // by `try_` (i.e. `.try_lock()` is fine).
    for (i, line) in handler.lines().enumerate() {
        // Strip simple line comments for the check.
        let code = line.split("//").next().unwrap_or(line);
        if code.contains(".lock()") && !code.contains(".try_lock()") {
            panic!(
                "prefs event handler must not use blocking .lock() — line {} of \
                 handle_prefs_event body: {:?}",
                i + 1,
                line,
            );
        }
    }

    // The prefs renderer is called every frame from the main thread; it
    // must likewise avoid blocking on any lock.
    for (n, line) in prefs_renderer_src.lines().enumerate() {
        let code = line.split("//").next().unwrap_or(line);
        if code.contains(".lock()") && !code.contains(".try_lock()") {
            panic!("prefs_renderer.rs must not use blocking .lock() — line {}: {:?}", n + 1, line,);
        }
    }
}

/// Extract the body (between the outermost `{` and matching `}`) of
/// the first function whose declaration contains `signature`. Returns
/// the body as a `String` — `None` if not found.
fn extract_fn_body(src: &str, signature: &str) -> Option<String> {
    let start = src.find(signature)?;
    let brace = src[start..].find('{')? + start;
    let bytes = src.as_bytes();
    let mut depth: i32 = 0;
    let mut end = brace;
    for (i, &b) in bytes.iter().enumerate().skip(brace) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    Some(src[brace + 1..end].to_string())
}

// =====================================================================
// Bug 4 — tab title font must equal the configured font family
// =====================================================================
//
// The render path builds tab title spans, then maps those color spans to
// glyphon rich-text spans before setting `tab_buffer`. Exercise that same
// Attrs-building path with a deliberately distinctive configured family so
// any hard-coded `Family::Name("JetBrains Mono")` regression fails here.
#[test]
fn tab_title_uses_config_font_family_not_hardcoded() {
    let mut cfg = Config::default();
    cfg.font.family = "DistinctMarker Mono".to_string();

    const ACTIVE: GColor = GColor::rgb(0xfa, 0xbd, 0x2f);
    const INACTIVE: GColor = GColor::rgb(0x92, 0x83, 0x74);
    let (title_text, tab_spans) = build_tab_title_spans(
        &[
            TabSpanInput {
                index: 0,
                title: "#1 shell",
                title_x: 0.0,
                title_w: 80.0,
                is_active: false,
            },
            TabSpanInput {
                index: 1,
                title: "#2 editor",
                title_x: 110.0,
                title_w: 110.0,
                is_active: true,
            },
        ],
        10.0,
        ACTIVE,
        INACTIVE,
    );
    assert!(!tab_spans.is_empty(), "tab title color spans must be non-empty");

    let rich = build_tab_title_rich_text_spans(
        &title_text,
        &tab_spans,
        cfg.font.family.as_str(),
        INACTIVE,
    );
    assert!(!rich.spans.is_empty(), "tab title rich-text spans must be non-empty");

    assert_attrs_family_is_distinct_marker(&rich.default_attrs, "default tab title attrs");
    for (text, attrs) in &rich.spans {
        assert!(!text.is_empty(), "tab title rich-text spans must not include empty text");
        assert_attrs_family_is_distinct_marker(attrs, text);
    }
}

fn assert_attrs_family_is_distinct_marker(attrs: &Attrs<'_>, label: &str) {
    match attrs.family {
        Family::Name(name) => assert_eq!(
            name, "DistinctMarker Mono",
            "{label} must use configured font family, not a hard-coded family"
        ),
        other => panic!("{label} must use Family::Name(\"DistinctMarker Mono\"), got {other:?}"),
    }
}
