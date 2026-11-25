//! OSC 5522, the kitty clipboard protocol: the metadata grammar and status replies.
//!
//! A packet is `5522;<metadata>[;<payload>]`, split at the first `;`. Metadata is `:`-separated
//! `key=value` records; a duplicate key keeps its last value and unknown keys are ignored. `mime`,
//! `name` and `pw` are strict padded Base64 of UTF-8, bounded as Ghostty bounds them, and `id`
//! keeps only `[A-Za-z0-9-_+.]`. Nothing here holds clipboard data or decides policy: the host
//! owns transactions and answers with [`kitty_clipboard_status_reply`].

use base64::Engine as _;

use super::*;

/// Most metadata bytes a packet may carry before its first `;`; longer metadata is truncated.
pub const MAX_KITTY_CLIPBOARD_METADATA_BYTES: usize = 4096;

/// Most wire bytes one packet may carry after `5522;` (metadata, `;` and payload); a longer packet
/// is classified only from its complete metadata and never queued.
pub const MAX_KITTY_CLIPBOARD_PACKET_BYTES: usize = 1024 * 1024;

/// Most bytes a sanitized `id` keeps.
pub const MAX_KITTY_CLIPBOARD_ID_BYTES: usize = 512;

/// Most decoded bytes of a `mime` or `name` value.
pub const MAX_KITTY_CLIPBOARD_MIME_BYTES: usize = 256;

/// Most decoded bytes of a `pw` value; a longer one is cleared.
pub const MAX_KITTY_CLIPBOARD_PASSWORD_BYTES: usize = 128;

/// The operation a packet's last `type` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyClipboardOperation {
    /// Ask for clipboard data.
    Read,
    /// Open a write transaction.
    Write,
    /// One chunk of a representation, or the commit when it names no MIME type.
    Wdata,
    /// Name another MIME type for a staged representation.
    Walias,
}

impl KittyClipboardOperation {
    /// The operation's wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Wdata => "wdata",
            Self::Walias => "walias",
        }
    }

    fn from_bytes(value: &[u8]) -> Option<Self> {
        match value {
            b"read" => Some(Self::Read),
            b"write" => Some(Self::Write),
            b"wdata" => Some(Self::Wdata),
            b"walias" => Some(Self::Walias),
            _ => None,
        }
    }
}

/// Which selection a packet addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyClipboardLocation {
    /// The standard clipboard.
    Standard,
    /// The X11/Wayland PRIMARY selection (`loc=primary`).
    Primary,
}

/// The decoded, validated metadata of one packet.
#[derive(Clone, PartialEq, Eq)]
pub struct KittyClipboardMetadata {
    /// The last `type`.
    pub operation: KittyClipboardOperation,
    /// `loc=primary`, else the standard clipboard.
    pub location: KittyClipboardLocation,
    /// The sanitized `id`; empty when absent.
    pub id: String,
    /// The decoded `mime`; empty when absent.
    pub mime: String,
    /// The decoded `name`; empty when absent.
    pub name: String,
    /// The decoded `pw`; empty when absent or over its bound.
    pub password: String,
}

impl std::fmt::Debug for KittyClipboardMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A password or name can be a secret, so only their lengths are printed.
        formatter
            .debug_struct("KittyClipboardMetadata")
            .field("operation", &self.operation)
            .field("location", &self.location)
            .field("id", &self.id)
            .field("mime", &self.mime)
            .field("name_bytes", &self.name.len())
            .field("password_bytes", &self.password.len())
            .finish()
    }
}

/// Why metadata was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyClipboardMetadataError {
    /// A record without `=`, a missing or unknown `type`, or metadata past its bound: the packet is
    /// dropped with no reply, even while a write is open.
    Structure,
    /// A `mime`, `name` or `pw` that is not strict Base64 of UTF-8, or a `mime` or `name` over its
    /// bound, in a packet whose operation is given.
    InvalidValue(KittyClipboardOperation),
}

/// A status a reply carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyClipboardStatus {
    /// The operation succeeded.
    Done,
    /// The policy refused it.
    Eperm,
    /// A malformed value.
    Einval,
    /// A native clipboard or storage failure.
    Eio,
    /// The location or representation cannot be held.
    Enosys,
    /// A temporary problem: a busy budget or an expired transaction.
    Ebusy,
    /// Over the size limit.
    Efbig,
}

impl KittyClipboardStatus {
    /// The status's wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "DONE",
            Self::Eperm => "EPERM",
            Self::Einval => "EINVAL",
            Self::Eio => "EIO",
            Self::Enosys => "ENOSYS",
            Self::Ebusy => "EBUSY",
            Self::Efbig => "EFBIG",
        }
    }
}

