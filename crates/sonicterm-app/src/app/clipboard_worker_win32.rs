//! Win32 reads the clipboard worker's reassertion makes: the change counter, and the
//! `CF_UNICODETEXT` block, sized with `GlobalSize` before `GlobalLock` so an oversized block is
//! never locked or copied.

use std::time::Duration;

use windows::Win32::{
    Foundation::HGLOBAL,
    System::{
        DataExchange::{
            CloseClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
            GetClipboardSequenceNumber, OpenClipboard,
        },
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
    },
};

use super::Snapshot;
use crate::app::osc52_read_source::{
    bounded_utf16, utf16_block_admits, ReadOutcome, MAX_NATIVE_TYPES_SCANNED,
};

/// The standard clipboard format for NUL-terminated UTF-16 text.
const CF_UNICODETEXT: u32 = 13;

/// Attempts to open a clipboard another process briefly holds.
const OPEN_ATTEMPTS: usize = 5;

/// The pause between open attempts.
const OPEN_RETRY: Duration = Duration::from_millis(5);

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
    /// Open the clipboard with no owner window, retrying while another process holds it.
    fn open() -> Option<Self> {
        Self::open_with(OPEN_ATTEMPTS, OPEN_RETRY)
    }

    /// Open the clipboard in at most `attempts` tries, `pause` apart.
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

/// Run `read` over the `CF_UNICODETEXT` block's size and its units before the terminator, only
/// when `admit` accepts the size `GlobalSize` reports before the block is locked.
fn with_text_block<T>(
    admit: impl FnOnce(usize) -> bool,
    read: impl FnOnce(usize, &[u16]) -> T,
) -> Option<T> {
    let _open = OpenGuard::open()?;
    let handle =
        // SAFETY: the clipboard is open on this thread; the handle stays owned by the clipboard.
        unsafe { GetClipboardData(CF_UNICODETEXT) }.ok()?;
    let block = HGLOBAL(handle.0);
    let block_bytes =
        // SAFETY: block is the clipboard's live text handle while the clipboard stays open.
        unsafe { GlobalSize(block) };
    if block_bytes == 0 || !admit(block_bytes) {
        // When: block_bytes is unknown or admit refuses it, the block is never locked.
        return None;
    }
    let base =
        // SAFETY: block is a live moveable allocation, so GlobalLock returns its base or null.
        unsafe { GlobalLock(block) }.cast::<u16>();
    if base.is_null() {
        // When: base is null, GlobalLock failed and there is nothing to read.
        return None;
    }
    let units =
        // SAFETY: base points at block_bytes locked bytes until GlobalUnlock; block_bytes / 2
        // u16 units stay in bounds.
        unsafe { std::slice::from_raw_parts(base, block_bytes / 2) };
    let text_units = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    let result = read(block_bytes, &units[..text_units]);
    let _ =
        // SAFETY: block was locked above; a final unlock reports an error with no lock left.
        unsafe { GlobalUnlock(block) };
    Some(result)
}

/// The clipboard's `CF_UNICODETEXT` for an OSC 52 read, within `max_bytes` of UTF-8. The block's
/// `GlobalSize` decides `TooLarge` before `GlobalLock`, so an oversized block is never locked or
/// copied; conversion is measured before it allocates. Delayed rendering runs inside
/// `GetClipboardData`, which the reader's deadline bounds for the caller.
pub(in crate::app) fn read_text(max_bytes: usize) -> ReadOutcome {
    let Some(_open) = OpenGuard::open_with(READ_OPEN_ATTEMPTS, READ_OPEN_RETRY) else {
        // When: open_with gives up, another process held the clipboard throughout.
        return ReadOutcome::Failed;
    };
    let data =
        // SAFETY: the clipboard is open on this thread; the handle stays owned by the clipboard.
        unsafe { GetClipboardData(CF_UNICODETEXT) };
    let Ok(handle) = data else {
        // When: data is Err, the clipboard offers no CF_UNICODETEXT, so it holds no text.
        return ReadOutcome::Empty;
    };
    let block = HGLOBAL(handle.0);
    let block_bytes =
        // SAFETY: block is the clipboard's live text handle while the clipboard stays open.
        unsafe { GlobalSize(block) };
    if block_bytes == 0 {
        // When: block_bytes is 0, GlobalSize could not size the handle, so nothing is readable.
        return ReadOutcome::Failed;
    }
    if !utf16_block_admits(block_bytes, max_bytes) {
        // When: utf16_block_admits refuses block_bytes, the block is never locked.
        return ReadOutcome::TooLarge;
    }
    let base =
        // SAFETY: block is a live moveable allocation, so GlobalLock returns its base or null.
        unsafe { GlobalLock(block) }.cast::<u16>();
    if base.is_null() {
        // When: base is null, GlobalLock failed and there is nothing to read.
        return ReadOutcome::Failed;
    }
    let units =
        // SAFETY: base points at block_bytes locked bytes until GlobalUnlock; block_bytes / 2
        // u16 units stay in bounds.
        unsafe { std::slice::from_raw_parts(base, block_bytes / 2) };
    let text_units = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    let outcome = bounded_utf16(&units[..text_units], max_bytes);
    let _ =
        // SAFETY: block was locked above; a final unlock reports an error with no lock left.
        unsafe { GlobalUnlock(block) };
    outcome
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

/// The clipboard text, when its block is at most `max_block_bytes`.
pub(super) fn snapshot(max_block_bytes: usize) -> Option<Snapshot> {
    with_text_block(
        |block_bytes| block_bytes <= max_block_bytes,
        |block_bytes, units| Snapshot { block_bytes, units: units.to_vec() },
    )
}

/// Whether the clipboard holds exactly `snapshot`: the block size is compared before the lock.
pub(super) fn holds(snapshot: &Snapshot) -> bool {
    with_text_block(
        |block_bytes| block_bytes == snapshot.block_bytes,
        |_, units| units == snapshot.units.as_slice(),
    )
    .unwrap_or(false)
}
