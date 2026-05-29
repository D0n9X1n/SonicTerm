//! Regression test for the Haiku review on PR #118: the `+` new-tab
//! button must draw a 28×28 rounded (radius 8) `hover_bg` background
//! while the cursor is inside it, and the plus glyph switches from
//! `secondary` to `primary` on hover.

use sonic_shared::render::{build_new_tab_button_quads, NewTabButtonColors};
use sonic_shared::tabbar_view::Rect;

const HOVER_BG: [f32; 4] = [0.10, 0.20, 0.30, 0.06];
const PRIMARY: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const SECONDARY: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

fn colors() -> NewTabButtonColors {
    NewTabButtonColors { hover_bg: HOVER_BG, primary: PRIMARY, secondary: SECONDARY }
}

#[test]
fn new_tab_button_renders_rounded_bg_on_hover() {
    let nt = Rect { x: 200.0, y: 6.0, w: 28.0, h: 28.0 };
    let mut quads = Vec::new();
    build_new_tab_button_quads(nt, true, colors(), 1000.0, 36.0, &mut quads);

    // Rounded BG + SVG-mask plus quads.
    assert!(quads.len() > 3, "hover should emit BG + mask plus quads");

    let bg = &quads[0];
    assert_eq!(bg.color, HOVER_BG, "first quad must be the hover BG color");
    assert!((bg.radius_px - 8.0).abs() < 1e-6, "BG corner radius must be 8 px");
    assert_eq!(bg.size_px, [28.0, 28.0], "BG size_px must match the 28x28 hit rect");

    // Plus mask quads pick up TEXT_PRIMARY on hover.
    for q in &quads[1..] {
        assert_eq!(q.color, PRIMARY);
        assert_eq!(q.radius_px, 0.0);
    }
}

#[test]
fn new_tab_button_no_bg_when_not_hovering() {
    let nt = Rect { x: 200.0, y: 6.0, w: 28.0, h: 28.0 };
    let mut quads = Vec::new();
    build_new_tab_button_quads(nt, false, colors(), 1000.0, 36.0, &mut quads);

    // Just the SVG-mask plus quads — no BG.
    assert!(quads.len() > 2, "idle state emits only mask plus quads");
    // Idle glyph uses TEXT_SECONDARY and no SDF rounded corners.
    for q in &quads {
        assert_eq!(q.color, SECONDARY);
        assert_eq!(q.radius_px, 0.0);
    }
}