/// The raw values of the keys the protocol reads, each the last one seen.
#[derive(Default)]
struct RawFields<'raw> {
    operation: Option<&'raw [u8]>,
    location: &'raw [u8],
    id: &'raw [u8],
    mime: &'raw [u8],
    name: &'raw [u8],
    password: &'raw [u8],
}

/// Split `raw` into its records; `None` when any record lacks `=` or `raw` is past its bound.
fn raw_fields(raw: &[u8]) -> Option<RawFields<'_>> {
    if raw.len() > MAX_KITTY_CLIPBOARD_METADATA_BYTES {
        // When: raw is past its bound, it was truncated, so its structure is unknown.
        return None;
    }
    let mut fields = RawFields::default();
    // Every record is visited before any value is decoded, so the last duplicate wins.
    for record in raw.split(|byte| *byte == b':') {
        let separator = record.iter().position(|byte| *byte == b'=')?;
        let (key, value) = (&record[..separator], &record[separator + 1..]);
        match key {
            b"type" => fields.operation = Some(value),
            b"loc" => fields.location = value,
            b"id" => fields.id = value,
            b"mime" => fields.mime = value,
            b"name" => fields.name = value,
            b"pw" => fields.password = value,
            _ => {
                // When: key names no field the protocol reads, the record is ignored, as kitty
                // and Ghostty ignore unknown keys.
            }
        }
    }
    Some(fields)
}

/// The operation complete, structurally valid metadata names, without decoding any value; `None`
/// for truncated or invalid metadata. An over-cap packet is classified only through this.
pub fn kitty_clipboard_operation(raw: &[u8]) -> Option<KittyClipboardOperation> {
    KittyClipboardOperation::from_bytes(raw_fields(raw)?.operation?)
}

/// Why a field value was not decoded.
enum FieldError {
    /// Not strict padded Base64 of UTF-8.
    Invalid,
    /// Valid, but over its bound.
    Overflow,
}

/// Decode `value` as strict padded Base64 of UTF-8 of at most `max_bytes`; the encoded length is
/// checked first, so an over-long value is refused before anything is allocated.
fn decode_field(value: &[u8], max_bytes: usize) -> Result<String, FieldError> {
    let encoded_cap = base64::encoded_len(max_bytes, true).unwrap_or(usize::MAX);
    if value.len() > encoded_cap {
        // When: value is longer than any encoding of max_bytes, it is over its bound.
        return Err(FieldError::Overflow);
    }
    let decoded =
        base64::engine::general_purpose::STANDARD.decode(value).map_err(|_| FieldError::Invalid)?;
    let text = String::from_utf8(decoded).map_err(|_| FieldError::Invalid)?;
    if text.len() > max_bytes {
        // When: the decoded text passes max_bytes, it is over its bound.
        return Err(FieldError::Overflow);
    }
    Ok(text)
}

/// Keep `[A-Za-z0-9-_+.]` of `value`, at most [`MAX_KITTY_CLIPBOARD_ID_BYTES`].
fn sanitize_id(value: &[u8]) -> String {
    let allowed = |byte: &&u8| byte.is_ascii_alphanumeric() || b"-_+.".contains(byte);
    value
        .iter()
        .filter(allowed)
        .take(MAX_KITTY_CLIPBOARD_ID_BYTES)
        .map(|byte| char::from(*byte))
        .collect()
}

/// Parse one packet's metadata (the bytes before its first `;`).
pub fn parse_kitty_clipboard_metadata(
    raw: &[u8],
) -> Result<KittyClipboardMetadata, KittyClipboardMetadataError> {
    let fields = raw_fields(raw).ok_or(KittyClipboardMetadataError::Structure)?;
    let operation = fields
        .operation
        .and_then(KittyClipboardOperation::from_bytes)
        .ok_or(KittyClipboardMetadataError::Structure)?;
    let invalid = |_| KittyClipboardMetadataError::InvalidValue(operation);
    let mime = decode_field(fields.mime, MAX_KITTY_CLIPBOARD_MIME_BYTES).map_err(invalid)?;
    let name = decode_field(fields.name, MAX_KITTY_CLIPBOARD_MIME_BYTES).map_err(invalid)?;
    let password = match decode_field(fields.password, MAX_KITTY_CLIPBOARD_PASSWORD_BYTES) {
        Ok(password) => password,
        Err(FieldError::Overflow) => String::new(),
        Err(FieldError::Invalid) => {
            // When: the password is not strict Base64 of UTF-8, the value is invalid.
            return Err(KittyClipboardMetadataError::InvalidValue(operation));
        }
    };
    let location = if fields.location == b"primary" {
        KittyClipboardLocation::Primary
    } else {
        // An absent loc, or any value but `primary`, means the standard clipboard.
        KittyClipboardLocation::Standard
    };
    Ok(KittyClipboardMetadata {
        operation,
        location,
        id: sanitize_id(fields.id),
        mime,
        name,
        password,
    })
}

