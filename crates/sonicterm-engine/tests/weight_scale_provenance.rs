//! Weight scaling must act only on glyphs from the configured family and keep
//! every geometry field fixed.
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

#[test]
fn a_fallback_at_index_zero_is_not_reweighted() {
    let _serial = serialized_font_test();
    let families = [(MISSING_FAMILY, false), (FALLBACK, true)];
    let mut base_stack = stack(&families, 1.0);
    let mut heavy_stack = stack(&families, HEAVY);
    assert_eq!(resolved_handle(&base_stack, 'm'), 0, "fallback must inherit handle zero");

    let base = facts(&mut base_stack, 'm');
    let heavy = facts(&mut heavy_stack, 'm');
    assert_eq!(heavy, base, "a fallback at handle zero must not change geometry or ink");
}

#[test]
fn a_fallback_after_the_primary_is_not_reweighted() {
    let _serial = serialized_font_test();
    let families = [(PRIMARY, false), (FALLBACK, true)];
    let mut base_stack = stack(&families, 1.0);
    let mut heavy_stack = stack(&families, HEAVY);
    let fallback_index = base_stack
        .font_index_for_test(FALLBACK)
        .expect("tracked Roboto fallback handle must resolve");
    assert!(fallback_index > 0, "Roboto fallback must follow the configured Rec Mono handle");

    // Address Roboto's nonzero handle directly with a glyph id from Roboto.
    // This kills a call-site mutant that asks provenance about handle 0
    // regardless of which handle actually produced the glyph.
    let glyph_id = base_stack
        .glyph_id_for_family_for_test(FALLBACK, 'm')
        .expect("tracked Roboto fixture must contain m");
    let key = GlyphKey {
        ch: 'm',
        font_slot: u8::try_from(fallback_index).expect("fixture handle fits GlyphKey"),
        weight_bold: false,
        italic: false,
        glyph_id,
    };
    let base = base_stack.rasterize(key).expect("base fallback tile");
    let heavy = heavy_stack.rasterize(key).expect("heavy fallback tile");
    assert_eq!(base.width, heavy.width);
    assert_eq!(base.height, heavy.height);
    assert_eq!(base.offset_x, heavy.offset_x);
    assert_eq!(base.offset_y, heavy.offset_y);
    assert_eq!(base.advance, heavy.advance);
    assert_eq!(base.coverage, heavy.coverage, "nonzero fallback handle must not be reweighted");
}

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
