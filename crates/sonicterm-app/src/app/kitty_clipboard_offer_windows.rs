//! The portable half of the Windows offer backend: the native block each representation becomes,
//! the per-process cap on registered custom names, and one clipboard session that sets every
//! staged block or empties the clipboard again. The Win32 calls live in
//! `kitty_clipboard_offer_win32.rs`; tests substitute a fake [`Win32Clipboard`].

use std::fmt::Write as _;
use std::sync::Mutex;

use super::{NativeFormat, OfferBackend};
use crate::app::osc52_read_source::TypeRead;

/// The standard clipboard format for NUL-terminated UTF-16 text.
pub(super) const CF_UNICODETEXT: u32 = 13;

/// The standard clipboard format for a DROPFILES file list.
pub(super) const CF_HDROP: u32 = 15;

/// The registered name of the CF_HTML format.
pub(super) const HTML_FORMAT_NAME: &str = "HTML Format";

/// The registered name of the PNG format other programs read.
pub(super) const PNG_FORMAT_NAME: &str = "PNG";

/// Distinct custom MIME names one process registers: registered names are session-global and
/// never freed, so a program cannot exhaust the session's format table through SonicTerm.
pub(super) const MAX_CUSTOM_FORMAT_NAMES: usize = 128;

/// A DROPFILES header: `pFiles`, `pt` (two `i32`), `fNC` and `fWide`, four bytes each.
const DROPFILES_BYTES: usize = 20;

/// What CF_HTML puts before the fragment, after the header.
const HTML_PREFIX: &[u8] = b"<html>\r\n<body>\r\n<!--StartFragment-->";

/// What CF_HTML puts after the fragment, before the terminating NUL.
const HTML_SUFFIX: &[u8] = b"<!--EndFragment-->\r\n</body>\r\n</html>";

/// The CF_HTML header's length: five lines whose offsets are ten digits each.
const HTML_HEADER_BYTES: usize = 105;

/// The custom MIME names admitted for registration, at most [`MAX_CUSTOM_FORMAT_NAMES`].
pub(super) struct CustomNames(Mutex<Vec<String>>);

impl CustomNames {
    /// No name admitted yet.
    pub(super) const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    /// Whether `name` may be registered: a known name (compared as Windows compares names)
    /// always, a new one while fewer than [`MAX_CUSTOM_FORMAT_NAMES`] are known.
    pub(super) fn admit(&self, name: &str) -> bool {
        // A poisoned list still holds whole names, so it is used as it is.
        let mut names = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            // When: names already holds name, registering it again takes no new entry.
            return true;
        }
        if names.len() >= MAX_CUSTOM_FORMAT_NAMES {
            // When: names is full, a new name is omitted at preflight.
            return false;
        }
        names.push(name.to_owned());
        true
    }
}

/// The custom names this process admitted, shared by every pane.
#[cfg(target_os = "windows")]
static PROCESS_NAMES: CustomNames = CustomNames::new();

/// Whether the Windows clipboard can hold `format` with `bytes`, under the process's name cap.
#[cfg(target_os = "windows")]
pub(super) fn holds(format: &NativeFormat, bytes: &[u8]) -> bool {
    holds_with(&PROCESS_NAMES, format, bytes)
}

/// Whether Windows can hold `format` with `bytes`: text and HTML always, PNG and a custom type
/// admitted by `names` when nonempty (a zero-byte global block cannot be locked), and
/// `text/uri-list` when every item is a `file:` URI with a Windows path.
pub(super) fn holds_with(names: &CustomNames, format: &NativeFormat, bytes: &[u8]) -> bool {
    match format {
        NativeFormat::Text | NativeFormat::Html => true,
        NativeFormat::Png => !bytes.is_empty(),
        NativeFormat::UriList => uri_list_windows_paths(bytes).is_some(),
        NativeFormat::Custom(name) => !bytes.is_empty() && names.admit(name),
    }
}

