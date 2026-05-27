//! Regression test for PR #45 review: overlay backgrounds + overlay text
//! must be drawn AFTER terminal glyphs in the render pass, otherwise the
//! terminal content bleeds over the palette / search / IME dialogs.
//!
//! The renderer uses two separate pipelines for the terminal grid
//! (`text_pipeline`, fed from a `GlyphInstance` buffer) and for overlay
//! UI text (`text_renderer_overlay`, a glyphon `TextRenderer` distinct
//! from the chrome `text_renderer`). The ordering invariant is therefore
//! best expressed at the source-of-truth level: inside the render pass,
//! the terminal `text_pipeline.draw(...)` call must appear BEFORE both
//! `quad_overlay.draw(...)` and `text_renderer_overlay.render(...)`.
//!
//! Equivalently, when an overlay (e.g. the command palette) and a
//! terminal cell occupy the same screen position, the overlay's quad
//! and glyph commands are submitted later in the encoder, so the GPU
//! composites them on top.

use std::fs;

const RENDER_RS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/render/core.rs");

fn must_find(src: &str, needle: &str) -> usize {
    src.find(needle).unwrap_or_else(|| panic!("expected `{needle}` somewhere in render.rs"))
}

#[test]
fn overlay_quads_and_text_render_after_terminal_glyphs() {
    let src = fs::read_to_string(RENDER_RS).expect("read render.rs");

    // Locate the begin_render_pass call — every assertion below is
    // about offsets *inside* that single pass so the GPU command order
    // is unambiguous.
    let pass_start = must_find(&src, "begin_render_pass");
    let after_pass = &src[pass_start..];

    // Terminal grid glyph draw (the atlas-backed text_pipeline). This
    // is what was previously LAST in the pass, hiding overlays behind it.
    let terminal_draw = must_find(after_pass, "self.text_pipeline.draw(");

    // Chrome / pre-overlay text (tab titles, legacy bottom status bar).
    let chrome_text = must_find(after_pass, "self.text_renderer.render(");

    // Overlay quad backgrounds (palette modal, search badge, IME preedit).
    let overlay_quads = must_find(after_pass, "self.quad_overlay.draw(");

    // Overlay text (palette query/rows, search input label, IME preedit
    // glyphs) — a SECOND glyphon TextRenderer drawn after the overlay
    // quads so glyphs sit on top of their own backgrounds.
    let overlay_text = must_find(after_pass, "self.text_renderer_overlay.render(");

    assert!(
        terminal_draw < overlay_quads,
        "terminal glyph pipeline must draw BEFORE overlay quads (terminal={terminal_draw}, overlay_quads={overlay_quads})"
    );
    assert!(
        terminal_draw < overlay_text,
        "terminal glyph pipeline must draw BEFORE overlay text (terminal={terminal_draw}, overlay_text={overlay_text})"
    );
    assert!(
        chrome_text < overlay_quads,
        "tab-bar / chrome text must render BEFORE overlay quads (chrome={chrome_text}, overlay_quads={overlay_quads})"
    );
    assert!(
        overlay_quads < overlay_text,
        "overlay backgrounds must draw BEFORE overlay text so glyphs sit on top (quads={overlay_quads}, text={overlay_text})"
    );
}

#[test]
fn overlay_areas_routed_to_overlay_text_renderer() {
    // The overlay TextRenderer must be `prepare()`d with the overlay
    // text areas — otherwise the palette/IME glyphs would silently
    // vanish even though the render-pass order is correct.
    let src = fs::read_to_string(RENDER_RS).expect("read render.rs");
    let prep_overlay = must_find(&src, "self.text_renderer_overlay.prepare(");
    let after = &src[prep_overlay..];
    // Within the prepare call's argument list, the overlay_areas vec
    // is the one consumed.
    let consumed = must_find(after, "overlay_areas");
    assert!(
        consumed < 400,
        "overlay_areas must be the iterable passed into text_renderer_overlay.prepare()"
    );
}

#[test]
fn overlay_quad_pushes_go_to_overlay_vec_not_main_vec() {
    // The palette / search / IME backgrounds should push into
    // `quads_overlay`, not `quads`. Spot-check distinctive markers
    // used by each overlay so we'd catch a future refactor that
    // accidentally routed them back through the main vec.
    let src = fs::read_to_string(RENDER_RS).expect("read render.rs");

    // The palette modal background now sources its color from the
    // theme-derived UiPalette (`palette_chrome.bg_elevated`), per
    // PR #119 review fix. Match that marker instead of a hardcoded
    // RGBA literal.
    let palette_bg = must_find(&src, "color: palette_chrome.bg_elevated,");
    // The IME preedit background still uses a distinctive literal.
    let ime_bg = must_find(&src, "[0.10, 0.11, 0.14, 0.95]");

    // Walk backwards from each marker to the nearest `quads*.push(`
    // and assert it is the overlay vec.
    for (label, pos) in [("palette", palette_bg), ("ime", ime_bg)] {
        let prefix = &src[..pos];
        let push_pos = prefix.rfind(".push(").expect("must be inside a push call");
        let line_start = prefix[..push_pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = &src[line_start..push_pos];
        assert!(
            line.contains("quads_overlay"),
            "{label} background must be pushed into quads_overlay, got: {line:?}"
        );
    }
}
