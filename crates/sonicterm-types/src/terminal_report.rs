//! Values a terminal reports to the program it runs: the color scheme for DSR 996 and DEC mode
//! 2031, and the xterm 256-color palette both `OSC 4` replies and the renderer draw from.

/// Whether the active theme is dark or light, as a `CSI ? 997 ; Ps n` report states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorScheme {
    /// A dark theme, reported as `Ps = 1`.
    Dark,
    /// A light theme, reported as `Ps = 2`.
    Light,
}

impl ColorScheme {
    /// The `CSI ? 997 ; Ps n` report naming this scheme.
    #[must_use]
    pub fn report(self) -> &'static [u8] {
        match self {
            Self::Dark => b"\x1b[?997;1n",
            Self::Light => b"\x1b[?997;2n",
        }
    }
}

/// The cursor shape a DECSCUSR request names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorRequestShape {
    /// A full cell.
    Block,
    /// A line under the cell.
    Underline,
    /// A vertical bar at the cell's left edge.
    Bar,
}

/// A program's DECSCUSR request: the shape, and whether it asked for a steady cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CursorRequest {
    /// The requested shape.
    pub shape: CursorRequestShape,
    /// `true` for the even DECSCUSR values, which ask the cursor not to blink.
    pub steady: bool,
}

impl CursorRequest {
    /// The request `CSI Ps SP q` makes: `None` for 0 (the terminal's configured cursor) and for
    /// values past 6, which change nothing.
    #[must_use]
    pub fn from_decscusr(value: u16) -> Option<Option<Self>> {
        // Odd values blink and even ones are steady; each pair names one shape.
        let shape = match value {
            0 => Some(None),
            1..=6 => Some(Some(
                [CursorRequestShape::Block, CursorRequestShape::Underline, CursorRequestShape::Bar]
                    [usize::from((value - 1) / 2)],
            )),
            _ => None,
        }?;
        Some(shape.map(|shape| Self { shape, steady: value.is_multiple_of(2) }))
    }
}

/// The xterm 256-color palette entry for `index` 16..=255: the 6x6x6 cube, then the gray ramp.
/// `None` for 0..=15, which come from the theme's ANSI colors.
#[must_use]
pub fn indexed_rgb(index: u8) -> Option<(u8, u8, u8)> {
    match index {
        0..=15 => None,
        16..=231 => {
            let cube = index - 16;
            // xterm spaces the cube's six levels at 0, then 55 + 40 per step, not evenly, so a
            // 256-color ramp matches every other terminal.
            let level = |step: u8| match step {
                0 => 0,
                _ => step * 40 + 55,
            };
            Some((level(cube / 36), level((cube / 6) % 6), level(cube % 6)))
        }
        232..=255 => {
            let gray = (index - 232) * 10 + 8;
            Some((gray, gray, gray))
        }
    }
}

#[cfg(test)]
#[path = "terminal_report_tests.rs"]
mod terminal_report_tests;
