//! OSC 52 clipboard reads (`ESC ] 52 ; <kinds> ; ? BEL|ST`) above the parser: the reply labels a
//! request names and the empty replies that answer it.
//!
//! Under `[terminal] osc52_read = "deny"` a read is dropped silently, as Ghostty and WezTerm do.
//! Otherwise it takes a ticket (`osc52_read_ticket.rs`), and a read no ticket can hold is answered
//! empty here, on the ordinary parser-reply path, because an empty reply holds no clipboard data.
//! Replies carrying clipboard text are built by `osc52_read_source.rs` and delivered only by
//! `osc52_read_pipeline.rs`, to the source PTY's sensitive-reply slot.

use base64::Engine as _;
use std::time::Duration;

/// Bytes of one empty reply, `ESC ] 52 ; <label> ; ESC \`; a reply batch splits only at multiples.
pub(in crate::app) const EMPTY_REPLY_BYTES: usize = 9;

/// The most clipboard text, in UTF-8 bytes, one read returns; `cp` reads each destination with
/// half of it, so one request's replies stay one message under the PTY writer's 16 MiB cap.
pub(in crate::app) const READ_TEXT_CAP: usize = 8 * 1024 * 1024;

/// Clipboard reads that may hold a ticket at once in the process; each pane holds at most one.
pub(in crate::app) const MAX_READS: usize = 8;

/// How long a ticket may wait or stay readable before it is answered or cancelled.
pub(in crate::app) const TICKET_LIFETIME: Duration = Duration::from_secs(120);

/// Bytes every in-flight read's raw text and reply frames may reserve together.
pub(in crate::app) const READ_REPLY_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// The text cap each destination of a read with `label_count` destinations reads with.
pub(in crate::app) fn destination_cap(label_count: usize) -> usize {
    if label_count > 1 {
        READ_TEXT_CAP / 2
    } else {
        // When: label_count is one, the destination reads with the whole cap.
        READ_TEXT_CAP
    }
}

/// Bytes of one reply frame carrying `text_bytes` of text: `ESC ] 52 ; l ;`, Base64, `ESC \`.
pub(in crate::app) fn frame_bytes(text_bytes: usize) -> usize {
    7 + base64::encoded_len(text_bytes, true).unwrap_or(usize::MAX - 9) + 2
}

/// Bytes a read of `label_count` destinations reserves before it starts: the largest raw text
/// one destination may hold plus every destination's largest frame.
pub(in crate::app) fn read_reservation_bytes(label_count: usize) -> usize {
    let cap = destination_cap(label_count);
    cap + label_count * frame_bytes(cap)
}

/// Append one ST-terminated reply labelled `label` carrying `text` as Base64; the frame is
/// reserved exactly, so the buffer never grows past what the read reserved for it.
pub(in crate::app) fn append_frame(replies: &mut Vec<u8>, label: u8, text: &[u8]) {
    let encoded = base64::encoded_len(text.len(), true).expect("a capped read encodes");
    replies.reserve_exact(7 + encoded + 2);
    replies.extend_from_slice(b"\x1b]52;");
    replies.push(label);
    replies.push(b';');
    let start = replies.len();
    replies.resize(start + encoded, 0);
    let written = base64::engine::general_purpose::STANDARD
        .encode_slice(text, &mut replies[start..])
        .expect("the slice holds the encoded length");
    replies.truncate(start + written);
    replies.extend_from_slice(b"\x1b\\");
}

/// One empty ST-terminated reply per label, in order.
pub(in crate::app) fn empty_frames(labels: &[u8]) -> Vec<u8> {
    let mut replies = Vec::with_capacity(labels.len() * EMPTY_REPLY_BYTES);
    for label in labels {
        append_frame(&mut replies, *label, b"");
    }
    replies
}

/// The labels of the replies one read gets: a single `c`, `s` or `p` is echoed and any other
/// single field answers `c` (Ghostty); a longer field answers once per destination it names,
/// clipboard `c` before PRIMARY `p` (kitty), and `c` when it names neither.
pub(in crate::app) fn reply_labels(kinds: &[u8]) -> Vec<u8> {
    if let [kind] = kinds {
        // When: kinds holds one letter `kind`, `c`, `s` and `p` are echoed and any other is `c`.
        return vec![if matches!(kind, b'c' | b's' | b'p') { *kind } else { b'c' }];
    }
    let clipboard = kinds.iter().any(|kind| matches!(kind, b'c' | b's' | b'0'..=b'9'));
    let primary = kinds.contains(&b'p');
    match (clipboard, primary) {
        (_, false) => vec![b'c'],
        (false, true) => vec![b'p'],
        (true, true) => vec![b'c', b'p'],
    }
}

/// The empty replies answering a read of `kinds`: `ESC ] 52 ; <label> ; ESC \` per label, always
/// ST-terminated whatever terminated the request.
pub(in crate::app) fn empty_replies(kinds: &[u8]) -> Vec<u8> {
    empty_frames(&reply_labels(kinds))
}

/// The reply spool's per-submission cap; a larger submission is refused and latches it failed.
const REPLY_SUBMISSION_CAP_BYTES: usize = 32 * 1024;

/// Append `answers`, whole empty replies, to `batch`, sending `batch` first whenever the next
/// answer plus `reserved_bytes` still to follow would pass the spool's submission cap. A section
/// of many reads can answer with more than one submission holds, so it splits between answers.
pub(in crate::app) fn append_answers(
    batch: &mut Vec<u8>,
    answers: &[u8],
    reserved_bytes: usize,
    mut send: impl FnMut(Vec<u8>),
) {
    for answer in answers.chunks(EMPTY_REPLY_BYTES) {
        let fits = batch.len() + answer.len() + reserved_bytes <= REPLY_SUBMISSION_CAP_BYTES;
        if !fits && !batch.is_empty() {
            // The answer would pass the cap, so the batch so far is sent before it joins.
            send(std::mem::take(batch));
        }
        batch.extend_from_slice(answer);
    }
}

#[cfg(test)]
#[path = "osc52_read_tests.rs"]
mod osc52_read_tests;
