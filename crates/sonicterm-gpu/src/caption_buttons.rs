//! The self-drawn Windows caption buttons: their fills and thin-stroke glyphs as plain rectangles,
//! so the drawing is pure, testable on every host, and needs no icon font.

use sonicterm_render_model::boundary::ui::window_band::{BandRect, CaptionButton, CaptionState};

use crate::quad::{px_to_ndc, QuadInstance};

/// The close button's hover fill, `#C42B1C`, the Windows 11 caption close red, as linear RGBA.
const CLOSE_HOVER: [f32; 4] = [0.552, 0.024, 0.012, 1.0];
/// A pressed close button: the hover red at 85%.
const CLOSE_PRESSED: [f32; 4] = [0.469, 0.020, 0.010, 1.0];
/// White strokes on the red close button.
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Side of a glyph's square, in logical px.
const GLYPH_SIDE: f32 = 10.0;

/// Colors the caption buttons draw with, premultiplied linear RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionColors {
    /// Glyph strokes.
    pub glyph: [f32; 4],
    /// Hover fill of minimise and maximise.
    pub hover: [f32; 4],
    /// Pressed fill of minimise and maximise.
    pub pressed: [f32; 4],
    /// Strokes of an unfocused window and of a held button: the band's quiet text tone.
    pub quiet: [f32; 4],
}

/// One rectangle to fill, in raster px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionFill {
    /// The rectangle.
    pub rect: BandRect,
    /// Premultiplied linear RGBA.
    pub color: [f32; 4],
}

/// The fills that draw `button` in `rect` at `scale`: its background when hovered or pressed, then
/// its glyph's strokes, each one raster pixel or more thick and snapped to whole pixels.
pub fn caption_fills(
    button: CaptionButton,
    rect: BandRect,
    state: &CaptionState,
    colors: &CaptionColors,
    scale: f32,
) -> Vec<CaptionFill> {
    let mut fills = Vec::new();
    let pressed = state.pressed == Some(button);
    let hovered = state.hovered == Some(button);
    let close_lit = button == CaptionButton::Close && (pressed || hovered);
    let background = match (button, pressed, hovered) {
        (CaptionButton::Close, true, _) => Some(CLOSE_PRESSED),
        (CaptionButton::Close, false, true) => Some(CLOSE_HOVER),
        (_, true, _) => Some(colors.pressed),
        (_, false, true) => Some(colors.hover),
        _ => None,
    };
    if let Some(color) = background {
        fills.push(CaptionFill { rect, color });
    }
    let glyph = if close_lit {
        // White strokes sit on the red close fill.
        WHITE
    } else if state.focused && !pressed {
        // When: state.focused and not pressed, the strokes take the full glyph colour.
        colors.glyph
    } else {
        // When: unfocused or pressed (and not close_lit), the strokes quieten like the system's.
        colors.quiet
    };
    let stroke = scale.round().max(1.0);
    let side = (GLYPH_SIDE * scale).round();
    let left = (rect.left + (rect.width - side) / 2.0).round();
    let top = (rect.top + (rect.height - side) / 2.0).round();
    let line = |left: f32, top: f32, width: f32, height: f32| CaptionFill {
        rect: BandRect { left, top, width, height },
        color: glyph,
    };
    match button {
        CaptionButton::Minimize => {
            fills.push(line(left, (rect.top + rect.height / 2.0).round(), side, stroke));
        }
        CaptionButton::Maximize if state.maximized => {
            // Restore: a back square peeking out up and right of a front square.
            let front = side - 2.0 * stroke;
            let offset = 2.0 * stroke;
            fills.extend(outline(left, top + offset, front, stroke, glyph));
            fills.push(line(left + offset, top, front, stroke));
            fills.push(line(left + side - stroke, top, stroke, front));
        }
        CaptionButton::Maximize => fills.extend(outline(left, top, side, stroke, glyph)),
        CaptionButton::Close => {
            // Two diagonals as a run of one-stroke squares, one per raster step along each.
            let steps = side as usize;
            for step in 0..steps {
                let offset = step as f32;
                fills.push(line(left + offset, top + offset, stroke, stroke));
                fills.push(line(left + side - stroke - offset, top + offset, stroke, stroke));
            }
        }
    }
    fills
}

/// The four sides of a `side`-px square at `(left, top)`, `stroke` px thick.
fn outline(left: f32, top: f32, side: f32, stroke: f32, color: [f32; 4]) -> [CaptionFill; 4] {
    let fill = |left, top, width, height| CaptionFill {
        rect: BandRect { left, top, width, height },
        color,
    };
    [
        fill(left, top, side, stroke),
        fill(left, top + side - stroke, side, stroke),
        fill(left, top, stroke, side),
        fill(left + side - stroke, top, stroke, side),
    ]
}

/// Push `fills` as quads on a `screen` raster-px surface.
pub(crate) fn push_fills(fills: &[CaptionFill], screen: (f32, f32), quads: &mut Vec<QuadInstance>) {
    for fill in fills {
        let BandRect { left, top, width, height } = fill.rect;
        quads.push(QuadInstance {
            rect: px_to_ndc(left, top, width, height, screen.0, screen.1),
            color: fill.color,
            ..Default::default()
        });
    }
}

#[cfg(test)]
#[path = "caption_buttons_tests.rs"]
mod caption_buttons_tests;
