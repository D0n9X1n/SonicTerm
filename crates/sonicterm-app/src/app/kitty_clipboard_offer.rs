//! OSC 5522 representations on the native clipboard: which MIME names a platform can hold, and
//! one atomic publish of every representation a commit kept.
//!
//! A commit first plans its offer without touching the clipboard: every staged name and alias is
//! mapped to a [`NativeFormat`], and a name that is not a valid MIME name, has no native mapping,
//! or that the platform cannot hold is omitted. The backend then stages every native buffer
//! before it publishes them in one operation, so a failure in any representation leaves the
//! previous clipboard untouched and never reports success.

/// A native clipboard representation an OSC 5522 MIME name maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) enum NativeFormat {
    /// `text/plain` and its aliases.
    Text,
    /// `text/html`.
    Html,
    /// `image/png`.
    Png,
    /// `text/uri-list`.
    UriList,
    /// Any other valid MIME name, held under that name where the platform allows it.
    Custom(String),
}

/// Most bytes of a MIME name (RFC 6838 restricts each part to 127 characters).
const MAX_MIME_NAME_BYTES: usize = 255;

/// The native format `name` maps to; `None` when it is not a valid MIME name or text alias.
pub(in crate::app) fn native_format(name: &str) -> Option<NativeFormat> {
    // The X11 text atoms are not MIME names, but programs offer text under them.
    if matches!(name, "UTF8_STRING" | "TEXT" | "STRING") || super::kitty_clipboard_tx::is_text(name)
    {
        // When: name is plain text or a text alias, it is the one text representation.
        return Some(NativeFormat::Text);
    }
    match name {
        "text/html" => Some(NativeFormat::Html),
        "image/png" => Some(NativeFormat::Png),
        "text/uri-list" => Some(NativeFormat::UriList),
        _ => valid_mime(name).then(|| NativeFormat::Custom(name.to_owned())),
    }
}

/// Whether `name` is an RFC 6838 `type/subtype` name of restricted-name characters.
pub(in crate::app) fn valid_mime(name: &str) -> bool {
    /// A restricted name: an alphanumeric first, then alphanumerics and `!#$&-^_.+`, 127 at most.
    fn restricted(part: &str) -> bool {
        let mut bytes = part.bytes();
        bytes.next().is_some_and(|first| first.is_ascii_alphanumeric())
            && part.len() <= 127
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&byte))
    }
    name.len() <= MAX_MIME_NAME_BYTES
        && name
            .split_once('/')
            .is_some_and(|(kind, subtype)| restricted(kind) && restricted(subtype))
}

/// Every representation a commit kept, over the staged buffers they share.
pub(in crate::app) struct NativeOffer {
    /// The staged buffers still referenced; an alias shares its target's buffer.
    entries: Vec<Vec<u8>>,
    /// Each kept format, in the commit's order, with the index of its buffer.
    representations: Vec<(NativeFormat, usize)>,
    /// Names that were invalid, unmapped or not holdable here.
    omitted: usize,
}

impl std::fmt::Debug for NativeOffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Only formats and lengths are printed, so an offer can be logged without its data.
        let kept: Vec<_> =
            self.representations().map(|(format, bytes)| (format, bytes.len())).collect();
        formatter
            .debug_struct("NativeOffer")
            .field("representations", &kept)
            .field("omitted", &self.omitted)
            .finish()
    }
}

impl NativeOffer {
    /// How many names were omitted.
    pub(in crate::app) fn omitted(&self) -> usize {
        self.omitted
    }

