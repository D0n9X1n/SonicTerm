//! Regression tests for PR #116 Haiku findings:
//!
//! 1. Modal panel quads must use the SDF rounded-rect path with radius 16
//!    (was: plain sharp rectangles, ignoring spec).
//! 2. The action-row glyphon buffer's line height must match the row
//!    stride so labels align vertically with their background quads.
//! 3. The footer hint must be rendered into the `layout.footer` rect, not
//!    appended to the multi-line rows buffer.
//!
//! These are source-level regression checks (the same shape as
//! `render_overlay_zorder.rs`): they grep `render.rs` for the wiring that
//! makes the three properties hold, so the test can run without a wgpu
//! device.

use std::fs;

const RENDER_RS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/render/core.rs");

fn render_src() -> String {
    fs::read_to_string(RENDER_RS).expect("read render.rs")
}

#[test]
fn palette_modal_uses_rounded_quad_radius_16() {
    let src = render_src();
    // Constants live in overlays.rs and are imported by render.rs.
    assert!(
        src.contains("PALETTE_PANEL_RADIUS"),
        "render.rs must reference PALETTE_PANEL_RADIUS (modal panel radius 16)"
    );
    // Confirm panel + border quads carry both `radius_px` and `size_px`,
    // i.e. they go through the SDF path rather than the sharp-rect path.
    let palette_block_start =
        src.find("Command palette overlay").expect("palette overlay block present");
    let palette_block = &src[palette_block_start..];
    let modal_bg_idx =
        palette_block.find("Modal background").expect("modal background comment present");
    let modal_bg_block =
        &palette_block[modal_bg_idx..modal_bg_idx + 600.min(palette_block.len() - modal_bg_idx)];
    assert!(
        modal_bg_block.contains("radius_px"),
        "modal background quad must set radius_px (rounded panel)"
    );
    assert!(
        modal_bg_block.contains("size_px"),
        "modal background quad must set size_px (SDF needs pixel size)"
    );
    assert!(
        modal_bg_block.contains("PALETTE_PANEL_RADIUS"),
        "modal background radius must come from PALETTE_PANEL_RADIUS constant"
    );

    // And the constant is wired to 16.0 px in overlays.rs.
    let overlays =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../sonicterm-ui/src/overlays.rs"))
            .expect("read overlays.rs");
    assert!(
        overlays.contains("pub const PALETTE_PANEL_RADIUS: f32 = 16.0"),
        "PALETTE_PANEL_RADIUS must equal 16.0 per spec"
    );
}

#[test]
fn palette_row_buffer_line_height_matches_stride() {
    let src = render_src();
    // The rows buffer must be created with a Metrics whose line height
    // is the palette row height (40px), not the default monospace line
    // height (~22px) which would visually compress the row labels.
    let rows_buffer_idx = src
        .find("palette_rows_metrics")
        .expect("palette_rows_metrics declared explicitly so the line height is the row stride");
    let after = &src[rows_buffer_idx..rows_buffer_idx + 400.min(src.len() - rows_buffer_idx)];
    assert!(
        after.contains("PALETTE_ROW_HEIGHT"),
        "palette_rows_metrics must derive line_height from PALETTE_ROW_HEIGHT"
    );
}

#[test]
fn palette_footer_positioned_in_footer_rect() {
    let src = render_src();
    // #384: footer text is now emitted via `emit_overlay_text_glyphs`
    // (Sonic atlas device-scale path), no longer via a glyphon
    // TextArea. The contract this test enforces is unchanged: the
    // footer label MUST anchor on `layout.footer.{x,y}` (so the hint
    // sits inside the footer strip, not pushed up into the rows list)
    // and MUST NOT be concatenated onto the rows buffer.
    assert!(
        src.contains("emit_overlay_text_glyphs"),
        "render.rs must emit palette text through the Sonic atlas device-scale path (#384)"
    );
    // Locate the footer emitter call. It's preceded by a `// Footer`
    // marker comment so the search is stable to formatter reflow.
    let footer_idx =
        src.find("// Footer hint").expect("render.rs must mark the palette footer emitter call");
    let after = &src[footer_idx..footer_idx + 2000.min(src.len() - footer_idx)];
    assert!(
        after.contains("layout.footer.x"),
        "palette footer emitter must anchor origin_x on layout.footer.x"
    );
    assert!(
        after.contains("layout.footer.y"),
        "palette footer emitter must anchor baseline_y on layout.footer.y"
    );
    assert!(
        after.contains("emit_overlay_text_glyphs"),
        "the Footer hint block must call emit_overlay_text_glyphs (#384)"
    );

    // And the rows buffer no longer carries the footer label.
    assert!(
        !src.contains("rows_text.push_str(&layout.footer_label)"),
        "footer_label must NOT be appended to the rows buffer any more"
    );
}
