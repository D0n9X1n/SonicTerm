//! The text a decoded OSC 52 write becomes: its UTF-8 conversion, reserved before it grows, and
//! the native copies each selection keeps, plus the Windows reassertion bound.

use super::{ClipLease, ClipTargets, Osc52Refusal, REASSERT_MAX_UTF16_UNITS};

/// Bytes the native clipboard copy of `text` takes: `CF_UNICODETEXT` on Windows (2 per UTF-16
/// unit plus the terminator), the UTF-8 length for `NSString` and arboard's X11 copy.
pub(in crate::app) fn native_clipboard_bytes(text: &str) -> usize {
    if cfg!(target_os = "windows") {
        // When: target_os is Windows, CF_UNICODETEXT holds UTF-16 and a terminator.
        text.encode_utf16().count() * 2 + 2
    } else {
        // When: target_os is not Windows, the native copy holds UTF-8.
        text.len()
    }
}

/// Whether Windows may reassert `text`: at most [`REASSERT_MAX_UTF16_UNITS`] UTF-16 units.
pub(in crate::app) fn reassert_eligible(text: &str) -> bool {
    text.encode_utf16().nth(REASSERT_MAX_UTF16_UNITS).is_none()
}

/// Build the text for decoded `bytes`, invalid sequences replaced by U+FFFD, with `lease` grown
/// first to cover the text and one native copy per selection `targets` names: arboard keeps a
/// separate copy of each selection it wrote (X11 PRIMARY and CLIPBOARD are independent buffers).
pub(super) fn into_text(
    bytes: Vec<u8>,
    mut lease: ClipLease,
    targets: ClipTargets,
) -> Result<(String, ClipLease), Osc52Refusal> {
    let copies = usize::from(targets.clipboard) + usize::from(targets.primary);
    let mut lossy_len = 0usize;
    let mut valid = true;
    for chunk in bytes.utf8_chunks() {
        lossy_len += chunk.valid().len();
        if !chunk.invalid().is_empty() {
            // An invalid sequence becomes one 3-byte U+FFFD.
            lossy_len += 3;
            valid = false;
        }
    }
    let text = if valid {
        String::from_utf8(bytes).expect("every chunk was valid UTF-8")
    } else {
        // When: valid is false, the expanded text is reserved, then built in a buffer of exactly
        // that length, so no growth passes what was reserved.
        if !lease.grow_to(bytes.capacity() + lossy_len) {
            // When: lease cannot grow over the expansion, the write is refused busy.
            return Err(Osc52Refusal::Busy);
        }
        let mut text = String::with_capacity(lossy_len);
        for chunk in bytes.utf8_chunks() {
            text.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                // An invalid sequence becomes one U+FFFD, as counted above.
                text.push(char::REPLACEMENT_CHARACTER);
            }
        }
        text
    };
    lease.shrink_to(text.capacity());
    let native_bytes = native_clipboard_bytes(&text).saturating_mul(copies);
    if !lease.grow_to(text.capacity().saturating_add(native_bytes)) {
        // When: lease cannot grow over every selection's native copy, the write is refused busy.
        return Err(Osc52Refusal::Busy);
    }
    Ok((text, lease))
}
