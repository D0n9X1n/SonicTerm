//! Win32 reads the clipboard worker's reassertion makes: the change counter, and the
//! `CF_UNICODETEXT` block, sized with `GlobalSize` before `GlobalLock` so an oversized block is
//! never locked or copied.

use std::time::Duration;

use windows::Win32::{
    Foundation::HGLOBAL,
    System::{
        DataExchange::{
            CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
        },
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
    },
};

use super::Snapshot;

/// The standard clipboard format for NUL-terminated UTF-16 text.
const CF_UNICODETEXT: u32 = 13;

/// Attempts to open a clipboard another process briefly holds.
const OPEN_ATTEMPTS: usize = 5;

/// The pause between open attempts.
const OPEN_RETRY: Duration = Duration::from_millis(5);

/// The clipboard's change counter, which every change to its contents advances.
pub(super) fn sequence() -> u32 {
    // SAFETY: GetClipboardSequenceNumber takes no arguments and only reads a session counter.
    unsafe { GetClipboardSequenceNumber() }
}

/// The clipboard, open on this thread until dropped.
struct OpenGuard;

impl OpenGuard {
    /// Open the clipboard with no owner window, retrying while another process holds it.
    fn open() -> Option<Self> {
        for _ in 0..OPEN_ATTEMPTS {
            let opened =
                // SAFETY: OpenClipboard with no owner window only claims the clipboard here.
                unsafe { OpenClipboard(None) }.is_ok();
            if opened {
                // When: opened, Drop closes the clipboard on this thread.
                return Some(Self);
            }
            std::thread::sleep(OPEN_RETRY);
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