    /// The text representation's bytes, if one was kept.
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::app) fn text(&self) -> Option<&[u8]> {
        self.representations()
            .find(|(format, _)| **format == NativeFormat::Text)
            .map(|(_, bytes)| bytes)
    }

    /// Every kept format with its bytes, in the commit's order.
    pub(in crate::app) fn representations(&self) -> impl Iterator<Item = (&NativeFormat, &[u8])> {
        self.representations.iter().map(|(format, index)| (format, self.entries[*index].as_slice()))
    }

    /// The bytes every representation publishes, counting a shared buffer once per format.
    pub(in crate::app) fn byte_len(&self) -> usize {
        self.representations().map(|(_, bytes)| bytes.len()).sum()
    }

    /// The staging this offer needs: its kept buffers plus one native copy per representation,
    /// sized as this platform's clipboard holds it.
    pub(in crate::app) fn reserved_bytes(&self) -> usize {
        let kept: usize = self.entries.iter().map(Vec::capacity).sum();
        let names: usize =
            self.representations.iter().map(|(format, _)| format_name_bytes(format)).sum();
        let copies: usize = self
            .representations()
            .map(|(format, bytes)| native_copy_bytes(format, bytes.len()))
            .sum();
        #[cfg(target_os = "linux")]
        let copies = copies + x11::owner_workspace(self);
        kept + names + copies
    }

    /// A text-only offer that preserves arbitrary bytes on the macOS pasteboard or X11 selection.
    pub(in crate::app) fn raw_text(bytes: Vec<u8>) -> Self {
        Self { entries: vec![bytes], representations: vec![(NativeFormat::Text, 0)], omitted: 0 }
    }

    /// Whether text is the only representation, which the text path writes as before.
    pub(in crate::app) fn is_text_only(&self) -> bool {
        matches!(self.representations.as_slice(), [(NativeFormat::Text, _)])
    }

    /// The text representation's buffer; empty when none was kept.
    pub(in crate::app) fn into_text(mut self) -> Vec<u8> {
        let index = self.representations.iter().find(|(format, _)| *format == NativeFormat::Text);
        match index.map(|(_, index)| *index) {
            Some(index) => self.entries.swap_remove(index),
            None => Vec::new(),
        }
    }
}

/// Reserve the decoded bytes and native copy before building even the raw offer's two vectors.
pub(in crate::app) fn raw_text_reservation(bytes: &Vec<u8>) -> usize {
    #[cfg(target_os = "linux")]
    let tables = x11::raw_tables(5);
    #[cfg(not(target_os = "linux"))]
    let tables = 0;
    bytes.capacity()
        + bytes.len()
        + tables
        + size_of::<Vec<u8>>()
        + size_of::<(NativeFormat, usize)>()
}

/// What a commit's representations plan to do to the clipboard.
#[derive(Debug)]
pub(in crate::app) enum OfferPlan {
    /// Nothing was staged: the clipboard is cleared.
    Clear,
    /// Every name was omitted: the platform can hold none of them.
    Unavailable,
    /// At least one representation is published.
    Offer(NativeOffer),
}

/// Plan the offer `entries` (name and decoded bytes, in staging order) and `aliases` (`(alias,
/// target)` in arrival order) give, keeping each format the platform `holds` with its bytes once,
/// first first.
pub(in crate::app) fn plan(
    entries: Vec<(String, Vec<u8>)>,
    aliases: &[(String, String)],
    holds: &dyn Fn(&NativeFormat, &[u8]) -> bool,
) -> OfferPlan {
    if entries.is_empty() {
        // When: entries is empty, the write staged nothing and the commit clears the clipboard.
        return OfferPlan::Clear;
    }
    // Names map to entry indices, so resolving aliases copies no data.
    let mut names: Vec<(&str, usize)> =
        entries.iter().enumerate().map(|(index, (name, _))| (name.as_str(), index)).collect();
    for (alias, target) in aliases {
        let Some(index) = names.iter().find(|(name, _)| name == target).map(|(_, index)| *index)
        else {
            // When: the alias's target has no representation, the alias is dropped.
            continue;
        };
        match names.iter_mut().find(|(name, _)| name == alias) {
            Some(existing) => existing.1 = index,
            None => names.push((alias.as_str(), index)),
        }
    }
    let mut omitted = 0;
    let mut representations: Vec<(NativeFormat, usize)> = Vec::new();
    for (name, index) in names {
        let bytes = entries[index].1.as_slice();
        let Some(format) = native_format(name).filter(|format| holds(format, bytes)) else {
            // When: native_format gives no format holds accepts, the name is omitted.
            omitted += 1;
            continue;
        };
        if !representations.iter().any(|(kept, _)| *kept == format) {
            // The first name of a format keeps it; a later name for the same format adds nothing.
            representations.push((format, index));
        }
    }
    if representations.is_empty() {
        // When: representations is empty, every name was omitted and nothing can be published.
        return OfferPlan::Unavailable;
    }
    // Buffers no kept format references are dropped here, so they leave the reservation.
    let mut kept = Vec::new();
    let mut remap = vec![None; entries.len()];
    for (index, (_, bytes)) in entries.into_iter().enumerate() {
        if representations.iter().any(|(_, used)| *used == index) {
            remap[index] = Some(kept.len());
            kept.push(bytes);
        }
    }
    for (_, index) in &mut representations {
        *index = remap[*index].expect("a kept representation's buffer is kept");
    }
    OfferPlan::Offer(NativeOffer { entries: kept, representations, omitted })
}

