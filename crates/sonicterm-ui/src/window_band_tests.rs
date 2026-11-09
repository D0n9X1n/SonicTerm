//! Band height, caption-button layout and resize-edge contracts on every host.

use super::*;

/// The band's centre is the traffic lights' centre while the strip fits beside them; a taller
/// strip sets the height instead. A native window has no band.
#[test]
fn the_band_centre_is_the_button_centre_while_the_strip_fits() {
    let band = WindowBand::traffic_lights(14.0, 66.0);
    assert_eq!(band.height(23.0), 28.0);
    assert_eq!(band.height(23.0) / 2.0, 14.0, "text centred in the band sits on the lights");
    assert_eq!(band.left_inset, 66.0 + TRAFFIC_LIGHT_GAP);
    assert_eq!(band.height(40.0), 40.0, "a strip taller than the buttons sets the height");
    assert_eq!(WindowBand::default().height(23.0), 0.0);
    assert_eq!(WindowBand::buttonless().height(23.0), 23.0, "full screen keeps only the strip");
}

/// Three 46-px buttons fill the band's top-right corner, minimise first, close last, and the
/// right inset is exactly their width; a traffic-light band has none.
#[test]
fn caption_buttons_fill_the_top_right_corner() {
    let band = WindowBand::caption();
    let buttons = caption_buttons(&band, 1000.0, 64.0, 2.0);
    let order: Vec<CaptionButton> = buttons.iter().map(|(button, _)| *button).collect();
    assert_eq!(order, CaptionButton::ALL);
    assert_eq!(
        buttons[0].1,
        BandRect { left: 1000.0 - 276.0, top: 0.0, width: 92.0, height: 64.0 }
    );
    assert_eq!(buttons[2].1.left + buttons[2].1.width, 1000.0);
    assert_eq!(band.right_inset * 2.0, 276.0);
    assert_eq!(caption_button_at(&band, 1000.0, 64.0, 2.0, 999.0, 1.0), Some(CaptionButton::Close));
    assert_eq!(caption_button_at(&band, 1000.0, 64.0, 2.0, 999.0, 64.0), None, "below the band");
    assert!(caption_buttons(&WindowBand::traffic_lights(14.0, 66.0), 1000.0, 56.0, 2.0).is_empty());
}

/// A self-framed window resizes from 6-px zones on every edge, corners first; a maximised window
/// and an AppKit-framed one never do.
#[test]
fn resize_edges_cover_each_side_and_corner_of_a_self_framed_window() {
    let band = WindowBand::caption();
    let surface = (800.0, 600.0);
    let edge =
        |pointer_x, pointer_y| resize_edge_at(&band, surface, 1.0, false, pointer_x, pointer_y);
    assert_eq!(edge(0.0, 0.0), Some(ResizeEdge::NorthWest));
    assert_eq!(edge(799.0, 0.0), Some(ResizeEdge::NorthEast));
    assert_eq!(edge(0.0, 599.0), Some(ResizeEdge::SouthWest));
    assert_eq!(edge(799.0, 599.0), Some(ResizeEdge::SouthEast));
    assert_eq!(edge(400.0, 5.0), Some(ResizeEdge::North));
    assert_eq!(edge(400.0, 6.0), None, "the zone is six pixels deep");
    assert_eq!(edge(400.0, 595.0), Some(ResizeEdge::South));
    assert_eq!(edge(2.0, 300.0), Some(ResizeEdge::West));
    assert_eq!(edge(797.0, 300.0), Some(ResizeEdge::East));
    assert_eq!(edge(400.0, 300.0), None);
    assert_eq!(resize_edge_at(&band, surface, 1.0, true, 0.0, 0.0), None, "maximised");
    let mac = WindowBand::traffic_lights(14.0, 66.0);
    assert_eq!(resize_edge_at(&mac, surface, 1.0, false, 0.0, 0.0), None, "AppKit frames it");
}