/// The most bytes staging `format` from `byte_len` bytes takes: its global block (UTF-16 text is
/// at most two bytes per UTF-8 byte; a path's UTF-16 at most two per URI byte) plus transient
/// copies (the CF_HTML header, decoded paths).
pub(super) fn copy_bound(format: &NativeFormat, byte_len: usize) -> usize {
    match format {
        NativeFormat::Text => byte_len.saturating_mul(2).saturating_add(2),
        NativeFormat::Html => byte_len
            .saturating_add(2 * HTML_HEADER_BYTES + HTML_PREFIX.len() + HTML_SUFFIX.len() + 1),
        NativeFormat::Png | NativeFormat::Custom(_) => byte_len,
        NativeFormat::UriList => byte_len.saturating_mul(4).saturating_add(DROPFILES_BYTES + 2),
    }
}

/// The native block one representation becomes.
pub(super) enum NativeData<'bytes> {
    /// `CF_UNICODETEXT`: UTF-16 and a terminator.
    Text(&'bytes [u8]),
    /// CF_HTML: the header and this fragment wrapped in a document.
    Html(&'bytes [u8]),
    /// `PNG` and custom formats: the bytes as staged.
    Raw(&'bytes [u8]),
    /// `CF_HDROP`: a DROPFILES list of these paths.
    Files(Vec<String>),
}

/// The block `format` gives `bytes`; `None` for a uri-list whose items are not all Windows paths.
pub(super) fn encode<'bytes>(
    format: &NativeFormat,
    bytes: &'bytes [u8],
) -> Option<NativeData<'bytes>> {
    Some(match format {
        NativeFormat::Text => NativeData::Text(bytes),
        NativeFormat::Html => NativeData::Html(bytes),
        NativeFormat::Png | NativeFormat::Custom(_) => NativeData::Raw(bytes),
        NativeFormat::UriList => NativeData::Files(uri_list_windows_paths(bytes)?),
    })
}

/// `bytes` as UTF-16 units, each invalid UTF-8 sequence as one U+FFFD, as `from_utf8_lossy` reads.
fn lossy_utf16(bytes: &[u8]) -> impl Iterator<Item = u16> + '_ {
    bytes.utf8_chunks().flat_map(|chunk| {
        let replacement = (!chunk.invalid().is_empty()).then_some(0xFFFD_u16);
        chunk.valid().encode_utf16().chain(replacement)
    })
}

/// The CF_HTML header for a fragment of `fragment_len` bytes: every offset counts bytes from the
/// block's start, and each is ten digits, so the header is always [`HTML_HEADER_BYTES`] long.
fn html_header(fragment_len: usize) -> String {
    let start_fragment = HTML_HEADER_BYTES + HTML_PREFIX.len();
    let end_fragment = start_fragment + fragment_len;
    let end_html = end_fragment + HTML_SUFFIX.len();
    format!(
        "Version:0.9\r\nStartHTML:{HTML_HEADER_BYTES:010}\r\nEndHTML:{end_html:010}\r\n\
         StartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    )
}

impl NativeData<'_> {
    /// The block's exact size in bytes.
    pub(super) fn byte_len(&self) -> usize {
        match self {
            Self::Text(bytes) => lossy_utf16(bytes).count() * 2 + 2,
            Self::Html(fragment) => {
                HTML_HEADER_BYTES + HTML_PREFIX.len() + fragment.len() + HTML_SUFFIX.len() + 1
            }
            Self::Raw(bytes) => bytes.len(),
            Self::Files(paths) => {
                let units: usize = paths.iter().map(|path| path.encode_utf16().count() + 1).sum();
                DROPFILES_BYTES + 2 * units + 2
            }
        }
    }

    /// Write the block into `out`, which is exactly [`Self::byte_len`] bytes.
    pub(super) fn fill(&self, out: &mut [u8]) {
        match self {
            Self::Text(bytes) => fill_units(out, lossy_utf16(bytes).chain([0])),
            Self::Html(fragment) => {
                let header = html_header(fragment.len());
                let mut offset = 0;
                for part in [header.as_bytes(), HTML_PREFIX, fragment, HTML_SUFFIX, &[0]] {
                    out[offset..offset + part.len()].copy_from_slice(part);
                    offset += part.len();
                }
            }
            Self::Raw(bytes) => out.copy_from_slice(bytes),
            Self::Files(paths) => {
                let header = u32::try_from(DROPFILES_BYTES).expect("a small header");
                out[..4].copy_from_slice(&header.to_le_bytes());
                out[4..16].fill(0);
                // fWide: the paths are UTF-16.
                out[16..20].copy_from_slice(&1u32.to_le_bytes());
                let units = paths.iter().flat_map(|path| path.encode_utf16().chain([0]));
                fill_units(&mut out[DROPFILES_BYTES..], units.chain([0]));
            }
        }
    }
}

