//! kitty's desktop-notification protocol, `OSC 99 ; metadata ; payload`: notifications sent in
//! one piece or in chunks under one id, plain or base64, kept bounded until each completes.

use super::program::MAX_NOTIFY_BYTES;

/// When a program asked for a notification to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyOccasion {
    /// Whatever the pane's state (`o=always`, the default).
    #[default]
    Always,
    /// Only while the pane is not the focused pane of a focused window (`o=unfocused`).
    Unfocused,
    /// Only while the pane is not drawn (`o=invisible`).
    Invisible,
}

/// At most this many notifications are being received at once; a new id evicts the oldest.
const MAX_PENDING: usize = 8;

/// What one `OSC 99` asks of the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KittyNotify {
    /// A completed notification.
    Show {
        /// The program's id, when it gave one.
        id: Option<String>,
        /// The title, if any part was a title.
        title: Option<String>,
        /// The body; empty when only a title was sent.
        body: String,
        /// When to show it.
        occasion: NotifyOccasion,
    },
    /// A capability query, answered with this reply payload.
    Query {
        /// The id to echo; `0` when the query gave none.
        id: String,
    },
    /// A chunk was stored, or the sequence is malformed or unsupported.
    Nothing,
}

/// One notification still receiving chunks.
#[derive(Debug, Default)]
struct Pending {
    id: String,
    title: Part,
    body: Part,
    occasion: NotifyOccasion,
}

/// One part's decoded bytes and, for base64, the characters of an unfinished group.
#[derive(Debug, Default)]
struct Part {
    bytes: Vec<u8>,
    carry: Vec<u8>,
    seen: bool,
}

impl Part {
    /// Append `payload`: as is, or base64-decoded with an unfinished group carried to the next
    /// chunk, so a split anywhere in the encoding, or chunks each encoded and padded on their own,
    /// decode alike. `None` on a character base64 does not use.
    fn push(&mut self, payload: &[u8], base64: bool) -> Option<()> {
        self.seen = true;
        if !base64 {
            // When: `base64` is off, the plain-text payload is kept as sent.
            self.bytes.extend_from_slice(payload);
            return Some(());
        }
        self.carry.extend(payload.iter().copied().filter(|byte| !byte.is_ascii_whitespace()));
        let whole = self.carry.len() / 4 * 4;
        for group in self.carry[..whole].chunks(4) {
            decode_group(group, &mut self.bytes)?;
        }
        self.carry.drain(..whole);
        Some(())
    }

    /// Bytes held, decoded and carried.
    fn held(&self) -> usize {
        self.bytes.len() + self.carry.len()
    }

    /// The completed text: `None` when nothing was sent for this part, or when a single base64
    /// character was left over or the bytes are not UTF-8. The protocol lets the final chunk omit
    /// its padding, so a two- or three-character final group decodes as if padded.
    fn finish(mut self) -> Option<Option<String>> {
        if !self.seen {
            // When: `seen` is false, no chunk named this part, which is no error.
            return Some(None);
        }
        let leftover = self.carry.len();
        if leftover == 1 {
            // When: one character is the `leftover`, it encodes no whole byte: the text was cut.
            return None;
        }
        if leftover > 1 {
            // Two or three characters left over are an unpadded final group, decoded as if padded.
            let mut group = std::mem::take(&mut self.carry);
            group.resize(4, b'=');
            decode_group(&group, &mut self.bytes)?;
        }
        String::from_utf8(self.bytes).ok().map(Some)
    }
}

/// Decode one four-character base64 group, padded or not, into `out`.
fn decode_group(group: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let padding = group.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 {
        // When: `padding` is more than two `=`, the group encodes nothing valid.
        return None;
    }
    let mut word = 0u32;
    for byte in &group[..4 - padding] {
        word = (word << 6) | u32::from(value(*byte)?);
    }
    word <<= 6 * padding as u32;
    let decoded = word.to_be_bytes();
    out.extend_from_slice(&decoded[1..4 - padding]);
    Some(())
}

/// Notifications being received in chunks, at most [`MAX_PENDING`] and [`MAX_NOTIFY_BYTES`].
#[derive(Debug, Default)]
pub(crate) struct KittyNotifications {
    pending: Vec<Pending>,
}

