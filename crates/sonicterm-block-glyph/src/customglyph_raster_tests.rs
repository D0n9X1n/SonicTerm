//! Collapsed and positive spinner-hole clearing preserve the polygon raster contract.

use super::*;
use crate::lib_tests::RasterCase;

/// Build only the metric fields the polygon seam reads, with fixed remaining
/// fields so the no-op and continuation checks cannot depend on font discovery.
fn spinner_test_metrics(case: RasterCase) -> RenderMetrics {
    RenderMetrics {
        descender: PixelLength::new(0.0),
        descender_row: 0,
        descender_plus_two: 0,
        underline_height: case.underline,
        strike_row: 0,
        cell_size: Size::new(case.width, case.height),
    }
}

/// Clearing a collapsed hole must preserve every existing byte, not clear the
/// tile or return from the entire polygon list. A subsequent full-cell clear in
/// the same list must still execute. The tested radii are -0.5 and 0 before hinting;
/// both become -0.5 and contribute no geometry. No stored raster is the oracle.
#[test]
fn collapsed_spinner_clear_is_a_noop_and_later_polys_still_run() {
    let collapsed = Poly {
        path: &[PolyCommand::Circle {
            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
        }],
        intensity: BlockAlpha::Full,
        style: PolyStyle::Fill,
    };
    let full_cell = Poly {
        path: &[
            PolyCommand::MoveTo(BlockCoord::Zero, BlockCoord::Zero),
            PolyCommand::LineTo(BlockCoord::One, BlockCoord::Zero),
            PolyCommand::LineTo(BlockCoord::One, BlockCoord::One),
            PolyCommand::LineTo(BlockCoord::Zero, BlockCoord::One),
            PolyCommand::Close,
        ],
        intensity: BlockAlpha::Full,
        style: PolyStyle::Fill,
    };
    for (width, height) in [(5, 9), (6, 12), (9, 5), (12, 6)] {
        let case = RasterCase { width, height, underline: 1 };
        let metrics = spinner_test_metrics(case);
        for anti_alias_mode in [PolyAA::AntiAlias, PolyAA::MoarPixels] {
            let mut image = Image::new(width as usize, height as usize);
            image.clear_rect(
                Rect::new(Point::new(0, 0), metrics.cell_size),
                SrgbaPixel::rgba(24, 48, 96, 255),
            );
            let before = image.bgra().to_vec();
            draw_polys(&metrics, &[collapsed], &mut image, anti_alias_mode, BlendMode::Clear);
            assert_eq!(image.bgra(), before.as_slice(), "{case:?}: empty clear changed pixels");
            draw_polys(
                &metrics,
                &[collapsed, full_cell],
                &mut image,
                anti_alias_mode,
                BlendMode::Clear,
            );
            assert!(
                image.bgra().iter().all(|&byte| byte == 0),
                "{case:?}: the polygon after the empty clear did not run"
            );
        }
    }
}

/// The guard must not discard all circle clears. At 7x14/1 the inner radius is
/// 0.5, entirely inside texel (3,6); it reduces that texel's alpha and leaves the
/// distant corner untouched. This pins the positive side of the same boundary.
#[test]
fn positive_spinner_hole_still_clears_pixels() {
    let case = RasterCase { width: 7, height: 14, underline: 1 };
    let metrics = spinner_test_metrics(case);
    let hole = Poly {
        path: &[PolyCommand::Circle {
            center: (BlockCoord::Frac(1, 2), BlockCoord::Frac(1, 2)),
            radius: BlockCoord::FracWithOffset(1, 2, LineScale::Mul(-3)),
        }],
        intensity: BlockAlpha::Full,
        style: PolyStyle::Fill,
    };
    let mut image = Image::new(7, 14);
    image.clear_rect(
        Rect::new(Point::new(0, 0), metrics.cell_size),
        SrgbaPixel::rgba(255, 255, 255, 255),
    );
    draw_polys(&metrics, &[hole], &mut image, PolyAA::AntiAlias, BlendMode::Clear);
    assert!(image.bgra()[(6 * 7 + 3) * 4 + 3] < 255, "the positive hole was skipped");
    assert_eq!(image.bgra()[3], 255, "clearing the hole changed a distant corner");
}
