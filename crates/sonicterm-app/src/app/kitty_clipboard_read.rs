//! OSC 5522 reads above the parser: the request a `type=read` packet makes, and the whole reply one
//! bounded read answers with.
//!
//! A read's payload is a strict Base64 list of MIME types, where `.` asks for the type listing. The
//! reply is one message: `OK`, an optional listing `DATA` packet, `DATA` chunks of at most 4096 raw
//! bytes per served type, each repeating the opening id and the MIME type, then `DONE`. Every bound
//! is checked before anything grows: a type's text is read with an 8 MiB cap and served only while
//! the whole frame stays within 12 MiB; otherwise it is omitted, which the protocol reads as
//! unavailable. Only text representations are read; other types are omitted.

use base64::Engine as _;
use sonicterm_vt::vt::{
    kitty_clipboard_status_reply, KittyClipboardLocation, KittyClipboardMetadata,
    KittyClipboardOperation, KittyClipboardStatus, MAX_KITTY_CLIPBOARD_MIME_BYTES,
};

pub(in crate::app) use super::osc52_read_source::{
    ClipboardTextSource, ReadLocation, ReadOutcome, TypeListing,
};

/// Raw bytes one `DATA` packet carries at most.
pub(in crate::app) const KITTY_READ_CHUNK_BYTES: usize = 4096;

/// Data types one read accepts; later ones are never read or served.
pub(in crate::app) const MAX_KITTY_READ_TYPES: usize = 16;

/// The most raw text one served type may hold.
pub(in crate::app) const KITTY_READ_TYPE_CAP_BYTES: usize = 8 * 1024 * 1024;

/// The most bytes one whole reply may take.
pub(in crate::app) const KITTY_READ_FRAME_CAP_BYTES: usize = 12 * 1024 * 1024;

/// The most bytes the joined type listing may take, its newline included.
pub(in crate::app) const KITTY_LISTING_CAP_BYTES: usize = 4096;

/// Names one listing carries at most.
const MAX_KITTY_LISTING_NAMES: usize = 16;

/// What one read job reserves from the reply budget before its first native read: its frame, one
/// type's raw text and the listing, rounded up, so three jobs fit the 64 MiB budget and a fourth
/// is refused.
pub(in crate::app) const KITTY_READ_RESERVATION_BYTES: usize = 21 * 1024 * 1024;

/// One parsed `type=read` request: where its replies go and what it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct KittyReadRequest {
    /// The sanitized `id` every reply packet repeats.
    pub(in crate::app) id: String,
    pub(in crate::app) location: KittyClipboardLocation,
    /// Whether `.` asked for the type listing.
    pub(in crate::app) listing: bool,
    /// The requested data types, in order, at most [`MAX_KITTY_READ_TYPES`].
    pub(in crate::app) types: Vec<String>,
    /// Whether the request ended with BEL, which every reply packet repeats.
    pub(in crate::app) bel: bool,
}

impl KittyReadRequest {
    /// Whether the request asks only for the type listing, which reads no content.
    pub(in crate::app) fn listing_only(&self) -> bool {
        self.listing && self.types.is_empty()
    }

    /// The one status packet answering this request with `status`.
    pub(in crate::app) fn status(&self, status: KittyClipboardStatus) -> Vec<u8> {
        let read = KittyClipboardOperation::Read;
        kitty_clipboard_status_reply(read, status, self.location, &self.id, self.bel)
    }

    /// The exact length of one packet answering this request with `mime` and `chunk_len` raw
    /// bytes; `loc=primary` adds its 12 bytes.
    fn data_len(&self, mime_len: usize, chunk_len: usize) -> usize {
        let primary = if self.location == KittyClipboardLocation::Primary { 12 } else { 0 };
        data_packet_len(self.id.len(), mime_len, chunk_len, self.bel) + primary
    }
}

