//! Regression guard: a rasterized Powerline glyph must carry real
//! antialiasing (partial-alpha pixels), not a binary 1-bit mask.
//!
//! User report (companion to the cell-rect-anchor fix): the diagonal of
//! the Powerline arrow showed visible stairs / jaggies while normal grid
//! text was smooth. The shipped rasterizer config (`Source::Outline` +
//! `Format::Alpha` with hinting on) produces grayscale-alpha coverage
//! masks — so partial-alpha pixels along curved edges are the proof
//! point that AA is in fact reaching the atlas. If a future change
//! flipped to a binary threshold path (or to a bitmap-only source that
//! returned a 0/255-only mask for these glyphs) the diagonal would
//! turn into pure stairs even with the anchor fix in place.
//!
//! Lives next to the anchor regression so a single test target covers
//! both halves of the user-reported P0.

#[cfg(not(target_os = "windows"))]
mod mac_linux {
    use cosmic_text::FontSystem;
    use sonic_text::{
        glyph_atlas::Rasterizer,
        swash_rasterizer::{load_bundled_fonts, SwashRasterizer, DEFAULT_RASTER_PX},
    };
    use sonic_types::GlyphKey;

    #[test]
    fn powerline_glyph_has_partial_alpha_pixels() {
        let mut fs = FontSystem::new();
        load_bundled_fonts(&mut fs);
        // Slot 0 = primary; the bundled Nerd Font is in the chain so
        // we let `resolve_slot` pick the right slot for U+E0B0.
        let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
        let Some(slot) = r.resolve_slot('\u{E0B0}', false, false) else {
            // No bundled font has the Powerline range in this env (e.g.
            // CI without the assets pulled). Don't fail the suite — the
            // anchor test still proves the positioning fix. Print a
            // diagnostic so a real regression is easy to spot.
            eprintln!("skip: no font in chain covers U+E0B0 in this environment");
            return;
        };
        let key = GlyphKey::with_slot('\u{E0B0}', slot, false, false);
        let tile = r.rasterize(key).expect("rasterize U+E0B0");

        assert!(!tile.is_color, "Powerline glyph must rasterize as grayscale-alpha, not color");
        assert!(tile.width > 0 && tile.height > 0, "non-empty Powerline tile");

        // Format::Alpha returns one byte/pixel (see mono_alpha_byte_layout).
        let mut zero = 0usize;
        let mut full = 0usize;
        let mut partial = 0usize;
        for &a in &tile.coverage {
            match a {
                0 => zero += 1,
                255 => full += 1,
                _ => partial += 1,
            }
        }
        // A 1-bit / pure-binary mask is the failure mode this test guards
        // against: it would have zero partial-alpha pixels even on a
        // diagonal edge. Real AA produces a band of partials along the
        // arrow's hypotenuse.
        assert!(
            partial > 0,
            "Powerline glyph rasterized without antialiasing (no partial-alpha pixels). \
             zero={zero} full={full} partial={partial} size={}x{}",
            tile.width,
            tile.height,
        );
    }
}
