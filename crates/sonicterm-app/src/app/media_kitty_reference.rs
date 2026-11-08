//! The store operations written as one scan per question (count, find, any), the simplest form
//! of their contract: the differential oracle `media_kitty_fold_tests.rs` checks the folded
//! operations against.

use sonicterm_render_model::{InlineImage, KittyShow, KittyTag};
use sonicterm_vt::vt::{KittyCommand, KittyDelete};

use super::{show_for, KittyOp, MAX_KITTY_PLACEMENTS};

/// Kitty placements (entries that are not plain uploads) in `images`.
fn placement_count(images: &[InlineImage]) -> usize {
    images
        .iter()
        .filter(|entry| matches!(entry.kitty, Some(tag) if tag.show != KittyShow::Upload))
        .count()
}

/// Apply one kitty operation to a pane's images, returning the reply message and the entries
/// removed, which the caller drops after releasing the image lock.
pub(super) fn apply_kitty(
    images: &mut Vec<InlineImage>,
    operation: KittyOp,
) -> (Option<&'static str>, Vec<InlineImage>) {
    match operation {
        KittyOp::Transmit(command, mut image, source_px) => {
            // When: operation is Transmit, the image replaces every entry of its id before it is tagged and stored.
            let show = show_for(&command);
            // A displayed transmit adds a placement; at the cap it is refused unless it replaces one of its own.
            let replaced_placements = images
                .iter()
                .filter(|entry| {
                    let tag = tag_of(entry);
                    command.image_id != 0
                        && tag.image_id == command.image_id
                        && tag.show != KittyShow::Upload
                })
                .count();
            if show != KittyShow::Upload
                && placement_count(images) - replaced_placements >= MAX_KITTY_PLACEMENTS
            {
                // When: show adds a placement and placement_count minus replaced_placements is at the cap, it is refused.
                return (Some("ENOSPC:too many placements"), vec![image]);
            }
            let removed = if command.image_id == 0 {
                // When: image_id is 0 the image is anonymous: nothing to replace, and an unshown upload is unreachable.
                if show == KittyShow::Upload {
                    // When: show is Upload for an anonymous image, nothing could ever place it, so it is dropped.
                    return (Some("OK"), vec![image]);
                }
                Vec::new()
            } else {
                // When: image_id is nonzero, every earlier entry under it is replaced.
                take_where(images, |entry| tag_of(entry).image_id == command.image_id)
            };
            image.kitty = Some(KittyTag {
                image_id: command.image_id,
                placement_id: command.placement_id,
                show,
                source_px: source_px.unwrap_or((image.width, image.height)),
            });
            images.push(image);
            (Some("OK"), removed)
        }
        KittyOp::Place(command, row, col, scroll_mark) => {
            // When: operation is Place, a stored entry of the id is cloned into a new placement.
            let Some(source) = images
                .iter()
                .find(|entry| tag_of(entry).image_id == command.image_id && command.image_id != 0)
            else {
                // When: no stored entry has image_id, kitty answers ENOENT and places nothing.
                return (Some("ENOENT:no such image"), Vec::new());
            };
            let source_px = tag_of(source).source_px;
            let mut placed = source.clone();
            let replaces = |entry: &InlineImage| {
                let tag = tag_of(entry);
                tag.image_id == command.image_id
                    && (tag.show == KittyShow::Upload
                        || (command.placement_id != 0 && tag.placement_id == command.placement_id))
            };
            // Only replacing an existing placement keeps the count; placing an upload adds one.
            let replaces_placement = images
                .iter()
                .any(|entry| replaces(entry) && tag_of(entry).show != KittyShow::Upload);
            if !replaces_placement && placement_count(images) >= MAX_KITTY_PLACEMENTS {
                // When: replaces_placement is false and placement_count is at the cap, the new placement is refused.
                return (Some("ENOSPC:too many placements"), Vec::new());
            }
            placed.row = row;
            placed.col = col;
            placed.scroll_mark = scroll_mark;
            placed.kitty = Some(KittyTag {
                image_id: command.image_id,
                placement_id: command.placement_id,
                source_px,
                show: if command.virtual_placement {
                    KittyShow::Virtual { cols: command.cols, rows: command.rows }
                } else {
                    // When: virtual_placement is clear, a=p draws at the cursor like a=T.
                    KittyShow::Anchored { cols: command.cols, rows: command.rows }
                },
            });
            let mut removed = take_where(images, replaces);
            images.push(placed);
            // An upload or a replaced placement shares the placed entry's buffer; dropping it frees nothing.
            removed.retain(|entry| {
                !std::sync::Arc::ptr_eq(&entry.bgra, &images[images.len() - 1].bgra)
            });
            (Some("OK"), removed)
        }
        KittyOp::Delete(command) => (None, delete(images, &command)),
    }
}

/// Remove the placements a `d=i`/`d=I` command names, keeping or freeing the data.
fn delete(images: &mut Vec<InlineImage>, command: &KittyCommand) -> Vec<InlineImage> {
    let free = match command.delete {
        Some(KittyDelete::ById) => false,
        Some(KittyDelete::ByIdFree) => true,
        Some(KittyDelete::Unsupported) | None => {
            // When: delete names another selector, this subset ignores it rather than guessing at visibility.
            return Vec::new();
        }
    };
    let id = command.image_id;
    if id == 0 {
        // When: id is 0, the command names no image, so nothing matches.
        return Vec::new();
    }
    let mut removed = take_where(images, |entry| {
        let tag = tag_of(entry);
        tag.image_id == id
            && tag.show != KittyShow::Upload
            && (command.placement_id == 0 || tag.placement_id == command.placement_id)
    });
    let still_placed = images.iter().any(|entry| {
        let tag = tag_of(entry);
        tag.image_id == id && tag.show != KittyShow::Upload
    });
    // With free set and no placement of id left, the stored upload is freed too; without free, it is kept.
    if free && !still_placed {
        removed.extend(take_where(images, |entry| tag_of(entry).image_id == id));
    } else if !still_placed {
        // When: still_placed is false without free, the data stays as an unshown upload.
        let has_upload = images.iter().any(|entry| tag_of(entry).image_id == id);
        if let (false, Some(mut kept)) = (has_upload, removed.pop()) {
            let source_px = tag_of(&kept).source_px;
            kept.kitty = Some(KittyTag {
                image_id: id,
                placement_id: 0,
                show: KittyShow::Upload,
                source_px,
            });
            images.push(kept);
        }
    }
    removed
}

/// The kitty tag of `entry`, or an id-0 tag for untagged images, which no kitty command names.
fn tag_of(entry: &InlineImage) -> KittyTag {
    entry.kitty.unwrap_or(KittyTag {
        image_id: 0,
        placement_id: 0,
        show: KittyShow::Anchored { cols: 0, rows: 0 },
        source_px: (entry.width, entry.height),
    })
}

/// Move every entry matching `predicate` out of `images`, preserving the order of the rest.
fn take_where(
    images: &mut Vec<InlineImage>,
    mut predicate: impl FnMut(&InlineImage) -> bool,
) -> Vec<InlineImage> {
    let (taken, kept): (Vec<InlineImage>, Vec<InlineImage>) =
        std::mem::take(images).into_iter().partition(|entry| predicate(entry));
    *images = kept;
    taken
}