/// The request `metadata` and its decoded `payload` make; `None`, answered with nothing, when the
/// payload is not strict Base64 of UTF-8. Names over the MIME bound and past the type bound drop.
pub(in crate::app) fn parse_read_request(
    metadata: &KittyClipboardMetadata,
    payload: &[u8],
    bel: bool,
) -> Option<KittyReadRequest> {
    let decoded = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    let names = String::from_utf8(decoded).ok()?;
    let mut listing = false;
    let mut types = Vec::new();
    for name in names.split_ascii_whitespace() {
        if name == "." {
            // A `.` asks for the type listing.
            listing = true;
        } else if name.len() <= MAX_KITTY_CLIPBOARD_MIME_BYTES && types.len() < MAX_KITTY_READ_TYPES
        {
            // When: name fits the MIME bound and a type slot is free, it is requested.
            types.push(name.to_owned());
        }
    }
    let id = metadata.id.clone();
    Some(KittyReadRequest { id, location: metadata.location, listing, types, bel })
}

/// The exact length of one read `DATA` packet: `ESC]5522;type=read:status=DATA` (28 bytes),
/// `:id=<id>`, `:mime=<b64>`, `;<b64 chunk>` when the chunk is nonempty, and BEL or ST.
pub(in crate::app) fn data_packet_len(
    id_len: usize,
    mime_len: usize,
    chunk_len: usize,
    bel: bool,
) -> usize {
    let encoded = |length: usize| 4 * length.div_ceil(3);
    // An empty id or chunk drops its field and separator; BEL is one byte, ST two.
    let id = usize::from(id_len > 0) * (4 + id_len);
    let chunk = usize::from(chunk_len > 0) * (1 + encoded(chunk_len));
    28 + id + 6 + encoded(mime_len) + chunk + 2 - usize::from(bel)
}

/// Append `bytes` to `out` as padded Base64, growing `out` by exactly the encoded length.
fn push_base64(out: &mut Vec<u8>, bytes: &[u8]) {
    let start = out.len();
    out.resize(start + 4 * bytes.len().div_ceil(3), 0);
    let written = base64::engine::general_purpose::STANDARD
        .encode_slice(bytes, &mut out[start..])
        .expect("the slice holds the encoded length");
    out.truncate(start + written);
}

/// Append one read packet carrying `status`, with `mime` and `chunk` for a `DATA` packet.
fn push_packet(
    out: &mut Vec<u8>,
    request: &KittyReadRequest,
    status: &str,
    data: Option<(&str, &[u8])>,
) {
    out.extend_from_slice(b"\x1b]5522;type=read:status=");
    out.extend_from_slice(status.as_bytes());
    if request.location == KittyClipboardLocation::Primary {
        out.extend_from_slice(b":loc=primary");
    }
    if !request.id.is_empty() {
        out.extend_from_slice(b":id=");
        out.extend_from_slice(request.id.as_bytes());
    }
    if let Some((mime, chunk)) = data {
        out.extend_from_slice(b":mime=");
        push_base64(out, mime.as_bytes());
        if !chunk.is_empty() {
            // A nonempty chunk follows the metadata; an empty payload is omitted.
            out.push(b';');
            push_base64(out, chunk);
        }
    }
    out.extend_from_slice(if request.bel { b"\x07" } else { b"\x1b\\" });
}

