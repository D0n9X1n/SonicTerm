//! The installed monospace families the font picker lists and a fresh install chooses from.
//!
//! Enumeration parses every installed font file through the platform locator, which takes
//! seconds on a large system, so callers run it on a worker thread and never on a frame path.

use std::collections::BTreeMap;

use crate::parser::ParsedFont;

/// Characters a face must set with one shared advance to count as monospace text: ASCII digits
/// and letters. Icon fonts that are fixed-pitch but lack these stay out of the list.
const TEXT_SAMPLE: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// The monospace families among `fonts` that can set text, sorted and de-duplicated.
///
/// A family counts when its first listed face has a glyph for every ASCII letter and digit and
/// all of them share one advance. The advances are measured rather than read from the
/// `isFixedPitch` flag, which Monaco and Courier leave clear. Icon fonts such as Symbols Nerd Font
/// Mono lack the letters and stay out. Families whose names start with `.` are private system
/// faces and are skipped. Only one face per family is opened.
pub fn monospace_families(fonts: &[ParsedFont]) -> Vec<String> {
    let Ok(library) = crate::ftwrap::Library::new() else {
        // When: `Library::new` fails, FreeType cannot open any face, so no family is listed.
        return Vec::new();
    };
    let mut verdicts: BTreeMap<&str, bool> = BTreeMap::new();
    for font in fonts {
        let family = font.names().family.as_str();
        if family.is_empty() || family.starts_with('.') || verdicts.contains_key(family) {
            // When: `family` is unnamed, private, or already judged by its first face, skip it.
            continue;
        }
        verdicts.insert(family, sets_monospace_text(&library, font));
    }
    let mut families: Vec<String> = verdicts
        .into_iter()
        .filter(|(_, sets_text)| *sets_text)
        .map(|(family, _)| family.to_owned())
        .collect();
    families.sort_by_key(|family| family.to_lowercase());
    families
}

/// Whether `font`'s face has every [`TEXT_SAMPLE`] glyph with one shared advance. A face that
/// cannot be opened does not count.
fn sets_monospace_text(library: &crate::ftwrap::Library, font: &ParsedFont) -> bool {
    let Ok(mut face) = library.face_from_locator(&font.handle) else {
        // When: `face_from_locator` fails, the file is unreadable and cannot set text.
        return false;
    };
    let mut advances = TEXT_SAMPLE.chars().map(|character| face.unscaled_advance(character));
    let Some(Some(first)) = advances.next() else {
        // When: the first sample glyph is missing, the face cannot set ASCII text.
        return false;
    };
    advances.all(|advance| advance == Some(first))
}

/// Every installed monospace family the platform locator reports (CoreText on macOS,
/// DirectWrite on Windows, fontconfig elsewhere), sorted and de-duplicated.
///
/// This parses every installed font file, so it runs on a worker thread.
pub fn installed_monospace_families() -> anyhow::Result<Vec<String>> {
    let locator = crate::locator::new_locator(config::FontLocatorSelection::default());
    let fonts = locator.enumerate_all_fonts()?;
    Ok(monospace_families(&fonts))
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
