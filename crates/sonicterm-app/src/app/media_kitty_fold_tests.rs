//! The folded kitty store scans: every operation answers and removes exactly what the per-question
//! scans in `media_kitty_reference.rs` do, over seeded random command sequences, and a placement at
//! the cap reads the store at most twice.

use std::sync::Arc;

use sonicterm_render_model::{InlineImage, KittyTag};
use sonicterm_vt::vt::KittyCommand;

use super::{apply_kitty, KittyOp, SCAN_VISITS};

/// A deterministic generator, so a failing sequence replays exactly.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// A one-pixel image whose buffer is `buffer_id`'s, so shared buffers are recognisable.
fn image(buffer_id: u64) -> InlineImage {
    InlineImage {
        id: buffer_id,
        row: 0,
        col: 0,
        scroll_mark: 0,
        width: 1,
        height: 1,
        bgra: Arc::from(vec![buffer_id as u8; 4]),
        kitty: None,
    }
}

/// The command `metadata` parses to.
fn command(metadata: &str) -> KittyCommand {
    KittyCommand::parse(metadata.as_bytes()).expect("valid command")
}

/// One random operation over a small id space, so replacements, shared buffers and deletes collide.
/// Id 0 is in range: an anonymous image is displayed but never replaced or named.
fn random_operation(rng: &mut Lcg, next_buffer: &mut u64) -> KittyOp {
    let image_id = rng.below(4);
    let placement_id = rng.below(3);
    match rng.below(5) {
        0 | 1 => {
            *next_buffer += 1;
            let action = if rng.below(2) == 0 { "t" } else { "T" };
            let metadata = format!("a={action},i={image_id},p={placement_id},f=32,s=1,v=1");
            KittyOp::Transmit(command(&metadata), image(*next_buffer), Some((1, 1)))
        }
        2 | 3 => {
            let virtual_flag = if rng.below(2) == 0 { ",U=1" } else { "" };
            let metadata = format!("a=p,i={image_id},p={placement_id}{virtual_flag}");
            KittyOp::Place(command(&metadata), rng.below(20) as u16, rng.below(20) as u16, 0)
        }
        _ => {
            let selector = if rng.below(2) == 0 { "i" } else { "I" };
            KittyOp::Delete(command(&format!("a=d,d={selector},i={image_id},p={placement_id}")))
        }
    }
}

/// The store's observable state: each entry's buffer, position and tag, in order.
fn state(images: &[InlineImage]) -> Vec<(u64, u16, u16, Option<KittyTag>)> {
    images.iter().map(|entry| (entry.id, entry.row, entry.col, entry.kitty)).collect()
}

/// The folded operations equal the pre-fold ones on every reply, every removed set and the whole
/// store, over 64 seeded sequences of 200 random commands each, so the fold changed no behaviour.
#[test]
fn the_folded_operations_match_the_pre_fold_ones() {
    for seed in 0..64u64 {
        let mut rng = Lcg(seed);
        let mut next_buffer = 0;
        // An untagged image (Sixel or iTerm2) shares the store; no kitty command may name it.
        let (mut folded, mut reference) = (vec![image(999)], vec![image(999)]);
        for step in 0..200 {
            let operation = random_operation(&mut rng, &mut next_buffer);
            let (answer, removed) = apply_kitty(&mut folded, operation.clone());
            let (expected_answer, expected_removed) =
                super::media_kitty_reference::apply_kitty(&mut reference, operation);
            assert_eq!(answer, expected_answer, "seed {seed} step {step}");
            assert_eq!(state(&removed), state(&expected_removed), "seed {seed} step {step}");
            assert_eq!(state(&folded), state(&reference), "seed {seed} step {step}");
        }
    }
}

/// At the placement cap, a refused placement and one that replaces its own both answer as the
/// reference does; the cap boundary is where a folded count and a separate count could disagree.
#[test]
fn the_cap_boundary_answers_as_before() {
    let mut folded = Vec::new();
    let mut reference = Vec::new();
    let upload = || KittyOp::Transmit(command("a=t,i=7,f=32,s=1,v=1"), image(1), Some((1, 1)));
    apply_kitty(&mut folded, upload());
    super::media_kitty_reference::apply_kitty(&mut reference, upload());
    for placement_id in 1..=(super::MAX_KITTY_PLACEMENTS as u64 + 2) {
        let place = || KittyOp::Place(command(&format!("a=p,i=7,p={placement_id}")), 0, 0, 0);
        let (answer, _) = apply_kitty(&mut folded, place());
        let (expected, _) = super::media_kitty_reference::apply_kitty(&mut reference, place());
        assert_eq!(answer, expected, "placement {placement_id}");
    }
    // Re-placing an existing placement at the cap replaces it rather than being refused.
    let again = || KittyOp::Place(command("a=p,i=7,p=1"), 1, 1, 0);
    assert_eq!(apply_kitty(&mut folded, again()).0, Some("OK"));
    assert_eq!(super::media_kitty_reference::apply_kitty(&mut reference, again()).0, Some("OK"));
    assert_eq!(state(&folded), state(&reference));
}

/// Anonymous (id 0) displayed transmits fill the cap and are never counted as replacing each other,
/// so the next one is refused, as the reference refuses it.
#[test]
fn anonymous_transmits_at_the_cap_are_refused_as_before() {
    let mut folded = Vec::new();
    let mut reference = Vec::new();
    for buffer in 0..=(super::MAX_KITTY_PLACEMENTS as u64) {
        let transmit =
            || KittyOp::Transmit(command("a=T,f=32,s=1,v=1"), image(buffer), Some((1, 1)));
        let (answer, _) = apply_kitty(&mut folded, transmit());
        let (expected, _) = super::media_kitty_reference::apply_kitty(&mut reference, transmit());
        assert_eq!(answer, expected, "anonymous transmit {buffer}");
    }
    assert_eq!(
        apply_kitty(
            &mut folded,
            KittyOp::Transmit(command("a=T,f=32,s=1,v=1"), image(5000), Some((1, 1)))
        )
        .0,
        Some("ENOSPC:too many placements")
    );
    assert_eq!(state(&folded), state(&reference));
}

/// A placement into a store holding the cap reads it in two passes (the fold and the take), where
/// the pre-fold code read it three times or more.
#[test]
fn a_placement_at_the_cap_reads_the_store_twice() {
    let mut images = Vec::new();
    apply_kitty(
        &mut images,
        KittyOp::Transmit(command("a=t,i=7,f=32,s=1,v=1"), image(1), Some((1, 1))),
    );
    for placement_id in 1..super::MAX_KITTY_PLACEMENTS as u64 {
        apply_kitty(
            &mut images,
            KittyOp::Place(command(&format!("a=p,i=7,p={placement_id}")), 0, 0, 0),
        );
    }
    let entries = images.len();
    SCAN_VISITS.with(|visits| visits.set(0));
    apply_kitty(&mut images, KittyOp::Place(command("a=p,i=7,p=5000"), 0, 0, 0));
    let visited = SCAN_VISITS.with(std::cell::Cell::get);
    assert!(visited <= 2 * entries, "{visited} visits over {entries} entries");
}
