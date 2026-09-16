use super::*;
use crate::locator::{FontDataHandle, FontOrigin};
use std::path::PathBuf;

fn tracked_rasterizer(style: &str) -> DirectWriteRasterizer {
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../assets/fonts/RecMonoSt.Helens-{style}.ttf")),
        ),
        index: 0,
        variation: 0,
        origin: FontOrigin::FontDirs,
        coverage: None,
    };
    let font = ParsedFont::from_locator(&handle).expect("tracked font parses");
    DirectWriteRasterizer::from_locator(&font, DisplayPixelGeometry::RGB)
        .expect("native DirectWrite must load the tracked face")
}

fn main_ink_rows(glyph: &RasterizedGlyph) -> (i32, i32) {
    let rows: Vec<_> = glyph
        .data
        .chunks_exact(glyph.width * 4)
        .enumerate()
        .filter(|(_, row)| row.chunks_exact(4).any(|pixel| pixel[3] >= 128))
        .map(|(row, _)| row as i32 - glyph.bearing_y.get() as i32)
        .collect();
    (*rows.first().expect("visible ink"), *rows.last().unwrap())
}

// Equal-height digit outlines share ink rows at a common baseline across native styles and raster DPI.
#[test]
fn directwrite_digits_preserve_outline_alignment_across_styles_and_dpi() {
    for style in ["Bold", "Regular", "Italic", "BoldItalic"] {
        let rasterizer = tracked_rasterizer(style);
        let ids = rasterizer.face.glyph_indices(&['2' as u32, '7' as u32]).unwrap();
        assert!(ids.iter().all(|id| *id != 0));
        for size in [14.5, 15.0, 13.0, 14.0, 16.0] {
            for dpi in [72, 90, 108, 126, 144] {
                let two = rasterizer.rasterize_directwrite_glyph(ids[0].into(), size, dpi).unwrap();
                let seven =
                    rasterizer.rasterize_directwrite_glyph(ids[1].into(), size, dpi).unwrap();
                if size == 14.5 && dpi == 72 {
                    // The tracked digits' 720-unit tops at 14.5px cover row -11 without hint-driven contraction.
                    assert_eq!(main_ink_rows(&two), (-11, -1), "unfitted {style} outline");
                }
                assert_eq!(
                    main_ink_rows(&two),
                    main_ink_rows(&seven),
                    "{style} size={size} dpi={dpi}"
                );
            }
        }
    }
}

// Blank glyphs and invalid requests retain explicit native outcomes rather than passing through fallback.
#[test]
fn directwrite_blank_and_invalid_glyph_requests_are_bounded() {
    let rasterizer = tracked_rasterizer("Regular");
    let space = rasterizer.face.glyph_indices(&[' ' as u32]).unwrap()[0];
    let blank = rasterizer.rasterize_directwrite_glyph(space.into(), 14.5, 72).unwrap();
    assert!(blank.data.is_empty());
    assert!(rasterizer.rasterize_directwrite_glyph(u32::MAX, 14.5, 72).is_err());
    for size in [f64::NAN, f64::INFINITY, 0.0, -1.0, 4096.0] {
        assert!(rasterizer.rasterize_directwrite_glyph(space.into(), size, 72).is_err());
    }
}

/// DirectWrite coverage stays native when the configured weight scale is identity.
#[test]
fn directwrite_coverage_preserves_native_channels() {
    assert_eq!(directwrite_coverage_pixel([0, 128, 255]), [0, 128, 255, 255]);
    assert_eq!(directwrite_coverage_pixel([1, 64, 254]), [1, 64, 254, 254]);
}