/// Write `units` into `out` as UTF-16LE.
fn fill_units(out: &mut [u8], units: impl Iterator<Item = u16>) {
    for (slot, unit) in out.as_chunks_mut::<2>().0.iter_mut().zip(units) {
        *slot = unit.to_le_bytes();
    }
}

/// The fragment a CF_HTML `block` marks with `StartFragment`/`EndFragment`; `None` when either
/// offset is missing or outside the block. Only the header, before the first `<`, is parsed.
pub(super) fn html_fragment(block: &[u8]) -> Option<&[u8]> {
    let header_end = block.iter().position(|byte| *byte == b'<').unwrap_or(block.len());
    let header = std::str::from_utf8(&block[..header_end]).ok()?;
    let offset = |key: &str| -> Option<usize> {
        header.lines().find_map(|line| line.strip_prefix(key))?.trim().parse().ok()
    };
    let (start, end) = (offset("StartFragment:")?, offset("EndFragment:")?);
    (start <= end && end <= block.len()).then(|| &block[start..end])
}

/// A DROPFILES `block` as a `text/uri-list` of `file:` URIs, CRLF after each, within `max_bytes`;
/// `Absent` when the header is malformed, the list is not UTF-16 or no path has a `file:` URI.
pub(super) fn hdrop_uri_list(block: &[u8], max_bytes: usize) -> TypeRead {
    let field = |at: usize| {
        block.get(at..at + 4).map(|bytes| u32::from_le_bytes(bytes.try_into().expect("4 bytes")))
    };
    let (Some(list_at), Some(wide)) = (field(0), field(16)) else {
        // When: field(0) or field(16) is None, block is shorter than a DROPFILES header.
        return TypeRead::Absent;
    };
    let list_at = usize::try_from(list_at).unwrap_or(usize::MAX);
    if wide == 0 || list_at < DROPFILES_BYTES || list_at > block.len() {
        // When: the list is ANSI or pFiles points outside the block, it is not decoded.
        return TypeRead::Absent;
    }
    let mut out = Vec::new();
    let mut path: Vec<u16> = Vec::new();
    for pair in block[list_at..].as_chunks::<2>().0 {
        let unit = u16::from_le_bytes(*pair);
        if unit != 0 {
            // When: unit is not 0, it extends the current path.
            path.push(unit);
            continue;
        }
        if path.is_empty() {
            // When: path is empty at a NUL, the list's final NUL ended it.
            break;
        }
        let uri = String::from_utf16(&path).ok().and_then(|path| windows_path_file_uri(&path));
        path.clear();
        let Some(uri) = uri else {
            // When: uri is None, the path is not UTF-16 or is relative, so it is skipped.
            continue;
        };
        if out.len() + uri.len() + 2 > max_bytes {
            // When: the URIs pass max_bytes, the list is refused before it grows further.
            return TypeRead::TooLarge;
        }
        out.extend_from_slice(uri.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if out.is_empty() {
        TypeRead::Absent
    } else {
        // When: out holds at least one URI, the list is served.
        TypeRead::Bytes(out)
    }
}

/// The Windows path of every item of a `text/uri-list`; `None` unless it has an item and every
/// item is a `file:` URI with a Windows path, so a list is never published in part.
pub(super) fn uri_list_windows_paths(bytes: &[u8]) -> Option<Vec<String>> {
    let items = super::uri_list_items(bytes)?;
    let paths: Option<Vec<String>> = items.into_iter().map(file_uri_windows_path).collect();
    paths.filter(|paths| !paths.is_empty())
}

/// Whether `path` starts with a drive letter and a colon.
fn has_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// The Windows path a `file:` URI names: a drive path for an empty or `localhost` host, a UNC path
/// (`\\host\share\…`) otherwise; percent-decoded to UTF-8 without control characters. `None` for
/// any other scheme, a query or fragment, or a path that is neither.
pub(super) fn file_uri_windows_path(uri: &str) -> Option<String> {
    let rest = uri.get(..7).filter(|scheme| scheme.eq_ignore_ascii_case("file://"))?;
    let rest = &uri[rest.len()..];
    if rest.contains(['?', '#']) {
        // When: rest contains a query or fragment mark, it names no plain file.
        return None;
    }
    let (host, path) = rest.split_once('/')?;
    let decoded = percent_decode(path)?;
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        // When: host is empty or localhost, the path must start with a drive.
        let drive =
            has_drive(&decoded) && decoded.as_bytes().get(2).is_none_or(|byte| *byte == b'/');
        return drive.then(|| decoded.replace('/', "\\"));
    }
    if host.contains(':') || decoded.is_empty() {
        // When: host holds a colon (`file://C:/…`) or no share follows it, it is no UNC path.
        return None;
    }
    let host = percent_decode(host)?;
    Some(format!("\\\\{host}\\{}", decoded.replace('/', "\\")))
}

/// `text` with every `%XX` escape decoded; `None` for a bad escape, invalid UTF-8 or a control.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte != b'%' {
            // When: byte is not an escape's %, it is copied as it is.
            bytes.push(byte);
            rest = tail;
            continue;
        }
        let high = char::from(*tail.first()?).to_digit(16)?;
        let low = char::from(*tail.get(1)?).to_digit(16)?;
        bytes.push(u8::try_from(high * 16 + low).ok()?);
        rest = &tail[2..];
    }
    let decoded = String::from_utf8(bytes).ok()?;
    (!decoded.chars().any(char::is_control)).then_some(decoded)
}

