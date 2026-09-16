//! Weight scaling follows face selection and preserves geometry for primary and fallback glyphs.
//!
//! These tests use only tracked font files through `ConfigDirsOnly`; no system
//! font lookup is involved, and a fixture that fails to resolve is a hard test
//! failure rather than a silent skip.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use sonicterm_engine::FontStack;
use sonicterm_text::glyph_atlas::Rasterizer;
use sonicterm_types::glyph_key::GlyphKey;

const PRIMARY: &str = "Rec Mono St.Helens";
const FALLBACK: &str = "Roboto";
const MISSING_FAMILY: &str = "SonicTermNoSuchFamily-A7F3E1";
const HEAVY: f32 = 3.0;

static FONT_TEST_LOCK: Mutex<()> = Mutex::new(());

fn serialized_font_test() -> MutexGuard<'static, ()> {
    FONT_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct TileFacts {
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
    advance: f32,
    ink: u64,
}

fn tracked_font_dirs() -> Vec<PathBuf> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    vec![
        repo.join("assets/fonts"),
        repo.join("crates/sonicterm-harfbuzz/harfbuzz/src/wasm/sample/c"),
    ]
}

fn stack(families: &[(&str, bool)], weight: f32) -> FontStack {
    FontStack::try_new_with_font_dirs_for_test(families, tracked_font_dirs(), 14.0, 72, weight)
        .expect("tracked Rec Mono/Roboto font fixtures must build a FontStack")
}

fn facts(stack: &mut FontStack, ch: char) -> TileFacts {
    let tile = stack
        .rasterize(GlyphKey {
            ch,
            font_slot: 0,
            weight_bold: false,
            italic: false,
            // Force shaping so the actual handle index is resolved.
            glyph_id: 0,
            raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
        })
        .unwrap_or_else(|| panic!("tracked font fixtures must rasterize {ch:?}"));
    let (w, h) = (tile.width as usize, tile.height as usize);
    let stride = tile.coverage.len() / h.max(1);
    let bytes_per_px = stride / w.max(1);
    let mut ink = 0u64;
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * bytes_per_px;
            let a = if bytes_per_px == 4 { tile.coverage[i + 3] } else { tile.coverage[i] };
            ink += u64::from(a);
        }
    }
    TileFacts {
        width: tile.width,
        height: tile.height,
        offset_x: tile.offset_x,
        offset_y: tile.offset_y,
        advance: tile.advance,
        ink,
    }
}

fn resolved_handle(stack: &FontStack, ch: char) -> usize {
    stack
        .shape_text(&ch.to_string())
        .expect("tracked fixtures must shape")
        .into_iter()
        .find(|glyph| glyph.glyph_pos != 0)
        .expect("tracked fixture must contain glyph")
        .font_idx
}

// A missing primary cannot exempt the selected monochrome fallback from weight scaling.
#[test]
fn a_fallback_at_index_zero_is_reweighted_without_geometry_changes() {
    let _serial = serialized_font_test();
    let families = [(MISSING_FAMILY, false), (FALLBACK, true)];
    let mut base_stack = stack(&families, 1.0);
    let mut heavy_stack = stack(&families, HEAVY);
    assert_eq!(resolved_handle(&base_stack, 'm'), 0, "fallback must inherit handle zero");

    let base = facts(&mut base_stack, 'm');
    let heavy = facts(&mut heavy_stack, 'm');
    assert_eq!(TileFacts { ink: base.ink, ..heavy }, base);
    assert!(heavy.ink > base.ink, "selected fallback must receive the same weight adjustment");
}

