//! Geometry invariants for the `from_char` mapping that need no stored data.
//!
//! Every expected value here comes from the Unicode charts and from the cell
//! geometry each glyph is specified to cover, never from reading the rasterizer
//! back. A stroke, centering, coverage, join, Braille, or Powerline change larger
//! than an invariant's stated tolerance fails that invariant as well as the reviewed
//! digest table in `lib_tests.rs`; a change within every tolerance fails only the
//! digests.

use super::*;
use crate::lib_tests::{rasterize, AlphaRaster, RasterCase, RASTER_CASES};

/// The block key `from_char` maps `c` to; panics naming `c` when it is unmapped.
fn key(c: char) -> BlockKey {
    BlockKey::from_char(c).unwrap_or_else(|| panic!("U+{:04X} is not mapped", c as u32))
}

/// Case sizes of at least 8x16. Below that a heavy stroke spans most of the cell,
/// so edge profiles and outline areas no longer separate reached from unreached
/// edges or strokes from fills.
fn roomy_cases() -> impl Iterator<Item = RasterCase> {
    RASTER_CASES.into_iter().filter(|case| case.w >= 8 && case.h >= 16)
}

/// Alpha-weighted centre of `profile`, measuring texel `i` at `i + 0.5`.
fn centroid(profile: &[u8]) -> f64 {
    let total: f64 = profile.iter().map(|&a| f64::from(a)).sum();
    let moment: f64 =
        profile.iter().enumerate().map(|(i, &a)| (i as f64 + 0.5) * f64::from(a)).sum();
    moment / total
}

/// First and last codepoint of each contiguous range `from_char` maps, so a
/// mapping that silently shrinks cannot also shrink the sweep below.
const MAPPED_RANGE_ENDS: [char; 14] = [
    '\u{2500}',
    '\u{259F}',
    '\u{2800}',
    '\u{28FF}',
    '\u{1FB00}',
    '\u{1FBAF}',
    '\u{1CD00}',
    '\u{1CDE5}',
    '\u{E0B0}',
    '\u{E0BF}',
    '\u{EE00}',
    '\u{EE0B}',
    '\u{F5D0}',
    '\u{F60D}',
];

/// Every codepoint `from_char` maps rasterizes at the renderer's two common cell
/// sizes, 8x16 with a 1-texel underline and 16x32 with a 2-texel underline: at the
/// requested dimensions, with full storage, zero offsets, an advance equal to the
/// width (all checked by `rasterize`), and visible ink. Only U+2800, the empty
/// Braille pattern, is blank by design; any other blank tile is a failure.
#[test]
fn every_mapped_codepoint_rasterizes_at_requested_size() {
    for c in MAPPED_RANGE_ENDS {
        assert!(BlockKey::from_char(c).is_some(), "U+{:04X} is no longer mapped", c as u32);
    }
    let mapped: Vec<(char, BlockKey)> = (0..=u32::from(char::MAX))
        .filter_map(char::from_u32)
        .filter_map(|c| BlockKey::from_char(c).map(|block| (c, block)))
        .collect();
    assert!(mapped.len() > 900, "only {} codepoints are mapped", mapped.len());

    let mut wrong = Vec::new();
    for case in [RASTER_CASES[1], RASTER_CASES[3]] {
        for &(c, block) in &mapped {
            let blank = rasterize(block, case).alpha.iter().all(|&a| a == 0);
            if blank != (c == '\u{2800}') {
                wrong.push(format!("U+{:04X} at {case:?}: blank={blank}", c as u32));
            }
        }
    }
    assert!(wrong.is_empty(), "only U+2800 may be blank, and it must be:\n{}", wrong.join("\n"));
}

/// U+2588 FULL BLOCK fills the whole cell with an unanti-aliased rectangle, so
/// every texel is fully opaque at every size.
#[test]
fn full_block_is_fully_opaque() {
    for case in RASTER_CASES {
        let raster = rasterize(key('\u{2588}'), case);
        assert!(raster.alpha.iter().all(|&a| a == 255), "U+2588 at {case:?} is not fully opaque");
    }
}