/// The `file:` URI for a Windows drive or UNC `path`, percent-encoding every byte outside the
/// unreserved and path characters; `None` for a relative path.
pub(super) fn windows_path_file_uri(path: &str) -> Option<String> {
    let (prefix, rest) = match path.strip_prefix("\\\\") {
        Some(unc) => ("file://", unc),
        None if has_drive(path) => ("file:///", path),
        None => {
            // When: path has neither a UNC prefix nor a drive, it is relative and has no URI.
            return None;
        }
    };
    let mut uri = String::with_capacity(prefix.len() + rest.len());
    uri.push_str(prefix);
    for byte in rest.bytes() {
        match byte {
            b'\\' => uri.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => uri.push(char::from(byte)),
            b'-' | b'.' | b'_' | b'~' | b'/' | b':' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')'
            | b'*' | b'+' | b',' | b';' | b'=' | b'@' => uri.push(char::from(byte)),
            _ => {
                // When: byte is outside the unreserved and path characters, it is escaped;
                // writing into a String cannot fail.
                let _ = write!(uri, "%{byte:02X}");
            }
        }
    }
    Some(uri)
}

/// The Win32 calls one publish makes; tests substitute a fake.
pub(super) trait Win32Clipboard {
    /// A global block SonicTerm owns until `set` hands it to the clipboard.
    type Block;
    /// The format id registered under `name`; `None` when registration failed.
    fn register(&mut self, name: &str) -> Option<u32>;
    /// A moveable block of `byte_len` bytes that `fill` wrote; `None` when it cannot be allocated.
    fn alloc(&mut self, byte_len: usize, fill: &dyn Fn(&mut [u8])) -> Option<Self::Block>;
    /// Free a block SonicTerm still owns.
    fn free(&mut self, block: Self::Block);
    /// Open the clipboard on this thread, retrying while another process holds it.
    fn open(&mut self) -> bool;
    /// Empty the open clipboard, taking ownership of it.
    fn empty(&mut self) -> bool;
    /// Hand `block` to the open clipboard as `format`; the block comes back when it is refused.
    fn set(&mut self, format: u32, block: Self::Block) -> Result<(), Self::Block>;
    /// Close the clipboard this thread opened.
    fn close(&mut self);
}

