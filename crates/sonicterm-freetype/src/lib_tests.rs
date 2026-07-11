//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{FT_Int16, FT_FACE_FLAG_SCALABLE, FT_LOAD_DEFAULT};

#[test]
fn exports_freetype_aliases_and_constants() {
    assert_eq!(std::mem::size_of::<FT_Int16>(), 2);
    assert_eq!(FT_LOAD_DEFAULT, 0);
    assert_ne!(FT_FACE_FLAG_SCALABLE, 0);
}
