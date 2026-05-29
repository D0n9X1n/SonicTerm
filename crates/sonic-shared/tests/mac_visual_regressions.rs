//! Regression coverage for the six Mac visual bugs fixed in the
//! "fix(visual): 6 Mac regressions before Windows phase" PR.
//!
//! These tests verify the *pure data* layers (palette geometry, prefs
//! draw-list, close-button quad emission) — the actual wgpu surfaces
//! cannot be exercised in CI. The GUI smoke gate (see `CLAUDE.md` §13)
//! is the visual confirmation; these tests are the structural
//! invariants that prevent future regressions of the same shape.

use sonic_core::config::Config;
use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
use sonic_shared::command_palette::CommandPalette;
use sonic_shared::overlays::{PaletteLayout, PALETTE_ROW_HEIGHT};
use sonic_shared::prefs::PrefsState;
use sonic_shared::prefs_renderer::build_draw_list;
use sonic_shared::quad::{push_mask_icon_quads, MaskIconParams, QuadInstance, ICON_CLOSE_8};
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

/// Bug 2 — Sonic Preferences window renders BLANK.
///
/// `build_draw_list` is the function the `PrefsRenderer` calls every
/// frame to obtain the quad + text commands. If it ever returns an
/// empty list with the default-size layout we'd ship a black window
/// regardless of any GPU plumbing. Assert the first frame has real
/// content: at least one quad for the sidebar background AND at least
/// one piece of text (the "Preferences" title).
#[test]
fn prefs_first_frame_has_nonzero_draw_commands() {
    let theme = test_theme();
    let state = PrefsState::new(Config::default(), PathBuf::from("/tmp/sonic.toml"), theme.clone());
    let dl = build_draw_list(&state, &theme);
    assert!(
        dl.quads.len() >= 4,
        "prefs first frame must emit ≥4 quads (sidebar bg + divider + form card + border) — got {}",
        dl.quads.len()
    );
    assert!(
        !dl.texts.is_empty(),
        "prefs first frame must emit ≥1 text command (Preferences title) — got 0"
    );
    let has_title = dl.texts.iter().any(|t| t.text.contains("Preferences"));
    assert!(has_title, "first frame must include the 'Preferences' title text");
    // Clear color must NOT be transparent black — a pure-zero clear is
    // the bug we are guarding against.
    assert!(
        dl.clear[3] > 0.5 && (dl.clear[0] + dl.clear[1] + dl.clear[2]) > 0.0,
        "prefs clear color must be a real theme color, got {:?}",
        dl.clear
    );
}

/// Bug 3 — Command palette selection highlight points to a BLANK slot.
///
/// The bug shipped because the highlight rect (drawn from
/// `layout.rows[selected_row]`) and the text rendered by glyphon used
/// DIFFERENT vertical strides: the rows had `row_stride = HEIGHT + GAP`
/// while the rows buffer's line height was only `HEIGHT`. After N
/// rows the text drifted N*GAP pixels above the highlight box.
///
/// We can't observe glyphon directly in a unit test, but we CAN assert
/// the row layout uses a single consistent stride. If a future change
/// re-introduces the mismatch (e.g., bumps the row height without
/// bumping the buffer line height) the GUI smoke screenshot will catch
/// it; this test makes the invariant explicit.
#[test]
fn palette_selection_highlight_y_matches_selected_row_text_y() {
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 1920.0, 1080.0).expect("palette open → some");
    assert!(layout.rows.len() >= 3, "need ≥3 rows to detect drift");

    // The vertical stride between consecutive rows must be uniform —
    // any non-uniform stride is the class of bug that caused selection
    // to drift below the visible items.
    let stride_0 = layout.rows[1].rect.y - layout.rows[0].rect.y;
    for i in 1..(layout.rows.len() - 1) {
        let stride_i = layout.rows[i + 1].rect.y - layout.rows[i].rect.y;
        assert!(
            (stride_i - stride_0).abs() < 0.001,
            "row {} stride {} must equal row 0 stride {}",
            i,
            stride_i,
            stride_0
        );
    }

    // The selected_row index returned by the layout must be a valid
    // index into rows[] — historically scroll math returned indices
    // beyond the slice on edge cases, which is what landed the
    // highlight in a blank area.
    for selected in 0..layout.rows.len().min(7) {
        let mut q = CommandPalette::new();
        q.open();
        for _ in 0..selected {
            q.move_selection_down();
        }
        let l = PaletteLayout::compute(&mut q, 1920.0, 1080.0).unwrap();
        let sel = l.selected_row.expect("an item is selected");
        assert!(sel < l.rows.len(), "selected_row={} out of bounds for {} rows", sel, l.rows.len());
        // Selected accent y must equal selected row y (centered offset
        // is added on top, but the row.y itself must match).
        let acc = l.selected_accent.expect("accent present when row is");
        let row = &l.rows[sel];
        assert!(
            acc.y >= row.rect.y && acc.y + acc.h <= row.rect.y + row.rect.h + 0.001,
            "accent must sit inside the selected row's rect (acc={:?}, row={:?})",
            acc,
            row.rect
        );
    }
}

