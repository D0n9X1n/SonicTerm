//! Regression test for the P0 macOS glyph-blur bug (post-#282).
//!
//! PR #267 changed `swash_rasterizer::rasterize` to ALWAYS return a
//! 4-bytes-per-pixel coverage buffer (BGRA-replicated alpha for the
//! `Format::Alpha` branch, real BGRA for `Format::Subpixel`). It did
//! not update `glyph_atlas::insert_glyph`, whose `is_color: false`
//! branch still reads coverage as **one byte per pixel** and replicates
//! the alpha into BGRA itself.
//!
//! Result on macOS: every monochrome glyph was read at 1/4 the actual
//! length, mis-aligning per-row indexing into the BGRA-replicated
//! buffer, producing "smeared color stripes" — the P0 the user
//! reported in the wake of #282 (which reverted the FORMAT but left
//! the expansion in place).
//!
//! This test pins the contract: the `Format::Alpha` (mac + linux)
//! branch of the rasterizer MUST return `width * height` bytes for a
//! non-color tile — NOT `width * height * 4`. The atlas blit assumes
//! 1 byte per pixel and any drift here corrupts every monochrome
//! glyph on screen.
//!
//! Windows still uses `Format::Subpixel` (4 bytes/pixel BGRA) and is
//! tracked as a separate follow-up.

#[cfg(not(target_os = "windows"))]
mod mac_linux {
    use cosmic_text::FontSystem;
    use sonic_text::{
        glyph_atlas::Rasterizer,
        swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX},
    };
    use sonic_types::GlyphKey;

    fn font_system_with_assets() -> FontSystem {
        let mut fs = FontSystem::new();
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
                if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                    if let Ok(bytes) = std::fs::read(&p) {
                        sonic_text::load_font_data_with_sonic_overrides(&mut fs, bytes);
                    }
                }
            }
        }
        fs
    }

    #[test]
    fn mono_alpha_returns_one_byte_per_pixel() {
        let mut fs = font_system_with_assets();
        let mut r = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", DEFAULT_RASTER_PX);
        let tile = r.rasterize(GlyphKey::new('A', false, false)).expect("rasterize A");

        assert!(!tile.is_color, "LCD outline glyph must not be flagged as color");
        assert!(tile.width > 0 && tile.height > 0, "non-empty glyph");

        let one_byte = (tile.width as usize) * (tile.height as usize);
        let four_byte = one_byte * 4;
        assert_eq!(
            tile.coverage.len(),
            one_byte,
            "Format::Alpha tile must be 1 byte/pixel: got {} bytes for {}x{} = {} pixels \
             (4× would be {}). The 4× layout is the post-#267 / pre-this-fix layout that \
             broke the glyph_atlas blit and produced the smeared-stripes P0 glyph blur on \
             macOS — see the fix's PR for the full trace.",
            tile.coverage.len(),
            tile.width,
            tile.height,
            one_byte,
            four_byte,
        );
    }
}
