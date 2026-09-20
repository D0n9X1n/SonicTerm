//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{FT_Int16, FT_FACE_FLAG_SCALABLE, FT_LOAD_DEFAULT};

#[test]
fn exports_freetype_aliases_and_constants() {
    // Fixed-width aliases and raster constants must retain their cross-platform ABI.
    assert_eq!(std::mem::size_of::<FT_Int16>(), 2);
    assert_eq!(FT_LOAD_DEFAULT, 0);
    assert_ne!(FT_FACE_FLAG_SCALABLE, 0);
}

#[test]
fn linked_freetype_matches_the_pinned_release_and_unsigned_span_abi() {
    // A successful link must use the imported library and the matching span field types.
    let mut library = std::ptr::null_mut();
    let (mut major, mut minor, mut patch) = (0, 0, 0);
    // SAFETY: library and version outputs are valid; the successful native allocation is released once.
    unsafe {
        assert_eq!(crate::FT_Init_FreeType(&mut library), 0);
        crate::FT_Library_Version(library, &mut major, &mut minor, &mut patch);
        assert_eq!(crate::FT_Done_FreeType(library), 0);
    }
    assert_eq!((major, minor, patch), (2, 14, 3));
    let span = crate::FT_Span_ { x: u16::MAX, len: 1, coverage: u8::MAX };
    assert_eq!(span.x, u16::MAX);
}