// A nonzero fallback handle follows the same post-selection policy as the primary face.
#[test]
fn a_fallback_after_the_primary_is_reweighted_without_geometry_changes() {
    let _serial = serialized_font_test();
    let families = [(PRIMARY, false), (FALLBACK, true)];
    let mut base_stack = stack(&families, 1.0);
    let mut heavy_stack = stack(&families, HEAVY);
    let fallback_index = base_stack
        .font_index_for_test(FALLBACK)
        .expect("tracked Roboto fallback handle must resolve");
    assert!(fallback_index > 0, "Roboto fallback must follow the configured Rec Mono handle");

    // Address Roboto directly so primary coverage cannot hide the selected fallback's behavior.
    let glyph_id = base_stack
        .glyph_id_for_family_for_test(FALLBACK, 'm')
        .expect("tracked Roboto fixture must contain m");
    let key = GlyphKey {
        ch: 'm',
        font_slot: u8::try_from(fallback_index).expect("fixture handle fits GlyphKey"),
        weight_bold: false,
        italic: false,
        glyph_id,
        raster_variant: sonicterm_types::GlyphRasterVariant::Normal,
    };
    let base = base_stack.rasterize(key).expect("base fallback tile");
    let heavy = heavy_stack.rasterize(key).expect("heavy fallback tile");
    assert_eq!(base.width, heavy.width);
    assert_eq!(base.height, heavy.height);
    assert_eq!(base.offset_x, heavy.offset_x);
    assert_eq!(base.offset_y, heavy.offset_y);
    assert_eq!(base.advance, heavy.advance);
    assert_ne!(base.coverage, heavy.coverage, "selected fallback must be reweighted");
}

// The primary family receives real ink growth without changes to its native tile placement.
#[test]
fn the_configured_family_adds_ink_without_moving_any_geometry() {
    let _serial = serialized_font_test();
    let families = [(PRIMARY, false), (FALLBACK, true)];
    let mut base_stack = stack(&families, 1.0);
    let mut heavy_stack = stack(&families, HEAVY);
    assert_eq!(resolved_handle(&base_stack, '\u{e0b0}'), 0, "tracked Rec Mono primary owns PUA");

    let base = facts(&mut base_stack, '\u{e0b0}');
    let heavy = facts(&mut heavy_stack, '\u{e0b0}');
    assert_eq!((heavy.width, heavy.height), (base.width, base.height));
    assert_eq!((heavy.offset_x, heavy.offset_y), (base.offset_x, base.offset_y));
    assert_eq!(heavy.advance, base.advance);
    assert!(heavy.ink > base.ink, "configured-family weight must add real ink");
}

// Both thinning and thickening preserve every tile geometry field at the supported endpoints.
#[test]
fn configured_family_geometry_is_fixed_across_the_weight_range() {
    let _serial = serialized_font_test();
    let families = [(PRIMARY, false), (FALLBACK, true)];
    let mut identity_stack = stack(&families, 1.0);
    let identity = facts(&mut identity_stack, '\u{e0b0}');

    for scale in [0.5, 0.75, 1.0, 1.5, 3.0, 5.0] {
        let mut candidate_stack = stack(&families, scale);
        let candidate = facts(&mut candidate_stack, '\u{e0b0}');
        assert_eq!(candidate.width, identity.width, "width drifted at {scale}");
        assert_eq!(candidate.height, identity.height, "height drifted at {scale}");
        assert_eq!(candidate.offset_x, identity.offset_x, "x offset drifted at {scale}");
        assert_eq!(candidate.offset_y, identity.offset_y, "y offset drifted at {scale}");
        assert_eq!(candidate.advance, identity.advance, "advance drifted at {scale}");
        if scale < 1.0 {
            assert!(candidate.ink < identity.ink, "thin scale {scale} must remove ink");
        } else if scale > 1.0 {
            assert!(candidate.ink > identity.ink, "heavy scale {scale} must add ink");
        } else {
            assert_eq!(candidate.ink, identity.ink);
        }
    }
}

fn tile_ink(tile: &sonicterm_text::glyph_atlas::RasterTile) -> u64 {
    if tile.is_subpixel || tile.is_color {
        tile.coverage.chunks_exact(4).map(|px| u64::from(px[3])).sum()
    } else {
        tile.coverage.iter().map(|value| u64::from(*value)).sum()
    }
}