/// The URIs of a `text/uri-list` (RFC 2483): its lines without line ends, blank lines and `#`
/// comments skipped; `None` when it is not UTF-8.
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn uri_list_items(bytes: &[u8]) -> Option<Vec<&str>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let items = text.lines().map(str::trim);
    Some(items.filter(|line| !line.is_empty() && !line.starts_with('#')).collect())
}

/// The staging one native copy of `format` from `byte_len` bytes takes: Windows converts text to
/// UTF-16 and wraps HTML and file lists; elsewhere the copy is the bytes.
fn native_copy_bytes(format: &NativeFormat, byte_len: usize) -> usize {
    #[cfg(target_os = "windows")]
    {
        windows_formats::copy_bound(format, byte_len)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = format;
        byte_len
    }
}

/// The bytes a format's native name takes beside its buffer.
fn format_name_bytes(format: &NativeFormat) -> usize {
    match format {
        NativeFormat::Custom(name) => name.len(),
        _ => 0,
    }
}

/// Whether this platform can publish `format` and `bytes`, including display availability,
/// platform layout restrictions and permanent-name caps. May run on any thread.
pub(in crate::app) fn platform_holds(format: &NativeFormat, bytes: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::holds(format, bytes)
    }
    #[cfg(target_os = "windows")]
    {
        windows_formats::holds(format, bytes)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = bytes;
        x11::holds(format)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = bytes;
        *format == NativeFormat::Text
    }
}

/// The check a write makes at its commit point, inside the native acquisition and immediately
/// before the clipboard changes: false leaves the clipboard untouched.
pub(in crate::app) type CommitGate<'gate> = &'gate dyn Fn() -> bool;

/// What one gated publish did to a single native clipboard.
#[cfg(any(test, not(target_os = "linux")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum Published {
    /// Every representation was set.
    Written,
    /// The clipboard could not be taken or refused a representation; it holds no partial offer.
    Failed,
    /// The gate refused at the commit point, so the clipboard was never changed.
    Unchanged,
}

/// Publish `offer` to the system clipboard in one operation, `gate` checked immediately before it
/// changes: on the event-loop thread on macOS, on the clipboard worker on Windows.
#[cfg(not(target_os = "linux"))]
pub(in crate::app) fn publish_native(offer: &NativeOffer, gate: CommitGate<'_>) -> Published {
    #[cfg(target_os = "macos")]
    {
        objc2::rc::autoreleasepool(|_| {
            publish_offer(&mut macos::Pasteboard::general(), offer, gate)
        })
    }
    #[cfg(target_os = "windows")]
    {
        win32::publish(offer, gate)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        publish_offer(&mut TextOnly, offer, gate)
    }
}

/// Replace the Windows clipboard with `text` in one acquisition: with `snapshot_max_bytes`, the
/// value it replaces is read first; `gate` is checked after that read, before `EmptyClipboard`.
#[cfg(target_os = "windows")]
pub(in crate::app) fn write_windows_text(
    text: &str,
    snapshot_max_bytes: Option<usize>,
    gate: CommitGate<'_>,
) -> (Published, Option<super::clipboard_worker::Snapshot>) {
    win32::write_text(text, snapshot_max_bytes, gate)
}

