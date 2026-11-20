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