// All selected styles retain geometry while one common policy adds or removes monochrome ink.
#[test]
fn every_style_scales_ink_without_resizing_or_repositioning_glyphs() {
    let _serial = serialized_font_test();
    for dpi in [72, 90, 108, 126, 144] {
        let make_stack = |weight| {
            FontStack::try_new_with_font_dirs_for_test(
                &[(PRIMARY, false)],
                tracked_font_dirs(),
                14.5,
                dpi,
                weight,
            )
            .unwrap()
        };
        let mut identity_stack = make_stack(1.0);
        let metrics = identity_stack.cell_metrics_raster_px().unwrap();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let shaped = identity_stack.shape_text_with_style("277 H0", bold, italic).unwrap();
            for scale in [0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0] {
                let mut candidate_stack = make_stack(scale);
                assert_eq!(candidate_stack.cell_metrics_raster_px().unwrap(), metrics);
                let candidate_shape =
                    candidate_stack.shape_text_with_style("277 H0", bold, italic).unwrap();
                assert_eq!(shaped.len(), candidate_shape.len());
                for (base, candidate) in shaped.iter().zip(&candidate_shape) {
                    assert_eq!(
                        (base.x_advance, base.y_advance, base.x_offset, base.y_offset),
                        (
                            candidate.x_advance,
                            candidate.y_advance,
                            candidate.x_offset,
                            candidate.y_offset
                        )
                    );
                }
                for ch in ['2', '7', 'H', '0', '\u{e0b0}'] {
                    let key = GlyphKey::new(ch, bold, italic);
                    let base = identity_stack.rasterize(key).expect("tracked base glyph");
                    let candidate = candidate_stack.rasterize(key).expect("tracked weighted glyph");
                    assert_eq!(
                        (base.width, base.height, base.offset_x, base.offset_y, base.advance),
                        (
                            candidate.width,
                            candidate.height,
                            candidate.offset_x,
                            candidate.offset_y,
                            candidate.advance
                        ),
                        "{ch} bold={bold} italic={italic} scale={scale} dpi={dpi}"
                    );
                    let base_ink = tile_ink(&base);
                    let candidate_ink = tile_ink(&candidate);
                    if scale < 1.0 {
                        assert!(
                            candidate_ink < base_ink,
                            "thin {ch} bold={bold} italic={italic} dpi={dpi}"
                        );
                    } else if scale > 1.0 {
                        assert!(
                            candidate_ink > base_ink,
                            "heavy {ch} bold={bold} italic={italic} dpi={dpi}"
                        );
                    } else {
                        assert_eq!(candidate.coverage, base.coverage);
                    }
                }
            }
        }
    }
}

// Windows digit alignment is already correct at identity weight, independent of the adjustment stage.
#[cfg(windows)]
#[test]
fn identity_weight_preserves_native_digit_alignment_for_all_styles() {
    let _serial = serialized_font_test();
    let mut stack = FontStack::try_new_with_font_dirs_for_test(
        &[(PRIMARY, false)],
        tracked_font_dirs(),
        14.5,
        72,
        1.0,
    )
    .unwrap();
    for (bold, italic) in [(true, false), (false, false), (false, true), (true, true)] {
        let two = stack.rasterize(GlyphKey::new('2', bold, italic)).unwrap();
        let seven = stack.rasterize(GlyphKey::new('7', bold, italic)).unwrap();
        assert_eq!(two.offset_y, seven.offset_y, "bold={bold} italic={italic}");
    }
}

// Weight is an ink control, never a terminal cell-size control.
#[test]
fn cell_metrics_are_fixed_across_the_weight_range() {
    let _serial = serialized_font_test();
    let families = [(PRIMARY, false), (FALLBACK, true)];
    let identity = stack(&families, 1.0)
        .cell_metrics_raster_px()
        .expect("tracked primary fixture must provide metrics");

    for scale in [0.5, 0.75, 1.0, 1.5, 3.0, 5.0] {
        let metrics = stack(&families, scale)
            .cell_metrics_raster_px()
            .expect("tracked primary fixture must provide metrics at every weight");
        assert_eq!(metrics, identity, "cell metrics drifted at weight {scale}");
    }
}
