//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::GlyphInstance;

#[test]
fn exports_gpu_neutral_glyph_instance() {
    let glyph = GlyphInstance {
        rect: [0.0, 1.0, 2.0, 3.0],
        uv: [0.0, 0.0, 1.0, 1.0],
        color: [1.0, 1.0, 1.0, 1.0],
        flags: [0.0; 4],
    };
    assert_eq!(glyph.rect[2], 2.0);
    assert_eq!(glyph.uv[3], 1.0);
}
