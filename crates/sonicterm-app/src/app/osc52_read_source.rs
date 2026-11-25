//! Bounded clipboard text reads for OSC 52: the [`ClipboardTextSource`] seam tests replace, the
//! per-platform native readers, the UTF-8 conversions that stop at the cap, and the reply frames
//! one read builds. Every read runs on a disposable reader thread, never on the event loop.

use super::osc52_read::{append_frame, destination_cap};

/// Which selection a read takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ReadLocation {
    /// The standard clipboard (`c`, `s`).
    Clipboard,
    /// The X11/Wayland PRIMARY selection (`p`), read only on Linux.
    Primary,
}

/// What one bounded read found. `Debug` prints only the text's length.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::app) enum ReadOutcome {
    /// The text, at most the cap in UTF-8 bytes.
    Text(String),
    /// The selection holds no text.
    Empty,
    /// The text is larger than the cap; nothing past the measurement was copied.
    TooLarge,
    /// The platform has no such selection, or no display server answered.
    Unsupported,
    /// The native read failed or answered in an unexpected shape.
    #[cfg_attr(not(any(test, target_os = "linux", target_os = "windows")), allow(dead_code))]
    Failed,
}

impl std::fmt::Debug for ReadOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => write!(formatter, "Text({} bytes)", text.len()),
            Self::Empty => formatter.write_str("Empty"),
            Self::TooLarge => formatter.write_str("TooLarge"),
            Self::Unsupported => formatter.write_str("Unsupported"),
            Self::Failed => formatter.write_str("Failed"),
        }
    }
}

/// What one bounded read of a non-text representation found. `Debug` prints only the length.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(
    not(any(test, target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]
pub(in crate::app) enum TypeRead {
    /// The representation's bytes, at most the cap.
    Bytes(Vec<u8>),
    /// The selection holds no such representation, or none SonicTerm can decode.
    Absent,
    /// The representation is larger than the cap; nothing past the measurement was copied.
    TooLarge,
    /// No native backend is available for the session.
    #[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
    Unsupported,
    /// The native read failed.
    #[cfg_attr(not(any(test, target_os = "windows", target_os = "linux")), allow(dead_code))]
    Failed,
}

impl std::fmt::Debug for TypeRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(formatter, "Bytes({} bytes)", bytes.len()),
            Self::Absent => formatter.write_str("Absent"),
            Self::Unsupported => formatter.write_str("Unsupported"),
            Self::TooLarge => formatter.write_str("TooLarge"),
            Self::Failed => formatter.write_str("Failed"),
        }
    }
}

/// What an OSC 5522 type listing found: MIME names only, never content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) enum TypeListing {
    /// The names the selection offers, in the platform's order.
    Types(Vec<String>),
    /// The platform cannot enumerate the selection's types.
    Unsupported,
    /// The native enumeration failed, as when another process holds the clipboard throughout.
    #[cfg_attr(not(any(test, target_os = "linux", target_os = "windows")), allow(dead_code))]
    Failed,
}

/// Native type names one listing inspects at most, so an owner offering thousands of formats
/// cannot make it unbounded.
pub(in crate::app) const MAX_NATIVE_TYPES_SCANNED: usize = 64;

/// MIME names one listing keeps at most, as the listing reply carries.
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
const MAX_LISTED_TYPES: usize = 64;

/// The MIME name a native clipboard type stands for: the platform's text, HTML, file and image
/// types map to theirs, a MIME-shaped name passes without its parameters, and private, dynamic and
/// protocol types map to `None`, so they are never listed.
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn native_type_mime(name: &str) -> Option<String> {
    let mapped = match name {
        "public.utf8-plain-text"
        | "public.plain-text"
        | "NSStringPboardType"
        | "CF_TEXT"
        | "CF_UNICODETEXT"
        | "CF_OEMTEXT"
        | "UTF8_STRING"
        | "STRING"
        | "TEXT" => "text/plain",
        "public.html" | "Apple HTML pasteboard type" | "HTML Format" => "text/html",
        "public.rtf" | "Rich Text Format" => "text/rtf",
        "public.file-url" | "public.url" | "CF_HDROP" => "text/uri-list",
        "public.png" | "PNG" => "image/png",
        "public.tiff" => "image/tiff",
        "public.jpeg" | "JFIF" => "image/jpeg",
        _ => {
            // When: the name is no known platform type, only a type/subtype MIME shape passes.
            let mime = name.split(';').next().unwrap_or_default().trim();
            let (kind, subtype) = mime.split_once('/')?;
            let token = |part: &str| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
            };
            return (token(kind) && token(subtype)).then(|| mime.to_ascii_lowercase());
        }
    };
    Some(mapped.to_owned())
}

