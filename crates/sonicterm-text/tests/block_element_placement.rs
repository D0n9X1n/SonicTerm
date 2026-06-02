//! #461 PR-A: block-element classifier + per-codepoint geometry coverage.
//!
//! Locks the geometry table at `crates/sonicterm-text/src/block_element_geometry.rs`
//! so a future refactor can't silently flip a codepoint to the wrong rect.
//! Covers all 32 Block Elements (U+2580..=U+259F) plus a control codepoint
//! outside the range to assert `block_element_rect` returns `None`.

use sonicterm_text::block_element_geometry::{block_element_rect, BlockGeometry};
use sonicterm_text::swash_rasterizer::{classify_symbol, SymbolFit};

const W: f32 = 10.0;
const H: f32 = 20.0;
const X: f32 = 100.0;
const Y: f32 = 200.0;

fn cell() -> ((f32, f32), (f32, f32)) {
    ((X, Y), (W, H))
}

fn rect_close(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    let eps = 1e-4;
    (a.0 - b.0).abs() < eps
        && (a.1 - b.1).abs() < eps
        && (a.2 - b.2).abs() < eps
        && (a.3 - b.3).abs() < eps
}

#[test]
fn classify_all_block_elements_returns_block_cell_fill() {
    for cp in 0x2580u32..=0x259F {
        let ch = char::from_u32(cp).unwrap();
        assert_eq!(
            classify_symbol(ch),
            SymbolFit::BlockCellFill,
            "U+{:04X} ({:?}) should classify as BlockCellFill",
            cp,
            ch
        );
    }
}

#[test]
fn classify_non_block_codepoint_returns_natural_or_other() {
    // U+2470 is outside Block Elements (Enclosed Alphanumerics).
    assert_eq!(classify_symbol(char::from_u32(0x2470).unwrap()), SymbolFit::Natural);
    // 'A' is plain text.
    assert_eq!(classify_symbol('A'), SymbolFit::Natural);
}

#[test]
fn block_element_rect_returns_none_for_non_block_codepoint() {
    let (origin, size) = cell();
    assert!(block_element_rect(char::from_u32(0x2470).unwrap(), origin, size).is_none());
    assert!(block_element_rect('A', origin, size).is_none());
    // U+2500 is Box Drawing, not Block Elements — must NOT return geometry here.
    assert!(block_element_rect(char::from_u32(0x2500).unwrap(), origin, size).is_none());
}

#[test]
fn full_block_returns_exact_cell_rect() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x2588).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X, Y, W, H)))
        }
        _ => panic!("U+2588 should be SingleRect"),
    }
}

#[test]
fn upper_half_block_is_top_half_of_cell() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x2580).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X, Y, W, H * 0.5)))
        }
        _ => panic!("U+2580 should be SingleRect"),
    }
}

#[test]
fn lower_half_block_is_bottom_half_of_cell() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x2584).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X, Y + H * 0.5, W, H * 0.5)))
        }
        _ => panic!("U+2584 should be SingleRect"),
    }
}

#[test]
fn left_half_block_is_left_half_of_cell() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x258C).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X, Y, W * 0.5, H)))
        }
        _ => panic!("U+258C should be SingleRect"),
    }
}

#[test]
fn right_half_block_is_right_half_of_cell() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x2590).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X + W * 0.5, Y, W * 0.5, H)))
        }
        _ => panic!("U+2590 should be SingleRect"),
    }
}

#[test]
fn lower_one_eighth_is_bottom_strip() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x2581).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X, Y + H * 0.875, W, H * 0.125)))
        }
        _ => panic!("U+2581 should be SingleRect"),
    }
}

#[test]
fn upper_one_eighth_is_top_strip() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x2594).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X, Y, W, H * 0.125)))
        }
        _ => panic!("U+2594 should be SingleRect"),
    }
}

