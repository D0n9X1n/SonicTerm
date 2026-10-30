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
/// `f=100` must be PNG; `f=24`/`f=32` with `s` and `v` are raw RGB/RGBA whose size must match the
/// data exactly. A raw format without a size, or a control section that does not parse, falls back
/// to a guessed encoded format, which is how kitty payloads decoded before typed commands existed.
pub(super) fn decode_kitty_pixels(event: &MediaEvent) -> Option<(u32, u32, Vec<u8>)> {
    let Ok(command) = KittyCommand::parse(event.metadata.as_bytes()) else {
        // When: parse fails, the payload keeps the untyped decode with a guessed format.
        return decode_encoded_image(&event.data, None);
    };
    match command.format {
        24 | 32 if command.width_px != 0 && command.height_px != 0 => {
            decode_raw(&event.data, &command)
        }
        100 => decode_encoded_image(&event.data, Some(image::ImageFormat::Png)),
        // A raw format without s and v is an encoded image sent without f=100; its format is guessed.
        _ => decode_encoded_image(&event.data, None),
    }
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

/// What the worker does with one kitty event once its pixels, if any, are decoded.
pub(in crate::app) enum KittyOp {
    /// Store `image` under the command's id and show it as the command asks.
    Transmit(KittyCommand, InlineImage),
    /// Add a placement of an existing image at the event's cursor.
    Place(KittyCommand, u16, u16),
    /// Remove placements and possibly data.
    Delete(KittyCommand),
}

/// How a transmit or place command shows its image.
fn show_for(command: &KittyCommand) -> KittyShow {
    if command.virtual_placement {
        KittyShow::Virtual { cols: command.cols, rows: command.rows }
    } else if command.action == KittyAction::Transmit {
        // When: action is Transmit without virtual_placement, a=t stores the image without showing it.
        KittyShow::Upload
    } else {
        // When: neither virtual_placement nor a Transmit action applies, a=T draws at the capture cursor.
        KittyShow::Anchored
    }
}

/// Apply one kitty operation to a pane's images, returning the reply message and the entries
/// removed, which the caller drops after releasing the image lock.
pub(in crate::app) fn apply_kitty(
    images: &mut Vec<InlineImage>,
    operation: KittyOp,
) -> (Option<&'static str>, Vec<InlineImage>) {
    match operation {
        KittyOp::Transmit(command, mut image) => {
            // When: operation is Transmit, the image replaces every entry of its id before it is tagged and stored.
            let show = show_for(&command);
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
            });
            images.push(image);
            (Some("OK"), removed)
        }
        KittyOp::Place(command, row, col) => {
            // When: operation is Place, a stored entry of the id is cloned into a new placement.
            let Some(source) = images
                .iter()
                .find(|entry| tag_of(entry).image_id == command.image_id && command.image_id != 0)
            else {
                // When: no stored entry has image_id, kitty answers ENOENT and places nothing.
                return (Some("ENOENT:no such image"), Vec::new());
            };
            let mut placed = source.clone();
            placed.row = row;
            placed.col = col;
            placed.kitty = Some(KittyTag {
                image_id: command.image_id,
                placement_id: command.placement_id,
                show: if command.virtual_placement {
                    KittyShow::Virtual { cols: command.cols, rows: command.rows }
                } else {
                    // When: virtual_placement is clear, a=p draws at the cursor like a=T.
                    KittyShow::Anchored
                },
            });
            let mut removed = take_where(images, |entry| {
                let tag = tag_of(entry);
                tag.image_id == command.image_id
                    && (tag.show == KittyShow::Upload
                        || (command.placement_id != 0 && tag.placement_id == command.placement_id))
            });
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
    let still_placed = images.iter().any(|entry| tag_of(entry).image_id == id);
    // With free set and no placement of id left, the stored upload is freed too; without free, it is kept.
    if free && !still_placed {
        removed.extend(take_where(images, |entry| tag_of(entry).image_id == id));
    } else if !still_placed {
        // When: still_placed is false without free, the data stays as an unshown upload.
        if let Some(mut kept) = removed.pop() {
            kept.kitty = Some(KittyTag { image_id: id, placement_id: 0, show: KittyShow::Upload });
            images.push(kept);
        }
    }
    removed
}

/// The kitty tag of `entry`, or an id-0 tag for untagged images, which no kitty command names.
fn tag_of(entry: &InlineImage) -> KittyTag {
    entry.kitty.unwrap_or(KittyTag { image_id: 0, placement_id: 0, show: KittyShow::Anchored })
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