/// The listing `names`, the owner's native type names in its order, makes: at most
/// [`MAX_NATIVE_TYPES_SCANNED`] are inspected and at most 64 distinct MIME names kept. Only names
/// are read, never content.
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
pub(in crate::app) fn list_native_types(names: impl IntoIterator<Item = String>) -> TypeListing {
    let mut listed: Vec<String> = Vec::new();
    for name in names.into_iter().take(MAX_NATIVE_TYPES_SCANNED) {
        let Some(mime) = native_type_mime(&name) else {
            // When: native_type_mime has no public name for it, the type stays unlisted.
            continue;
        };
        if !listed.contains(&mime) {
            listed.push(mime);
        }
        if listed.len() == MAX_LISTED_TYPES {
            // When: MAX_LISTED_TYPES names are kept, the listing is full.
            break;
        }
    }
    TypeListing::Types(listed)
}

/// Reads clipboard text with a byte cap; tests inject a fake, production uses [`NativeSource`].
pub(in crate::app) trait ClipboardTextSource: Send + Sync {
    /// The text at `location`, refused as [`ReadOutcome::TooLarge`] past `max_bytes` of UTF-8.
    fn read_text_bounded(&self, location: ReadLocation, max_bytes: usize) -> ReadOutcome;

    /// The MIME types `location` offers, found without reading any content; a source that cannot
    /// enumerate types answers `Unsupported`.
    fn list_types(&self, _location: ReadLocation) -> TypeListing {
        TypeListing::Unsupported
    }

    /// The bytes `location` holds under the non-text MIME name `mime`, refused as
    /// [`TypeRead::TooLarge`] past `max_bytes`; a source holding text only answers `Absent`.
    fn read_type_bounded(
        &self,
        _location: ReadLocation,
        _mime: &str,
        _max_bytes: usize,
    ) -> TypeRead {
        TypeRead::Absent
    }

    /// A version of the offer `location` holds now, which changes whenever the offer does; `None`
    /// when the platform cannot say, so no one-time paste grant can be bound to it.
    fn offer_version(&self, _location: ReadLocation) -> Option<u64> {
        None
    }

    /// What a paste gesture learns of the offer at `location` on the event loop, without any call
    /// that can block on another program; a source that cannot say answers `Unavailable`.
    fn gesture_offer(&self, _location: ReadLocation) -> GestureOffer {
        GestureOffer::Unavailable
    }
}

/// What a paste gesture learns of the offer the user pasted, read on the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum GestureOffer {
    /// The offer's version, read from a counter without opening the clipboard (macOS, Windows).
    #[cfg_attr(not(any(test, target_os = "macos", target_os = "windows")), allow(dead_code))]
    Version(u64),
    /// The version needs a round trip to the owner (X11), so the gesture reads none.
    #[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
    RoundTrip,
    /// The platform cannot version an offer, so no grant can bind to it.
    Unavailable,
}

/// The selection a reply label reads: `c` and `s` the clipboard, `p` PRIMARY on Linux only.
fn label_location(label: u8) -> Option<ReadLocation> {
    match label {
        b'p' if cfg!(target_os = "linux") => {
            // When: label is p and target_os is Linux, PRIMARY exists and is read.
            Some(ReadLocation::Primary)
        }
        b'p' => None,
        _ => Some(ReadLocation::Clipboard),
    }
}