/// One status reply: `ESC ] 5522 ; type=<op>:status=<S>[:loc=primary][:id=<id>]`, ended by BEL when
/// `bel` (the request's terminator) and otherwise ST. `id` must already be sanitized.
pub fn kitty_clipboard_status_reply(
    operation: KittyClipboardOperation,
    status: KittyClipboardStatus,
    location: KittyClipboardLocation,
    id: &str,
    bel: bool,
) -> Vec<u8> {
    let mut reply =
        format!("\x1b]5522;type={}:status={}", operation.as_str(), status.as_str()).into_bytes();
    if location == KittyClipboardLocation::Primary {
        reply.extend_from_slice(b":loc=primary");
    }
    if !id.is_empty() {
        reply.extend_from_slice(b":id=");
        reply.extend_from_slice(id.as_bytes());
    }
    reply.extend_from_slice(if bel { b"\x07" } else { b"\x1b\\" });
    reply
}

/// What one complete OSC 5522 packet asks of the host. Payload bytes never ride on it: a valid
/// packet's encoded payload waits in [`Parser::kitty_clipboard_inbox`] under the event's stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KittyClipboardRequest {
    /// Valid metadata; `payload` says whether one inbox chunk holds the packet's payload.
    Packet {
        /// The decoded metadata.
        metadata: KittyClipboardMetadata,
        /// Whether the packet carried a nonempty payload, queued in the kitty inbox.
        payload: bool,
    },
    /// A decoded-field error in a packet of this operation; its payload is not queued.
    InvalidValue(KittyClipboardOperation),
    /// A packet past [`MAX_KITTY_CLIPBOARD_PACKET_BYTES`] whose complete, valid metadata names this
    /// operation; its payload was discarded.
    Overflow(KittyClipboardOperation),
}

/// The collector state of one OSC 5522 packet after its `5522;` prefix.
pub(super) struct KittyClipboardCollector {
    /// Metadata bytes before the first `;`, at most [`MAX_KITTY_CLIPBOARD_METADATA_BYTES`].
    metadata: Vec<u8>,
    /// Encoded payload bytes after the first `;`; emptied once the packet overflows.
    payload: Vec<u8>,
    /// Whether the first `;` arrived, so later bytes are payload.
    in_payload: bool,
    /// Wire bytes after `5522;` so far, counted against [`MAX_KITTY_CLIPBOARD_PACKET_BYTES`].
    wire_bytes: usize,
    /// Whether the packet passed its wire cap.
    overflowed: bool,
    /// The previous byte was an ESC that may start ST.
    pub(super) pending_esc: bool,
}

impl Parser {
    /// The inbox each valid OSC 5522 packet's encoded payload waits in, one chunk per packet.
    pub fn kitty_clipboard_inbox(&self) -> &Arc<ClipboardInbox> {
        &self.kitty_clipboard_inbox
    }

    /// Start collecting an OSC 5522 packet after its `5522;` prefix.
    pub(super) fn begin_kitty_clipboard(&mut self) {
        self.inner = vte::Parser::new();
        self.pending_esc = false;
        self.raw_osc = Some(RawOsc::KittyClipboard(Box::new(KittyClipboardCollector {
            metadata: Vec::new(),
            payload: Vec::new(),
            in_payload: false,
            wire_bytes: 0,
            overflowed: false,
            pending_esc: false,
        })));
    }

