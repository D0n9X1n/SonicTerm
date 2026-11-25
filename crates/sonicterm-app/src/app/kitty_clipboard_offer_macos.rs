//! The macOS offer backend: one `NSPasteboardItem` carries every representation, plus one item per
//! further file of a `text/uri-list`, so a publish is one `clearContents` and `writeObjects` pair
//! and the pasteboard's change count moves once.
//!
//! Text, HTML and PNG use their system types; any other MIME name uses the uniform type
//! `UTType.typeWithMIMEType:` gives it, a system type when one exists, else the dynamic `dyn.*` type
//! the system derives. A `text/uri-list` of `file:` URIs is one `public.file-url` item per file; one
//! other absolute URL is `public.url`.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

use super::{CommitGate, NativeFormat, OfferBackend, Published};
use crate::app::osc52_read_source::{
    list_native_types, native_type_mime, TypeListing, TypeRead, MAX_NATIVE_TYPES_SCANNED,
};

// `UTType` lives in UniformTypeIdentifiers; linking it here keeps the class resolvable in every
// binary that holds this backend, not only one that links the framework for its own use.
#[link(name = "UniformTypeIdentifiers", kind = "framework")]
extern "C" {}

/// The pasteboard type of one file URL.
const FILE_URL_TYPE: &str = "public.file-url";

/// The pasteboard type of one other URL.
const URL_TYPE: &str = "public.url";

/// Pasteboard items a `text/uri-list` read inspects at most.
const MAX_URI_ITEMS_SCANNED: usize = 4096;

/// The pasteboard type `format` is published under; `None` when this build cannot hold it.
/// `text/uri-list` is published as items, so it has no single type here.
pub(super) fn pasteboard_type(format: &NativeFormat) -> Option<Retained<NSString>> {
    match format {
        NativeFormat::Text => Some(NSString::from_str("public.utf8-plain-text")),
        NativeFormat::Html => Some(NSString::from_str("public.html")),
        NativeFormat::Png => Some(NSString::from_str("public.png")),
        NativeFormat::UriList => None,
        NativeFormat::Custom(mime) => uniform_type(mime),
    }
}

/// The identifier of the uniform type for MIME name `mime`; `None` when the class is missing or
/// the system gives no type.
fn uniform_type(mime: &str) -> Option<Retained<NSString>> {
    let class: &AnyClass = AnyClass::get(c"UTType")?;
    let name = NSString::from_str(mime);
    // SAFETY: `typeWithMIMEType:` is a UTType class method taking an NSString and returning a
    // nullable UTType; `identifier` returns its non-null NSString. Both are thread-safe.
    unsafe {
        let uniform: Option<Retained<AnyObject>> = msg_send![class, typeWithMIMEType: &*name];
        let identifier: Retained<NSString> = msg_send![&*uniform?, identifier];
        Some(identifier)
    }
}

/// The preferred MIME name of the uniform type `identifier` names; `None` when it has none.
fn preferred_mime(identifier: &NSString) -> Option<String> {
    let class: &AnyClass = AnyClass::get(c"UTType")?;
    // SAFETY: `typeWithIdentifier:` is a UTType class method taking an NSString and returning a
    // nullable UTType; `preferredMIMEType` returns a nullable NSString. Both are thread-safe.
    unsafe {
        let uniform: Option<Retained<AnyObject>> = msg_send![class, typeWithIdentifier: identifier];
        let mime: Option<Retained<NSString>> = msg_send![&*uniform?, preferredMIMEType];
        mime.map(|mime| mime.to_string())
    }
}

