//! OSC 52 and OSC 1337 `Copy=:` clipboard writes, streamed rather than captured whole.
//!
//! The raw collector owns `52;` and `1337;Copy=:`. Encoded payload bytes leave the parser in
//! chunks of at most [`MAX_CLIPBOARD_CHUNK_BYTES`] through the parser's [`ClipboardInbox`], and the
//! parser raises only payload-free [`VtEvent::ClipboardStream`] events, so a clipboard write is
//! never bounded by the generic escape cap and never cloned or printed with an event. Decoding and
//! every policy decision belong to the host.

use super::*;

/// Most encoded payload bytes one inbox chunk carries.
pub const MAX_CLIPBOARD_CHUNK_BYTES: usize = 64 * 1024;

/// Clipboard payload one parse section may queue, over both inboxes, before the parser yields so
/// the host drains it: the inboxes hold encoded bytes the staging budget does not charge, so a
/// section of many packets must not pile them up.
pub const MAX_CLIPBOARD_INBOX_SECTION_BYTES: usize = 1024 * 1024;

/// Most bytes an OSC 52 kinds field may hold; a longer one is swallowed to its terminator.
pub const MAX_CLIPBOARD_KINDS_BYTES: usize = 16;

/// The OSC 1337 prefix, matched in any ASCII case, whose value the collector streams as a `c` write.
pub(super) const COPY_OSC_PREFIX: &[u8; 11] = b"1337;Copy=:";

/// One stage of a clipboard stream, carried without payload bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardPhase {
    /// One more chunk of this stream waits in the inbox.
    Data,
    /// BEL or ST completed the stream: its kinds field, and whether the payload was a `?` read.
    End {
        /// The OSC 52 kinds field as sent; `c` for OSC 1337 `Copy=:`.
        kinds: ClipboardKinds,
        /// The payload was exactly `?`, a clipboard read.
        query: bool,
    },
    /// CAN, SUB, an unrelated ESC or the host's expiry ended the stream; nothing commits.
    Abort,
}

/// An OSC 52 kinds field of at most [`MAX_CLIPBOARD_KINDS_BYTES`] bytes, held inline.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ClipboardKinds {
    bytes: [u8; MAX_CLIPBOARD_KINDS_BYTES],
    len: u8,
}

impl ClipboardKinds {
    /// The kinds field's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// A kinds field holding `bytes`, which must fit [`MAX_CLIPBOARD_KINDS_BYTES`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut kinds = Self { bytes: [0; MAX_CLIPBOARD_KINDS_BYTES], len: 0 };
        for byte in bytes {
            if !kinds.push(*byte) {
                // When: `bytes` exceeds the inline capacity, there is no such kinds field.
                return None;
            }
        }
        Some(kinds)
    }

    /// Append `byte`; false when the field is already full.
    fn push(&mut self, byte: u8) -> bool {
        let index = usize::from(self.len);
        if index == MAX_CLIPBOARD_KINDS_BYTES {
            // When: index reaches MAX_CLIPBOARD_KINDS_BYTES, the field cannot grow.
            return false;
        }
        self.bytes[index] = byte;
        self.len += 1;
        true
    }
}

impl std::fmt::Debug for ClipboardKinds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ClipboardKinds({:?})", String::from_utf8_lossy(self.as_bytes()))
    }
}

/// Encoded clipboard chunks waiting for the host, tagged with their stream.
///
/// Not `Clone`, and its `Debug` prints only lengths, so queued bytes cannot be copied or logged.
#[derive(Default)]
pub struct ClipboardInbox {
    chunks: std::sync::Mutex<std::collections::VecDeque<(u64, Vec<u8>)>>,
}

impl ClipboardInbox {
    /// Lock the queue; a poisoned lock still holds a valid queue of whole chunks.
    fn queue(&self) -> std::sync::MutexGuard<'_, std::collections::VecDeque<(u64, Vec<u8>)>> {
        self.chunks.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn push(&self, stream: u64, chunk: Vec<u8>) {
        self.queue().push_back((stream, chunk));
    }

