//! The X11 selection reply checks for OSC 52 reads, apart from the connection so every platform
//! tests them: a text reply's type, format and size, the `INCR` announcement (ICCCM 2.7.2), and
//! the incremental transfer's chunks and deadlines.

use std::time::{Duration, Instant};

use super::osc52_read_source::{bounded_latin1, bounded_utf8, ReadOutcome};

/// How long the selection owner may take to answer `ConvertSelection`.
#[cfg(target_os = "linux")]
pub(in crate::app) const SELECTION_WAIT: Duration = Duration::from_secs(2);

/// How long one `INCR` chunk may take to arrive.
pub(in crate::app) const CHUNK_WAIT: Duration = Duration::from_secs(2);

/// How long a whole `INCR` transfer may take.
pub(in crate::app) const INCR_TOTAL_WAIT: Duration = Duration::from_secs(5);

/// A property's type, as far as a read cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum PropertyKind {
    /// `UTF8_STRING` or `text/plain;charset=utf-8`.
    Utf8,
    /// `STRING`, which ICCCM defines as Latin-1.
    Latin1,
    /// `INCR`: the value arrives in chunks.
    Incr,
    /// Anything else.
    Other,
}

/// One `GetProperty` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct PropertyReply {
    /// The property's type.
    pub(in crate::app) kind: PropertyKind,
    /// 8, 16 or 32 bits per item.
    pub(in crate::app) format: u8,
    /// Bytes the fetch left unread.
    pub(in crate::app) bytes_after: u32,
    /// The bytes the fetch returned.
    pub(in crate::app) value: Vec<u8>,
}

/// What the first reply to `ConvertSelection` decides.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::app) enum FirstReply {
    /// The read is answered.
    Done(ReadOutcome),
    /// An `INCR` transfer within the cap starts.
    Incr,
}

/// The 32-bit units a fetch asks for: one past the cap, so a value one byte over it is seen.
pub(in crate::app) fn fetch_length_units(max_bytes: usize) -> u32 {
    u32::try_from(max_bytes / 4 + 1).unwrap_or(u32::MAX)
}

/// Decode a format-8 text value of `kind` within `max_bytes`.
fn decode(kind: PropertyKind, value: Vec<u8>, max_bytes: usize) -> ReadOutcome {
    match kind {
        PropertyKind::Utf8 => bounded_utf8(value, max_bytes),
        PropertyKind::Latin1 => bounded_latin1(&value, max_bytes),
        PropertyKind::Incr | PropertyKind::Other => ReadOutcome::Failed,
    }
}

/// Classify the first reply: an `INCR` announcement is format 32 holding the lower-bound size,
/// refused past the cap; a text reply is format 8 of a text type with nothing left unread. The
/// reply is taken by value, so a text value moves into the outcome instead of being copied.
pub(in crate::app) fn classify_first(reply: PropertyReply, max_bytes: usize) -> FirstReply {
    if reply.kind == PropertyKind::Incr {
        // When: kind is Incr, only a format-32 size announcement starts a transfer.
        let Some(size) = reply.value.get(..4).filter(|_| reply.format == 32) else {
            // When: the announcement is not format 32 with a size, the reply is malformed.
            return FirstReply::Done(ReadOutcome::Failed);
        };
        // x11rb returns property items in the client's byte order.
        let size = u32::from_ne_bytes([size[0], size[1], size[2], size[3]]) as usize;
        if size > max_bytes {
            // When: the announced lower bound passes max_bytes, no chunk is ever requested.
            return FirstReply::Done(ReadOutcome::TooLarge);
        }
        return FirstReply::Incr;
    }
    if reply.format != 8 || !matches!(reply.kind, PropertyKind::Utf8 | PropertyKind::Latin1) {
        // When: format is not 8 or kind is not text, the owner answered something else.
        return FirstReply::Done(ReadOutcome::Failed);
    }
    if reply.bytes_after > 0 || reply.value.len() > max_bytes {
        // When: bytes remain or the value passes max_bytes, the text is too large.
        return FirstReply::Done(ReadOutcome::TooLarge);
    }
    FirstReply::Done(decode(reply.kind, reply.value, max_bytes))
}

/// One `INCR` transfer: format-8 chunks of one text type, until a zero-length chunk.
pub(in crate::app) struct IncrTransfer {
    max_bytes: usize,
    started_at: Instant,
    kind: Option<PropertyKind>,
    bytes: Vec<u8>,
}

impl IncrTransfer {
    /// A transfer capped at `max_bytes`, started at `started_at`.
    pub(in crate::app) fn new(max_bytes: usize, started_at: Instant) -> Self {
        Self { max_bytes, started_at, kind: None, bytes: Vec::new() }
    }

    /// Bytes the next chunk may still add.
    pub(in crate::app) fn remaining(&self) -> usize {
        self.max_bytes - self.bytes.len()
    }

    /// When the next chunk must arrive: 2 s from `now`, and never past 5 s from the start.
    pub(in crate::app) fn chunk_deadline(&self, now: Instant) -> Instant {
        (now + CHUNK_WAIT).min(self.started_at + INCR_TOTAL_WAIT)
    }

    /// Take one chunk; `Some` ends the transfer.
    pub(in crate::app) fn push(&mut self, chunk: PropertyReply) -> Option<ReadOutcome> {
        let text = matches!(chunk.kind, PropertyKind::Utf8 | PropertyKind::Latin1);
        if chunk.format != 8 || !text || self.kind.is_some_and(|kind| kind != chunk.kind) {
            // When: the chunk is not format-8 text of the transfer's one type, it fails.
            return Some(ReadOutcome::Failed);
        }
        self.kind = Some(chunk.kind);
        if chunk.value.is_empty() && chunk.bytes_after == 0 {
            // When: the chunk is empty, the owner ended the transfer.
            let bytes = std::mem::take(&mut self.bytes);
            return Some(decode(chunk.kind, bytes, self.max_bytes));
        }
        if chunk.bytes_after > 0 || chunk.value.len() > self.remaining() {
            // When: the chunk holds more than the room left, the transfer stops too large.
            return Some(ReadOutcome::TooLarge);
        }
        self.bytes.reserve_exact(chunk.value.len());
        self.bytes.extend_from_slice(&chunk.value);
        None
    }
}

#[cfg(test)]
#[path = "osc52_read_x11_tests.rs"]
mod osc52_read_x11_tests;
