//! The integrated titlebar band: its size and insets, the self-drawn caption buttons and the
//! window-edge resize zones, as pure geometry. The renderer draws from it and the App hit-tests
//! with it; neither reads the operating system here, so every host tests both platforms' layouts.

/// Width of one self-drawn caption button, in logical px: the Windows 11 caption button width.
pub const CAPTION_BUTTON_WIDTH: f32 = 46.0;

/// Height of the self-drawn caption buttons, in logical px: the Windows 11 caption height.
pub const CAPTION_BUTTON_HEIGHT: f32 = 32.0;

/// Width of the zone along each window edge that resizes an undecorated window, in logical px.
pub const RESIZE_EDGE: f32 = 6.0;

/// Space between the traffic lights' right edge and the first left-list segment, in logical px.
pub const TRAFFIC_LIGHT_GAP: f32 = 8.0;

/// Which window buttons the band makes room for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BandButtons {
    /// No buttons in the band: full screen, or a platform with no integrated titlebar.
    #[default]
    None,
    /// macOS's traffic lights, which AppKit draws on the left.
    TrafficLights,
    /// Minimise, maximise and close, which SonicTerm draws on the right.
    Caption,
}

/// The window edge that holds the platform's window buttons. It decides which side of the band the
/// status segments may use: Windows draws its caption buttons on the right, macOS its traffic
/// lights on the left. It follows the platform, not the band's current buttons, so segments stay
/// put when full screen hides the buttons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ButtonEdge {
    /// Buttons on the left edge (macOS traffic lights): segments keep their configured sides.
    #[default]
    Left,
    /// Buttons on the right edge (Windows caption buttons): right-list segments move to the left.
    Right,
}

/// An integrated window's titlebar band, in logical px. The default is no band: a native titlebar.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WindowBand {
    /// The band exists: the window's titlebar is integrated.
    pub integrated: bool,
    /// Height the buttons need; the band is at least this tall, and its centre line is theirs.
    pub button_height: f32,
    /// Width reserved at the left edge (traffic lights plus gap).
    pub left_inset: f32,
    /// Width reserved at the right edge (caption buttons).
    pub right_inset: f32,
    /// The buttons the band holds.
    pub buttons: BandButtons,
}

impl WindowBand {
    /// The macOS band: `close_center_y` is the traffic lights' vertical centre and `zoom_right` the
    /// zoom button's right edge, both in content coordinates from AppKit. The band is twice the
    /// centre tall, so its centre line is the lights'.
    pub fn traffic_lights(close_center_y: f32, zoom_right: f32) -> Self {
        Self {
            integrated: true,
            button_height: (2.0 * close_center_y).max(0.0),
            left_inset: zoom_right + TRAFFIC_LIGHT_GAP,
            right_inset: 0.0,
            buttons: BandButtons::TrafficLights,
        }
    }

    /// The Windows band: three caption buttons on the right, [`CAPTION_BUTTON_HEIGHT`] tall.
    pub fn caption() -> Self {
        Self {
            integrated: true,
            button_height: CAPTION_BUTTON_HEIGHT,
            left_inset: 0.0,
            right_inset: 3.0 * CAPTION_BUTTON_WIDTH,
            buttons: BandButtons::Caption,
        }
    }

    /// An integrated window with no buttons in its band, such as one in full screen.
    pub fn buttonless() -> Self {
        Self { integrated: true, ..Self::default() }
    }

    /// The band's height in logical px for a strip `strip_height` tall: the taller of the two, so
    /// the band's centre is the buttons' centre whenever the strip fits beside them.
    pub fn height(&self, strip_height: f32) -> f32 {
        if self.integrated {
            self.button_height.max(strip_height)
        } else {
            // When: integrated is false, the titlebar is native and there is no band.
            0.0
        }
    }
}

/// One self-drawn caption button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptionButton {
    /// Minimise the window.
    Minimize,
    /// Maximise the window, or restore it when maximised.
    Maximize,
    /// Close the window.
    Close,
}

impl CaptionButton {
    /// Every caption button, left to right.
    pub const ALL: [Self; 3] = [Self::Minimize, Self::Maximize, Self::Close];
}

