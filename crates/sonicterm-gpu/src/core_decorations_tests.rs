//! Renderer decorations contracts, sharing one root fixture owner.

use super::*;

#[test]
fn broadcast_warning_keeps_red_highlighting_without_label_text() {
    // Production uses the overlay layer so terminal ink cannot cover safety edges; no banner glyphs remain.
    let source: String =
        crate::core::CORE_SOURCE.chars().filter(|ch| !ch.is_whitespace()).collect();
    let render: String =
        concat!(include_str!("core_assemble.rs"), include_str!("core_assemble_decorations.rs"))
            .split_whitespace()
            .collect();
    assert!(source.contains("self.assemble_decorations("));
    assert!(!render.contains("BROADCAST"), "broadcast chrome must not emit warning text");
    // The overlay quads are the frame scratch's, reborrowed from the pass's lease.
    assert!(render.contains("emit_broadcast_borders(&mut*quads_overlay,"));
    assert!(render.contains("theme.colors.bright.red"));
    assert!(render.contains("broadcast_participants_hash"));
}

#[test]
fn broadcast_borders_outline_only_participants_with_thin_edges() {
    // Both participants receive four exact 2px edges; the unrelated pane and interior stay untouched.
    let panes = [
        (1, PaneRect::new(10.0, 20.0, 100.0, 80.0)),
        (2, PaneRect::new(110.0, 20.0, 100.0, 80.0)),
        (3, PaneRect::new(210.0, 20.0, 100.0, 80.0)),
    ];
    let warning = [1.0, 0.0, 0.0, 1.0];
    let mut quads = Vec::new();
    emit_broadcast_borders(&mut quads, &panes, &[1, 2], warning, 400.0, 200.0);
    assert_eq!(quads.len(), 8);
    for (index, x) in [10.0, 110.0].into_iter().enumerate() {
        let expected = [
            px_to_ndc(x, 20.0, 100.0, 2.0, 400.0, 200.0),
            px_to_ndc(x, 98.0, 100.0, 2.0, 400.0, 200.0),
            px_to_ndc(x, 22.0, 2.0, 76.0, 400.0, 200.0),
            px_to_ndc(x + 98.0, 22.0, 2.0, 76.0, 400.0, 200.0),
        ];
        for (quad, rect) in quads[index * 4..index * 4 + 4].iter().zip(expected) {
            assert_eq!(quad.rect, rect);
            assert_eq!(quad.color, warning);
        }
    }
    quads.clear();
    emit_broadcast_borders(&mut quads, &panes, &[], warning, 400.0, 200.0);
    assert!(quads.is_empty());
}

#[test]
fn broadcast_borders_fit_tiny_panes_and_skip_empty_rectangles() {
    // A 1px-wide pane clamps all edges to 0.5px without crossing its bounds or overlapping corners.
    let panes = [
        (1, PaneRect::new(10.0, 20.0, 1.0, 3.0)),
        (2, PaneRect::new(30.0, 20.0, 0.0, 30.0)),
        (3, PaneRect::new(40.0, 20.0, 30.0, 0.0)),
    ];
    let mut quads = Vec::new();
    emit_broadcast_borders(&mut quads, &panes, &[1, 2, 3], [1.0, 0.0, 0.0, 1.0], 100.0, 100.0);
    let expected = [
        px_to_ndc(10.0, 20.0, 1.0, 0.5, 100.0, 100.0),
        px_to_ndc(10.0, 22.5, 1.0, 0.5, 100.0, 100.0),
        px_to_ndc(10.0, 20.5, 0.5, 2.0, 100.0, 100.0),
        px_to_ndc(10.5, 20.5, 0.5, 2.0, 100.0, 100.0),
    ];
    assert_eq!(quads.len(), 4);
    assert_eq!(quads.iter().map(|quad| quad.rect).collect::<Vec<_>>(), expected);
}

/// Curly underline geometry must preserve its resolved color while joining alternating segments.
#[test]
fn curly_underline_segments_preserve_resolved_color() {
    let mut cell = Cell::plain('x', Color::Rgb(0, 255, 255), Color::Default, CellFlags::UNDERLINE);
    cell.set_underline_style(UnderlineStyle::Curly);
    let (style, resolved) = underline_key(&cell).expect("visible underline has a resolved color");
    assert_eq!(resolved, cell.fg, "missing SGR 58 must fall back to foreground");
    let color = [0.0, 1.0, 1.0, 1.0];
    let surface = (100.0, 80.0);
    let mut quads = Vec::new();

    push_underline_quads(
        &mut quads, style, 10.0, 20.0, 24.0, 12.0, 2.0, surface.0, surface.1, color,
    );

    assert!(quads.len() >= 2);
    let mut previous_end: Option<[f32; 2]> = None;
    let mut previous_slope: Option<f32> = None;
    for quad in quads {
        assert_eq!(quad.color, color);
        assert!(quad.line_thickness_px > 0.0);
        let (x, y, w, h) =
            crate::wezterm_pipeline::ndc_rect_to_pixels(quad.rect, surface.0, surface.1)
                .expect("curly segment has drawable geometry");
        let center = [x + w * 0.5, y + h * 0.5];
        let start = [center[0] + quad.line_a[0], center[1] + quad.line_a[1]];
        let end = [center[0] + quad.line_b[0], center[1] + quad.line_b[1]];
        if let Some(previous_end) = previous_end {
            assert!((start[0] - previous_end[0]).abs() < 0.001);
            assert!((start[1] - previous_end[1]).abs() < 0.001);
        }
        let slope = end[1] - start[1];
        if let Some(previous_slope) = previous_slope {
            assert!(slope * previous_slope < 0.0, "adjacent curl segments must alternate slope");
        }
        previous_end = Some(end);
        previous_slope = Some(slope);
    }
}

/// Styled whitespace continues the decoration; unstyled whitespace remains a deliberate break.
#[test]
fn underline_key_preserves_styled_spaces() {
    let mut blank = Cell::plain(' ', Color::Indexed(1), Color::Default, CellFlags::UNDERLINE);
    blank.set_underline_style(UnderlineStyle::Dashed);
    assert_eq!(underline_key(&blank), Some((UnderlineStyle::Dashed, Color::Indexed(1))));
    blank.flags.remove(CellFlags::UNDERLINE);
    assert_eq!(underline_key(&blank), None);

    let underlined = Cell::plain('x', Color::Indexed(1), Color::Default, CellFlags::UNDERLINE);
    assert_eq!(underline_key(&underlined), Some((UnderlineStyle::Single, Color::Indexed(1))));
}

#[test]
fn inverse_swaps_foreground_and_background_for_rendering() {
    let theme = Theme::default();
    let cell = Cell::plain('x', Color::Indexed(1), Color::Indexed(2), CellFlags::INVERSE);
    assert_eq!(cell_fg(&cell, &theme, ChromeColor::WHITE), indexed(2, &theme).unwrap());
    assert_eq!(
        cell_bg_rgba(&cell, &theme),
        Some(chrome_color_to_linear_rgba(indexed(1, &theme).unwrap()))
    );
}
