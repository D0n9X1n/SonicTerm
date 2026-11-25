//! The X11 selection protocol for OSC 52 reads, apart from the connection so every platform tests
//! it: a text reply's type, format and size, the `INCR` announcement (ICCCM 2.7.2), the incremental
//! transfer's chunks and deadlines, and the offer version a paste grant binds. The native client in
//! `osc52_read_x11_native.rs` implements [`SelectionPort`]; tests drive a scripted owner.

use std::time::{Duration, Instant};

use super::osc52_read_source::{
    bounded_latin1, bounded_utf8, ReadOutcome, TypeListing, TypeRead, MAX_NATIVE_TYPES_SCANNED,
};

/// How long the selection owner may take to answer `ConvertSelection`.
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
    /// The exact atom requested by a non-text MIME read.
    Requested,
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
        PropertyKind::Incr | PropertyKind::Requested | PropertyKind::Other => ReadOutcome::Failed,
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

/// What one poll of the X11 connection found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum Polled {
    /// The owner answered the conversion: `true` when it stored the property, `false` when it
    /// refused.
    Notify(bool),
    /// The read property received a new value: the next `INCR` chunk.
    NewValue,
    /// Any other event.
    Other,
    /// No event is waiting.
    Idle,
    /// The connection failed.
    Closed,
}

/// The target a conversion asks the owner for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ConvertTarget {
    /// The text, as `UTF8_STRING`.
    Text,
    /// The existing atom named by a MIME read.
    Mime(u32),
    /// When the owner took the selection, as `TIMESTAMP`.
    Timestamp,
}

/// The X11 calls one read makes on its own connection and requestor window.
pub(in crate::app) trait SelectionPort {
    /// The selection's owner window: `Some(0)` with no owner, `None` on a protocol error.
    fn owner(&mut self) -> Option<u32>;
    /// Ask the owner to convert the selection to `target` into the read property; false on error.
    fn convert(&mut self, target: ConvertTarget) -> bool;
    /// The next waiting event, without blocking.
    fn poll(&mut self) -> Polled;
    /// Fetch the read property, asking for one unit past `max_bytes`, deleting it when `delete`.
    fn fetch(&mut self, delete: bool, max_bytes: usize) -> Option<PropertyReply>;
    /// Delete the read property.
    fn delete(&mut self);
    /// The current time.
    fn now(&self) -> Instant;
    /// Wait briefly before the next poll.
    fn pause(&mut self);
}

/// Wait until `deadline` for an event `select` accepts; `None` on timeout or a closed connection.
/// The deadline is checked before every poll, so an owner or client that keeps events coming
/// cannot hold the wait past it.
pub(in crate::app) fn wait_for_event<T>(
    port: &mut dyn SelectionPort,
    deadline: Instant,
    mut select: impl FnMut(Polled) -> Option<T>,
) -> Option<T> {
    loop {
        if port.now() >= deadline {
            // When: now reached the deadline, the owner did not answer in time.
            return None;
        }
        match port.poll() {
            Polled::Idle => port.pause(),
            Polled::Closed => {
                // When: poll reports Closed, the connection failed and no answer can come.
                return None;
            }
            event => {
                // When: poll returned an event, select decides whether it ends the wait.
                if let Some(found) = select(event) {
                    // When: select accepts the event, the wait ends with what it found.
                    return Some(found);
                }
            }
        }
    }
}

/// Wait until `deadline` for the owner's answer to a conversion: whether it stored the property.
pub(in crate::app) fn wait_for_notify(
    port: &mut dyn SelectionPort,
    deadline: Instant,
) -> Option<bool> {
    wait_for_event(port, deadline, |event| match event {
        Polled::Notify(stored) => Some(stored),
        Polled::NewValue | Polled::Other | Polled::Idle | Polled::Closed => None,
    })
}

/// Read the selection as text within `max_bytes` through `port`; `None` on a protocol error or a
/// missed deadline.
pub(in crate::app) fn read_selection(
    port: &mut dyn SelectionPort,
    max_bytes: usize,
) -> Option<ReadOutcome> {
    if !port.convert(ConvertTarget::Text) {
        // When: convert fails, the request never reached the server.
        return None;
    }
    let deadline = port.now() + SELECTION_WAIT;
    if !wait_for_notify(port, deadline)? {
        // When: wait_for_notify reports no stored property, the owner has nothing to convert.
        return Some(ReadOutcome::Empty);
    }
    // The first reply is read without deleting it: deleting an `INCR` announcement asks the owner
    // for the first chunk, so an announced size is validated before that.
    let first = port.fetch(false, max_bytes)?;
    let incr = first.kind == PropertyKind::Incr;
    match classify_first(first, max_bytes) {
        FirstReply::Done(outcome) => {
            // When: classify_first is Done, the read ends; a text or malformed reply is removed,
            // and a refused INCR announcement stays, so the owner is never asked for a chunk.
            if !incr {
                port.delete();
            }
            return Some(outcome);
        }
        FirstReply::Incr => {
            // Deleting the validated announcement asks the owner for the first chunk.
            port.delete();
        }
    }
    let mut transfer = IncrTransfer::new(max_bytes, port.now());
    loop {
        let deadline = transfer.chunk_deadline(port.now());
        wait_for_event(port, deadline, |event| (event == Polled::NewValue).then_some(()))?;
        let chunk = port.fetch(true, transfer.remaining())?;
        if let Some(outcome) = transfer.push(chunk) {
            // When: push ends the transfer, a property left by a refused chunk is removed.
            port.delete();
            return Some(outcome);
        }
    }
}