/// A caption button's state, which sets its fill.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CaptionState {
    /// The button under the pointer.
    pub hovered: Option<CaptionButton>,
    /// The button pressed and not yet released.
    pub pressed: Option<CaptionButton>,
    /// The window is maximised, so the middle button restores.
    pub maximized: bool,
    /// The window is focused; an unfocused window draws its glyphs dimmer.
    pub focused: bool,
}

/// A rectangle in the renderer's raster px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandRect {
    /// Left edge.
    pub left: f32,
    /// Top edge.
    pub top: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl BandRect {
    /// Whether `(pointer_x, pointer_y)` is inside the rectangle; the right and bottom edges are
    /// outside.
    pub fn contains(&self, pointer_x: f32, pointer_y: f32) -> bool {
        pointer_x >= self.left
            && pointer_x < self.left + self.width
            && pointer_y >= self.top
            && pointer_y < self.top + self.height
    }
}

/// The three caption buttons' rectangles on a `surface_width` raster-px surface at `scale`, at the
/// band's top-right corner, `band_height` raster px tall; empty for a band with other buttons.
pub fn caption_buttons(
    band: &WindowBand,
    surface_width: f32,
    band_height: f32,
    scale: f32,
) -> Vec<(CaptionButton, BandRect)> {
    if band.buttons != BandButtons::Caption {
        // When: the band holds traffic lights or nothing, SonicTerm draws no caption buttons.
        return Vec::new();
    }
    let width = (CAPTION_BUTTON_WIDTH * scale).round();
    let first = surface_width - width * CaptionButton::ALL.len() as f32;
    CaptionButton::ALL
        .into_iter()
        .enumerate()
        .map(|(index, button)| {
            let rect = BandRect {
                left: first + width * index as f32,
                top: 0.0,
                width,
                height: band_height,
            };
            (button, rect)
        })
        .collect()
}

/// The caption button under raster-px `(pointer_x, pointer_y)`.
pub fn caption_button_at(
    band: &WindowBand,
    surface_width: f32,
    band_height: f32,
    scale: f32,
    pointer_x: f32,
    pointer_y: f32,
) -> Option<CaptionButton> {
    caption_buttons(band, surface_width, band_height, scale)
        .into_iter()
        .find_map(|(button, rect)| rect.contains(pointer_x, pointer_y).then_some(button))
}

/// A window edge or corner an undecorated window resizes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResizeEdge {
    /// The top edge.
    North,
    /// The bottom edge.
    South,
    /// The right edge.
    East,
    /// The left edge.
    West,
    /// The top-right corner.
    NorthEast,
    /// The top-left corner.
    NorthWest,
    /// The bottom-right corner.
    SouthEast,
    /// The bottom-left corner.
    SouthWest,
}

/// The edge or corner raster-px `(pointer_x, pointer_y)` resizes on a `(width, height)` surface at `scale`, for a
/// window SonicTerm frames itself; `None` inside, and always `None` while `maximized`, since a
/// maximised window has no edge to drag. Caption buttons sit inside the top zone; the caller tests
/// them first.
pub fn resize_edge_at(
    band: &WindowBand,
    surface: (f32, f32),
    scale: f32,
    maximized: bool,
    pointer_x: f32,
    pointer_y: f32,
) -> Option<ResizeEdge> {
    if band.buttons != BandButtons::Caption || maximized {
        // When: buttons is not Caption (AppKit frames it) or maximized, no edge resizes.
        return None;
    }
    let zone = RESIZE_EDGE * scale;
    let (west, east) = (pointer_x < zone, pointer_x >= surface.0 - zone);
    let (north, south) = (pointer_y < zone, pointer_y >= surface.1 - zone);
    match (north, south, west, east) {
        (true, _, true, _) => Some(ResizeEdge::NorthWest),
        (true, _, _, true) => Some(ResizeEdge::NorthEast),
        (_, true, true, _) => Some(ResizeEdge::SouthWest),
        (_, true, _, true) => Some(ResizeEdge::SouthEast),
        (true, _, _, _) => Some(ResizeEdge::North),
        (_, true, _, _) => Some(ResizeEdge::South),
        (_, _, true, _) => Some(ResizeEdge::West),
        (_, _, _, true) => Some(ResizeEdge::East),
        _ => None,
    }
}

#[cfg(test)]
#[path = "window_band_tests.rs"]
mod window_band_tests;
