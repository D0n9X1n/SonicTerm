use super::*;

/// The BGRA image owns its bytes before the face-owned source can change or expire.
#[test]
fn bgra_image_copies_borrowed_glyph_bytes() {
    let mut source = vec![1, 2, 3, 4];
    let image = owned_bgra_image(1, 1, &source).expect("one BGRA pixel builds an image");

    source.fill(0);

    assert_eq!(image.as_raw(), &[1, 2, 3, 4]);
}