/// How a `text/uri-list` is published.
enum UriLayout<'list> {
    /// One `public.file-url` item per `file:` URI.
    Files(Vec<&'list str>),
    /// One other absolute URL as `public.url`.
    Url(&'list str),
}

/// Whether `item` starts with an RFC 3986 scheme and holds no whitespace.
fn is_absolute_uri(item: &str) -> bool {
    let Some((scheme, _)) = item.split_once(':') else {
        // When: item has no colon, it has no scheme.
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
        && !item.contains(char::is_whitespace)
}

/// Whether `item` is a `file:` URI.
fn is_file_uri(item: &str) -> bool {
    item.get(..5).is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
}

/// The layout a `text/uri-list` takes: every item a `file:` URI, or one other absolute URL;
/// `None` for an empty list, a mix, several other URLs or a line that is not a URI.
fn uri_layout(bytes: &[u8]) -> Option<UriLayout<'_>> {
    let items = super::uri_list_items(bytes)?;
    if items.is_empty() || !items.iter().all(|item| is_absolute_uri(item)) {
        // When: items is empty or is_absolute_uri refuses a line, the list is not published.
        return None;
    }
    if items.iter().all(|item| is_file_uri(item)) {
        // When: every item is a file URI, each becomes its own file-url item.
        return Some(UriLayout::Files(items));
    }
    match items.as_slice() {
        [url] => Some(UriLayout::Url(url)),
        _ => None,
    }
}

/// Whether macOS can hold `format` with `bytes`; called on the VT worker, so it drains its own pool.
pub(super) fn holds(format: &NativeFormat, bytes: &[u8]) -> bool {
    if *format == NativeFormat::UriList {
        // When: format is UriList, it is held only when its items form a layout.
        return uri_layout(bytes).is_some();
    }
    objc2::rc::autoreleasepool(|_| pasteboard_type(format).is_some())
}

/// A pasteboard, the item a publish stages its representations on, and one more item per
/// further file of a `text/uri-list`.
pub(super) struct Pasteboard {
    board: Retained<NSPasteboard>,
    item: Retained<NSPasteboardItem>,
    extra: Vec<Retained<NSPasteboardItem>>,
}

impl Pasteboard {
    /// The general pasteboard; used on the event-loop thread only.
    pub(super) fn general() -> Self {
        Self::on(NSPasteboard::generalPasteboard())
    }

    /// Stage onto `board`.
    pub(super) fn on(board: Retained<NSPasteboard>) -> Self {
        Self { board, item: NSPasteboardItem::new(), extra: Vec::new() }
    }

    /// Stage a `text/uri-list`: its first file URL on the item, each further one on its own item,
    /// or its one other URL as `public.url`.
    fn stage_uri_list(&mut self, bytes: &[u8]) -> bool {
        let files = match uri_layout(bytes) {
            Some(UriLayout::Files(files)) => files,
            Some(UriLayout::Url(url)) => {
                // When: the list is one other URL, it is the item's public.url.
                let kind = NSString::from_str(URL_TYPE);
                return self.item.setString_forType(&NSString::from_str(url), &kind);
            }
            None => {
                // When: uri_layout refuses the list, it cannot be staged.
                return false;
            }
        };
        let kind = NSString::from_str(FILE_URL_TYPE);
        for (index, file) in files.iter().enumerate() {
            let item = if index == 0 {
                self.item.clone()
            } else {
                // When: index is past 0, each further file URL takes its own item.
                NSPasteboardItem::new()
            };
            if !item.setString_forType(&NSString::from_str(file), &kind) {
                // When: setString_forType refuses a file URL, the offer cannot be staged.
                return false;
            }
            if index > 0 {
                self.extra.push(item);
            }
        }
        true
    }
}

impl OfferBackend for Pasteboard {
    fn stage(&mut self, format: &NativeFormat, bytes: &[u8]) -> bool {
        if *format == NativeFormat::UriList {
            // When: format is UriList, its URLs are staged as items.
            return self.stage_uri_list(bytes);
        }
        let Some(kind) = pasteboard_type(format) else {
            // When: format has no pasteboard type, the offer cannot be staged.
            return false;
        };
        if self.item.types().containsObject(&kind) {
            // When: the item's types already contain kind (`image/jpeg` and `image/jpg` are both
            // `public.jpeg`), the first name keeps it, never replaced.
            return true;
        }
        self.item.setData_forType(&NSData::with_bytes(bytes), &kind)
    }

    fn publish(&mut self, gate: CommitGate<'_>) -> Published {
        let item = std::mem::replace(&mut self.item, NSPasteboardItem::new());
        let mut items = vec![ProtocolObject::<dyn NSPasteboardWriting>::from_retained(item)];
        let extra = self.extra.drain(..);
        items.extend(extra.map(ProtocolObject::<dyn NSPasteboardWriting>::from_retained));
        if !gate() {
            // When: gate refuses at the commit point, the staged items drop and nothing changes.
            return Published::Unchanged;
        }
        self.board.clearContents();
        if self.board.writeObjects(&NSArray::from_retained_slice(&items)) {
            Published::Written
        } else {
            // When: writeObjects refuses the items, the cleared pasteboard holds no partial offer.
            Published::Failed
        }
    }

    fn discard(&mut self) {
        self.item = NSPasteboardItem::new();
        self.extra.clear();
    }
}

/// The bytes `board` holds under the non-text MIME name `mime`, within `max_bytes`: HTML, PNG and
/// custom types through their pasteboard type, `text/uri-list` from every item's file or other URL.
/// A value over the cap is refused by its length before the copy into Rust.
pub(super) fn read_type(board: &NSPasteboard, mime: &str, max_bytes: usize) -> TypeRead {
    let Some(format) = super::native_format(mime) else {
        // When: mime is no MIME name, nothing is held under it.
        return TypeRead::Absent;
    };
    if format == NativeFormat::UriList {
        // When: format is UriList, it is joined from the items' URLs.
        return read_uri_list(board, max_bytes);
    }
    if format == NativeFormat::Text {
        // When: format is Text, the bounded text reader owns that conversion.
        return TypeRead::Absent;
    }
    let kind = board
        .types()
        .and_then(|types| {
            (0..types.count().min(MAX_NATIVE_TYPES_SCANNED))
                .map(|index| types.objectAtIndex(index))
                .find(|identifier| {
                    native_type_mime(&listing_name(identifier))
                        .is_some_and(|name| name.eq_ignore_ascii_case(mime))
                })
        })
        .or_else(|| pasteboard_type(&format));
    let Some(data) = kind.and_then(|kind| board.dataForType(&kind)) else {
        // When: the type has no pasteboard type or the board does not hold it, it is absent.
        return TypeRead::Absent;
    };
    if data.length() > max_bytes {
        // When: data.length passes max_bytes, it is refused before the copy into Rust.
        return TypeRead::TooLarge;
    }
    TypeRead::Bytes(data.to_vec())
}

/// `board`'s items' file URLs, else other URLs, as a `text/uri-list` with CRLF after each.
fn read_uri_list(board: &NSPasteboard, max_bytes: usize) -> TypeRead {
    let Some(items) = board.pasteboardItems() else {
        // When: pasteboardItems is None, the board holds nothing.
        return TypeRead::Absent;
    };
    let (file, url) = (NSString::from_str(FILE_URL_TYPE), NSString::from_str(URL_TYPE));
    let mut out = Vec::new();
    for index in 0..items.count().min(MAX_URI_ITEMS_SCANNED) {
        let item = items.objectAtIndex(index);
        let Some(uri) = item.stringForType(&file).or_else(|| item.stringForType(&url)) else {
            // When: the item holds no URL, it adds nothing.
            continue;
        };
        if out.len() + uri.len() + 2 > max_bytes {
            // When: the joined URLs would pass max_bytes, the list is refused before it grows.
            return TypeRead::TooLarge;
        }
        out.extend_from_slice(uri.to_string().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if out.is_empty() {
        TypeRead::Absent
    } else {
        // When: out holds at least one URL, the list is served.
        TypeRead::Bytes(out)
    }
}

/// The native name fed to the listing mapper. URL pasteboard items are interpreted as URI lists.
fn listing_name(identifier: &NSString) -> String {
    let native = identifier.to_string();
    match native.as_str() {
        FILE_URL_TYPE | URL_TYPE => "text/uri-list".into(),
        _ => preferred_mime(identifier).unwrap_or(native),
    }
}

/// The MIME names `board` offers: each pasteboard type named by its uniform type's preferred MIME
/// name where it has one, else by its identifier, then mapped as every platform's listing is, so
/// private and protocol types are never listed. Only names are read, never content.
pub(super) fn list_types(board: &NSPasteboard) -> TypeListing {
    let Some(types) = board.types() else {
        // When: types is None, the pasteboard offers nothing.
        return TypeListing::Types(Vec::new());
    };
    let count = types.count().min(MAX_NATIVE_TYPES_SCANNED);
    list_native_types((0..count).map(|index| listing_name(&types.objectAtIndex(index))))
}

#[cfg(test)]
#[path = "kitty_clipboard_offer_macos_tests.rs"]
mod kitty_clipboard_offer_macos_tests;