/// One Windows publish: every representation staged as a filled global block before the
/// clipboard opens, then set in one `OpenClipboard` session.
pub(super) struct WindowsOffer<C: Win32Clipboard> {
    pub(super) clipboard: C,
    /// Each staged block and its format id, in the commit's order.
    pub(super) staged: Vec<(u32, C::Block)>,
}

impl<C: Win32Clipboard> WindowsOffer<C> {
    /// Stage on `clipboard`.
    pub(super) fn new(clipboard: C) -> Self {
        Self { clipboard, staged: Vec::new() }
    }

    /// The clipboard format `format` is set as: a standard id, else the id its name registers.
    fn format_id(&mut self, format: &NativeFormat) -> Option<u32> {
        match format {
            NativeFormat::Text => Some(CF_UNICODETEXT),
            NativeFormat::UriList => Some(CF_HDROP),
            NativeFormat::Html => self.clipboard.register(HTML_FORMAT_NAME),
            NativeFormat::Png => self.clipboard.register(PNG_FORMAT_NAME),
            NativeFormat::Custom(name) => self.clipboard.register(name),
        }
    }
}

impl<C: Win32Clipboard> OfferBackend for WindowsOffer<C> {
    fn stage(&mut self, format: &NativeFormat, bytes: &[u8]) -> bool {
        let (Some(id), Some(data)) = (self.format_id(format), encode(format, bytes)) else {
            // When: the format cannot be registered or encoded, the offer cannot be staged.
            return false;
        };
        let fill = |out: &mut [u8]| data.fill(out);
        let Some(block) = self.clipboard.alloc(data.byte_len(), &fill) else {
            // When: alloc fails, the offer cannot be staged.
            return false;
        };
        self.staged.push((id, block));
        true
    }

    fn publish(&mut self) -> bool {
        if !self.clipboard.open() {
            // When: open gives up, another process held the clipboard: nothing changed.
            self.discard();
            return false;
        }
        if !self.clipboard.empty() {
            // When: empty fails, the clipboard was not taken, so the previous value stays.
            self.clipboard.close();
            self.discard();
            return false;
        }
        let mut published = true;
        for (id, block) in std::mem::take(&mut self.staged) {
            if !published {
                // When: published is false, an earlier format was refused, so this one is freed.
                self.clipboard.free(block);
                continue;
            }
            if let Err(block) = self.clipboard.set(id, block) {
                // SetClipboardData refused the block, so SonicTerm still owns it and frees it.
                self.clipboard.free(block);
                published = false;
            }
        }
        if !published {
            // When: published is false, the clipboard is emptied again before it closes, so no
            // program sees a partial offer; a failure there leaves nothing to undo.
            let _ = self.clipboard.empty();
        }
        self.clipboard.close();
        published
    }

    fn discard(&mut self) {
        for (_, block) in std::mem::take(&mut self.staged) {
            self.clipboard.free(block);
        }
    }
}

// Lifecycle: WindowsOffer over clipboard C calls discard on drop, freeing every block still staged.
impl<C: Win32Clipboard> Drop for WindowsOffer<C> {
    fn drop(&mut self) {
        self.discard();
    }
}

#[cfg(test)]
#[path = "kitty_clipboard_offer_windows_tests.rs"]
mod kitty_clipboard_offer_windows_tests;