#[test]
fn right_one_eighth_is_right_strip() {
    let (origin, size) = cell();
    let g = block_element_rect(char::from_u32(0x2595).unwrap(), origin, size).unwrap();
    match g {
        BlockGeometry::SingleRect(x, y, w, h) => {
            assert!(rect_close((x, y, w, h), (X + W * 0.875, Y, W * 0.125, H)))
        }
        _ => panic!("U+2595 should be SingleRect"),
    }
}

#[test]
fn shades_return_shaded_rect_with_correct_alpha() {
    let (origin, size) = cell();
    let cases = [(0x2591u32, 0.25_f32), (0x2592, 0.5), (0x2593, 0.75)];
    for (cp, expected_alpha) in cases {
        let g = block_element_rect(char::from_u32(cp).unwrap(), origin, size).unwrap();
        match g {
            BlockGeometry::ShadedRect(rect, alpha) => {
                assert!(rect_close(rect, (X, Y, W, H)));
                assert!((alpha - expected_alpha).abs() < 1e-4, "U+{:04X}", cp);
            }
            _ => panic!("U+{:04X} should be ShadedRect", cp),
        }
    }
}

#[test]
fn single_quadrants_return_single_rect_at_correct_sub_cell() {
    let (origin, size) = cell();
    let ul = (X, Y, W * 0.5, H * 0.5);
    let ur = (X + W * 0.5, Y, W * 0.5, H * 0.5);
    let ll = (X, Y + H * 0.5, W * 0.5, H * 0.5);
    let lr = (X + W * 0.5, Y + H * 0.5, W * 0.5, H * 0.5);
    let cases = [(0x2596u32, ll), (0x2597, lr), (0x2598, ul), (0x259D, ur)];
    for (cp, expected) in cases {
        let g = block_element_rect(char::from_u32(cp).unwrap(), origin, size).unwrap();
        match g {
            BlockGeometry::SingleRect(x, y, w, h) => {
                assert!(rect_close((x, y, w, h), expected), "U+{:04X}", cp);
            }
            _ => panic!("U+{:04X} should be SingleRect", cp),
        }
    }
}

#[test]
fn multi_quadrants_return_multi_rect_with_correct_membership() {
    let (origin, size) = cell();
    let ul = (X, Y, W * 0.5, H * 0.5);
    let ur = (X + W * 0.5, Y, W * 0.5, H * 0.5);
    let ll = (X, Y + H * 0.5, W * 0.5, H * 0.5);
    let lr = (X + W * 0.5, Y + H * 0.5, W * 0.5, H * 0.5);
    let cases: &[(u32, &[(f32, f32, f32, f32)])] = &[
        (0x2599, &[ul, ll, lr]),
        (0x259A, &[ul, lr]),
        (0x259B, &[ul, ur, ll]),
        (0x259C, &[ul, ur, lr]),
        (0x259E, &[ur, ll]),
        (0x259F, &[ur, ll, lr]),
    ];
    for (cp, expected_rects) in cases {
        let g = block_element_rect(char::from_u32(*cp).unwrap(), origin, size).unwrap();
        match g {
            BlockGeometry::MultiRect(rects) => {
                assert_eq!(
                    rects.len(),
                    expected_rects.len(),
                    "U+{:04X} expected {} rects, got {}",
                    cp,
                    expected_rects.len(),
                    rects.len()
                );
                for (i, exp) in expected_rects.iter().enumerate() {
                    assert!(
                        rect_close(rects[i], *exp),
                        "U+{:04X} rect[{}] mismatch: got {:?} expected {:?}",
                        cp,
                        i,
                        rects[i],
                        exp
                    );
                }
            }
            _ => panic!("U+{:04X} should be MultiRect", cp),
        }
    }
}

#[test]
fn all_32_block_elements_return_some_geometry() {
    let (origin, size) = cell();
    for cp in 0x2580u32..=0x259F {
        let ch = char::from_u32(cp).unwrap();
        let g = block_element_rect(ch, origin, size);
        assert!(g.is_some(), "U+{:04X} ({:?}) must have geometry", cp, ch);
    }
}
