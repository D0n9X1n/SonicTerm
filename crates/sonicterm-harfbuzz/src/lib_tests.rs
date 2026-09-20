//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{hb_codepoint_t, hb_direction_t};

#[test]
fn exports_harfbuzz_aliases_and_enums() {
    // Direction discriminants and codepoint widths are part of the wrapper ABI.
    assert_eq!(std::mem::size_of::<hb_codepoint_t>(), 4);
    assert_eq!(hb_direction_t::HB_DIRECTION_LTR as u32, 4);
    assert_eq!(hb_direction_t::HB_DIRECTION_RTL as u32, 5);
}

#[test]
fn linked_harfbuzz_matches_the_pinned_release() {
    // Version reporting catches a stale or system library satisfying the native link unexpectedly.
    let (mut major, mut minor, mut patch) = (0, 0, 0);
    // SAFETY: hb_version writes three initialized, correctly sized output pointers without retaining them.
    unsafe { crate::hb_version(&mut major, &mut minor, &mut patch) };
    assert_eq!((major, minor, patch), (14, 4, 0));
}