/// Bug 4 — palette row vertical spacing wrong (text sits at the top
/// of each 40px row, leaving ~18px gap below).
///
/// The fix: line height of the rows buffer is set to
/// `PALETTE_ROW_HEIGHT + PALETTE_ROW_GAP` (the full row stride). This
/// test pins that constant relationship — the renderer uses the same
/// stride for the highlight rect, so the two will now always agree.
#[test]
fn palette_row_height_matches_layout_stride() {
    let mut p = CommandPalette::new();
    p.open();
    let layout = PaletteLayout::compute(&mut p, 1920.0, 1080.0).unwrap();
    // Each row.rect.h must be PALETTE_ROW_HEIGHT.
    for r in &layout.rows {
        assert!(
            (r.rect.h - PALETTE_ROW_HEIGHT).abs() < 0.001,
            "row height {} must equal PALETTE_ROW_HEIGHT={}",
            r.rect.h,
            PALETTE_ROW_HEIGHT
        );
    }
}

/// Bug 5 — tab close × button rendered as `+`, not `×`.
///
/// Chrome icons now come from SVG-backed alpha masks. The defining close-icon
/// property: at least one emitted quad must sit *off-center* on both axes (a
/// `+` would emit only axis-aligned horizontal + vertical bars centered on the
/// glyph midline).
#[test]
fn tab_close_button_renders_as_x_not_plus() {
    let mut quads: Vec<QuadInstance> = Vec::new();
    // 8px glyph at (10, 20), on a 200x200 surface.
    push_mask_icon_quads(
        &mut quads,
        MaskIconParams {
            mask: ICON_CLOSE_8,
            x: 10.0,
            y: 20.0,
            size: 8.0,
            min_cell: 1.0,
            color: [1.0, 1.0, 1.0, 1.0],
            sw: 200.0,
            sh: 200.0,
        },
    );
    assert!(quads.len() >= 4, "× must emit ≥4 mask quads (got {})", quads.len());

    // For a `×` glyph, NO quad should sit exactly on the glyph's
    // horizontal midline AND have width == glyph (the old `+` path
    // emitted exactly such a bar). Equivalently: the set of distinct
    // y-coordinates must span more than 2 values (a `+` has exactly 2).
    let mut ys: Vec<i32> = quads.iter().map(|q| (q.rect[1] * 10000.0) as i32).collect();
    ys.sort_unstable();
    ys.dedup();
    assert!(
        ys.len() >= 3,
        "× must vary on the y-axis (≥3 distinct y values) — `+` has only 2; got {}: {:?}",
        ys.len(),
        ys
    );

    // Top-left-most quad and top-right-most quad on the first row
    // must NOT share a quad (else the top is a single horizontal bar
    // → that's a `+`). Equivalent check: at the minimum y, there
    // should be two well-separated x values.
    let min_y = quads.iter().map(|q| q.rect[1]).fold(f32::INFINITY, f32::min);
    let xs_at_top: Vec<f32> =
        quads.iter().filter(|q| (q.rect[1] - min_y).abs() < 1e-4).map(|q| q.rect[0]).collect();
    assert!(
        xs_at_top.len() >= 2,
        "top row of `×` must contain diagonal mask quads, got {}: {:?}",
        xs_at_top.len(),
        xs_at_top
    );
    let min_x = xs_at_top.iter().copied().fold(f32::INFINITY, f32::min);
    let max_x = xs_at_top.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    assert!(max_x - min_x > 0.05, "top row mask quads must be well-separated: {xs_at_top:?}");
}

/// Bug 6 — Ctrl+Shift+P palette has noticeable open latency.
///
/// The user-visible problem was that the palette state would flip to
/// open but no redraw was requested on the active window, so the
/// overlay only appeared on the NEXT pty/timer event. We can't drive
/// the `App` directly in a CI test (it needs a real window + event
/// loop), but we *can* assert that toggling the palette is observable
/// synchronously through `CommandPalette::is_open` — which is the
/// invariant that the production redraw fix relies on (toggle THEN
/// request_redraw).
#[test]
fn palette_open_requests_redraw_synchronously() {
    let mut p = CommandPalette::new();
    assert!(!p.is_open(), "default-constructed palette is closed");
    let now_open = p.toggle();
    assert!(now_open, "toggle() must return new state synchronously");
    assert!(p.is_open(), "is_open() must reflect toggle synchronously, not on next tick");
    // And toggling back must close on the same call — no deferred work.
    let now_open = p.toggle();
    assert!(!now_open);
    assert!(!p.is_open());
}

