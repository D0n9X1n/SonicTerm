//! Colors a program set for its own pane with OSC 4, 10, 11, 12 and 21, over the theme defaults.
//!
//! The parser owns one store per pane and the renderer reads it borrowed for a frame, so both
//! agree on which entries a program overrode. An entry records presence, not a difference from
//! the theme: an override equal to today's default still masks a later theme change.

use std::sync::atomic::{AtomicU64, Ordering};

/// One sRGB color, 8 bits per channel.
pub type Rgb = (u8, u8, u8);

/// The xterm dynamic colors a program can set and reset (OSC 10/11/12 and 110/111/112).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DynamicColor {
    /// Default foreground, OSC 10.
    Foreground,
    /// Default background, OSC 11.
    Background,
    /// Text cursor, OSC 12.
    Cursor,
}

/// Source of store generations, shared by every store in the process so no two states of two
/// stores carry the same value; a renderer cache salted with a generation cannot mix panes.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A program's color overrides for one pane: the three dynamic colors and the 256-entry palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorOverrides {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    cursor: Option<Rgb>,
    /// Palette entries; an entry is meaningful only while its bit in `palette_mask` is set.
    palette: [Rgb; 256],
    /// One bit per palette index: set while that index holds a program override.
    palette_mask: [u64; 4],
    /// Changes whenever a stored override changes; 0 until the first change.
    generation: u64,
}

impl Default for ColorOverrides {
    fn default() -> Self {
        Self {
            foreground: None,
            background: None,
            cursor: None,
            palette: [(0, 0, 0); 256],
            palette_mask: [0; 4],
            generation: 0,
        }
    }
}

impl ColorOverrides {
    /// Whether the program has no override at all, so the pane draws with the theme only.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.foreground.is_none()
            && self.background.is_none()
            && self.cursor.is_none()
            && self.palette_mask == [0; 4]
    }

    /// An identity of the stored overrides: it changes exactly when a stored entry changes and
    /// is unique across every store in the process, so caches can key on it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The program's override of `target`, if it set one.
    #[must_use]
    pub fn dynamic(&self, target: DynamicColor) -> Option<Rgb> {
        match target {
            DynamicColor::Foreground => self.foreground,
            DynamicColor::Background => self.background,
            DynamicColor::Cursor => self.cursor,
        }
    }

    /// The program's override of palette entry `index`, if it set one.
    #[must_use]
    pub fn palette(&self, index: u8) -> Option<Rgb> {
        let (word, bit) = mask_position(index);
        (self.palette_mask[word] & bit != 0).then(|| self.palette[usize::from(index)])
    }

    /// Store `color` as the override of `target`, or remove the override on `None`.
    pub fn set_dynamic(&mut self, target: DynamicColor, color: Option<Rgb>) {
        let slot = match target {
            DynamicColor::Foreground => &mut self.foreground,
            DynamicColor::Background => &mut self.background,
            DynamicColor::Cursor => &mut self.cursor,
        };
        if *slot != color {
            // Only a changed entry moves the generation, so a repeated set causes no repaint.
            *slot = color;
            self.advance_generation();
        }
    }

    /// Store `color` as the override of palette entry `index`, or remove it on `None`.
    pub fn set_palette(&mut self, index: u8, color: Option<Rgb>) {
        if self.palette(index) == color {
            // When: the stored `palette` entry already equals `color`, nothing changes.
            return;
        }
        let (word, bit) = mask_position(index);
        match color {
            Some(rgb) => {
                self.palette[usize::from(index)] = rgb;
                self.palette_mask[word] |= bit;
            }
            None => self.palette_mask[word] &= !bit,
        }
        self.advance_generation();
    }

    /// Remove every palette override, keeping the dynamic colors (RIS and a bare OSC 104).
    pub fn clear_palette(&mut self) {
        if self.palette_mask != [0; 4] {
            // Clearing is a change only while an entry is still marked.
            self.palette_mask = [0; 4];
            self.advance_generation();
        }
    }

    // Ordering: NEXT_GENERATION.fetch_add is Relaxed: it hands out distinct values and
    // publishes no other memory.
    fn advance_generation(&mut self) {
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// The `palette_mask` word and bit of palette entry `index`.
fn mask_position(index: u8) -> (usize, u64) {
    (usize::from(index / 64), 1u64 << (index % 64))
}

#[cfg(test)]
#[path = "color_overrides_tests.rs"]
mod color_overrides_tests;