    /// Take the oldest chunk of `stream`, dropping older chunks of streams the host abandoned.
    /// Stream ids only grow, so a chunk of a newer stream ends the search and stays queued: a
    /// stale request for an abandoned stream never takes a later write's data.
    pub fn pop(&self, stream: u64) -> Option<Vec<u8>> {
        let mut queue = self.queue();
        while let Some((queued, _)) = queue.front() {
            if *queued > stream {
                // When: the front chunk belongs to a newer stream, `stream` has nothing queued.
                return None;
            }
            let (queued, chunk) = queue.pop_front().expect("a front chunk");
            if queued == stream {
                // When: the front chunk belongs to `stream`, it is the next one in order.
                return Some(chunk);
            }
        }
        None
    }

    /// Drop every chunk of `stream`.
    pub fn discard(&self, stream: u64) {
        self.queue().retain(|(queued, _)| *queued != stream);
    }

    /// Whether no chunk is queued.
    pub fn is_empty(&self) -> bool {
        self.queue().is_empty()
    }

    /// Encoded bytes queued over every stream.
    pub fn queued_bytes(&self) -> usize {
        self.queue().iter().map(|(_, chunk)| chunk.len()).sum()
    }
}

impl std::fmt::Debug for ClipboardInbox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let queue = self.queue();
        formatter
            .debug_struct("ClipboardInbox")
            .field("chunks", &queue.len())
            .field("queued_bytes", &queue.iter().map(|(_, chunk)| chunk.len()).sum::<usize>())
            .finish()
    }
}

/// The collector state of one clipboard stream.
pub(super) struct ClipboardCollector {
    /// The stream's id, named in every event and inbox chunk.
    stream: u64,
    /// The kinds field read so far; fixed `c` for `Copy=:`.
    kinds: ClipboardKinds,
    /// Whether the kinds field has ended, so later bytes are payload.
    in_payload: bool,
    /// Payload bytes not yet queued; at most one chunk.
    chunk: Vec<u8>,
    /// Whether any chunk of this stream has been queued.
    pushed: bool,
    /// The previous byte was an ESC that may start ST.
    pending_esc: bool,
    /// The stream came from OSC 1337 `Copy=:`, whose empty or `?` value writes nothing.
    copy_form: bool,
}

impl Parser {
    /// The inbox this parser's clipboard chunks wait in.
    pub fn clipboard_inbox(&self) -> &Arc<ClipboardInbox> {
        &self.clipboard_inbox
    }

    /// The id of the clipboard stream still open, if one is.
    pub fn open_clipboard_stream(&self) -> Option<u64> {
        match &self.raw_osc {
            Some(RawOsc::Clipboard(collector)) => Some(collector.stream),
            _ => None,
        }
    }

    /// End the open clipboard stream for the host (its 60 s expiry): release its queued chunks and
    /// swallow the rest of the OSC through its terminator. Returns the aborted stream's id.
    pub fn abort_clipboard_stream(&mut self) -> Option<u64> {
        let Some(RawOsc::Clipboard(collector)) = self.raw_osc.take() else {
            // When: no clipboard stream is open, leave the collector untouched.
            return None;
        };
        let stream = collector.stream;
        self.clipboard_inbox.discard(stream);
        self.raw_osc = Some(RawOsc::Refused { pending_esc: collector.pending_esc });
        Some(stream)
    }

    /// Start collecting a clipboard stream after its `52;` or `1337;Copy=:` prefix.
    pub(super) fn begin_clipboard_stream(&mut self, copy_form: bool) {
        self.inner = vte::Parser::new();
        self.pending_esc = false;
        self.clipboard_streams = self.clipboard_streams.wrapping_add(1);
        let kinds = if copy_form {
            ClipboardKinds::from_bytes(b"c").expect("one kind fits")
        } else {
            // When: copy_form is false, the stream is OSC 52 and its kinds field is read from the wire.
            ClipboardKinds::from_bytes(b"").expect("empty kinds fit")
        };
        self.raw_osc = Some(RawOsc::Clipboard(Box::new(ClipboardCollector {
            stream: self.clipboard_streams,
            kinds,
            in_payload: copy_form,
            chunk: Vec::new(),
            pushed: false,
            pending_esc: false,
            copy_form,
        })));
    }

