//! Epic #300 P1: assert hot-path [`sonicterm_types::Cell`] stays ≤ 24 bytes.
//!
//! Sentinel test. If this regresses, the compact-cell optimization has
//! been undone — investigate before bumping the cap.

use sonicterm_types::{Cell, FatAttributes};

#[test]
fn cell_size_is_at_most_24_bytes() {
    let sz = std::mem::size_of::<Cell>();
    eprintln!("size_of::<Cell> = {sz}");
    assert!(sz <= 24, "Cell grew to {sz} bytes; Epic #300 P1 budget is 24");
}

#[test]
fn fat_attributes_is_reasonable() {
    // Sanity: FatAttributes itself is small. We don't pin an exact size
    // because Option<HyperlinkId> + Option<Box<str>> is platform-laid,
    // but it should never blow past 32 bytes.
    let sz = std::mem::size_of::<FatAttributes>();
    assert!(sz <= 32, "FatAttributes is {sz} bytes; expected <= 32");
}
