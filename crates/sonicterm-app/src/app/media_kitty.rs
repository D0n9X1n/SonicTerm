//! Kitty graphics on the host side: pixel decoding for `f=24/32/100`, and the store operations
//! that tag, place and delete kitty images inside a pane's inline-image vector.
//!
//! Kitty images live in the same per-pane vector, behind the same lock and charge, as every other
//! inline image, so the process ceiling, the idle trim and the memory report cover them without
//! a second store. An entry's [`KittyTag`] says how it is shown; several entries share one pixel
//! buffer and `id` when an image has several placements, and retention counts that buffer once.

use sonicterm_render_model::{InlineImage, KittyShow, KittyTag};
use sonicterm_vt::vt::{KittyAction, KittyCommand, KittyDelete, MediaEvent};

use super::{decode_encoded_image, inline_image_decode_dimensions_allowed, rgba_to_retained_bgra};

/// Decode a kitty transmit's pixels by its declared format.
///
/// `f=100` must be PNG; `f=24`/`f=32` (32 is the default) are raw RGB/RGBA whose size `s`x`v` must
/// match the data exactly. Nothing is guessed: a raw format without its size, or a control
/// section that does not parse, decodes to nothing, so a malformed query never answers `OK`.
pub(super) fn decode_kitty_pixels(event: &MediaEvent) -> Option<(u32, u32, Vec<u8>)> {
    let command = KittyCommand::parse(event.metadata.as_bytes()).ok()?;
    if command.format == 100 {
        // When: format is 100, the payload must be a PNG and nothing else.
        return decode_encoded_image(&event.data, Some(image::ImageFormat::Png));
    }
    decode_raw(&event.data, &command)
}

/// Decode raw RGB or RGBA after checking the declared size against the data, before allocating.
fn decode_raw(encoded: &[u8], command: &KittyCommand) -> Option<(u32, u32, Vec<u8>)> {
    use base64::Engine;
    let (width, height) = (command.width_px, command.height_px);
    if !inline_image_decode_dimensions_allowed(width, height) {
        // When: inline_image_decode_dimensions_allowed refuses width by height, nothing is decoded.
        return None;
    }
    let channels: usize = if command.format == 24 { 3 } else { 4 };
    let expected = (width as usize).checked_mul(height as usize)?.checked_mul(channels)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    if bytes.len() != expected {
        // When: bytes disagrees with s x v x channels, the image is malformed, not partially drawable.
        return None;
    }
    let image = if channels == 3 {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(width, height, bytes)?)
    } else {
        // When: channels is 4, the data is RGBA with straight alpha.
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(width, height, bytes)?)
    };
    rgba_to_retained_bgra(image)
}

/// Most kitty placements one pane may hold. Each is a cheap entry sharing its image's buffer, but
/// a client repeating `a=p` must not grow the store without bound.
pub(in crate::app) const MAX_KITTY_PLACEMENTS: usize = 1024;

/// The pixel size a kitty transmit declares before any downscale: `s`x`v` for raw data, the PNG
/// header for `f=100`. `None` when neither is readable, and the decoded size is used instead.
pub(in crate::app) fn kitty_source_size(event: &MediaEvent) -> Option<(u32, u32)> {
    use base64::Engine;
    let command = KittyCommand::parse(event.metadata.as_bytes()).ok()?;
    if command.format != 100 {
        // When: format is raw RGB or RGBA, the control keys s and v carry the size.
        return (command.width_px != 0 && command.height_px != 0)
            .then_some((command.width_px, command.height_px));
    }
    // A PNG's size is in its IHDR chunk, bytes 16..24; 32 base64 characters decode 24 bytes.
    let prefix = event.data.get(..32)?;
    let header = base64::engine::general_purpose::STANDARD.decode(prefix).ok()?;
    let width = u32::from_be_bytes(header.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(header.get(20..24)?.try_into().ok()?);
    (width != 0 && height != 0).then_some((width, height))
}

/// What the worker does with one kitty event once its pixels, if any, are decoded.
pub(in crate::app) enum KittyOp {
    /// Store `image` under the command's id and show it as the command asks; the size is the
    /// transmitted one, or `None` to use the decoded size.
    Transmit(KittyCommand, InlineImage, Option<(u32, u32)>),
    /// Add a placement of an existing image at the event's cursor: its row, column and scroll mark.
    Place(KittyCommand, u16, u16, u64),
    /// Remove placements and possibly data.
    Delete(KittyCommand),
}

/// How a transmit or place command shows its image.
fn show_for(command: &KittyCommand) -> KittyShow {
    // a=t stores the image without showing it, even with U=1.
    if command.action == KittyAction::Transmit {
        KittyShow::Upload
    } else if command.virtual_placement {
        // When: virtual_placement is set on a=T or a=p, the image draws only through placeholder cells.
        KittyShow::Virtual { cols: command.cols, rows: command.rows }
    } else {
        // When: neither a Transmit action nor virtual_placement applies, a=T draws at the capture cursor.
        KittyShow::Anchored { cols: command.cols, rows: command.rows }
    }
}

/// Kitty placements (entries that are not plain uploads) in `images`.
fn placement_count(images: &[InlineImage]) -> usize {
    images
        .iter()
        .filter(|entry| matches!(entry.kitty, Some(tag) if tag.show != KittyShow::Upload))
        .count()
}

/// Apply one kitty operation to a pane's images, returning the reply message and the entries
/// removed, which the caller drops after releasing the image lock.
pub(in crate::app) fn apply_kitty(
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

#[cfg(test)]
#[path = "media_kitty_tests.rs"]
mod media_kitty_tests;
