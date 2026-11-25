//! The macOS offer backend: one `NSPasteboardItem` carries every representation, so a publish is
//! one `clearContents` and `writeObjects` pair and the pasteboard's change count moves once.
//!
//! Text, HTML and PNG use their system types; any other MIME name uses the uniform type
//! `UTType.typeWithMIMEType:` gives it, a system type when one exists, else the dynamic `dyn.*` type
//! the system derives. `text/uri-list` is omitted: file URLs need one item each.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

use super::{NativeFormat, OfferBackend};

/// The pasteboard type `format` is published under; `None` when this build cannot hold it.
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

/// Whether macOS can hold `format`; called on the VT worker, so it drains its own pool.
pub(super) fn holds(format: &NativeFormat) -> bool {
    objc2::rc::autoreleasepool(|_| pasteboard_type(format).is_some())
}

/// A pasteboard and the one item a publish stages its representations on.
pub(super) struct Pasteboard {
    board: Retained<NSPasteboard>,
    item: Retained<NSPasteboardItem>,
}

impl Pasteboard {
    /// The general pasteboard; used on the event-loop thread only.
    pub(super) fn general() -> Self {
        Self::on(NSPasteboard::generalPasteboard())
    }

    /// Stage onto `board`.
    pub(super) fn on(board: Retained<NSPasteboard>) -> Self {
        Self { board, item: NSPasteboardItem::new() }
    }
}

impl OfferBackend for Pasteboard {
    fn stage(&mut self, format: &NativeFormat, bytes: &[u8]) -> bool {
        let Some(kind) = pasteboard_type(format) else {
            // When: format has no pasteboard type, the offer cannot be staged.
            return false;
        };
        self.item.setData_forType(&NSData::with_bytes(bytes), &kind)
    }

    fn publish(&mut self) -> bool {
        let item = std::mem::replace(&mut self.item, NSPasteboardItem::new());
        let items = [ProtocolObject::<dyn NSPasteboardWriting>::from_retained(item)];
        self.board.clearContents();
        self.board.writeObjects(&NSArray::from_retained_slice(&items))
    }

    fn discard(&mut self) {
        self.item = NSPasteboardItem::new();
    }
}

#[cfg(test)]
#[path = "kitty_clipboard_offer_macos_tests.rs"]
mod kitty_clipboard_offer_macos_tests;
