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

/// PR #92 set the wezterm theme foreground to `#cfbc97`, but the body
/// glyph pipeline was pushing raw sRGB bytes into a `Bgra8UnormSrgb`
/// surface — the GPU then re-encoded linear→sRGB on store, brightening
/// the visible pixel to `#e9dfca`. Linearizing the per-instance color
/// (via `glyphon_color_to_linear_rgba`) round-trips back to the
/// authored `#cfbc97` within ±5/255 after the surface re-encodes.
#[test]
fn wezterm_fg_linearized_round_trips_to_authored_hex() {
    fn linear_to_srgb_u8(c: f32) -> u8 {
        let c = f64::from(c);
        let s = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (s.clamp(0.0, 1.0) * 255.0).round() as u8
    }
    let theme_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/wezterm.toml");
    let theme = sonic_core::theme::Theme::load(&theme_path).expect("load wezterm.toml");
    let (er, eg, eb) = theme.colors.foreground.rgb().expect("parse #cfbc97");
    assert_eq!((er, eg, eb), (0xcf, 0xbc, 0x97), "PR #92 hex pinned");

    let g = glyphon::Color::rgb(er, eg, eb);
    let rgba = sonic_shared::render::glyphon_color_to_linear_rgba(g);
    let r = linear_to_srgb_u8(rgba[0]);
    let gc = linear_to_srgb_u8(rgba[1]);
    let b = linear_to_srgb_u8(rgba[2]);
    assert!((i16::from(r) - i16::from(er)).abs() <= 5, "r={r:#04x} want ~{er:#04x}");
    assert!((i16::from(gc) - i16::from(eg)).abs() <= 5, "g={gc:#04x} want ~{eg:#04x}");
    assert!((i16::from(b) - i16::from(eb)).abs() <= 5, "b={b:#04x} want ~{eb:#04x}");
}
