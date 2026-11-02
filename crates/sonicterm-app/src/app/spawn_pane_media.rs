//! The media half of a VT batch: decode outside every lock, store into the pane's images, and
//! produce kitty replies in stream order.

use sonicterm_vt::vt::{KittyAction, KittyCommand, MediaProtocol};

use super::super::media::{apply_kitty, kitty_source_size, KittyOp};
use super::*;

/// A cursor move the host owes the parser after an image it now holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImageCursorMove {
    /// A kitty direct placement that was stored or placed.
    Kitty {
        /// Placement box width in cells (`c`), or 0.
        cols: u32,
        /// Placement box height in cells (`r`), or 0.
        rows: u32,
        /// The image's transmitted pixel size.
        source_px: (u32, u32),
    },
    /// A Sixel image of this decoded pixel size.
    Sixel {
        /// The decoded image's width and height in pixels.
        image_px: (u32, u32),
    },
    /// An iTerm2 image of this decoded pixel size, sent without `doNotMoveCursor=1`.
    Iterm2 {
        /// The decoded image's width and height in pixels.
        image_px: (u32, u32),
    },
}

impl ImageCursorMove {
    /// Move `parser`'s cursor past the image this move is owed for.
    pub(super) fn apply(self, parser: &mut Parser) {
        match self {
            Self::Kitty { cols, rows, source_px } => {
                parser.move_cursor_past_kitty_image(cols, rows, source_px);
            }
            Self::Sixel { image_px } => parser.move_cursor_past_sixel_image(image_px),
            Self::Iterm2 { image_px } => parser.move_cursor_past_iterm2_image(image_px),
        }
    }
}

/// Whether an iTerm2 `File=` attribute section asks the cursor to stay put (`doNotMoveCursor=1`).
fn iterm2_keeps_cursor(metadata: &str) -> bool {
    let attributes = metadata.strip_prefix("File=").unwrap_or(metadata);
    attributes.split(';').any(|attribute| attribute == "doNotMoveCursor=1")
}

/// The cursor move owed for an ordinary `media` event decoded to `image`, if any: a Sixel image,
/// or an iTerm2 image without `doNotMoveCursor=1`. An untyped kitty payload moves nothing.
fn ordinary_image_move(media: &MediaEvent, image: &InlineImage) -> Option<ImageCursorMove> {
    let image_px = (image.width, image.height);
    match media.protocol {
        MediaProtocol::Sixel => Some(ImageCursorMove::Sixel { image_px }),
        MediaProtocol::Iterm2File if !iterm2_keeps_cursor(&media.metadata) => {
            Some(ImageCursorMove::Iterm2 { image_px })
        }
        MediaProtocol::Iterm2File | MediaProtocol::Kitty => None,
    }
}

/// Decode and store one batch's media events, returning the kitty replies they owe and, in stream
/// order, the cursor moves owed for the Sixel and iTerm2 images held and for kitty direct
/// placements that were stored or placed.
///
/// Ordinary images (Sixel, iTerm2, untyped kitty) are staged and merged once, trimmed to the
/// pane's budget as they are today. A kitty command is applied in order after any staged images
/// ahead of it, so a delete or placement sees exactly the transmits before it. Decoding happens
/// before the image lock is taken, and every buffer an operation removes is dropped after the
/// lock is released.
pub(super) fn apply_batch_media<Decode>(
    handles: &PaneVtHandles,
    media_events: Vec<MediaEvent>,
    decode_media: &mut Decode,
) -> (Vec<u8>, Vec<ImageCursorMove>)
where
    Decode: FnMut(&MediaEvent) -> Option<InlineImage>,
{
    let mut replies = Vec::new();
    let mut moves = Vec::new();
    let mut staged = Vec::new();
    for media in media_events {
        let command = (media.protocol == MediaProtocol::Kitty)
            .then(|| KittyCommand::parse(media.metadata.as_bytes()).ok())
            .flatten();
        let Some(command) = command else {
            // When: command is None, the event is Sixel, iTerm2 or an untyped kitty payload, drawn at its cursor.
            if let Some(image) = decode_media(&media) {
                // Staged trimming always keeps the newest image, so this one is held.
                moves.extend(ordinary_image_move(&media, &image));
                staged.push(image);
                super::super::media::trim_staged_inline_images(
                    &mut staged,
                    &handles.inline_media_charge,
                );
            }
            continue;
        };
        let operation = match command.action {
            KittyAction::Query => {
                // When: action is Query, the image is decoded to validate it and then dropped unstored.
                let message =
                    if decode_media(&media).is_some() { "OK" } else { "EBADPNG:cannot decode" };
                push_reply(&mut replies, &command, message);
                continue;
            }
            KittyAction::Transmit | KittyAction::TransmitAndDisplay => {
                // When: action is Transmit or TransmitAndDisplay, the pixels are decoded here, outside every lock.
                let Some(image) = decode_media(&media) else {
                    // When: decode_media fails, nothing is stored and the client hears why.
                    push_reply(&mut replies, &command, "EBADPNG:cannot decode");
                    continue;
                };
                KittyOp::Transmit(command.clone(), image, kitty_source_size(&media))
            }
            KittyAction::Place => {
                KittyOp::Place(command.clone(), media.row, media.col, media.scroll_mark)
            }
            KittyAction::Delete => KittyOp::Delete(command.clone()),
        };
        merge_staged(handles, &mut staged);
        let (message, removed) = {
            let mut images = handles.inline_images.lock();
            let (message, mut removed) = apply_kitty(&mut images, operation);
            // Only a stored or placed image moves the cursor; a refusal leaves it where it is. The
            // held entry is the newest, read before the trim may drop it.
            let held = message == Some("OK") && command.moves_cursor();
            if let Some(tag) = images.last().and_then(|entry| entry.kitty).filter(|_| held) {
                moves.push(ImageCursorMove::Kitty {
                    cols: command.cols,
                    rows: command.rows,
                    source_px: tag.source_px,
                });
            }
            removed.extend(super::super::media::trim_inline_images_charged(
                &mut images,
                &handles.inline_media_charge,
            ));
            (message, removed)
        };
        drop(removed);
        if let Some(message) = message {
            push_reply(&mut replies, &command, message);
        }
    }
    merge_staged(handles, &mut staged);
    (replies, moves)
}

/// Merge staged ordinary images into the pane's store, dropping evicted buffers after the lock.
fn merge_staged(handles: &PaneVtHandles, staged: &mut Vec<InlineImage>) {
    if staged.is_empty() {
        // When: staged is empty, no lock is taken.
        return;
    }
    let evicted = {
        let mut images = handles.inline_images.lock();
        images.append(staged);
        super::super::media::trim_inline_images_charged(&mut images, &handles.inline_media_charge)
    };
    drop(evicted);
}

/// Append `command`'s reply for `message`, unless its quiet mode or missing id suppresses it.
fn push_reply(replies: &mut Vec<u8>, command: &KittyCommand, message: &str) {
    if let Some(reply) = command.reply(message) {
        replies.extend_from_slice(&reply);
    }
}

#[cfg(test)]
#[path = "spawn_pane_media_tests.rs"]
mod spawn_pane_media_tests;
