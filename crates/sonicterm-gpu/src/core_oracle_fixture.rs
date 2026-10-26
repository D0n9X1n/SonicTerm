#![cfg(test)]
//! Shared test-only oracle fixture state and native custody.

use super::*;

/// An oracle row's projected glyphs, tofu outlines, missing characters and completeness.
pub(in crate::core) type OracleRow =
    (Vec<GlyphInstance>, Vec<frame_scratch::TofuQuad>, Vec<char>, bool);

/// Draw `grid`'s row 0 the way an independent oracle does: group the row's non-continuation
/// cells into consecutive style runs, build each run from owned copies of its cells and project
/// it. Returns the projected glyphs, tofu and missing characters and the row's completeness.
pub(in crate::core) fn oracle_row(grid: &Grid, stack: &sonicterm_engine::FontStack) -> OracleRow {
    let row = grid.row(0);
    let visible: Vec<(u16, Cell)> = row
        .iter()
        .enumerate()
        .filter(|(_, cell)| !cell.flags.contains(CellFlags::WIDE_CONT))
        .map(|(col, cell)| (col as u16, cell.clone()))
        .collect();
    let mut runs: Vec<(RunStyle, Vec<(u16, Cell)>)> = Vec::new();
    for (col, cell) in visible {
        let style = RunStyle::from_cell(&cell);
        match runs.last_mut() {
            Some((open, cells)) if *open == style => cells.push((col, cell)),
            _ => runs.push((style, vec![(col, cell)])),
        }
    }
    let mut atlas = GlyphAtlas::new(1024, 1024);
    let mut raster = stack.clone();
    let theme = Theme::default();
    let snapped = build_snapped_cell_x(0.0, 10.0, grid.cols);
    let (mut glyphs, mut tofu, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    let mut complete = true;
    for (style, cells) in &runs {
        complete &= shape_run_for_test(
            ShapeRunFixture {
                atlas: &mut atlas,
                row: 0,
                style: *style,
                cells,
                theme: &theme,
                fg_default: ChromeColor::rgb(230, 230, 230),
                cell_size: (10.0, 20.0),
                origin: (0.0, 0.0),
                surface: (400.0, 200.0),
                baseline_y_in_cell: 16.0,
                snapped_cell_x: &snapped,
                font_stack: Some(stack),
                wt_raster: Some(&mut raster),
                hovered_url_cells: None,
                hovered_url_accent: [0.0; 4],
                software_presenter: false,
            },
            &mut glyphs,
            &mut tofu,
            &mut missing,
        );
    }
    (glyphs, tofu, missing, complete)
}

/// A one-row grid of `cols` columns written cell by cell with each `(character, flags)`.
pub(in crate::core) fn styled_grid(cols: u16, cells: &[(char, CellFlags)]) -> Grid {
    let mut grid = Grid::new(cols, 1);
    for (character, flags) in cells {
        grid.put_char(*character, Color::Default, Color::Default, *flags);
    }
    grid.clear_dirty();
    grid
}

/// `owned` cells borrowed as the run builder takes them.
pub(in crate::core) fn borrow_cells(owned: &[(u16, Cell)]) -> Vec<(u16, &Cell)> {
    owned.iter().map(|(col, cell)| (*col, cell)).collect()
}

/// The cell at column `col` of a borrowed run, by a linear scan independent of the builder.
pub(in crate::core) fn oracle_cell_at<'cell>(
    cells: &[(u16, &'cell Cell)],
    col: u16,
) -> Option<&'cell Cell> {
    cells.iter().find(|(cell_col, _)| *cell_col == col).map(|(_, cell)| *cell)
}

/// Build one style run through the production run builder into fresh records with `atlas` and
/// the Rec Mono fixture, at row 0 of a 16-column, 10x20-cell row; returns the records and
/// whether the run was complete.
pub(in crate::core) fn build_run_records(
    atlas: &mut GlyphAtlas,
    stack: &sonicterm_engine::FontStack,
    style: RunStyle,
    cells: &[(u16, &Cell)],
    records: &mut sonicterm_text::row_glyph_cache::CachedRow,
) -> bool {
    let mut raster = stack.clone();
    let theme = Theme::default();
    let snapped = build_snapped_cell_x(0.0, 10.0, 16);
    GpuRenderer::build_shape_run(
        atlas,
        records,
        0,
        style,
        cells,
        &theme,
        ChromeColor::rgb(230, 230, 230),
        10.0,
        20.0,
        0.0,
        &snapped,
        Some(stack),
        Some(&mut raster),
        None,
        [0.0; 4],
        false,
    )
}

/// Runs the row emitter materialized while emitting `grid`'s row 0 on `rig`, with the row.
pub(in crate::core) fn emit_counting_materialized(
    rig: &mut GlyphRig,
    grid: &Grid,
) -> (EmittedRow, u64) {
    rig.begin(grid);
    let before = crate::row_runs::materialized_runs();
    let emitted = rig.emit(grid, 0, 0);
    (emitted, crate::row_runs::materialized_runs() - before)
}

/// The shaped atlas key a fresh Rec Mono fixture rig draws `character` with: a real (nonzero) glyph id from
/// its face, so a test can seed that exact key before the row is drawn again.
pub(in crate::core) fn shaped_key_of(character: char) -> sonicterm_types::glyph_key::GlyphKey {
    let grid = text_grid(2, &[&character.to_string()]);
    let mut rig = GlyphRig::new(false);
    rig.begin(&grid);
    let drawn = rig.emit(&grid, 0, 0);
    assert!(drawn.missing.is_empty() && !drawn.glyphs.is_empty(), "{character} draws a real tile");
    rig.atlas
        .resident_tile_keys()
        .into_iter()
        .find(|key| key.ch == character && key.glyph_id != 0)
        .expect("the Rec Mono fixture stack shapes the character to a real glyph id")
}

/// Every way the quick-select draw sites in `source`, a copy of `core.rs`, fail to place a hint
/// through `quick_select_hint_display_row`, each named by its site; empty when both comply.
///
/// Comments and literal contents are blanked and tokens are matched whatever the layout, so a name
/// in prose or a string neither satisfies nor fails the check, and a read split across lines is
/// still seen. The background site is the body of `prepare_quick_select_overlay`; the letter site
/// is the block `render` runs when `quick_select_hint_count > 0`.
pub(in crate::core) fn quick_select_placement_violations(source: &str) -> Vec<String> {
    use crate::source_scan::{block_span, code_only, token_matches};
    let code = code_only(&source.replace("\r\n", "\n"));
    let Some(helper_start) =
        token_matches(&code, "pub fn quick_select_hint_display_row(").first().copied()
    else {
        // When: the helper is gone, no site can place a hint through it.
        return vec!["the placement helper quick_select_hint_display_row is missing".to_string()];
    };
    let mut violations = Vec::new();
    for (site, anchor) in [
        ("background", "fn prepare_quick_select_overlay("),
        ("letter", "if quick_select_hint_count > 0"),
    ] {
        let anchor_offsets = token_matches(&code, anchor);
        let [anchor_offset] = anchor_offsets[..] else {
            // When: the anchor is missing or repeated, the site cannot be told apart; fail closed.
            violations.push(format!("{site}: expected one `{anchor}`, found {anchor_offsets:?}"));
            continue;
        };
        let Some(site_span) = block_span(&code, anchor_offset) else {
            // When: the site's block never closes, its body cannot be checked; fail closed.
            violations.push(format!("{site}: the block after `{anchor}` never closes"));
            continue;
        };
        let body = &code[site_span];
        let helper_calls = token_matches(body, "quick_select_hint_display_row(").len();
        if helper_calls != 1 {
            // When: the site does not place its hints through exactly one helper call, report it.
            violations.push(format!("{site}: {helper_calls} placement-helper calls, expected 1"));
        }
        for raw_placement in ["hint.row", "hint_row_now("] {
            if !token_matches(body, raw_placement).is_empty() {
                // When: the site reads the scan-time row or shifts it itself, it bypasses the helper.
                violations.push(format!("{site}: reads `{raw_placement}` directly"));
            }
        }
    }
    let helper_span = block_span(&code, helper_start).unwrap_or(helper_start..code.len());
    let outside_helper = format!("{}{}", &code[..helper_start], &code[helper_span.end..]);
    if !token_matches(&outside_helper, "hint_row_now(").is_empty() {
        // When: code outside the helper shifts a hint for eviction, a third placement exists.
        violations.push("hint_row_now is called outside the placement helper".to_string());
    }
    violations
}

/// A minimal `core.rs` stand-in whose two draw sites both comply. The background site follows a
/// string holding `//`, so a check that took the URL for a comment would lose its helper call.
pub(in crate::core) const COMPLIANT_PLACEMENT_FIXTURE: &str = r#"
pub fn quick_select_hint_display_row(qs: &Q, hint: &H, evicted: u64) -> Option<usize> {
    qs.hint_row_now(hint, evicted).and_then(|row| quick_select_viewport_row(row))
}
fn prepare_quick_select_overlay(&mut self, quick_select: &Q) {
    for hint in &quick_select.hints {
        let link = "https://a.co"; let Some(visible_row) = quick_select_hint_display_row(
            quick_select, hint, evicted,
        ) else {
            continue;
        };
        draw_background(hint.col_start, visible_row, '{');
    }
}
fn render(&mut self) {
    if quick_select_hint_count > 0 {
        for hint in &qs.hints {
            // Prose may name hint.row and hint_row_now( without counting.
            let Some(visible_row) = quick_select_hint_display_row(qs, hint, evicted) else {
                continue;
            };
            draw_letter(hint.hint, hint.col_start, visible_row, "hint.row");
        }
    }
}
"#;