/// The replies one read of `labels` answers with, read through `source` in order: one frame per
/// label, carrying the text when the read found some and empty otherwise. Each destination's text
/// is dropped before the next is read, so at most one raw text is alive at once.
pub(in crate::app) fn read_replies(source: &dyn ClipboardTextSource, labels: &[u8]) -> Vec<u8> {
    let cap = destination_cap(labels.len());
    let mut replies = Vec::new();
    for label in labels {
        let outcome = label_location(*label)
            .map_or(ReadOutcome::Unsupported, |location| source.read_text_bounded(location, cap));
        let text = match &outcome {
            ReadOutcome::Text(text) => text.as_bytes(),
            ReadOutcome::Empty
            | ReadOutcome::TooLarge
            | ReadOutcome::Unsupported
            | ReadOutcome::Failed => b"",
        };
        append_frame(&mut replies, *label, text);
    }
    replies
}

/// Whether a `CF_UNICODETEXT` block of `block_bytes` may hold text within `max_bytes` of UTF-8:
/// every UTF-16 unit is at least one UTF-8 byte, so more units than the cap plus a terminator
/// cannot fit, which `GlobalSize` decides before the block is locked.
#[cfg(any(test, target_os = "windows"))]
pub(in crate::app) fn utf16_block_admits(block_bytes: usize, max_bytes: usize) -> bool {
    block_bytes / 2 <= max_bytes.saturating_add(1)
}

/// `units` as text, lone surrogates as U+FFFD, measured before anything is allocated.
#[cfg(any(test, target_os = "windows"))]
pub(in crate::app) fn bounded_utf16(units: &[u16], max_bytes: usize) -> ReadOutcome {
    if units.is_empty() {
        // When: units is empty, the clipboard holds no text.
        return ReadOutcome::Empty;
    }
    let decoded = || char::decode_utf16(units.iter().copied());
    let mut length = 0usize;
    for unit in decoded() {
        length += unit.map_or(3, char::len_utf8);
        if length > max_bytes {
            // When: length passes max_bytes, the text is refused before it is built.
            return ReadOutcome::TooLarge;
        }
    }
    let mut text = String::with_capacity(length);
    text.extend(decoded().map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)));
    ReadOutcome::Text(text)
}

/// `bytes` as text, invalid sequences as U+FFFD, refused when the result passes `max_bytes`.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub(in crate::app) fn bounded_utf8(bytes: Vec<u8>, max_bytes: usize) -> ReadOutcome {
    if bytes.is_empty() {
        // When: bytes is empty, the selection holds no text.
        return ReadOutcome::Empty;
    }
    let length: usize = bytes
        .utf8_chunks()
        .map(|chunk| chunk.valid().len() + if chunk.invalid().is_empty() { 0 } else { 3 })
        .sum();
    if length > max_bytes {
        // When: the converted length passes max_bytes, nothing is built.
        return ReadOutcome::TooLarge;
    }
    match String::from_utf8(bytes) {
        Ok(text) => ReadOutcome::Text(text),
        Err(error) => {
            // The replacement is built into one buffer reserved at the measured length, so beside
            // its input the conversion holds only the text it returns, never a growing copy.
            let mut text = String::with_capacity(length);
            for chunk in error.as_bytes().utf8_chunks() {
                text.push_str(chunk.valid());
                if !chunk.invalid().is_empty() {
                    text.push(char::REPLACEMENT_CHARACTER);
                }
            }
            ReadOutcome::Text(text)
        }
    }
}

/// `STRING` (Latin-1) `bytes` as text; a byte above 0x7F takes two UTF-8 bytes.
#[cfg(any(test, target_os = "linux"))]
pub(in crate::app) fn bounded_latin1(bytes: &[u8], max_bytes: usize) -> ReadOutcome {
    if bytes.is_empty() {
        // When: bytes is empty, the selection holds no text.
        return ReadOutcome::Empty;
    }
    let length: usize = bytes.iter().map(|byte| if *byte < 0x80 { 1 } else { 2 }).sum();
    if length > max_bytes {
        // When: the converted length passes max_bytes, nothing is built.
        return ReadOutcome::TooLarge;
    }
    // Built into one buffer reserved at the measured length, so it never grows while converting.
    let mut text = String::with_capacity(length);
    text.extend(bytes.iter().map(|byte| char::from(*byte)));
    ReadOutcome::Text(text)
}

