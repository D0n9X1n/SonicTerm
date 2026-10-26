//! Validated visible-pane layout before parser and image acquisition.

use super::*;

/// Validate live topology using ids only; hidden parser and image state is never read.
pub(super) fn validate_layout(
    leaves: &[u64],
    rects: Vec<(u64, Rect)>,
    active: u64,
    zoom: Option<u64>,
    exists: impl Fn(u64) -> bool,
) -> Result<VisibleLayout, LayoutInvalid> {
    let mut unique = HashSet::with_capacity(leaves.len());
    for &id in leaves {
        if !unique.insert(id) {
            // When: `unique` already contains `id`, two leaves claim the same pane.
            return Err(LayoutInvalid::DuplicatePane(id));
        }
        if !exists(id) {
            // When: `exists` rejects a leaf, presenting a subset would conceal broken topology.
            return Err(LayoutInvalid::MissingPane(id));
        }
    }
    if !unique.contains(&active) {
        // When: `active` is not a leaf, no coherent active parser index can be supplied.
        return Err(LayoutInvalid::ActiveNotLeaf(active));
    }
    if let Some(zoom) = zoom {
        // When: `zoom` is set, its owner must also be the active live leaf.
        if zoom != active || !unique.contains(&zoom) {
            // When: `zoom` does not identify the active live leaf, do not guess another pane.
            return Err(LayoutInvalid::ZoomDisagrees(zoom));
        }
    }
    let expected = zoom.map_or_else(|| leaves.to_vec(), |id| vec![id]);
    let actual: Vec<_> = rects.iter().map(|(id, _)| *id).collect();
    if actual != expected {
        // When: `actual` differs from tree order, the renderer cannot receive a coherent layout.
        return Err(LayoutInvalid::VisibleDisagrees);
    }
    let Some(active_pos) = actual.iter().position(|id| *id == active) else {
        // When: `active_pos` is absent, even a nonempty layout cannot supply the active grid.
        return Err(LayoutInvalid::ActiveNotLeaf(active));
    };
    Ok(VisibleLayout { rects, active_pos })
}

#[cfg(test)]
#[path = "visible_frame_layout_tests.rs"]
mod visible_frame_layout_tests;