impl KittyNotifications {
    /// Bytes the pending notifications hold.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.pending.iter().map(|pending| pending.title.held() + pending.body.held()).sum()
    }

    /// Drop every pending notification, as RIS does.
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
    }

    /// Read one `OSC 99` whose text after `99;` is `content`: `metadata;payload`.
    pub(crate) fn receive(&mut self, content: &[u8]) -> KittyNotify {
        let (metadata, payload) = match content.iter().position(|byte| *byte == b';') {
            Some(split) => (&content[..split], &content[split + 1..]),
            None => (content, &[][..]),
        };
        let Some(meta) = Metadata::parse(metadata) else {
            // When: the metadata is malformed, the sequence is ignored.
            return KittyNotify::Nothing;
        };
        match meta.part {
            PartKind::Query => {
                // When: the part is a capability query, it is answered, never shown.
                return KittyNotify::Query { id: meta.id.unwrap_or_else(|| "0".into()) };
            }
            PartKind::Other => {
                // When: the part is one SonicTerm does not show (close, alive, icon, buttons).
                return KittyNotify::Nothing;
            }
            PartKind::Title | PartKind::Body => {
                // When: the part is a title or body, it is received below.
            }
        }
        let Some(id) = meta.id.clone() else {
            // When: the notification has no id, it cannot be chunked: it is complete now.
            let mut pending = Pending { occasion: meta.occasion, ..Pending::default() };
            return self.complete(pending_push(&mut pending, &meta, payload).map(|()| pending));
        };
        let index = match self.pending.iter().position(|pending| pending.id == id) {
            Some(index) => index,
            None => {
                // A new id; at MAX_PENDING open notifications the oldest goes first.
                if self.pending.len() == MAX_PENDING {
                    self.pending.remove(0);
                }
                self.pending.push(Pending {
                    id: id.clone(),
                    occasion: meta.occasion,
                    ..Pending::default()
                });
                self.pending.len() - 1
            }
        };
        let pushed = pending_push(&mut self.pending[index], &meta, payload);
        let total = self.retained_bytes();
        if pushed.is_none() || total > MAX_NOTIFY_BYTES {
            // When: the chunk was invalid base64 or would pass MAX_NOTIFY_BYTES, the whole
            // notification is dropped, never cut.
            self.pending.remove(index);
            return KittyNotify::Nothing;
        }
        if !meta.done {
            // When: `done` is false (`d=0`), more chunks follow.
            return KittyNotify::Nothing;
        }
        let pending = self.pending.remove(index);
        self.complete(Some(pending))
    }

    /// Turn a completed notification into what the host shows; `None` (a failed chunk) is nothing.
    fn complete(&self, pending: Option<Pending>) -> KittyNotify {
        let Some(pending) = pending else {
            // When: `pending` is `None`, the last chunk was invalid and nothing shows.
            return KittyNotify::Nothing;
        };
        let id = (!pending.id.is_empty()).then_some(pending.id);
        match (pending.title.finish(), pending.body.finish()) {
            (Some(title), Some(body)) if title.is_some() || body.is_some() => KittyNotify::Show {
                id,
                title,
                body: body.unwrap_or_default(),
                occasion: pending.occasion,
            },
            _ => KittyNotify::Nothing,
        }
    }
}

/// Append `payload` to the part `meta` names; the occasion of the latest chunk wins.
fn pending_push(pending: &mut Pending, meta: &Metadata, payload: &[u8]) -> Option<()> {
    pending.occasion = meta.occasion;
    let part = match meta.part {
        PartKind::Body => &mut pending.body,
        _ => &mut pending.title,
    };
    part.push(payload, meta.base64)
}

/// Which part a chunk carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Title,
    Body,
    Query,
    /// A part SonicTerm does not show (close, alive, icon, buttons).
    Other,
}

/// The metadata keys SonicTerm reads; others are ignored, as kitty ignores unknown keys.
#[derive(Debug, Clone)]
struct Metadata {
    id: Option<String>,
    done: bool,
    part: PartKind,
    base64: bool,
    occasion: NotifyOccasion,
}

impl Metadata {
    /// Parse `key=value` pairs joined by `:`; `None` on a malformed id or flag.
    fn parse(metadata: &[u8]) -> Option<Self> {
        let mut meta = Self {
            id: None,
            done: true,
            part: PartKind::Title,
            base64: false,
            occasion: NotifyOccasion::Always,
        };
        for pair in metadata.split(|byte| *byte == b':').filter(|pair| !pair.is_empty()) {
            let (key, value) = match pair.iter().position(|byte| *byte == b'=') {
                Some(split) => (&pair[..split], &pair[split + 1..]),
                None => (pair, &[][..]),
            };
            match key {
                b"i" => {
                    // When: the key is `i`, the value is the notification's id.
                    let valid = !value.is_empty()
                        && value.len() <= 64
                        && value
                            .iter()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"_+.-".contains(byte));
                    if !valid {
                        // When: the id is not `valid` (empty, too long or another character), it is refused.
                        return None;
                    }
                    meta.id = Some(String::from_utf8_lossy(value).into_owned());
                }
                b"d" => meta.done = value != b"0",
                b"e" => meta.base64 = value == b"1",
                b"p" => {
                    meta.part = match value {
                        b"title" | b"" => PartKind::Title,
                        b"body" => PartKind::Body,
                        b"?" => PartKind::Query,
                        _ => PartKind::Other,
                    }
                }
                b"o" => {
                    meta.occasion = match value {
                        b"unfocused" => NotifyOccasion::Unfocused,
                        b"invisible" => NotifyOccasion::Invisible,
                        _ => NotifyOccasion::Always,
                    }
                }
                _ => {
                    // When: the key is one SonicTerm does not read, it is ignored.
                }
            }
        }
        Some(meta)
    }
}

#[cfg(test)]
#[path = "kitty_notify_tests.rs"]
mod kitty_notify_tests;
