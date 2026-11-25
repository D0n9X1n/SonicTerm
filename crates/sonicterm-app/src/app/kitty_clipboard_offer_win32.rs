//! The Win32 calls of the Windows offer backend, made on the clipboard worker: each block is a
//! filled moveable global allocation before `OpenClipboard`, and one session sets them all. Reads
//! of non-text formats size each block with `GlobalSize` before `GlobalLock`.

use std::time::Duration;

use windows::core::HSTRING;
use windows::Win32::{
    Foundation::{GlobalFree, HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
            GetClipboardFormatNameW, GetClipboardSequenceNumber, OpenClipboard,
            RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT},
    },
};

use super::windows_formats::{
    hdrop_uri_list, html_fragment, Win32Clipboard, WindowsOffer, CF_HDROP, CF_UNICODETEXT,
    HTML_FORMAT_NAME, PNG_FORMAT_NAME,
};
use super::{native_format, publish_offer, CommitGate, NativeFormat, NativeOffer, Published};
use crate::app::clipboard_worker::Snapshot;
use crate::app::osc52_read_source::{TypeRead, MAX_NATIVE_TYPES_SCANNED};

/// Attempts to open a clipboard another process briefly holds.
const OPEN_ATTEMPTS: usize = 10;

/// The pause between open attempts.
const OPEN_RETRY: Duration = Duration::from_millis(10);

/// Open the clipboard with no owner window in at most [`OPEN_ATTEMPTS`] tries.
fn open_clipboard() -> bool {
    for _ in 0..OPEN_ATTEMPTS {
        let opened =
            // SAFETY: OpenClipboard with no owner window only claims the clipboard for this thread.
            unsafe { OpenClipboard(None) }.is_ok();
        if opened {
            // When: opened, the caller closes the clipboard on this thread.
            return true;
        }
        std::thread::sleep(OPEN_RETRY);
    }
    false
}

/// The real Win32 clipboard.
struct Win32;

impl Win32Clipboard for Win32 {
    type Block = HGLOBAL;

    fn register(&mut self, name: &str) -> Option<u32> {
        let id =
            // SAFETY: RegisterClipboardFormatW reads the NUL-terminated name HSTRING holds.
            unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) };
        (id != 0).then_some(id)
    }

    fn alloc(&mut self, byte_len: usize, fill: &dyn Fn(&mut [u8])) -> Option<HGLOBAL> {
        let block =
            // SAFETY: GlobalAlloc returns a new zeroed moveable block this thread owns, or fails.
            unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, byte_len) }.ok()?;
        let base =
            // SAFETY: block is a live moveable allocation, so GlobalLock returns its base or null.
            unsafe { GlobalLock(block) }.cast::<u8>();
        if base.is_null() {
            // When: base is null, the block cannot be filled, so it is freed.
            self.free(block);
            return None;
        }
        let bytes =
            // SAFETY: base points at byte_len zeroed bytes, locked and owned here until GlobalUnlock.
            unsafe { std::slice::from_raw_parts_mut(base, byte_len) };
        fill(bytes);
        let _ =
            // SAFETY: block was locked above; a final unlock reports an error with no lock left.
            unsafe { GlobalUnlock(block) };
        Some(block)
    }

    fn free(&mut self, block: HGLOBAL) {
        let _ =
            // SAFETY: block is an unlocked allocation SonicTerm still owns; it is freed once here.
            unsafe { GlobalFree(Some(block)) };
    }

    fn open(&mut self) -> bool {
        open_clipboard()
    }

    fn empty(&mut self) -> bool {
        // SAFETY: the clipboard is open on this thread; EmptyClipboard frees its current data.
        unsafe { EmptyClipboard() }.is_ok()
    }

    fn set(&mut self, format: u32, block: HGLOBAL) -> Result<(), HGLOBAL> {
        let set =
            // SAFETY: the clipboard is open and emptied by this thread; on success the system owns
            // block, on failure it stays SonicTerm's.
            unsafe { SetClipboardData(format, Some(HANDLE(block.0))) };
        set.map(|_| ()).map_err(|_| block)
    }

    fn close(&mut self) {
        let _ =
            // SAFETY: this thread opened the clipboard in open; it is closed once here.
            unsafe { CloseClipboard() };
    }

    fn sequence(&mut self) -> u32 {
        // SAFETY: GetClipboardSequenceNumber takes no arguments and only reads a session counter.
        unsafe { GetClipboardSequenceNumber() }
    }

    fn with_text(
        &mut self,
        admit: &dyn Fn(usize) -> bool,
        read: &mut dyn FnMut(usize, &[u16]),
    ) -> Option<()> {
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
        read(block_bytes, &units[..text_units]);
        let _ =
            // SAFETY: block was locked above; a final unlock reports an error with no lock left.
            unsafe { GlobalUnlock(block) };
        Some(())
    }
}

/// Publish `offer` on the Windows clipboard in one `OpenClipboard` session, `gate` checked
/// immediately before `EmptyClipboard`.
pub(super) fn publish(offer: &NativeOffer, gate: CommitGate<'_>) -> Published {
    publish_offer(&mut WindowsOffer::new(Win32), offer, gate)
}

/// Replace the clipboard with `text` in one acquisition, reading what it replaces first when
/// `snapshot_max_bytes` is set; `gate` is checked after that read, before `EmptyClipboard`.
pub(super) fn write_text(
    text: &str,
    snapshot_max_bytes: Option<usize>,
    gate: CommitGate<'_>,
) -> (Published, Option<Snapshot>) {
    WindowsOffer::new(Win32).write_text(text, snapshot_max_bytes, gate)
}