    /// Feed one byte to the open OSC 5522 packet; returns whether the collector consumed it.
    pub(super) fn consume_kitty_clipboard_byte(
        &mut self,
        mut collector: Box<KittyClipboardCollector>,
        byte: u8,
    ) -> bool {
        if matches!(byte, 0x18 | 0x1a) {
            // When: byte matches 0x18 or 0x1a (CAN or SUB), the packet is dropped whole and the
            // sequence ends here.
            self.finish_escape();
            return true;
        }
        if collector.pending_esc {
            // When: pending_esc is set, only `\` completes ST; any other ESC ends the packet
            // unapplied and reaches vte with this byte, as a semantic OSC's does.
            if byte == b'\\' {
                // When: byte completes ST, the complete packet is raised.
                self.end_kitty_clipboard(*collector, false);
                self.finish_escape();
                return true;
            }
            self.finish_escape();
            self.feed_vte_byte(0x1b);
            return false;
        }
        if byte == 0x07 {
            // When: byte is BEL, the packet is complete and its reply ends with BEL.
            self.end_kitty_clipboard(*collector, true);
            self.finish_escape();
            return true;
        }
        if byte == 0x1b {
            collector.pending_esc = true;
        } else if !collector.in_payload && byte == b';' {
            // When: byte is the first `;`, the metadata is complete and the payload begins.
            collector.in_payload = true;
            collector.wire_bytes += 1;
        } else if !collector.in_payload {
            // When: collector is not in_payload, byte is metadata before the first `;`.
            if collector.metadata.len() == MAX_KITTY_CLIPBOARD_METADATA_BYTES {
                // When: metadata.len() reaches MAX_KITTY_CLIPBOARD_METADATA_BYTES with no `;`, the
                // metadata is truncated, so the packet is dropped and swallowed to its terminator.
                self.raw_osc = Some(RawOsc::Refused { pending_esc: false });
                return true;
            }
            collector.metadata.push(byte);
            collector.wire_bytes += 1;
        } else if collector.wire_bytes >= MAX_KITTY_CLIPBOARD_PACKET_BYTES {
            // When: wire_bytes reaches MAX_KITTY_CLIPBOARD_PACKET_BYTES, the payload is released;
            // the complete metadata still classifies the packet at its end.
            if !collector.overflowed {
                collector.overflowed = true;
                collector.payload = Vec::new();
            }
        } else {
            // When: byte is payload under the cap, it is held until the packet completes, so the
            // host never sees part of a packet that a CAN or an overflow later drops.
            collector.payload.push(byte);
            collector.wire_bytes += 1;
        }
        self.raw_osc = Some(RawOsc::KittyClipboard(collector));
        true
    }

    /// Raise one complete packet: a valid one queues its payload first; an overflow is raised only
    /// when its metadata is complete and valid; a structure error raises nothing.
    fn end_kitty_clipboard(&mut self, collector: KittyClipboardCollector, bel: bool) {
        let KittyClipboardCollector { metadata, payload, overflowed, .. } = collector;
        let request = if overflowed {
            // When: overflowed is set, only complete, valid metadata classifies the packet.
            let Some(operation) = kitty_clipboard_operation(&metadata) else {
                // When: the over-cap packet's metadata is invalid, it is dropped with no event.
                return;
            };
            KittyClipboardRequest::Overflow(operation)
        } else {
            // When: overflowed is false, the packet is under its cap and its metadata decides
            // what is raised.
            match parse_kitty_clipboard_metadata(&metadata) {
                Ok(metadata) => {
                    KittyClipboardRequest::Packet { metadata, payload: !payload.is_empty() }
                }
                Err(KittyClipboardMetadataError::Structure) => {
                    // When: the metadata has a structure error, the packet is dropped silently.
                    return;
                }
                Err(KittyClipboardMetadataError::InvalidValue(operation)) => {
                    KittyClipboardRequest::InvalidValue(operation)
                }
            }
        };
        self.kitty_clipboard_streams = self.kitty_clipboard_streams.wrapping_add(1);
        let stream = self.kitty_clipboard_streams;
        if matches!(request, KittyClipboardRequest::Packet { payload: true, .. }) {
            // The payload is queued before its event, so the host finds it when the event arrives.
            self.kitty_clipboard_inbox.push(stream, payload);
            self.yield_on_full_clipboard_inboxes();
        }
        self.performer.events.push(VtEvent::KittyClipboard { stream, request, bel });
    }

    /// End the section once the clipboard inboxes hold [`MAX_CLIPBOARD_INBOX_SECTION_BYTES`], so
    /// the host drains them before more packets queue; the staging budget does not charge them.
    pub(super) fn yield_on_full_clipboard_inboxes(&mut self) {
        let queued =
            self.kitty_clipboard_inbox.queued_bytes() + self.clipboard_inbox.queued_bytes();
        self.kitty_yield |= queued >= super::vt_clipboard::MAX_CLIPBOARD_INBOX_SECTION_BYTES;
    }

    /// Bytes the open OSC 5522 packet holds in the parser.
    pub(super) fn kitty_clipboard_retained_bytes(&self) -> usize {
        match &self.raw_osc {
            Some(RawOsc::KittyClipboard(collector)) => {
                collector.metadata.capacity() + collector.payload.capacity()
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
#[path = "vt_kitty_clipboard_tests.rs"]
mod vt_kitty_clipboard_tests;
