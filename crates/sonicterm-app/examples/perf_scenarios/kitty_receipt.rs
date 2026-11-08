//! S11/kitty-placeholders' receipts: what the pane holds after each phase, read from its image store
//! and grid. A run whose images, placements or resolved runs differ from the plan measured another
//! workload (a refused transmit makes the frames cheaper), so the probe invalidates it.

use serde::Serialize;
use sonicterm_render_model::kitty_placeholder::decode_row;
use sonicterm_render_model::{InlineImage, KittyShow};

/// One phase's receipt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct KittyReceipt {
    /// Distinct pixel buffers the pane retains among its kitty entries.
    pub(crate) images: usize,
    /// Kitty placements the pane holds: entries that are not the upload itself.
    pub(crate) placements: usize,
    /// Placeholder runs visible in the grid that name a virtual placement the pane holds.
    pub(crate) runs: usize,
}

/// The receipt for a pane holding `images` with `rows` visible.
pub(crate) fn receipt<'grid>(
    images: &[InlineImage],
    rows: impl Iterator<Item = &'grid sonicterm_grid::grid::Row>,
) -> KittyReceipt {
    let tagged = images.iter().filter_map(|image| Some((image, image.kitty?)));
    let buffers: std::collections::BTreeSet<u64> =
        tagged.clone().map(|(image, _)| image.id).collect();
    let placements = tagged.clone().filter(|(_, tag)| tag.show != KittyShow::Upload).count();
    let virtual_keys: std::collections::BTreeSet<(u32, u32)> = tagged
        .filter(|(_, tag)| matches!(tag.show, KittyShow::Virtual { .. }))
        .map(|(_, tag)| (tag.image_id, tag.placement_id))
        .collect();
    let runs = rows
        .flat_map(|row| decode_row(row.iter().enumerate().map(|(col, cell)| (col as u16, cell))))
        .filter(|run| virtual_keys.contains(&(run.image_id, run.placement_id)))
        .count();
    KittyReceipt { images: buffers.len(), placements, runs }
}

/// The receipt `phase` must end with, or `None` for a phase that is not checked.
pub(crate) fn expected(phase: &str) -> Option<KittyReceipt> {
    use crate::workload::{KITTY_DRAWN_PLACEMENTS, KITTY_IMAGES, KITTY_PLACEMENT_CAP};
    let (images, drawn) = (KITTY_IMAGES as usize, KITTY_DRAWN_PLACEMENTS as usize);
    match phase {
        "print" => Some(KittyReceipt { images, placements: drawn, runs: drawn }),
        // The cap admits placements up to it and refuses the one past it.
        "cap" => {
            Some(KittyReceipt { images, placements: KITTY_PLACEMENT_CAP as usize, runs: drawn })
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "kitty_receipt_tests.rs"]
mod kitty_receipt_tests;
