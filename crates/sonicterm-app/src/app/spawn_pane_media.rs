//! The media half of a VT batch: decode outside every lock, store into the pane's images, and
//! produce kitty replies in stream order.

use sonicterm_vt::vt::{KittyAction, KittyCommand, MediaProtocol};

use super::super::media::{apply_kitty, KittyOp};
use super::*;

/// Decode and store one batch's media events, returning the kitty replies they owe.
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
) -> Vec<u8>
where
    Decode: FnMut(&MediaEvent) -> Option<InlineImage>,
{
    let mut replies = Vec::new();
    let mut staged = Vec::new();
    for media in media_events {
        let command = (media.protocol == MediaProtocol::Kitty)
            .then(|| KittyCommand::parse(media.metadata.as_bytes()).ok())
            .flatten();
        let Some(command) = command else {
            // When: command is None, the event is Sixel, iTerm2 or an untyped kitty payload, drawn at its cursor.
            if let Some(image) = decode_media(&media) {
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
                KittyOp::Transmit(command.clone(), image)
            }
            KittyAction::Place => KittyOp::Place(command.clone(), media.row, media.col),
            KittyAction::Delete => KittyOp::Delete(command.clone()),
        };
        merge_staged(handles, &mut staged);
        let (message, removed) = {
            let mut images = handles.inline_images.lock();
            let (message, mut removed) = apply_kitty(&mut images, operation);
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
    replies
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
