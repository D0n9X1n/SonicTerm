//! The planned trim evicts exactly what a per-buffer set selects, keeps entry order, and reports
//! the retained figure the trim leaves.

use std::sync::Arc;

use super::*;

/// The per-buffer oracle: the oldest distinct buffers by id until the rest fit, every entry of an
/// evicted buffer removed.
fn oracle_trim(images: &mut Vec<InlineImage>, byte_budget: usize) -> Vec<InlineImage> {
    let mut buffers: Vec<(u64, usize)> = Vec::new();
    for image in images.iter() {
        if !buffers.iter().any(|(id, _)| *id == image.id) {
            buffers.push((image.id, image.bgra.len()));
        }
    }
    buffers.sort_by_key(|(id, _)| *id);
    let mut retained: usize = buffers.iter().map(|(_, bytes)| bytes).sum();
    let mut evict = 0;
    while buffers.len() - evict > MAX_RETAINED_INLINE_IMAGES
        || (retained > byte_budget && buffers.len() - evict > 1)
    {
        retained -= buffers[evict].1;
        evict += 1;
    }
    let evicted_ids: Vec<u64> = buffers[..evict].iter().map(|(id, _)| *id).collect();
    let (evicted, kept) =
        std::mem::take(images).into_iter().partition(|image| evicted_ids.contains(&image.id));
    *images = kept;
    evicted
}

/// An entry of buffer `id` holding `bytes`; entries made with one `id` share one buffer.
fn entry(
    id: u64,
    bytes: usize,
    buffers: &mut std::collections::HashMap<u64, Arc<[u8]>>,
) -> InlineImage {
    let bgra = Arc::clone(buffers.entry(id).or_insert_with(|| Arc::from(vec![0u8; bytes])));
    InlineImage { id, row: 0, col: 0, scroll_mark: 0, width: 1, height: 1, bgra, kitty: None }
}

/// A deterministic generator, so a failing list replays exactly.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// Over seeded lists whose entries repeat ids out of order (kitty placements re-placed after newer
/// images), the plan evicts the same entries in the same order as the oracle, keeps the rest in
/// order, and its retained figure is what the list holds afterwards.
#[test]
fn the_planned_trim_matches_the_per_buffer_set() {
    for seed in 0..128u64 {
        let mut rng = Lcg(seed);
        let mut buffers = std::collections::HashMap::new();
        let count = rng.below(160) as usize;
        let mut images: Vec<InlineImage> = (0..count)
            .map(|_| {
                let id = rng.below(140) + 1;
                entry(id, (id as usize % 7 + 1) * 100, &mut buffers)
            })
            .collect();
        let budget = rng.below(6000) as usize;
        let mut expected = images.clone();
        let expected_evicted = oracle_trim(&mut expected, budget);

        let plan = TrimPlan::new(&images, budget);
        let evicted = plan.take(&mut images);
        let ids = |list: &[InlineImage]| list.iter().map(|image| image.id).collect::<Vec<_>>();
        assert_eq!(ids(&evicted), ids(&expected_evicted), "seed {seed}: evicted");
        assert_eq!(ids(&images), ids(&expected), "seed {seed}: kept");
        assert_eq!(plan.after, retained_inline_media(&images), "seed {seed}: after");
    }
}
