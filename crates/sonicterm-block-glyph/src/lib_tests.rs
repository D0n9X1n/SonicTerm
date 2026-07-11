//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::glue::BgraPixel;

#[test]
fn exports_pixel_glue() {
    let px = BgraPixel::rgba(1, 2, 3, 4);
    assert_eq!(px, BgraPixel(3, 2, 1, 4));
    assert_eq!(px.a(), 4);
}
