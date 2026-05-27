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

    // Three quads: rounded BG + two plus-stroke quads.
    assert_eq!(quads.len(), 3, "hover should emit BG + 2 plus quads");

    let bg = &quads[0];
    assert_eq!(bg.color, HOVER_BG, "first quad must be the hover BG color");
    assert!((bg.radius_px - 8.0).abs() < 1e-6, "BG corner radius must be 8 px");
    assert_eq!(bg.size_px, [28.0, 28.0], "BG size_px must match the 28x28 hit rect");

    // Plus strokes pick up TEXT_PRIMARY on hover.
    assert_eq!(quads[1].color, PRIMARY);
    assert_eq!(quads[2].color, PRIMARY);
    // And the plus strokes are sharp rects (no SDF).
    assert_eq!(quads[1].radius_px, 0.0);
    assert_eq!(quads[2].radius_px, 0.0);
}

#[test]
fn new_tab_button_no_bg_when_not_hovering() {
    let nt = Rect { x: 200.0, y: 6.0, w: 28.0, h: 28.0 };
    let mut quads = Vec::new();
    build_new_tab_button_quads(nt, false, colors(), 1000.0, 36.0, &mut quads);

    // Just the two plus strokes — no BG.
    assert_eq!(quads.len(), 2, "idle state emits only the 2 plus quads");
    // Idle glyph uses TEXT_SECONDARY.
    assert_eq!(quads[0].color, SECONDARY);
    assert_eq!(quads[1].color, SECONDARY);
    // No SDF rounded corners on the plus strokes.
    assert_eq!(quads[0].radius_px, 0.0);
    assert_eq!(quads[1].radius_px, 0.0);
}