/// The native clipboard, read on whichever reader thread calls it.
#[derive(Debug, Default)]
pub(in crate::app) struct NativeSource;

impl ClipboardTextSource for NativeSource {
    fn read_text_bounded(&self, location: ReadLocation, max_bytes: usize) -> ReadOutcome {
        native_read(location, max_bytes)
    }

    fn list_types(&self, location: ReadLocation) -> TypeListing {
        native_list(location)
    }

    fn read_type_bounded(&self, location: ReadLocation, mime: &str, max_bytes: usize) -> TypeRead {
        native_read_type(location, mime, max_bytes)
    }

    fn offer_version(&self, location: ReadLocation) -> Option<u64> {
        native_offer_version(location)
    }

    fn gesture_offer(&self, location: ReadLocation) -> GestureOffer {
        native_gesture_offer(location)
    }
}

/// macOS and Windows: the gesture reads the version itself, since `changeCount` and
/// `GetClipboardSequenceNumber` read a counter without opening the clipboard or reading content.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn native_gesture_offer(location: ReadLocation) -> GestureOffer {
    native_offer_version(location).map_or(GestureOffer::Unavailable, GestureOffer::Version)
}

/// Linux: the X11 owner's `TIMESTAMP` needs a round trip, so the gesture reads none.
#[cfg(target_os = "linux")]
fn native_gesture_offer(_location: ReadLocation) -> GestureOffer {
    GestureOffer::RoundTrip
}

/// Other platforms cannot version an offer.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn native_gesture_offer(_location: ReadLocation) -> GestureOffer {
    GestureOffer::Unavailable
}

/// Windows: `GetClipboardSequenceNumber`, which every change to the clipboard advances; 0 means
/// the session has no clipboard access, so there is no version. There is no PRIMARY.
#[cfg(target_os = "windows")]
fn native_offer_version(location: ReadLocation) -> Option<u64> {
    match location {
        ReadLocation::Clipboard => {
            Some(super::clipboard_worker::clipboard_sequence()).filter(|sequence| *sequence != 0)
        }
        ReadLocation::Primary => None,
    }
    .map(u64::from)
}

/// macOS: the general pasteboard's `changeCount`, which every new offer advances; read without
/// reading any content. There is no PRIMARY.
#[cfg(target_os = "macos")]
fn native_offer_version(location: ReadLocation) -> Option<u64> {
    use objc2_app_kit::NSPasteboard;
    if location == ReadLocation::Primary {
        // When: location is Primary, macOS has no such selection.
        return None;
    }
    let count = objc2::rc::autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount());
    u64::try_from(count).ok()
}

/// Linux: the X11 selection owner and its `TIMESTAMP` answer, which a new offer changes.
#[cfg(target_os = "linux")]
fn native_offer_version(location: ReadLocation) -> Option<u64> {
    super::osc52_read_x11_native::offer_version(location)
}

/// Other platforms cannot version an offer.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn native_offer_version(_location: ReadLocation) -> Option<u64> {
    None
}

/// Windows: the open clipboard's formats through `EnumClipboardFormats`, named without reading any
/// data; there is no PRIMARY.
#[cfg(target_os = "windows")]
fn native_list(location: ReadLocation) -> TypeListing {
    match location {
        ReadLocation::Clipboard => super::clipboard_worker::list_windows_formats()
            .map_or(TypeListing::Failed, list_native_types),
        ReadLocation::Primary => TypeListing::Unsupported,
    }
}

/// macOS: the general pasteboard's `types`, each named by its uniform type's preferred MIME name
/// where it has one, without reading any representation.
#[cfg(target_os = "macos")]
fn native_list(location: ReadLocation) -> TypeListing {
    if location == ReadLocation::Primary {
        // When: location is Primary, macOS has no such selection.
        return TypeListing::Unsupported;
    }
    super::kitty_clipboard_offer::list_general_types()
}

