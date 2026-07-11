//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{hb_codepoint_t, hb_direction_t};

#[test]
fn exports_harfbuzz_aliases_and_enums() {
    assert_eq!(std::mem::size_of::<hb_codepoint_t>(), 4);
    assert_eq!(hb_direction_t::HB_DIRECTION_LTR as u32, 4);
    assert_eq!(hb_direction_t::HB_DIRECTION_RTL as u32, 5);
}
