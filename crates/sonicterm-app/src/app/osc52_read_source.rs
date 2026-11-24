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
const MAX_LISTED_TYPES: usize = 16;

/// The MIME name a native clipboard type stands for: the platform's text, HTML, file and image
/// types map to theirs, a MIME-shaped name passes without its parameters, and private, dynamic and
/// protocol types map to `None`, so they are never listed.
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
/// [`MAX_NATIVE_TYPES_SCANNED`] are inspected and at most 16 distinct MIME names kept. Only names
/// are read, never content.
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
        Err(error) => ReadOutcome::Text(String::from_utf8_lossy(error.as_bytes()).into_owned()),
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
    ReadOutcome::Text(bytes.iter().map(|byte| char::from(*byte)).collect())
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

/// macOS: the general pasteboard's `types`, which names its representations without reading any.
#[cfg(target_os = "macos")]
fn native_list(location: ReadLocation) -> TypeListing {
    use objc2_app_kit::NSPasteboard;
    if location == ReadLocation::Primary {
        // When: location is Primary, macOS has no such selection.
        return TypeListing::Unsupported;
    }
    objc2::rc::autoreleasepool(|_| {
        let Some(types) = NSPasteboard::generalPasteboard().types() else {
            // When: types is None, the pasteboard offers nothing.
            return TypeListing::Types(Vec::new());
        };
        let count = types.count().min(MAX_NATIVE_TYPES_SCANNED);
        list_native_types((0..count).map(|index| types.objectAtIndex(index).to_string()))
    })
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