    /// Feed one byte to the open clipboard stream; returns whether the collector consumed it.
    pub(super) fn consume_clipboard_byte(
        &mut self,
        mut collector: Box<ClipboardCollector>,
        byte: u8,
    ) -> bool {
        if matches!(byte, 0x18 | 0x1a) {
            // When: byte matches CAN or SUB, the stream aborts and the sequence ends here.
            self.abort_collector(&collector);
            self.finish_escape();
            return true;
        }
        if collector.pending_esc {
            // When: pending_esc is set, only `\` completes ST; any other byte aborts the stream and
            // the collector swallows the rest of the OSC through its terminator.
            if byte == b'\\' {
                // When: byte is `\\` after ESC, ST completes the stream.
                self.end_clipboard_stream(*collector);
                self.finish_escape();
                return true;
            }
            self.abort_collector(&collector);
            self.raw_osc = Some(RawOsc::Refused { pending_esc: false });
            return self.consume_raw_osc_byte(byte);
        }
        if byte == 0x07 {
            // When: byte is BEL, the stream is complete.
            self.end_clipboard_stream(*collector);
            self.finish_escape();
            return true;
        }
        if byte == 0x1b {
            collector.pending_esc = true;
        } else if collector.in_payload {
            // When: collector is in_payload, the byte is encoded payload.
            collector.chunk.push(byte);
            if collector.chunk.len() == MAX_CLIPBOARD_CHUNK_BYTES {
                // A full chunk goes to the host, so the parser holds one at most.
                let chunk = std::mem::take(&mut collector.chunk);
                self.queue_clipboard_chunk(&mut collector, chunk);
            }
        } else if byte == b';' {
            // When: byte is `;`, the kinds field ends and the payload begins.
            collector.in_payload = true;
        } else if !collector.kinds.push(byte) {
            // When: the kinds field passes MAX_CLIPBOARD_KINDS_BYTES, nothing of the OSC applies;
            // no event was raised yet, so the rest is swallowed silently.
            self.raw_osc = Some(RawOsc::Refused { pending_esc: false });
            return true;
        }
        self.raw_osc = Some(RawOsc::Clipboard(collector));
        true
    }

    /// Queue `chunk` for the host and announce it.
    fn queue_clipboard_chunk(&mut self, collector: &mut ClipboardCollector, chunk: Vec<u8>) {
        self.clipboard_inbox.push(collector.stream, chunk);
        self.yield_on_full_clipboard_inboxes();
        collector.pushed = true;
        self.performer.events.push(VtEvent::ClipboardStream {
            stream: collector.stream,
            phase: ClipboardPhase::Data,
        });
    }

    /// Release `collector`'s queued chunks and tell the host the stream aborted.
    fn abort_collector(&mut self, collector: &ClipboardCollector) {
        self.clipboard_inbox.discard(collector.stream);
        self.performer.events.push(VtEvent::ClipboardStream {
            stream: collector.stream,
            phase: ClipboardPhase::Abort,
        });
    }

    /// Complete a terminated stream: queue its last chunk and raise `End`.
    fn end_clipboard_stream(&mut self, mut collector: ClipboardCollector) {
        let query = !collector.pushed && collector.chunk == b"?";
        if collector.copy_form && !collector.pushed && (collector.chunk.is_empty() || query) {
            // When: copy_form holds an empty or `?` value, OSC 1337 `Copy=:` writes and raises nothing.
            return;
        }
        let chunk = std::mem::take(&mut collector.chunk);
        if !chunk.is_empty() && !query {
            // Payload bytes that remain, when the payload is no read, are the last chunk.
            self.queue_clipboard_chunk(&mut collector, chunk);
        }
        self.performer.events.push(VtEvent::ClipboardStream {
            stream: collector.stream,
            phase: ClipboardPhase::End { kinds: collector.kinds, query },
        });
    }

    /// Bytes the open clipboard stream holds in the parser, its partial chunk.
    pub(super) fn clipboard_retained_bytes(&self) -> usize {
        match &self.raw_osc {
            Some(RawOsc::Clipboard(collector)) => collector.chunk.capacity(),
            _ => 0,
        }
    }
}

/// Whether `probed` is a prefix of `1337;Copy=:` in any ASCII case.
pub(super) fn is_copy_prefix(probed: &[u8]) -> bool {
    probed.len() <= COPY_OSC_PREFIX.len()
        && probed.eq_ignore_ascii_case(&COPY_OSC_PREFIX[..probed.len()])
}

#[cfg(test)]
#[path = "vt_clipboard_tests.rs"]
mod vt_clipboard_tests;
