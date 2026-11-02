//! Which private-use glyphs the platform's window-title font can draw.
//!
//! The native title is drawn by the platform, not by SonicTerm: AppKit's window-title font on
//! macOS, DWM's font on Windows and the compositor's on Linux. None carries Nerd Font icons, so
//! an icon would show as an empty box. macOS is asked; elsewhere nothing reliable can be asked.

/// Whether the platform's window-title font can draw `character`. Only private-use scalars are
/// asked: every other character is drawable, because the platform's font cascade supplies CJK,
/// emoji and other scripts the title font itself lacks, while no system font has Nerd icons.
pub(super) fn title_font_draws(character: char) -> bool {
    !super::native_title::is_private_use(character) || platform_title_font_draws(character)
}

/// macOS: whether CoreText's window-title UI font maps a private-use `character`, asked once
/// per scalar and remembered on the asking thread, which for the title writer is the event loop.
#[cfg(target_os = "macos")]
fn platform_title_font_draws(character: char) -> bool {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use core_text::font::{kCTFontWindowTitleFontType, new_ui_font_for_language, CTFont};

    thread_local! {
        static TITLE_FONT: CTFont = new_ui_font_for_language(kCTFontWindowTitleFontType, 0.0, None);
        static COVERAGE: RefCell<HashMap<char, bool>> = RefCell::new(HashMap::new());
    }
    if let Some(known) = COVERAGE.with(|coverage| coverage.borrow().get(&character).copied()) {
        // When: COVERAGE already holds character, the font's answer cannot change, so it is reused.
        return known;
    }
    let mut utf16 = [0u16; 2];
    let units = character.encode_utf16(&mut utf16);
    let mut glyphs = [0u16; 2];
    let draws = TITLE_FONT.with(|font| {
        // SAFETY: units and glyphs each hold units.len() (1 or 2) elements, which is the count passed.
        unsafe {
            font.get_glyphs_for_characters(
                units.as_ptr(),
                glyphs.as_mut_ptr(),
                units.len() as isize,
            )
        }
    });
    // A surrogate pair maps to one glyph in the first slot; zero is the missing glyph.
    let draws = draws && glyphs[0] != 0;
    COVERAGE.with(|coverage| coverage.borrow_mut().insert(character, draws));
    draws
}

/// Windows and Linux: the title font belongs to DWM or the compositor, so no private-use scalar
/// is assumed drawable except U+F8FF, kept as the one private-use mark a title font may carry.
#[cfg(not(target_os = "macos"))]
fn platform_title_font_draws(character: char) -> bool {
    character == '\u{F8FF}'
}
