//! VT batch processing and publication without extending parser custody.

use super::*;

/// Publish one completed nonempty batch only after parser, media, and host side effects return.
pub(in crate::app) fn process_pane_vt_batch_and_publish<Bytes: AsRef<[u8]>>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    read_at: Option<Instant>,
    command_started: &mut Option<Instant>,
    sync_latch: &mut SyncLatch,
    proxy: Option<&EventLoopProxy<UserEvent>>,
    send_reply: impl FnMut(Vec<u8>),
) {
    publish_pane_vt_batch_with(
        handles,
        bytes,
        read_at,
        command_started,
        sync_latch,
        super::super::media::decode_inline_image,
        |event| {
            if let Some(proxy) = proxy {
                // When: proxy exists, deliver the typed host event on the app event loop.
                let _ = proxy.send_event(event);
            }
        },
        Instant::now,
        send_reply,
    );
}

/// Parse one batch with the given decoder, event sink and clock, then publish its generation.
// Ordering: output_generation Release pairs with the window's pre-lock Acquire snapshot.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn publish_pane_vt_batch_with<Bytes, Decode, Emit, Now, Send>(
    handles: &PaneVtHandles,
    bytes: Bytes,
    read_at: Option<Instant>,
    command_started: &mut Option<Instant>,
    sync_latch: &mut SyncLatch,
    decode_media: Decode,
    emit_event: Emit,
    now: Now,
    send_reply: Send,
) where
    Bytes: AsRef<[u8]>,
    Decode: FnMut(&MediaEvent) -> Option<InlineImage>,
    Emit: FnMut(UserEvent),
    Now: FnMut() -> Instant,
    Send: FnMut(Vec<u8>),
{
    let nonempty = !bytes.as_ref().is_empty();
    process_pane_vt_batch_with(
        handles,
        bytes,
        read_at,
        command_started,
        sync_latch,
        decode_media,
        emit_event,
        now,
        send_reply,
    );
    if nonempty {
        handles.output_generation.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "spawn_pane_vt_tests.rs"]
mod spawn_pane_vt_tests;
