use sonicterm_freetype::{FT_Int16, FT_FACE_FLAG_SCALABLE, FT_LOAD_DEFAULT};

#[test]
fn exports_freetype_aliases_and_constants() {
    assert_eq!(std::mem::size_of::<FT_Int16>(), 2);
    assert_eq!(FT_LOAD_DEFAULT, 0);
    assert_ne!(FT_FACE_FLAG_SCALABLE, 0);
}
