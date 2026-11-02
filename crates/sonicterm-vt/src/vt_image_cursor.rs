//! Where the cursor goes after a Sixel or iTerm2 image is displayed.
//!
//! An image covers the cells its decoded pixels span on the pane's cell size. After a Sixel image,
//! as in xterm with sixel scrolling and in WezTerm, the cursor stays in the image's left column and
//! moves to the image's last cell row, so the line feed that ends the image puts the next text
//! below it. After an iTerm2 image the cursor moves to just right of the image's bottom-right
//! cell, stopping in the last column rather than wrapping, as iTerm2 and WezTerm do; the host skips
//! the move for `doNotMoveCursor=1`, WezTerm's extension. Both moves scroll the region at the
//! bottom margin, as a kitty placement's move does.
//!
//! Only the host knows whether an image decoded, so the parser ends its advance prefix after a
//! Sixel or iTerm2 image and the host moves the cursor once the image is held, before later bytes
//! are parsed.

use super::*;

impl Parser {
    /// Move the cursor past a Sixel image the host has just stored, whose decoded size is
    /// `image_px`: down to the image's last cell row, in the column where it started. Without a
    /// known cell size, or for an empty image, the cursor stays still.
    pub fn move_cursor_past_sixel_image(&mut self, image_px: (u32, u32)) {
        if let Some((_cols, rows)) = vt_kitty_cursor::placement_cells(0, 0, image_px, self.cell_px)
        {
            self.move_cursor_by_box(0, rows, false);
        }
    }

    /// Move the cursor past an iTerm2 image the host has just stored, whose decoded size is
    /// `image_px`: right of the image's bottom-right cell, or in the last column when that is past
    /// the right edge. Without a
    /// known cell size, or for an empty image, the cursor stays still.
    pub fn move_cursor_past_iterm2_image(&mut self, image_px: (u32, u32)) {
        if let Some((cols, rows)) = vt_kitty_cursor::placement_cells(0, 0, image_px, self.cell_px) {
            self.move_cursor_by_box(cols, rows, false);
        }
    }
}

#[cfg(test)]
#[path = "vt_image_cursor_tests.rs"]
mod vt_image_cursor_tests;