/// U+2591, U+2592, and U+2593 fill the whole cell at 25%, 50%, and 75% alpha:
/// each shade is uniform, its mean is within 1 of that fraction of 255, and the
/// three shades strictly increase.
#[test]
fn shades_have_uniform_quarter_step_alpha() {
    for case in RASTER_CASES {
        let mut previous = 0.0f64;
        for (c, quarters) in [('\u{2591}', 1.0f64), ('\u{2592}', 2.0), ('\u{2593}', 3.0)] {
            let raster = rasterize(key(c), case);
            let name = format!("U+{:04X} at {case:?}", c as u32);
            let first = raster.alpha[0];
            assert!(raster.alpha.iter().all(|&a| a == first), "{name} is not uniform");
            let mean = raster.sum() as f64 / raster.alpha.len() as f64;
            let expected = 255.0 * quarters / 4.0;
            assert!((mean - expected).abs() <= 1.0, "{name} has mean {mean}, not {expected}");
            assert!(mean > previous, "{name} is not darker than the lighter shade");
            previous = mean;
        }
    }
}

/// ─ and ━ run the full cell width and │ and ┃ the full height, so every cross
/// section of one line matches the first within 1. Each line's centroid lies
/// within half a texel, plus rounding slack, of the cell midline, because hinting
/// may move an integral midline by half a texel. A cross section carries one
/// underline of ink for a light line and 3.01 underlines for a heavy one, within
/// the quarter-texel sampling of an anti-aliased edge.
#[test]
fn straight_lines_are_centered_and_uniform() {
    for case in RASTER_CASES {
        for (c, horizontal, weight) in [
            ('\u{2500}', true, 1.0f64),
            ('\u{2501}', true, 3.01),
            ('\u{2502}', false, 1.0),
            ('\u{2503}', false, 3.01),
        ] {
            let raster = rasterize(key(c), case);
            let name = format!("U+{:04X} at {case:?}", c as u32);
            let sections: Vec<Vec<u8>> = if horizontal {
                (0..raster.w).map(|x| raster.column(x)).collect()
            } else {
                (0..raster.h).map(|y| raster.row(y)).collect()
            };
            let first = &sections[0];
            for section in &sections {
                let same = section.iter().zip(first).all(|(&a, &b)| a.abs_diff(b) <= 1);
                assert!(same, "{name}: cross sections differ: {section:?} vs {first:?}");
            }
            let side = if horizontal { case.h as f64 } else { case.w as f64 };
            let offset = (centroid(first) - side / 2.0).abs();
            assert!(offset <= 0.55, "{name}: centroid is {offset} texels from the midline");
            let ink = first.iter().map(|&a| f64::from(a)).sum::<f64>() / 255.0;
            let expected = weight * case.underline as f64;
            assert!((ink - expected).abs() <= 0.3, "{name}: {ink} texels thick, not {expected}");
        }
    }
}

/// A heavy line is specified as 3.01 underlines against a light line's one, so a
/// heavy line's total ink is between 2.5 and 3.5 times its light twin's at every
/// size, horizontally and vertically.
#[test]
fn heavy_strokes_are_distinct_from_light() {
    for case in RASTER_CASES {
        for (light, heavy) in [('\u{2500}', '\u{2501}'), ('\u{2502}', '\u{2503}')] {
            let light_ink = rasterize(key(light), case).sum() as f64;
            let heavy_ink = rasterize(key(heavy), case).sum() as f64;
            let ratio = heavy_ink / light_ink;
            assert!(
                (2.5..=3.5).contains(&ratio),
                "U+{:04X} at {case:?} carries {ratio} times the ink of U+{:04X}",
                heavy as u32,
                light as u32
            );
        }
    }
}

/// Corners, tees, and crosses: each glyph, whether its strokes are heavy, and
/// whether its arms reach the left, right, top, and bottom cell edges.
const JOINS: [(char, bool, [bool; 4]); 14] = [
    ('\u{250C}', false, [false, true, false, true]),
    ('\u{2510}', false, [true, false, false, true]),
    ('\u{2514}', false, [false, true, true, false]),
    ('\u{2518}', false, [true, false, true, false]),
    ('\u{251C}', false, [false, true, true, true]),
    ('\u{2524}', false, [true, false, true, true]),
    ('\u{252C}', false, [true, true, false, true]),
    ('\u{2534}', false, [true, true, true, false]),
    ('\u{253C}', false, [true, true, true, true]),
    ('\u{250F}', true, [false, true, false, true]),
    ('\u{2513}', true, [true, false, false, true]),
    ('\u{2517}', true, [false, true, true, false]),
    ('\u{251B}', true, [true, false, true, false]),
    ('\u{254B}', true, [true, true, true, true]),
];