/// List only valid MIME atoms, preserving the case needed to request them back from X11.
pub(in crate::app) fn list_mime_atoms(names: impl IntoIterator<Item = String>) -> TypeListing {
    let mut listed = Vec::new();
    for name in names.into_iter().take(MAX_NATIVE_TYPES_SCANNED) {
        let mime = match name.as_str() {
            "UTF8_STRING" | "TEXT" | "STRING" | "text/plain;charset=utf-8" => "text/plain".into(),
            _ if super::kitty_clipboard_offer::valid_mime(&name) => name,
            _ => {
                // When: name is neither text nor a valid MIME, it is not a listed representation.
                continue;
            }
        };
        if !listed.contains(&mime) {
            listed.push(mime);
        }
    }
    TypeListing::Types(listed)
}

/// Fetch an existing MIME atom under the text reader's size and time limits, without character
/// conversion. An INCR announcement is never acknowledged before its size passes the cap.
pub(in crate::app) fn read_mime(
    port: &mut dyn SelectionPort,
    atom: u32,
    max_bytes: usize,
) -> Option<TypeRead> {
    if !port.convert(ConvertTarget::Mime(atom)) {
        // When: convert failed, no answer can arrive.
        return None;
    }
    let deadline = port.now() + SELECTION_WAIT;
    if !wait_for_notify(port, deadline)? {
        // When: wait_for_notify reports refusal, the selection lacks this type.
        return Some(TypeRead::Absent);
    }
    let first = port.fetch(false, max_bytes)?;
    if first.kind != PropertyKind::Incr {
        // When: first is not INCR, the bounded property is the entire representation.
        port.delete();
        return Some(mime_bytes(first, max_bytes));
    }
    let Ok(size) = <[u8; 4]>::try_from(first.value.as_slice()) else {
        // When: size is not one word, this is not an INCR announcement.
        return None;
    };
    if first.format != 32 || first.bytes_after != 0 {
        // When: format or bytes_after rules out a single INCR word, leave it unacknowledged.
        return None;
    }
    if u32::from_ne_bytes(size) as usize > max_bytes {
        // When: size exceeds max_bytes, leave the announcement unacknowledged.
        return Some(TypeRead::TooLarge);
    }
    port.delete();
    let end = port.now() + INCR_TOTAL_WAIT;
    let mut bytes = Vec::new();
    loop {
        let deadline = end.min(port.now() + CHUNK_WAIT);
        wait_for_event(port, deadline, |event| (event == Polled::NewValue).then_some(()))?;
        let left = max_bytes - bytes.len();
        match mime_bytes(port.fetch(true, left)?, left) {
            TypeRead::Bytes(chunk) if chunk.is_empty() => {
                // When: chunk is empty, the requestor has the complete representation.
                return Some(TypeRead::Bytes(bytes));
            }
            TypeRead::Bytes(chunk) => {
                bytes.reserve_exact(chunk.len());
                bytes.extend_from_slice(&chunk);
            }
            refusal => {
                // When: refusal rejects a chunk, discard the prefix and report its outcome.
                return Some(refusal);
            }
        }
    }
}

fn mime_bytes(reply: PropertyReply, room: usize) -> TypeRead {
    if reply.kind != PropertyKind::Requested || reply.format != 8 {
        // If kind or format differs, the bytes do not represent the requested MIME.
        TypeRead::Failed
    } else if reply.bytes_after != 0 || reply.value.len() > room {
        // When: bytes_after or value length indicates truncation, omit rather than serve a prefix.
        TypeRead::TooLarge
    } else {
        // When: reply fits room with no bytes_after, move the validated property value.
        TypeRead::Bytes(reply.value)
    }
}

/// The offer the selection holds now: its owner's window in the high 32 bits and its `TIMESTAMP`
/// answer in the low 32. X11 cannot read both at once, so the owner is read before the conversion
/// and again after the answer, and must be the same. `None`, so no paste grant can match, with no
/// owner, a raced owner, a protocol error, a missed selection wait, or a malformed answer: anything
/// but one nonzero format-32 item.
pub(in crate::app) fn offer_version_with(port: &mut dyn SelectionPort) -> Option<u64> {
    let owner = port.owner()?;
    if owner == 0 {
        // When: owner is 0, nothing is offered, so there is no version.
        return None;
    }
    if !port.convert(ConvertTarget::Timestamp) {
        // When: convert fails, the request never reached the server.
        return None;
    }
    let deadline = port.now() + SELECTION_WAIT;
    if !wait_for_notify(port, deadline)? {
        // When: wait_for_notify reports no stored property, the owner cannot answer TIMESTAMP.
        return None;
    }
    // Fetched one unit past a single item, without deleting, so an extra item and an `INCR`
    // announcement both show, and an announcement is never answered with a chunk request.
    let reply = port.fetch(false, 4)?;
    let incr = reply.kind == PropertyKind::Incr;
    // The answer is removed; an INCR announcement is left unanswered.
    if !incr {
        port.delete();
    }
    let well_formed = !incr && reply.format == 32 && reply.bytes_after == 0;
    let item: [u8; 4] = reply.value.as_slice().try_into().ok().filter(|_| well_formed)?;
    let stamp = u32::from_ne_bytes(item);
    if stamp == 0 || port.owner()? != owner {
        // When: stamp is 0 (CurrentTime, no real time) or the owner changed during the
        // conversion, the answer may not describe the offer, so there is no version.
        return None;
    }
    Some((u64::from(owner) << 32) | u64::from(stamp))
}

#[cfg(test)]
#[path = "osc52_read_x11_tests.rs"]
mod osc52_read_x11_tests;
