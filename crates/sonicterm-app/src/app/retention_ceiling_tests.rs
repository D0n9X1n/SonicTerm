//! The idle-pane walk stops reclaiming once the process is back under its ceiling.

use super::retention_tests::{decode_into, pane_in, retained_bytes};
use super::*;

/// Four panes fill to the per-pane cap and a fifth pushes the pool over the ceiling. Trimming the
/// first pane to the floor brings the pool back under, so the walk stops there: the other panes
/// keep every image, where a walk that checked the ceiling only on entry would trim each of them to
/// the five-pane share as well.
#[test]
fn the_walk_stops_once_the_pool_is_back_under_its_ceiling() {
    let pool = crate::app::media::InlineMediaPool::new();
    // One MiB per image, so a pane at the 64 MiB cap holds 64 images and every trim is visible.
    const IMAGE_BYTES: usize = 1024 * 1024;
    let cap = crate::app::media::MAX_RETAINED_INLINE_IMAGE_BYTES;
    let ceiling = crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES;
    let mut id = 0u64;

    // Filled one at a time, each under a budget no smaller than the cap, so the four reach it.
    let mut panes = Vec::new();
    for _ in 0..4 {
        let pane = pane_in(&pool, 80, 24);
        decode_into(&pane, &mut id, cap / IMAGE_BYTES, IMAGE_BYTES);
        panes.push(pane);
    }
    let newcomer = pane_in(&pool, 80, 24);
    decode_into(&newcomer, &mut id, 8, IMAGE_BYTES);
    panes.push(newcomer);
    let before: Vec<usize> = panes.iter().map(retained_bytes).collect();
    assert_eq!(before[..4], [cap; 4], "precondition: the first four panes reached the cap");
    assert!(pool.bytes() > ceiling, "precondition: the pool is over its ceiling");

    let reclaimed = trim_panes_over_pool_ceiling(&pool, panes.iter());

    // The first pane is trimmed to the floor, which alone frees more than the overage.
    assert!(retained_bytes(&panes[0]) < before[0], "the first pane is trimmed");
    assert!(pool.bytes() <= ceiling, "the walk brings the pool back under its ceiling");
    assert_eq!(reclaimed, before[0] - retained_bytes(&panes[0]), "only the first pane reclaimed");
    for (index, pane) in panes.iter().enumerate().skip(1) {
        assert_eq!(
            retained_bytes(pane),
            before[index],
            "pane {index} was trimmed although the pool was already under its ceiling"
        );
    }
}

/// A pool exactly at its ceiling is not over it: the walk trims nothing and reclaims nothing.
#[test]
fn a_pool_exactly_at_its_ceiling_is_left_alone() {
    let pool = crate::app::media::InlineMediaPool::new();
    const IMAGE_BYTES: usize = 1024 * 1024;
    let cap = crate::app::media::MAX_RETAINED_INLINE_IMAGE_BYTES;
    let mut id = 0u64;
    // Four panes at the cap sum to the ceiling exactly.
    let panes: Vec<PaneState> = (0..4)
        .map(|_| {
            let pane = pane_in(&pool, 80, 24);
            decode_into(&pane, &mut id, cap / IMAGE_BYTES, IMAGE_BYTES);
            pane
        })
        .collect();
    assert_eq!(pool.bytes(), crate::app::media::MAX_PROCESS_INLINE_MEDIA_BYTES);

    assert_eq!(trim_panes_over_pool_ceiling(&pool, panes.iter()), 0);
    assert!(panes.iter().all(|pane| retained_bytes(pane) == cap), "no pane was trimmed");
}
