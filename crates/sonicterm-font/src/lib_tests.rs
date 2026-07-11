//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::color::{linear_u8_to_srgb8, SrgbaPixel};

#[test]
fn exports_color_primitives() {
    assert_eq!(linear_u8_to_srgb8(0), 0);
    assert_eq!(SrgbaPixel::rgba(1, 2, 3, 4).as_rgba(), (1, 2, 3, 4));
}
