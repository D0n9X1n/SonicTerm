//! Caption button fills: backgrounds by state, glyph shapes, and strokes inside the button.

use super::*;

/// Plain colors so the assertions can tell glyph strokes from fills.
const COLORS: CaptionColors = CaptionColors {
    glyph: [0.9, 0.9, 0.9, 1.0],
    hover: [0.06, 0.06, 0.06, 0.06],
    pressed: [0.1, 0.1, 0.1, 0.1],
};

/// A 46 × 32 button at the origin, at scale 1.
const RECT: BandRect = BandRect { left: 0.0, top: 0.0, width: 46.0, height: 32.0 };

/// A focused window's state with `hovered` and `pressed`.
fn state(hovered: Option<CaptionButton>, pressed: Option<CaptionButton>) -> CaptionState {
    CaptionState { hovered, pressed, maximized: false, focused: true }
}

/// An idle button draws only its glyph; hover and press add a fill under it; close hovers red with
/// white strokes, the only colour the buttons use.
#[test]
fn fills_follow_hover_and_press_and_close_alone_turns_red() {
    let idle = caption_fills(CaptionButton::Minimize, RECT, &state(None, None), &COLORS, 1.0);
    assert!(idle.iter().all(|fill| fill.color == COLORS.glyph), "idle: glyph only");
    let hovered = caption_fills(
        CaptionButton::Minimize,
        RECT,
        &state(Some(CaptionButton::Minimize), None),
        &COLORS,
        1.0,
    );
    assert_eq!(hovered[0], CaptionFill { rect: RECT, color: COLORS.hover });
    let pressed = caption_fills(
        CaptionButton::Minimize,
        RECT,
        &state(Some(CaptionButton::Minimize), Some(CaptionButton::Minimize)),
        &COLORS,
        1.0,
    );
    assert_eq!(pressed[0].color, COLORS.pressed, "press wins over hover");
    let close = caption_fills(
        CaptionButton::Close,
        RECT,
        &state(Some(CaptionButton::Close), None),
        &COLORS,
        1.0,
    );
    assert_eq!(close[0].color, CLOSE_HOVER);
    assert!(close[1..].iter().all(|fill| fill.color == WHITE));
}

/// Every stroke lies inside its button and on whole pixels, at fractional scales too, so the
/// glyphs stay crisp and never bleed into a neighbour.
#[test]
fn strokes_stay_inside_the_button_on_whole_pixels() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let rect = BandRect { left: 100.0, top: 0.0, width: 46.0 * scale, height: 32.0 * scale };
        for maximized in [false, true] {
            let state = CaptionState { maximized, ..state(None, None) };
            for button in CaptionButton::ALL {
                for fill in caption_fills(button, rect, &state, &COLORS, scale) {
                    let BandRect { left, top, width, height } = fill.rect;
                    assert!(
                        left >= rect.left && left + width <= rect.left + rect.width,
                        "{button:?} {scale}"
                    );
                    assert!(
                        top >= rect.top && top + height <= rect.top + rect.height,
                        "{button:?} {scale}"
                    );
                    assert_eq!((left.fract(), top.fract()), (0.0, 0.0), "{button:?} {scale}");
                }
            }
        }
    }
}

/// Maximise draws one square and restore two; an unfocused window dims every stroke.
#[test]
fn maximise_and_restore_differ_and_unfocused_strokes_dim() {
    let square = caption_fills(CaptionButton::Maximize, RECT, &state(None, None), &COLORS, 1.0);
    assert_eq!(square.len(), 4);
    let restore = CaptionState { maximized: true, ..state(None, None) };
    assert_eq!(caption_fills(CaptionButton::Maximize, RECT, &restore, &COLORS, 1.0).len(), 6);
    let unfocused = CaptionState { focused: false, ..state(None, None) };
    let dim = caption_fills(CaptionButton::Minimize, RECT, &unfocused, &COLORS, 1.0);
    assert!(dim.iter().all(|fill| fill.color[3] < COLORS.glyph[3]));
}