/// The listing payload: at most 16 names of at most 256 bytes, space-joined with a newline, at
/// most [`KITTY_LISTING_CAP_BYTES`]; empty when no name fits.
fn join_listing(names: Vec<String>) -> String {
    let mut joined = String::new();
    let mut count = 0;
    for name in names {
        if name.is_empty() || name.len() > MAX_KITTY_CLIPBOARD_MIME_BYTES || name.contains(' ') {
            // When: the name is empty, over the MIME bound or holds a separator, it is skipped.
            continue;
        }
        let separator = usize::from(!joined.is_empty());
        if count == MAX_KITTY_LISTING_NAMES
            || joined.len() + separator + name.len() + 1 > KITTY_LISTING_CAP_BYTES
        {
            // When: the name count or the joined length would pass its bound, the listing ends.
            break;
        }
        if separator == 1 {
            joined.push(' ');
        }
        joined.push_str(&name);
        count += 1;
    }
    if !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

/// The whole reply to `request`, read through `source`: a status alone when the location is
/// unavailable (`ENOSYS`) or a native read fails (`EIO`), else OK, the listing, every servable
/// type's chunks and DONE. At most one type's raw text is alive at once, and the frame never
/// grows past [`KITTY_READ_FRAME_CAP_BYTES`].
pub(in crate::app) fn build_read_reply(
    source: &dyn ClipboardTextSource,
    request: &KittyReadRequest,
) -> Vec<u8> {
    let location = match request.location {
        KittyClipboardLocation::Standard => ReadLocation::Clipboard,
        KittyClipboardLocation::Primary if cfg!(target_os = "linux") => {
            // When: location is Primary and target_os is Linux, PRIMARY exists and is read.
            ReadLocation::Primary
        }
        KittyClipboardLocation::Primary => {
            // When: location is Primary off Linux, the location is unavailable: `ENOSYS`.
            return request.status(KittyClipboardStatus::Enosys);
        }
    };
    let listing = match request.listing.then(|| source.list_types(location)) {
        Some(TypeListing::Types(names)) => Some(join_listing(names)),
        Some(TypeListing::Unsupported) => {
            // When: list_types is Unsupported, the platform cannot enumerate types: `ENOSYS`.
            return request.status(KittyClipboardStatus::Enosys);
        }
        None => None,
    };
    let fixed = request.data_len(0, 0) * 2
        + listing.as_ref().map_or(0, |joined| request.data_len(1, joined.len()));
    let done_len = request.data_len(0, 0);
    let mut reply = Vec::with_capacity(fixed);
    push_packet(&mut reply, request, "OK", None);
    if let Some(joined) = &listing {
        push_packet(&mut reply, request, "DATA", Some((".", joined.as_bytes())));
    }
    for mime in &request.types {
        if !super::kitty_clipboard_tx::is_text(mime) {
            // When: is_text refuses mime, the type is omitted, which reads as unavailable.
            // shortcut: only text representations are read; native multi-format reads come later.
            continue;
        }
        let text = match source.read_text_bounded(location, KITTY_READ_TYPE_CAP_BYTES) {
            ReadOutcome::Text(text) => text,
            ReadOutcome::Empty | ReadOutcome::TooLarge => {
                // When: the read is Empty or TooLarge (refused before any copy), the type is omitted.
                continue;
            }
            ReadOutcome::Unsupported => {
                // When: the read is Unsupported, the location cannot be read: `ENOSYS`.
                return request.status(KittyClipboardStatus::Enosys);
            }
            ReadOutcome::Failed => {
                // When: the native read Failed, the whole read answers `EIO`, not an empty read.
                return request.status(KittyClipboardStatus::Eio);
            }
        };
        let chunks = text.as_bytes().chunks(KITTY_READ_CHUNK_BYTES);
        let cost: usize =
            chunks.clone().map(|chunk| request.data_len(mime.len(), chunk.len())).sum();
        if reply.len() + cost + done_len > KITTY_READ_FRAME_CAP_BYTES {
            // When: reply.len plus cost and done_len passes KITTY_READ_FRAME_CAP_BYTES, the type is
            // omitted before the reply grows.
            continue;
        }
        reply.reserve_exact(cost + done_len);
        for chunk in chunks {
            push_packet(&mut reply, request, "DATA", Some((mime, chunk)));
        }
    }
    push_packet(&mut reply, request, "DONE", None);
    reply
}

#[cfg(test)]
#[path = "kitty_clipboard_read_tests.rs"]
mod kitty_clipboard_read_tests;