/// Write `text` to the Windows clipboard again only while it holds exactly `previous`, compared
/// and replaced in one acquisition, `gate` checked after the comparison, before `EmptyClipboard`.
#[cfg(target_os = "windows")]
pub(in crate::app) fn reassert_windows_text(
    text: &str,
    previous: &super::clipboard_worker::Snapshot,
    gate: CommitGate<'_>,
) -> Published {
    win32::reassert_text(text, previous, gate)
}

/// The general pasteboard's MIME names for an OSC 5522 listing; names only, never content.
#[cfg(target_os = "macos")]
pub(in crate::app) fn list_general_types() -> super::osc52_read_source::TypeListing {
    use objc2_app_kit::NSPasteboard;
    objc2::rc::autoreleasepool(|_| macos::list_types(&NSPasteboard::generalPasteboard()))
}

/// The general pasteboard's bytes under the non-text MIME name `mime`, within `max_bytes`.
#[cfg(target_os = "macos")]
pub(in crate::app) fn read_general_type(
    mime: &str,
    max_bytes: usize,
) -> super::osc52_read_source::TypeRead {
    use objc2_app_kit::NSPasteboard;
    objc2::rc::autoreleasepool(|_| {
        macos::read_type(&NSPasteboard::generalPasteboard(), mime, max_bytes)
    })
}

/// The Windows clipboard's text for an OSC 52 read within `max_bytes`, sized before it is locked.
#[cfg(target_os = "windows")]
pub(in crate::app) fn read_windows_text(max_bytes: usize) -> super::osc52_read_source::ReadOutcome {
    win32::read_text(max_bytes)
}

/// The Windows clipboard's bytes under the non-text MIME name `mime`, within `max_bytes`.
#[cfg(target_os = "windows")]
pub(in crate::app) fn read_windows_type(
    mime: &str,
    max_bytes: usize,
) -> super::osc52_read_source::TypeRead {
    win32::read_type(mime, max_bytes)
}

/// Where only text is held no offer is planned; one that arrives fails closed, untouched.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
struct TextOnly;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl OfferBackend for TextOnly {
    fn stage(&mut self, _format: &NativeFormat, _bytes: &[u8]) -> bool {
        false
    }

    fn publish(&mut self, _gate: CommitGate<'_>) -> Published {
        Published::Failed
    }

    fn discard(&mut self) {}
}

/// The native calls one publish makes; tests substitute a fake.
#[cfg(any(test, not(target_os = "linux")))]
pub(in crate::app) trait OfferBackend {
    /// Allocate and fill the native buffer for `format`; false when it cannot be staged.
    fn stage(&mut self, format: &NativeFormat, bytes: &[u8]) -> bool;
    /// Replace the clipboard with every staged buffer in one operation, checking `gate` immediately
    /// before the clipboard changes; a refusal discards the buffers, leaving it untouched.
    fn publish(&mut self, gate: CommitGate<'_>) -> Published;
    /// Free every staged buffer without touching the clipboard.
    fn discard(&mut self);
}

/// Stage every representation of `offer`, then publish them at once behind `gate`. A staging
/// failure discards the rest and leaves the clipboard untouched.
#[cfg(any(test, not(target_os = "linux")))]
pub(in crate::app) fn publish_offer(
    backend: &mut dyn OfferBackend,
    offer: &NativeOffer,
    gate: CommitGate<'_>,
) -> Published {
    for (format, bytes) in offer.representations() {
        if !backend.stage(format, bytes) {
            // When: backend.stage fails, nothing was published, so the previous clipboard stays.
            backend.discard();
            return Published::Failed;
        }
    }
    backend.publish(gate)
}

#[cfg(target_os = "macos")]
#[path = "kitty_clipboard_offer_macos.rs"]
mod macos;

#[cfg(any(test, target_os = "windows"))]
#[path = "kitty_clipboard_offer_windows.rs"]
mod windows_formats;

#[cfg(target_os = "windows")]
#[path = "kitty_clipboard_offer_win32.rs"]
mod win32;

#[cfg(test)]
#[path = "kitty_clipboard_offer_tests.rs"]
mod kitty_clipboard_offer_tests;

#[cfg(any(test, target_os = "linux"))]
#[path = "kitty_clipboard_offer_x11.rs"]
mod x11;

#[cfg(target_os = "linux")]
pub(in crate::app) use x11::SelectionServer;