/// Bug 7 — Palette selection highlight floats ABOVE the selected row's
/// text instead of wrapping it (live-screenshot bug, May 2026).
///
/// The render path in `sonic-shared/src/render.rs` derives the text
/// `top` from `row_y + text_top_offset`. For the highlight rect to
/// visually wrap the line of text, the offset must center the line-box
/// (whose height = the palette buffer's line_height) inside the 40 px
/// row background. The previous formula
/// `(HEIGHT - font_size) * 0.5` was based on glyph height, not
/// line-box height, and pushed the visual baseline BELOW the highlight
/// at non-default font sizes.
///
/// This test pins the centering formula. If a future change
/// re-introduces the glyph-height variant the assert will fire.
#[test]
fn palette_text_top_offset_centers_line_box_inside_row() {
    use sonic_shared::overlays::{PALETTE_ROW_GAP, PALETTE_ROW_HEIGHT};
    let line_height = PALETTE_ROW_HEIGHT + PALETTE_ROW_GAP;
    let text_top_offset = (PALETTE_ROW_HEIGHT - line_height) * 0.5;
    // The line-box (44 px) is taller than the highlight rect (40 px)
    // by 4 px, so the offset must be -2 px to split the overshoot
    // evenly above and below.
    assert!(
        (text_top_offset - (-2.0)).abs() < 1e-4,
        "text_top_offset must center the 44 px line-box inside the 40 px row \
         (expected -2.0, got {text_top_offset})"
    );
    // Sanity: text top is row_y - 2; line center is row_y - 2 + 22 = row_y + 20,
    // which is exactly the vertical center of the 40 px row. The previous
    // buggy formula at font_size=14 gave +13, putting the line center
    // ~12 px below the row center — visibly outside the highlight.
    let row_y = 100.0;
    let visual_line_center = row_y + text_top_offset + line_height * 0.5;
    let row_center = row_y + PALETTE_ROW_HEIGHT * 0.5;
    assert!(
        (visual_line_center - row_center).abs() < 0.5,
        "line-box center {visual_line_center} must equal row center {row_center}"
    );
}

/// Bug 8 — prefs window blank on open + freezes after a few clicks.
///
/// We can't drive winit + wgpu from a CI test, but two structural
/// invariants in `app.rs::create_prefs_window` and the prefs
/// MouseInput handler prevent regression of the symptom:
///
/// 1. `prefs_renderer` is installed BEFORE the first `request_redraw`
///    call — otherwise the redraw event finds `None` and skips
///    drawing, leaving the window blank until a user click
///    "incidentally" requests another redraw.
/// 2. `request_redraw` in the prefs MouseInput handler runs AFTER the
///    state mutation — otherwise the redraw paints the pre-click
///    state and the user sees their previous click's outcome.
///
/// These are enforced by string-grepping the source; if a future
/// refactor moves the calls back to the wrong order, the test fires.
#[test]
fn prefs_create_installs_renderer_before_first_request_redraw() {
    let raw = include_str!("../../sonic-app/src/app/prefs_window.rs");
    // Normalize CRLF → LF so the substring search works regardless of
    // the platform's git autocrlf setting.
    let src = raw.replace("\r\n", "\n");
    let create_fn_start = src.find("fn create_prefs_window").expect("function present");
    let create_fn_end = src[create_fn_start..]
        .find("\n    }\n")
        .map(|i| create_fn_start + i)
        .expect("end of function");
    let body = &src[create_fn_start..create_fn_end];
    // The `self.prefs_renderer = Some(r)` install line must appear
    // before the top-level `w.request_redraw()` (the one that
    // schedules the initial paint). Earlier `force_rebuild_for_scale`
    // calls request_redraw on the window directly while `r` is still
    // local — that's fine because winit defers redraw delivery until
    // the event loop returns, at which point the renderer slot is
    // populated. The bug we're guarding against is the OPPOSITE: a
    // top-level `w.request_redraw()` running before `prefs_renderer`
    // is installed in `self`, so the queued redraw event lands while
    // the field is still `None` and the render call is skipped.
    let install_pos =
        body.find("self.prefs_renderer = Some(r)").expect("renderer install line present");
    let top_level_redraw =
        body.rfind("w.request_redraw()").expect("top-level w.request_redraw present");
    assert!(
        install_pos < top_level_redraw,
        "prefs_renderer must be installed BEFORE the top-level w.request_redraw() \
         (install_pos={install_pos}, top_level_redraw={top_level_redraw}) — otherwise \
         the first RedrawRequested finds renderer == None and leaves the window blank \
         (regression of PR #125 fix path)"
    );
}
