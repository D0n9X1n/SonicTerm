//! The placeholder index resolves every `(image, placement)` exactly as the `find_virtual` scan
//! does, duplicates and placement 0 included, and a frame visits each image once rather than once
//! per run.

use std::sync::Arc;

use sonicterm_render_model::boundary::grid::grid::{Color, Grid};
use sonicterm_render_model::kitty_placeholder::{diacritic_value, PLACEHOLDER};
use sonicterm_render_model::{InlineImage, KittyShow, KittyTag};

use super::*;

/// An entry of buffer `id` tagged with `image_id`/`placement_id` and `show`.
fn entry(id: u64, image_id: u32, placement_id: u32, show: KittyShow) -> InlineImage {
    InlineImage {
        scroll_mark: 0,
        id,
        row: 0,
        col: 0,
        width: 40,
        height: 20,
        bgra: Arc::from(vec![0u8; 16]),
        kitty: Some(KittyTag { image_id, placement_id, show, source_px: (40, 20) }),
    }
}

/// The index answers every query exactly as the scan does, over seeded stores that mix virtual,
/// anchored and upload entries, repeat keys (the first in store order must win), and untagged
/// images, for placement 0, named placements and ids that are absent.
#[test]
fn the_index_resolves_every_run_as_the_scan_did() {
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut below = |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };
    let mut index = crate::core::frame_scratch::PlaceholderIndexScratch::default();
    for _ in 0..200 {
        let count = below(40) as usize;
        let images: Vec<InlineImage> = (0..count as u64)
            .map(|id| {
                let show = match below(3) {
                    0 => KittyShow::Virtual { cols: 4, rows: 2 },
                    1 => KittyShow::Anchored { cols: 0, rows: 0 },
                    _ => KittyShow::Upload,
                };
                let mut image = entry(id, below(5) as u32, below(3) as u32, show);
                if below(8) == 0 {
                    // An untagged image (Sixel or iTerm2) shares the list.
                    image.kitty = None;
                }
                image
            })
            .collect();
        // One index reused across seeds, as the frame scratch reuses it across panes: each rebuild
        // must leave nothing of the previous list behind.
        index.rebuild(&images);
        for image_id in 0..6 {
            for placement_id in 0..4 {
                let found =
                    index.get(&images, image_id, placement_id).map(|(image, tag)| (image.id, tag));
                let expected = find_virtual(&images, image_id, placement_id)
                    .map(|(image, tag)| (image.id, tag));
                assert_eq!(found, expected, "image {image_id} placement {placement_id}");
            }
        }
    }
}

/// The diacritic for `value`.
fn mark(value: u32) -> char {
    (0x0300..=0x1D244)
        .filter_map(char::from_u32)
        .find(|candidate| diacritic_value(*candidate) == Some(value))
        .expect("every small value has a diacritic")
}

/// A grid of `rows` rows, each one 4-cell placeholder run naming image `image_id` row by row.
fn placeholder_rows(image_id: u32, rows: u16) -> Grid {
    let mut grid = Grid::new(8, rows);
    let fg = Color::Rgb(0, 0, image_id as u8);
    for row in 0..rows {
        for col in 0..4u16 {
            let cell = &mut grid.row_mut(row)[usize::from(col)];
            cell.ch = PLACEHOLDER;
            cell.fg = fg;
            cell.set_extras(Some(
                format!("{}{}", mark(u32::from(row)), mark(u32::from(col))).into_boxed_str(),
            ));
        }
    }
    grid
}

/// 512 images and 64 placeholder runs: the frame visits each image once to index it, where the scan
/// visited every image once per run (512 × 64).
#[test]
fn a_frame_visits_each_image_once_not_once_per_run() {
    let mut images: Vec<InlineImage> = (0..511)
        .map(|id| entry(id, 100 + id as u32, 0, KittyShow::Anchored { cols: 0, rows: 0 }))
        .collect();
    // The image the runs name is last, so the scan would read every other image per run.
    images.push(entry(999, 7, 0, KittyShow::Virtual { cols: 4, rows: 64 }));
    let grid = placeholder_rows(7, 64);
    let rows = (0..64u16).map(|slot| (slot, grid.row(slot)));
    LOOKUP_VISITS.with(|visits| visits.set(0));
    let slices = slices_for_rows(rows, &images, (0.0, 0.0), (10.0, 10.0));
    let visited = LOOKUP_VISITS.with(std::cell::Cell::get);
    // Only the rows the fitted image covers draw; every one of the 64 runs is still resolved.
    assert!(!slices.is_empty(), "the runs resolve to the virtual placement");
    assert!(visited <= images.len(), "{visited} visits for {} images", images.len());
    // The scan, by contrast, visits every image once per run.
    LOOKUP_VISITS.with(|visits| visits.set(0));
    for _ in 0..64 {
        find_virtual(&images, 7, 0);
    }
    assert_eq!(LOOKUP_VISITS.with(std::cell::Cell::get), 64 * images.len());
}

/// Two panes that use the same kitty ids, drawn one after the other through one scratch index,
/// each resolve to their own images; across steady frames the index keeps its maps, so a frame
/// allocates none, and a completed pass holds them within their cap.
#[test]
fn the_scratch_index_is_rebuilt_per_pane_and_reused_across_frames() {
    let left = [entry(1, 7, 0, KittyShow::Virtual { cols: 4, rows: 1 })];
    let right = [entry(2, 7, 0, KittyShow::Virtual { cols: 4, rows: 1 })];
    let grid = placeholder_rows(7, 1);
    let mut scratch = crate::core::frame_scratch::FrameScratch::default();
    let mut warm = None;
    for frame in 0..3 {
        for (images, expected) in [(&left[..], 1), (&right[..], 2)] {
            let mut placements = Vec::new();
            push_pane_placements(
                &mut placements,
                &mut scratch.placeholder_index,
                images,
                std::iter::once((0, grid.row(0))),
                (0.0, 0.0),
                PaneRect::new(0.0, 0.0, 200.0, 100.0),
                (10.0, 10.0),
                AnchorScroll::default(),
            );
            assert!(!placements.is_empty(), "frame {frame}: the run resolves");
            assert!(
                placements.iter().all(|placement| placement.image.id == expected),
                "frame {frame}: a pane's run names its own image {expected}"
            );
        }
        let index = &scratch.placeholder_index;
        let held = (index.exact.capacity(), index.any.capacity());
        assert!(held.0 > 0 && held.1 > 0, "the maps are kept warm");
        // The same capacity every frame: the maps were cleared and refilled, never reallocated.
        assert_eq!(*warm.get_or_insert(held), held, "frame {frame} reuses the maps");
        scratch.finish(crate::core::frame_scratch::PassEnd::Completed);
    }
    let bytes = scratch.placeholder_index.retained_bytes();
    assert!(bytes > 0 && bytes <= 2 * crate::core::frame_scratch::PLACEHOLDER_INDEX_CAP_BYTES);
}

/// An index grown past its cap by a pathological pane is dropped when the pass finishes, so one
/// such frame does not pin its maps.
#[test]
fn an_index_past_its_cap_is_released_on_finish() {
    let mut scratch = crate::core::frame_scratch::FrameScratch::default();
    scratch.placeholder_index.exact.reserve(10_000);
    scratch.placeholder_index.any.reserve(10_000);
    assert!(
        scratch.placeholder_index.retained_bytes()
            > 2 * crate::core::frame_scratch::PLACEHOLDER_INDEX_CAP_BYTES
    );
    scratch.finish(crate::core::frame_scratch::PassEnd::Incomplete);
    assert_eq!(scratch.placeholder_index.retained_bytes(), 0);
}