/// Write `text` again only while the clipboard holds exactly `previous`, compared and replaced in
/// one acquisition; `gate` is checked after the comparison, before `EmptyClipboard`.
pub(super) fn reassert_text(text: &str, previous: &Snapshot, gate: CommitGate<'_>) -> Published {
    WindowsOffer::new(Win32).reassert_text(text, previous, gate)
}

/// The clipboard, open on this thread for one read until dropped.
struct ReadGuard;

// Lifecycle: ReadGuard drop calls CloseClipboard once; another thread may then open it.
impl Drop for ReadGuard {
    fn drop(&mut self) {
        let _ =
            // SAFETY: this thread opened the clipboard for the guard; it is closed once here.
            unsafe { CloseClipboard() };
    }
}

/// The bytes the clipboard holds under the non-text MIME name `mime`, within `max_bytes`: CF_HTML
/// stripped to its fragment, `PNG` and custom formats (found by enumeration, so a read registers
/// nothing) as held, `CF_HDROP` as `file:` URIs. `Failed` when the clipboard stays held.
pub(super) fn read_type(mime: &str, max_bytes: usize) -> TypeRead {
    let Some(format) = native_format(mime).filter(|format| *format != NativeFormat::Text) else {
        // When: mime is no MIME name or names text, the typed read holds nothing for it.
        return TypeRead::Absent;
    };
    if !open_clipboard() {
        // When: open_clipboard gives up, another process held the clipboard throughout.
        return TypeRead::Failed;
    }
    let _open = ReadGuard;
    let raw = |bytes: &[u8]| TypeRead::Bytes(bytes.to_vec());
    match format {
        NativeFormat::UriList => match find_format(|id| id == CF_HDROP) {
            // A URI is at least as many bytes as its path's UTF-16 units, so a block past twice
            // the cap and its header cannot fit.
            Some(id) => read_block(id, max_bytes.saturating_mul(2).saturating_add(64), |bytes| {
                hdrop_uri_list(bytes, max_bytes)
            }),
            None => TypeRead::Absent,
        },
        NativeFormat::Html => match find_named(HTML_FORMAT_NAME) {
            Some(id) => read_block(id, max_bytes, |bytes| {
                html_fragment(bytes).map_or(TypeRead::Absent, raw)
            }),
            None => TypeRead::Absent,
        },
        NativeFormat::Png => match find_named(PNG_FORMAT_NAME) {
            Some(id) => read_block(id, max_bytes, raw),
            None => TypeRead::Absent,
        },
        NativeFormat::Custom(name) => match find_named(&name) {
            Some(id) => read_block(id, max_bytes, raw),
            None => TypeRead::Absent,
        },
        NativeFormat::Text => TypeRead::Absent,
    }
}

/// The first of the open clipboard's formats `wanted` accepts, among at most
/// [`MAX_NATIVE_TYPES_SCANNED`].
fn find_format(wanted: impl Fn(u32) -> bool) -> Option<u32> {
    let mut format = 0;
    for _ in 0..MAX_NATIVE_TYPES_SCANNED {
        format =
            // SAFETY: the clipboard is open on this thread; EnumClipboardFormats reads its list.
            unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            // When: format is 0, the enumeration ended without a match.
            return None;
        }
        if wanted(format) {
            // When: wanted accepts format, it is the one read.
            return Some(format);
        }
    }
    None
}

/// The open clipboard's format registered under `name`, compared as Windows compares names.
fn find_named(name: &str) -> Option<u32> {
    find_format(|format| {
        let mut buffer = [0u16; 256];
        let length =
            // SAFETY: GetClipboardFormatNameW writes at most buffer.len() units into buffer.
            unsafe { GetClipboardFormatNameW(format, &mut buffer) };
        let length = usize::try_from(length).unwrap_or(0).min(buffer.len());
        String::from_utf16_lossy(&buffer[..length]).eq_ignore_ascii_case(name)
    })
}

/// Run `decode` over clipboard `format`'s block when `GlobalSize` reports at most `cap_bytes`,
/// decided before `GlobalLock`, so an oversized block is never locked or copied.
fn read_block(format: u32, cap_bytes: usize, decode: impl FnOnce(&[u8]) -> TypeRead) -> TypeRead {
    let data =
        // SAFETY: the clipboard is open on this thread; the handle stays owned by the clipboard.
        unsafe { GetClipboardData(format) };
    let Ok(handle) = data else {
        // When: data is Err, the format could not be rendered, so the read failed.
        return TypeRead::Failed;
    };
    let block = HGLOBAL(handle.0);
    let block_bytes =
        // SAFETY: block is the clipboard's live handle while the clipboard stays open.
        unsafe { GlobalSize(block) };
    if block_bytes == 0 {
        // When: block_bytes is 0, GlobalSize could not size the handle.
        return TypeRead::Failed;
    }
    if block_bytes > cap_bytes {
        // When: block_bytes passes cap_bytes, the block is refused before it is locked.
        return TypeRead::TooLarge;
    }
    let base =
        // SAFETY: block is a live moveable allocation, so GlobalLock returns its base or null.
        unsafe { GlobalLock(block) }.cast::<u8>();
    if base.is_null() {
        // When: base is null, GlobalLock failed and there is nothing to read.
        return TypeRead::Failed;
    }
    let bytes =
        // SAFETY: base points at block_bytes locked bytes until GlobalUnlock.
        unsafe { std::slice::from_raw_parts(base, block_bytes) };
    let result = decode(bytes);
    let _ =
        // SAFETY: block was locked above; a final unlock reports an error with no lock left.
        unsafe { GlobalUnlock(block) };
    result
}