/// A corner, tee, or cross must meet its neighbours without a step or gap: every
/// edge an arm reaches carries the profile, within 1, of the straight line of the
/// same weight at that edge, and every edge no arm reaches has alpha of at most 1.
#[test]
fn box_joins_match_straight_line_edges() {
    for case in roomy_cases() {
        for (c, heavy, reach) in JOINS {
            let (across, down) =
                if heavy { ('\u{2501}', '\u{2503}') } else { ('\u{2500}', '\u{2502}') };
            let across = rasterize(key(across), case);
            let down = rasterize(key(down), case);
            let glyph = rasterize(key(c), case);
            let (w, h) = (glyph.w, glyph.h);
            let edges = [
                ("left", glyph.column(0), across.column(0)),
                ("right", glyph.column(w - 1), across.column(w - 1)),
                ("top", glyph.row(0), down.row(0)),
                ("bottom", glyph.row(h - 1), down.row(h - 1)),
            ];
            for ((edge, actual, line), reached) in edges.into_iter().zip(reach) {
                let name = format!("U+{:04X} at {case:?}, {edge} edge", c as u32);
                if reached {
                    let same = actual.iter().zip(&line).all(|(&a, &b)| a.abs_diff(b) <= 1);
                    assert!(same, "{name} differs from the straight line: {actual:?} vs {line:?}");
                } else {
                    assert!(actual.iter().all(|&a| a <= 1), "{name} is inked: {actual:?}");
                }
            }
        }
    }
}

/// Grid position `(column, row)` of Braille dot `dot` in the Unicode Braille
/// Patterns chart: dots 1 to 3 run down the left column and 4 to 6 down the
/// right, and dots 7 and 8 form the bottom row, left then right. Bit `n` of a
/// codepoint's offset from U+2800 raises dot `n + 1`.
fn braille_dot_position(dot: u8) -> (usize, usize) {
    match dot {
        1..=3 => (0, usize::from(dot - 1)),
        4..=6 => (1, usize::from(dot - 4)),
        7 => (0, 3),
        8 => (1, 3),
        _ => panic!("a Braille cell has dots 1 to 8, not {dot}"),
    }
}

/// Dot region `(column, row)` that holds texel `(x, y)`, judged by the texel's
/// centre: the cell splits into two equal columns and four equal rows.
fn braille_region(case: RasterCase, x: usize, y: usize) -> (usize, usize) {
    let (w, h) = (case.w as usize, case.h as usize);
    (((2 * x + 1) / w).min(1), ((4 * y + 2) / h).min(3))
}

/// Offset bits whose Unicode dot regions hold any ink in `raster`.
fn occupied_dot_bits(raster: &AlphaRaster, case: RasterCase) -> u8 {
    let mut bits = 0u8;
    for y in 0..raster.h {
        for x in 0..raster.w {
            if raster.at(x, y) == 0 {
                continue;
            }
            let region = braille_region(case, x, y);
            let bit = (0..8u8)
                .find(|&bit| braille_dot_position(bit + 1) == region)
                .expect("each of the eight regions holds one dot");
            bits |= 1 << bit;
        }
    }
    bits
}

/// At 8x16 and 16x32 every Braille dot region is a whole number of texels, so the
/// single-dot pattern for each bit must ink the region Unicode assigns that bit's
/// dot, and no texel outside it.
#[test]
fn braille_bits_map_to_unicode_dot_positions() {
    for case in [RASTER_CASES[1], RASTER_CASES[3]] {
        for bit in 0..8u8 {
            let c = char::from_u32(0x2800 + (1u32 << bit)).expect("Braille codepoint");
            let raster = rasterize(key(c), case);
            let expected = braille_dot_position(bit + 1);
            let name = format!("U+{:04X} (dot {}) at {case:?}", c as u32, bit + 1);
            let mut inked = false;
            for y in 0..raster.h {
                for x in 0..raster.w {
                    if raster.at(x, y) > 0 {
                        inked = true;
                        let region = braille_region(case, x, y);
                        let outside = format!("{name} inks texel ({x}, {y}) outside its dot");
                        assert_eq!(region, expected, "{outside}");
                    }
                }
            }
            assert!(inked, "{name} drew no ink");
        }
    }
}

