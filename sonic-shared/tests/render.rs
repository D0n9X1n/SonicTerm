use sonic_core::{
    grid::{Cell, CellFlags, Color, Grid},
    hyperlink::HyperlinkId,
};

use sonic_shared::render::collect_hyperlink_runs;

#[test]
fn collect_hyperlink_runs_coalesces_three_contiguous_cells() {
    let mut g = Grid::new(8, 1);
    let hid = HyperlinkId(42);
    for c in 0..3u16 {
        g.row_mut(0)[c as usize] = Cell {
            ch: 'x',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            hyperlink: Some(hid),
            extras: None,
        };
    }
    let runs = collect_hyperlink_runs(&g);
    assert_eq!(runs, vec![(0u16, 0u16, 2u16)]);
}

#[test]
fn collect_hyperlink_runs_splits_on_different_id() {
    let mut g = Grid::new(6, 1);
    let a = HyperlinkId(1);
    let b = HyperlinkId(2);
    for (c, h) in [(0usize, a), (1, a), (3, b), (4, b)] {
        g.row_mut(0)[c] = Cell {
            ch: 'x',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            hyperlink: Some(h),
            extras: None,
        };
    }
    let runs = collect_hyperlink_runs(&g);
    assert_eq!(runs, vec![(0, 0, 1), (0, 3, 4)]);
}

#[test]
fn collect_hyperlink_runs_empty_when_no_links() {
    let g = Grid::new(4, 2);
    assert!(collect_hyperlink_runs(&g).is_empty());
}

/// Gamma path sanity: the wezterm bg `#141617` must survive sRGB→linear
/// conversion without being crushed to near-black. When the GPU surface
/// re-encodes linear → sRGB at write time, the on-screen pixel should
/// come back to within ±1/255 of the original sRGB bytes (20, 22, 23).
///
/// We round-trip through `hex_to_wgpu` (which linearizes for the sRGB
/// surface clear color) and then re-encode by the sRGB OETF, comparing
/// to the original 8-bit channel. This is a regression test for the
/// "background looks near-black" visual delta vs WezTerm.
#[test]
fn wezterm_bg_survives_gamma_roundtrip() {
    fn linear_to_srgb_u8(c: f64) -> u8 {
        let s = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (s.clamp(0.0, 1.0) * 255.0).round() as u8
    }
    let c = sonic_shared::render::hex_to_wgpu("#141617");
    let r = linear_to_srgb_u8(c.r);
    let g = linear_to_srgb_u8(c.g);
    let b = linear_to_srgb_u8(c.b);
    assert!((r as i16 - 0x14).abs() <= 1, "r={r:#04x} expected ~0x14");
    assert!((g as i16 - 0x16).abs() <= 1, "g={g:#04x} expected ~0x16");
    assert!((b as i16 - 0x17).abs() <= 1, "b={b:#04x} expected ~0x17");
}
