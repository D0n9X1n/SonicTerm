use sonicterm_block_glyph::glue::BgraPixel;

#[test]
fn exports_pixel_glue() {
    let px = BgraPixel::rgba(1, 2, 3, 4);
    assert_eq!(px, BgraPixel(3, 2, 1, 4));
    assert_eq!(px.a(), 4);
}
