//! OSC 52 clipboard reads (`ESC ] 52 ; <kinds> ; ? BEL|ST`) above the parser: the reply labels a
//! request names and the empty replies that answer it.
//!
//! No reply carries clipboard bytes: under `[terminal] osc52_read = "ask"` or `"allow"` every read
//! is answered empty, as a declined consent prompt is in Ghostty and kitty, so a program waiting
//! for an answer never hangs; under `"deny"` the read is dropped silently, as Ghostty and WezTerm
//! do. The replies ride the ordinary parser-reply path, because they hold no sensitive data.

/// Bytes of one empty reply, `ESC ] 52 ; <label> ; ESC \`; a reply batch splits only at multiples.
pub(in crate::app) const EMPTY_REPLY_BYTES: usize = 9;

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
    let mut replies = Vec::new();
    for label in reply_labels(kinds) {
        replies.extend_from_slice(b"\x1b]52;");
        replies.push(label);
        replies.extend_from_slice(b";\x1b\\");
    }
    replies
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
