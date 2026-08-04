#![cfg(target_os = "macos")]

use crate::locator::{FontDataSource, FontLocator, FontOrigin};
use crate::parser::ParsedFont;
use crate::rangeset::RangeSet;
use config::{FontAttributes, FontStretch, FontStyle, FontWeight};
use core_foundation::array::CFArray;
use core_foundation::base::{CFRange, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use core_text::font::*;
use core_text::font_descriptor::*;
use std::cmp::Ordering;
use std::collections::HashSet;

lazy_static::lazy_static! {
    static ref FALLBACK: Vec<ParsedFont> = build_fallback_list();
}

#[link(name = "CoreText", kind = "framework")]
extern "C" {
    fn CTFontCreateForString(
        currentFont: CTFontRef,
        string: CFStringRef,
        range: CFRange,
    ) -> CTFontRef;
}

/// A FontLocator implemented using the system font loading
/// functions provided by core text.
pub struct CoreTextFontLocator {}

fn descriptor_from_attr(attr: &FontAttributes) -> anyhow::Result<CFArray<CTFontDescriptor>> {
    let family_name = attr
        .family
        .parse::<CFString>()
        .map_err(|_| anyhow::anyhow!("failed to parse family name {} as CFString", attr.family))?;

    let family_attr: CFString =
        // SAFETY: `kCTFontFamilyNameAttribute` is a non-null process-lifetime CoreText constant;
        // get-rule wrapping retains it for this Rust owner without consuming the global reference.
        unsafe { TCFType::wrap_under_get_rule(kCTFontFamilyNameAttribute) };

    let attributes = CFDictionary::from_CFType_pairs(&[(family_attr, family_name.as_CFType())]);
    let desc = core_text::font_descriptor::new_from_attributes(&attributes);

    let array =
        // SAFETY: `desc` is live; null requests no mandatory attributes, and CoreText returns
        // a create-rule array or null without retaining either input pointer.
        unsafe {
            core_text::font_descriptor::CTFontDescriptorCreateMatchingFontDescriptors(
                desc.as_concrete_TypeRef(),
                std::ptr::null(),
            )
        };
    if array.is_null() {
        anyhow::bail!("no font matches {:?}", attr);
    } else {
        // When: `array.is_null()` is false, wrap the matching descriptor array.
        Ok(
            // SAFETY: the Create-rule function returned non-null +1 ownership;
            // the wrapper consumes exactly that ownership.
            unsafe { CFArray::wrap_under_create_rule(array) },
        )
    }
}

/// Given a descriptor, return a handle that can be used to open it.
/// The descriptor may not refer to an on-disk font and thus may
/// not have a path.
/// In addition, it may point to a ttc; so we'll need to reference
/// each contained font to figure out which one is the one that
/// the descriptor is referencing.
fn handles_from_descriptor(descriptor: &CTFontDescriptor) -> Vec<ParsedFont> {
    let mut result = vec![];
    if let Some(path) = descriptor.font_path() {
        // When: `descriptor.font_path()` is `Some`, parse every face in that file or collection.
        let source = FontDataSource::OnDisk(path);
        let _ =
            crate::parser::parse_and_collect_font_info(&source, &mut result, FontOrigin::CoreText);
    }

    result
}

impl FontLocator for CoreTextFontLocator {
    fn load_fonts(
        &self,
        fonts_selection: &[FontAttributes],
        loaded: &mut HashSet<FontAttributes>,
        pixel_size: u16,
    ) -> anyhow::Result<Vec<ParsedFont>> {
        let mut fonts = vec![];

        for attr in fonts_selection {
            match descriptor_from_attr(attr) {
                Ok(descriptors) => {
                    let mut handles = vec![];
                    for descriptor in descriptors.iter() {
                        handles.append(&mut handles_from_descriptor(&descriptor));
                    }
                    log::trace!("core text matched {:?} to {:#?}", attr, handles);

                    // If we got a series of .ttc files, we may have a selection of
                    // different font families.  Let's make a first pass a limit
                    // ourselves to name matches
                    let name_matches: Vec<_> = handles
                        .iter()
                        .filter_map(|p| {
                            if p.matches_name(attr) {
                                Some(p.clone())
                            } else {
                                // When: `p.matches_name(attr)` is false, exclude this TTC face.
                                None
                            }
                        })
                        .collect();
                    if !name_matches.is_empty() {
                        handles = name_matches;
                    }

                    if let Some(parsed) = ParsedFont::best_match(attr, pixel_size, handles) {
                        log::trace!("best match from core text is {:?}", parsed);
                        fonts.push(parsed);
                        loaded.insert(attr.clone());
                    }
                }
                Err(err) => log::trace!("load_fonts: descriptor_from_attr: {:#}", err),
            }
        }

        Ok(fonts)
    }

    fn locate_fallback_for_codepoints(
        &self,
        codepoints: &[char],
    ) -> anyhow::Result<Vec<ParsedFont>> {
        let mut matches = vec![];

        let menlo =
            new_from_name("Menlo", 0.0).map_err(|_| anyhow::anyhow!("failed to get Menlo font"))?;

        for &c in codepoints {
            let mut wanted = RangeSet::new();
            wanted.add(c as u32);
            let text = CFString::new(&c.to_string());
            let utf16_len = c.len_utf16() as isize;

            let font =
                // SAFETY: `menlo` and `text` are live; `utf16_len` covers this
                // scalar in CoreFoundation's UTF-16 coordinate space, and
                // CoreText returns create-rule ownership or null.
                unsafe {
                    CTFontCreateForString(
                        menlo.as_concrete_TypeRef(),
                        text.as_concrete_TypeRef(),
                        CFRange::init(0, utf16_len),
                    )
                };

            if font.is_null() {
                // When: `font.is_null()` is true, CoreText found no fallback for this character.
                continue;
            }

            let font =
                // SAFETY: `font` is non-null with +1 ownership; the wrapper consumes that ownership.
                unsafe { CTFont::wrap_under_create_rule(font) };

            let candidates = handles_from_descriptor(&font.copy_descriptor());

            let mut matched_any = false;

            for font in candidates {
                if font.names().family == ".LastResort"
                    || font.names().postscript_name.as_deref() == Some("LastResort")
                {
                    // When: the family/PS-name LastResort predicate is true, reject it.
                    // Always exclude a last resort font, as it has placeholder glyphs for everything
                    continue;
                }

                let is_normal = font.weight() == FontWeight::REGULAR
                    && font.stretch() == FontStretch::Normal
                    && font.style() == FontStyle::Normal;
                if !is_normal {
                    // When: `is_normal` is false, reject styled fallback candidates.
                    // Only use normal attributed text for fallbacks; other attributes are undefined.
                    continue;
                }

                if let Ok(cov) = font.coverage_intersection(&wanted) {
                    // Explicitly check coverage because the list may not
                    // actually match the text we asked about(!)
                    if !cov.is_empty() {
                        matches.push((cov.len(), font));
                        matched_any = true;
                    }
                }
            }

            if !matched_any {
                // Consult our global, more general list of fallbacks
                for font in FALLBACK.iter() {
                    if let Ok(cov) = font.coverage_intersection(&wanted) {
                        if !cov.is_empty() {
                            matches.push((cov.len(), font.clone()));
                        }
                    }
                }
            }
        }

        // Add the handles in order of descending coverage; the idea being
        // that if a font has a large coverage then it is probably a better
        // candidate and more likely to result in other glyphs matching
        // in future shaping calls.
        let mut wanted = RangeSet::new();
        for &c in codepoints {
            wanted.add(c as u32);
        }
        for (cov_len, font) in &mut matches {
            if let Ok(cov) = font.coverage_intersection(&wanted) {
                *cov_len = cov.len();
            }
        }

        matches.sort_by(|(a_len, a), (b_len, b)| {
            let primary = a_len.cmp(b_len).reverse();
            if primary == Ordering::Equal {
                a.cmp(b)
            } else {
                // When: `primary == Ordering::Equal` is false, keep coverage ordering.
                primary
            }
        });
        matches.dedup();

        log::trace!("fallback candidates for {codepoints:?} is {matches:#?}");

        Ok(matches.into_iter().map(|(_len, handle)| handle).collect())
    }

    fn enumerate_all_fonts(&self) -> anyhow::Result<Vec<ParsedFont>> {
        let mut fonts = vec![];

        let collection = core_text::font_collection::create_for_all_families();
        if let Some(descriptors) = collection.get_descriptors() {
            for descriptor in descriptors.iter() {
                fonts.append(&mut handles_from_descriptor(&descriptor));
            }
        }

        fonts.sort();
        fonts.dedup();
        Ok(fonts)
    }
}

fn build_fallback_list() -> Vec<ParsedFont> {
    build_fallback_list_impl().unwrap_or_else(|err| {
        log::error!("Error getting system fallback fonts: {:#}", err);
        Vec::new()
    })
}

mod objc_compat {
    #![allow(deprecated)]

    use cocoa::base::id;
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    pub(super) fn apple_languages() -> anyhow::Result<CFArray<CFString>> {
        use objc::*;
        let user_defaults: id =
            // SAFETY: Objective-C returns the shared autoreleased `NSUserDefaults` object; the
            // pointer remains valid for the immediately following synchronous message send.
            unsafe { msg_send![class!(NSUserDefaults), standardUserDefaults] };

        let apple_lang = "AppleLanguages"
            .parse::<CFString>()
            .map_err(|_| anyhow::anyhow!("failed to parse lang name en as CFString"))?;

        let languages: CFArrayRef =
            // SAFETY: the receiver and key are live; this non-owning Objective-C
            // result remains valid through the immediately following retain.
            unsafe { msg_send![user_defaults, stringArrayForKey:apple_lang] };
        if languages.is_null() {
            anyhow::bail!("AppleLanguages is unavailable");
        }
        Ok(
            // SAFETY: `languages` is a live +0 NSArray; get-rule wrapping
            // retains the ownership later released by `CFArray::drop`.
            unsafe { CFArray::wrap_under_get_rule(languages) },
        )
    }
}

#[cfg(test)]
#[path = "core_text_tests.rs"]
mod core_text_tests;

fn build_fallback_list_impl() -> anyhow::Result<Vec<ParsedFont>> {
    let menlo =
        new_from_name("Menlo", 0.0).map_err(|_| anyhow::anyhow!("failed to get Menlo font"))?;

    let langs = objc_compat::apple_languages()?;

    let cascade = cascade_list_for_languages(&menlo, &langs);
    let mut fonts = vec![];
    // Explicitly include Menlo itself, as it appears to be the only
    // font on macOS that contains U+2718.
    fonts.append(&mut handles_from_descriptor(&menlo.copy_descriptor()));
    for descriptor in &cascade {
        fonts.append(&mut handles_from_descriptor(&descriptor));
    }
    // Some of the fallback fonts are special fonts that don't exist on
    // disk, and that we can't open.
    // In particular, `.AppleSymbolsFB` is one such font.  Let's try
    // a nearby approximation.
    let symbols = FontAttributes {
        family: "Apple Symbols".to_string(),
        weight: FontWeight::REGULAR,
        stretch: FontStretch::Normal,
        style: FontStyle::Normal,
        is_fallback: true,
        is_synthetic: true,
        harfbuzz_features: None,
        freetype_load_target: None,
        freetype_render_target: None,
        freetype_load_flags: None,
        scale: None,
        assume_emoji_presentation: None,
    };
    if let Ok(descriptors) = descriptor_from_attr(&symbols) {
        for descriptor in descriptors.iter() {
            fonts.append(&mut handles_from_descriptor(&descriptor));
        }
    }

    // Constrain to default weight/stretch/style
    fonts.retain(|f| {
        f.weight() == FontWeight::REGULAR
            && f.stretch() == FontStretch::Normal
            && f.style() == FontStyle::Normal
    });

    let mut seen = HashSet::new();
    let fonts: Vec<ParsedFont> = fonts
        .into_iter()
        .filter_map(|f| {
            if seen.contains(&f.handle) {
                None
            } else {
                // When: `seen.contains(&f.handle)` is false, retain this first unique handle.
                seen.insert(f.handle.clone());
                Some(f)
            }
        })
        .collect();

    // Pre-compute coverage
    let empty = RangeSet::new();
    for font in &fonts {
        if let Err(err) = font.coverage_intersection(&empty) {
            log::error!("Error computing coverage for {:?}: {:#}", font, err);
        }
    }

    Ok(fonts)
}
