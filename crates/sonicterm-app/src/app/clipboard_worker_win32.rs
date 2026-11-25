//! Win32 clipboard reads: the change counter and the OSC 5522 format listing. The OSC 52 text read,
//! program writes, their pre-write snapshot and the reassertion's comparison run over the
//! `Win32Clipboard` seam in `kitty_clipboard_offer_windows.rs` and `kitty_clipboard_offer_win32.rs`,
//! which size a text block with `GlobalSize` before `GlobalLock`.

use std::time::Duration;

use windows::Win32::System::DataExchange::{
    CloseClipboard, EnumClipboardFormats, GetClipboardFormatNameW, GetClipboardSequenceNumber,
    OpenClipboard,
};

use crate::app::osc52_read_source::MAX_NATIVE_TYPES_SCANNED;

/// The standard clipboard format for NUL-terminated UTF-16 text.
const CF_UNICODETEXT: u32 = 13;

/// The clipboard's change counter, which every change to its contents advances.
pub(in crate::app) fn sequence() -> u32 {
    // SAFETY: GetClipboardSequenceNumber takes no arguments and only reads a session counter.
    unsafe { GetClipboardSequenceNumber() }
}

/// Attempts an OSC 52 read makes to open a clipboard another process holds.
const READ_OPEN_ATTEMPTS: usize = 10;

/// The pause between an OSC 52 read's open attempts.
const READ_OPEN_RETRY: Duration = Duration::from_millis(10);

/// The clipboard, open on this thread until dropped.
struct OpenGuard;

impl OpenGuard {
    /// Open the clipboard with no owner window in at most `attempts` tries, `pause` apart.
    fn open_with(attempts: usize, pause: Duration) -> Option<Self> {
        for _ in 0..attempts {
            let opened =
                // SAFETY: OpenClipboard with no owner window only claims the clipboard here.
                unsafe { OpenClipboard(None) }.is_ok();
            if opened {
                // When: opened, Drop closes the clipboard on this thread.
                return Some(Self);
            }
            std::thread::sleep(pause);
        }
        None
    }
}

// Lifecycle: OpenGuard drop calls CloseClipboard once; another thread may then open it.
impl Drop for OpenGuard {
    fn drop(&mut self) {
        let _ =
            // SAFETY: this thread opened the clipboard in OpenGuard::open, closed once here.
            unsafe { CloseClipboard() };
    }
}

/// The names of the clipboard's formats for an OSC 5522 listing, at most
/// [`MAX_NATIVE_TYPES_SCANNED`]; `None` when another process held the clipboard throughout.
/// `EnumClipboardFormats` names formats without rendering or reading any data.
pub(in crate::app) fn list_formats() -> Option<Vec<String>> {
    let _open = OpenGuard::open_with(READ_OPEN_ATTEMPTS, READ_OPEN_RETRY)?;
    let mut names = Vec::new();
    let mut format = 0;
    while names.len() < MAX_NATIVE_TYPES_SCANNED {
        format =
            // SAFETY: the clipboard is open on this thread; EnumClipboardFormats reads its format list.
            unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            // When: format is 0, the enumeration ended; the names so far stand.
            break;
        }
        names.push(format_name(format));
    }
    Some(names)
}

/// The name of clipboard `format`: a predefined text or file format's constant name, else the
/// name it was registered under (empty for an unnamed predefined format).
fn format_name(format: u32) -> String {
    match format {
        1 => "CF_TEXT".to_owned(),
        7 => "CF_OEMTEXT".to_owned(),
        CF_UNICODETEXT => "CF_UNICODETEXT".to_owned(),
        15 => "CF_HDROP".to_owned(),
        _ => {
            // When: format is no predefined text or file format, its registered name is asked for.
            let mut buffer = [0u16; 256];
            let length =
                // SAFETY: GetClipboardFormatNameW writes at most buffer.len() units into buffer.
                unsafe { GetClipboardFormatNameW(format, &mut buffer) };
            let length = usize::try_from(length).unwrap_or(0).min(buffer.len());
            String::from_utf16_lossy(&buffer[..length])
        }
    }
}