/// Windows: the registered or standard format a non-text MIME name maps to, sized with
/// `GlobalSize` before it is locked; there is no PRIMARY.
#[cfg(target_os = "windows")]
fn native_read_type(location: ReadLocation, mime: &str, max_bytes: usize) -> TypeRead {
    match location {
        ReadLocation::Clipboard => super::kitty_clipboard_offer::read_windows_type(mime, max_bytes),
        ReadLocation::Primary => TypeRead::Absent,
    }
}

/// macOS: the general pasteboard's data for the type a MIME name maps to, refused by its length
/// before the copy into Rust; there is no PRIMARY.
#[cfg(target_os = "macos")]
fn native_read_type(location: ReadLocation, mime: &str, max_bytes: usize) -> TypeRead {
    if location == ReadLocation::Primary {
        // When: location is Primary, macOS has no such selection.
        return TypeRead::Absent;
    }
    super::kitty_clipboard_offer::read_general_type(mime, max_bytes)
}

/// X11 reads the exact existing atom; unknown MIME names allocate no server atoms.
#[cfg(target_os = "linux")]
fn native_read_type(location: ReadLocation, mime: &str, max_bytes: usize) -> TypeRead {
    super::osc52_read_x11_native::read_type(location, mime, max_bytes)
}

/// Other platforms read text only.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn native_read_type(_location: ReadLocation, _mime: &str, _max_bytes: usize) -> TypeRead {
    TypeRead::Absent
}

/// Linux: the X11 owner's `TARGETS`, named through the server; no X server reads as unsupported.
#[cfg(target_os = "linux")]
fn native_list(location: ReadLocation) -> TypeListing {
    super::osc52_read_x11_native::list_targets(location)
}

/// Other platforms cannot enumerate a clipboard.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn native_list(_location: ReadLocation) -> TypeListing {
    TypeListing::Unsupported
}

/// Windows: `CF_UNICODETEXT`, sized before it is locked; there is no PRIMARY.
#[cfg(target_os = "windows")]
fn native_read(location: ReadLocation, max_bytes: usize) -> ReadOutcome {
    match location {
        ReadLocation::Clipboard => super::clipboard_worker::read_windows_text(max_bytes),
        ReadLocation::Primary => ReadOutcome::Unsupported,
    }
}

/// macOS: `NSPasteboardTypeString` from the general pasteboard. AppKit materializes the value
/// before its length can be measured, as any paste does; the copy into Rust is refused past the
/// cap.
#[cfg(target_os = "macos")]
fn native_read(location: ReadLocation, max_bytes: usize) -> ReadOutcome {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    if location == ReadLocation::Primary {
        // When: location is Primary, macOS has no such selection.
        return ReadOutcome::Unsupported;
    }
    objc2::rc::autoreleasepool(|_| {
        let string_type =
            // SAFETY: NSPasteboardTypeString is an immutable AppKit constant that lives for the process.
            unsafe { NSPasteboardTypeString };
        let Some(data) = NSPasteboard::generalPasteboard().dataForType(string_type) else {
            // When: dataForType finds no string, the pasteboard holds no text.
            return ReadOutcome::Empty;
        };
        if data.length() > max_bytes {
            // When: data.length passes max_bytes, every byte converts to at least one, so it is
            // refused before the copy into Rust.
            return ReadOutcome::TooLarge;
        }
        bounded_utf8(data.to_vec(), max_bytes)
    })
}

/// Linux: X11 `ConvertSelection` with bounded replies; no X server reads as unsupported.
#[cfg(target_os = "linux")]
fn native_read(location: ReadLocation, max_bytes: usize) -> ReadOutcome {
    super::osc52_read_x11_native::read(location, max_bytes)
}

/// Other platforms have no native read.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn native_read(_location: ReadLocation, _max_bytes: usize) -> ReadOutcome {
    ReadOutcome::Unsupported
}

#[cfg(test)]
#[path = "osc52_read_source_tests.rs"]
mod osc52_read_source_tests;
