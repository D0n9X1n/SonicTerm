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
    /// Whether the placements are exactly the plan's `(image, placement)` pairs: placements 1 to 4
    /// of every image after `print`, 1 to 8 after `cap`. A refusal of anything but the one past the
    /// cap, or an admission of that one in place of another, shows here.
    pub(crate) exact: bool,
}

/// The receipt for a pane holding `images` with `rows` visible, its placements compared with
/// placements `1..=placements_per_image` of every image.
pub(crate) fn receipt<'grid>(
    images: &[InlineImage],
    rows: impl Iterator<Item = &'grid sonicterm_grid::grid::Row>,
    placements_per_image: u32,
) -> KittyReceipt {
    let tagged = images.iter().filter_map(|image| Some((image, image.kitty?)));
    let buffers: std::collections::BTreeSet<u64> =
        tagged.clone().map(|(image, _)| image.id).collect();
    let placed: Vec<(u32, u32)> = tagged
        .clone()
        .filter(|(_, tag)| tag.show != KittyShow::Upload)
        .map(|(_, tag)| (tag.image_id, tag.placement_id))
        .collect();
    let virtual_keys: std::collections::BTreeSet<(u32, u32)> = tagged
        .filter(|(_, tag)| matches!(tag.show, KittyShow::Virtual { .. }))
        .map(|(_, tag)| (tag.image_id, tag.placement_id))
        .collect();
    let runs = rows
        .flat_map(|row| decode_row(row.iter().enumerate().map(|(col, cell)| (col as u16, cell))))
        .filter(|run| virtual_keys.contains(&(run.image_id, run.placement_id)))
        .count();
    let planned: std::collections::BTreeSet<(u32, u32)> = (1..=crate::workload::KITTY_IMAGES)
        .flat_map(|image_id| (1..=placements_per_image).map(move |placement| (image_id, placement)))
        .collect();
    // Equal sets of equal size: no pair is missing, extra or held twice.
    let held: std::collections::BTreeSet<(u32, u32)> = placed.iter().copied().collect();
    let exact = held == planned && placed.len() == held.len();
    KittyReceipt { images: buffers.len(), placements: placed.len(), runs, exact }
}

/// Placements per image the pane must hold after `phase`: every placement up to the cap after
/// `cap`, the drawn ones after any other phase.
pub(crate) fn placements_per_image(phase: &str) -> u32 {
    use crate::workload::{KITTY_DRAWN_PLACEMENTS, KITTY_IMAGES, KITTY_PLACEMENT_CAP};
    let held = if phase == "cap" { KITTY_PLACEMENT_CAP } else { KITTY_DRAWN_PLACEMENTS };
    held / KITTY_IMAGES
}

/// The receipt `phase` must end with, or `None` for a phase that is not checked.
pub(crate) fn expected(phase: &str) -> Option<KittyReceipt> {
    use crate::workload::{KITTY_DRAWN_PLACEMENTS, KITTY_IMAGES, KITTY_PLACEMENT_CAP};
    let (images, drawn) = (KITTY_IMAGES as usize, KITTY_DRAWN_PLACEMENTS as usize);
    let placements = match phase {
        "print" => drawn,
        // The cap admits placements up to it and refuses the one past it.
        "cap" => KITTY_PLACEMENT_CAP as usize,
        _ => return None,
    };
    Some(KittyReceipt { images, placements, runs: drawn, exact: true })
}

/// Kitty commands the store must answer in `phase`, read from the `kitty_store_hold` sample count
/// on a tree that times the store: 128 transmits and 512 placements in `print`, and in `cap` 512
/// placements plus the one past the cap, which reaches the store and is refused. `None` for a
/// phase that is not checked.
pub(crate) fn store_commands(phase: &str) -> Option<u64> {
    use crate::workload::{KITTY_DRAWN_PLACEMENTS, KITTY_IMAGES, KITTY_PLACEMENT_CAP};
    match phase {
        "print" => Some(u64::from(KITTY_IMAGES + KITTY_DRAWN_PLACEMENTS)),
        "cap" => Some(u64::from(KITTY_PLACEMENT_CAP - KITTY_DRAWN_PLACEMENTS + 1)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "kitty_receipt_tests.rs"]
mod kitty_receipt_tests;
