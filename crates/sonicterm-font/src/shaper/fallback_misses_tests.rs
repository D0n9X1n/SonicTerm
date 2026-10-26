use super::*;
use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use std::path::PathBuf;

/// The Rec Mono fixture's regular face, opened the way the shaper opens a fallback.
fn rec_mono_face(library: &ftwrap::Library) -> ftwrap::Face {
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test-fonts/rec-mono/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: FontOrigin::BuiltIn,
        coverage: None,
    };
    library.face_from_locator(&handle).expect("the Rec Mono fixture opens")
}

/// The record stays bounded: the main slot, which is never skipped, records nothing, and a
/// fallback slot fed 5,000 characters its face lacks starts over once it holds
/// `MAX_MISSES_PER_SLOT`, so it ends with only the characters recorded since.
#[test]
fn the_record_skips_the_main_slot_and_caps_each_fallback_slot() {
    let library = ftwrap::Library::new().unwrap();
    let face = rec_mono_face(&library);
    let run: String = (0x10_0000_u32..0x10_0000 + 5_000).filter_map(char::from_u32).collect();
    let misses = FallbackMisses::new(2);
    misses.record(0, &face, &run);
    misses.record(1, &face, &run);
    assert_eq!(misses.recorded(0), 0, "the main slot records nothing");
    assert_eq!(misses.recorded(1), 5_000 - MAX_MISSES_PER_SLOT, "the fallback slot started over");
}

/// A character is recorded only when the face cannot draw it. Rec Mono lacks U+01F8 (N with
/// grave) but maps N, so HarfBuzz's normalizer can draw it and it is not recorded; a plane 16
/// private-use character it lacks outright is.
#[test]
fn a_decomposable_character_is_not_recorded() {
    let library = ftwrap::Library::new().unwrap();
    let face = rec_mono_face(&library);
    let misses = FallbackMisses::new(2);
    misses.record(1, &face, "\u{1F8}\u{100000}");
    assert!(!misses.misses_all(1, "\u{1F8}"), "U+01F8 decomposes to a mapped N");
    assert!(misses.misses_all(1, "\u{100000}"));
}