/// Each of the 256 Braille patterns inks exactly the dot regions of its set bits
/// at every case size, so the number of occupied regions equals the popcount of
/// its offset from U+2800, and U+2800 itself is fully transparent.
#[test]
fn braille_occupied_dots_equal_popcount() {
    for case in RASTER_CASES {
        for offset in 0..=255u8 {
            let c = char::from_u32(0x2800 + u32::from(offset)).expect("Braille codepoint");
            let occupied = occupied_dot_bits(&rasterize(key(c), case), case);
            let name = format!("U+{:04X} at {case:?}", c as u32);
            assert_eq!(occupied.count_ones(), offset.count_ones(), "{name}: wrong dot count");
            assert_eq!(occupied, offset, "{name}: ink sits in the wrong dot regions");
        }
        let blank = rasterize(key('\u{2800}'), case);
        assert!(blank.alpha.iter().all(|&a| a == 0), "U+2800 at {case:?} is not blank");
    }
}

/// At 15x31, where no Powerline vertex is hinted, the filled arrows and the
/// filled half-cell triangles cover half the cell within 2%, and the filled
/// semicircles cover five sixths within 3%: two quadratic curves from a corner to
/// the far mid-edge and on to the other corner enclose 5/6 of the cell. A texel
/// deep inside each shape is fully opaque and the texel in the corner the shape
/// leaves open is fully transparent.
#[test]
fn powerline_filled_shapes_cover_specified_area() {
    let case = RASTER_CASES[2];
    let cell = (case.w * case.h) as f64;
    // Glyph, covered fraction of the cell, relative tolerance, deep texel, open texel.
    let shapes = [
        ('\u{E0B0}', 0.5, 0.02, (1, 15), (14, 0)),
        ('\u{E0B2}', 0.5, 0.02, (13, 15), (0, 0)),
        ('\u{E0B8}', 0.5, 0.02, (1, 29), (14, 0)),
        ('\u{E0BA}', 0.5, 0.02, (13, 29), (0, 0)),
        ('\u{E0BC}', 0.5, 0.02, (1, 1), (14, 30)),
        ('\u{E0BE}', 0.5, 0.02, (13, 1), (0, 30)),
        ('\u{E0B4}', 5.0 / 6.0, 0.03, (1, 15), (14, 0)),
        ('\u{E0B6}', 5.0 / 6.0, 0.03, (13, 15), (0, 0)),
    ];
    for (c, fraction, tolerance, (deep_x, deep_y), (open_x, open_y)) in shapes {
        let raster = rasterize(key(c), case);
        let name = format!("U+{:04X} at {case:?}", c as u32);
        let area = raster.sum() as f64 / 255.0;
        let expected = fraction * cell;
        assert!(
            (area - expected).abs() <= tolerance * expected,
            "{name} covers {area} texels, not {expected}"
        );
        assert_eq!(raster.at(deep_x, deep_y), 255, "{name} is not opaque inside the shape");
        assert_eq!(raster.at(open_x, open_y), 0, "{name} is inked in its open corner");
    }
}

/// The outline arrows stroke only their two slanted edges, so the midpoint of the
/// flat edge they leave open stays transparent at every size. From 8x16 up, where
/// a stroke is thin against the cell, an outline carries less than half the ink
/// of its filled twin.
#[test]
fn powerline_outline_shapes_stroke_without_fill() {
    for case in RASTER_CASES {
        let (right, mid) = (case.w as usize - 1, case.h as usize / 2);
        for (c, flat_x) in [('\u{E0B1}', 0), ('\u{E0B3}', right)] {
            let raster = rasterize(key(c), case);
            let name = format!("U+{:04X} at {case:?}", c as u32);
            assert_eq!(raster.at(flat_x, mid), 0, "{name} is inked on its open flat edge");
        }
    }
    for case in roomy_cases() {
        for (outline, filled) in [('\u{E0B1}', '\u{E0B0}'), ('\u{E0B3}', '\u{E0B2}')] {
            let outline_ink = rasterize(key(outline), case).sum();
            let filled_ink = rasterize(key(filled), case).sum();
            assert!(
                2 * outline_ink < filled_ink,
                "U+{:04X} at {case:?} carries {outline_ink} alpha against {filled_ink} filled",
                outline as u32
            );
        }
    }
}
