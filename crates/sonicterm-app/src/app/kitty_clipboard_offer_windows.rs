//! The portable half of the Windows offer backend: the native block each representation becomes,
//! the per-process cap on registered custom names, and one clipboard session that sets every
//! staged block or empties the clipboard again. The Win32 calls live in
//! `kitty_clipboard_offer_win32.rs`; tests substitute a fake [`Win32Clipboard`].

use std::sync::Mutex;

use super::{CommitGate, NativeFormat, OfferBackend, Published};
use crate::app::clipboard_worker::Snapshot;
use crate::app::osc52_read_source::{bounded_utf16, utf16_block_admits, ReadOutcome, TypeRead};

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
        NativeFormat::UriList => super::uri::items(bytes).is_some_and(|mut items| {
            items.all(|uri| super::uri::windows_path(uri, |_| {}).is_some())
        }),
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
        NativeFormat::UriList => byte_len
            .saturating_mul(3)
            .saturating_add(DROPFILES_BYTES + 2)
            .saturating_add((byte_len / 9).min(super::uri::ITEM_CAP) * size_of::<String>()),
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

/// Whether this registered format is advertised under the requested custom MIME name.
pub(super) fn registered_mime_matches(registered: &str, mime: &str) -> bool {
    registered.eq_ignore_ascii_case(mime)
        || crate::app::osc52_read_source::native_type_mime(registered)
            .is_some_and(|listed| listed.eq_ignore_ascii_case(mime))
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
    let data = &block[list_at..];
    // First validate and measure directly from the locked UTF-16LE bytes; then allocate only
    // the exact output. Neither pass builds path, Unicode, or escaped-URI scratch buffers.
    let walk = |emit: &mut dyn FnMut(&[u8]) -> Result<(), TypeRead>| -> Result<(), TypeRead> {
        let mut start = 0;
        let mut count = 0;
        for (index, pair) in data.as_chunks::<2>().0.iter().enumerate() {
            if *pair != [0, 0] {
                // When: pair is nonzero, it still belongs to the current UTF-16 path.
                continue;
            }
            let end = index * 2;
            if start == end {
                // When: start equals end, this empty path is the list terminator.
                return (count > 0).then_some(()).ok_or(TypeRead::Absent);
            }
            count += 1;
            if count > super::uri::ITEM_CAP {
                // When: count exceeds ITEM_CAP, omit the entire list, never its tail.
                return Err(TypeRead::TooLarge);
            }
            let units = data[start..end].as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes);
            emit_file_uri(char::decode_utf16(units).map(Result::ok), emit)?;
            emit(b"\r\n")?;
            start = end + 2;
        }
        Err(TypeRead::Absent)
    };
    let mut length = 0;
    if let Err(refusal) = walk(&mut |part| {
        if part.len() > max_bytes - length {
            // When: part cannot fit within max_bytes, refuse before allocating any URI buffer.
            return Err(TypeRead::TooLarge);
        }
        length += part.len();
        Ok(())
    }) {
        // When: walk reports refusal, measurement did not admit an entire list.
        return refusal;
    }
    let mut out = Vec::with_capacity(length);
    match walk(&mut |part| {
        out.extend_from_slice(part);
        Ok(())
    }) {
        Ok(()) => TypeRead::Bytes(out),
        Err(refusal) => refusal,
    }
}

/// The Windows path of every item of a `text/uri-list`; `None` unless it has an item and every
/// item is a `file:` URI with a Windows path, so a list is never published in part.
pub(super) fn uri_list_windows_paths(bytes: &[u8]) -> Option<Vec<String>> {
    let items = super::uri::items(bytes)?;
    let mut paths = Vec::with_capacity(items.clone().count());
    for item in items {
        paths.push(file_uri_windows_path(item)?);
    }
    Some(paths)
}

/// Decode one validated absolute path into a single pre-sized buffer during staging only.
pub(super) fn file_uri_windows_path(uri: &str) -> Option<String> {
    let mut path = String::with_capacity(uri.len());
    super::uri::windows_path(uri, |scalar| path.push(scalar))?;
    Some(path)
}

/// Emit one absolute drive or UNC URI without materializing either a path or an escaped string.
fn emit_file_uri(
    mut chars: impl Iterator<Item = Option<char>> + Clone,
    emit: &mut dyn FnMut(&[u8]) -> Result<(), TypeRead>,
) -> Result<(), TypeRead> {
    let mut head = chars.clone();
    let pair = (head.next().flatten(), head.next().flatten());
    match pair {
        (Some('\\'), Some('\\')) => {
            // When: pair is the UNC prefix, validate the host and share before emitting its URI.
            chars = head.clone();
            let mut host_len = 0;
            let mut dots = true;
            loop {
                let scalar = head.next().flatten().ok_or(TypeRead::Absent)?;
                if scalar == '/' || scalar == '\\' {
                    // When: scalar separates the host from its share, end authority validation.
                    break;
                }
                if !(scalar.is_alphanumeric() || matches!(scalar, '.' | '-' | '_')) {
                    // When: scalar cannot belong to a host, do not emit an invalid authority.
                    return Err(TypeRead::Absent);
                }
                host_len += 1;
                dots &= scalar == '.';
            }
            if host_len == 0
                || (dots && host_len <= 2)
                || head.next().flatten().is_none_or(|scalar| matches!(scalar, '/' | '\\'))
            {
                // When: host_len or the share is invalid, reject the whole file list.
                return Err(TypeRead::Absent);
            }
            emit(b"file://")?;
        }
        (Some(drive), Some(':'))
            if drive.is_ascii_alphabetic() && matches!(head.next(), Some(Some('/' | '\\'))) =>
        {
            emit(b"file:///")?
        }
        _ => {
            // When: pair starts neither a UNC nor an absolute drive path, no file URI can represent it.
            return Err(TypeRead::Absent);
        }
    }
    const HEX: &[u8] = b"0123456789ABCDEF";
    for scalar in chars {
        let scalar = scalar.filter(|scalar| !scalar.is_control()).ok_or(TypeRead::Absent)?;
        for byte in scalar.encode_utf8(&mut [0; 4]).bytes() {
            match byte {
                b'\\' => emit(b"/")?,
                byte if byte.is_ascii_alphanumeric() || b"-._~/:!$&'()*+,;=@".contains(&byte) => {
                    emit(&[byte])?
                }
                byte => emit(&[b'%', HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 15)]])?,
            }
        }
    }
    Ok(())
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
    /// Mutation window token; dropping it after close destroys the window on its creating thread.
    type Owner;
    /// Open for mutation with a fresh owner window, never with a null owner.
    fn open_owned(&mut self) -> Option<Self::Owner>;
    /// Empty the open clipboard, taking ownership of it.
    fn empty(&mut self) -> bool;
    /// Hand `block` to the open clipboard as `format`; the block comes back when it is refused.
    fn set(&mut self, format: u32, block: Self::Block) -> Result<(), Self::Block>;
    /// Close the clipboard this thread opened.
    fn close(&mut self);
    /// Find a registered name among the open clipboard's bounded enumeration, without registering one.
    fn registered(&self, matches: impl Fn(&str) -> bool) -> Option<u32>;
    /// Whether this standard format can be read, including formats synthesized by Windows.
    fn available(&self, id: u32) -> bool;
    /// Read a block only when its native size fits the cap; decode the borrowed bytes while locked.
    fn read_block(
        &mut self,
        id: u32,
        cap_bytes: usize,
        decode: impl FnOnce(&[u8]) -> TypeRead,
    ) -> TypeRead;
    /// The clipboard's change counter, which every change to its contents advances.
    fn sequence(&mut self) -> u32;
    /// The `GlobalSize` of the open clipboard's `CF_UNICODETEXT` block, read without locking it
    /// (0 when the size is unknown); `None` when the clipboard holds no text block.
    fn text_block_bytes(&mut self) -> Option<usize>;
    /// Lock the open clipboard's text block, whose size was admitted as `block_bytes`, and run
    /// `read` over its units before the terminator; nothing runs when the block is gone, has
    /// another size, or cannot be locked.
    fn lock_text(&mut self, block_bytes: usize, read: &mut dyn FnMut(&[u16]));
}

/// Run `read` over the open clipboard's text block size and units, only when `admit` accepts the
/// size `GlobalSize` reported before the block is locked; nothing runs otherwise.
fn with_text<C: Win32Clipboard>(
    clipboard: &mut C,
    admit: impl Fn(usize) -> bool,
    read: &mut dyn FnMut(usize, &[u16]),
) {
    let Some(block_bytes) = clipboard.text_block_bytes() else {
        // When: text_block_bytes is None, the clipboard holds no text block.
        return;
    };
    if block_bytes != 0 && admit(block_bytes) {
        clipboard.lock_text(block_bytes, &mut |units| read(block_bytes, units));
    }
}

/// The text the open clipboard holds as a [`Snapshot`], when its block is at most `max_bytes`.
fn read_snapshot<C: Win32Clipboard>(clipboard: &mut C, max_bytes: usize) -> Option<Snapshot> {
    let mut snapshot = None;
    with_text(clipboard, |block_bytes| block_bytes <= max_bytes, &mut |block_bytes, units| {
        snapshot = Some(Snapshot { block_bytes, units: units.to_vec() });
    });
    snapshot
}

/// Whether the open clipboard holds exactly `previous`: the same block size, checked before the
/// block is locked, then the same units.
fn holds_snapshot<C: Win32Clipboard>(clipboard: &mut C, previous: &Snapshot) -> bool {
    let mut same = false;
    let admit = |block_bytes: usize| block_bytes == previous.block_bytes;
    with_text(clipboard, admit, &mut |_, units| same = units == previous.units.as_slice());
    same
}

/// The clipboard's `CF_UNICODETEXT`, or its literal text/plain, within `max_bytes` of UTF-8 in one
/// acquisition. The block's `GlobalSize` decides `TooLarge` before it is locked, so an oversized
/// block is never locked or copied; conversion is measured before it allocates. Delayed rendering
/// runs inside `GetClipboardData`, which the reader's deadline bounds for the caller.
pub(super) fn read_text_with<C: Win32Clipboard>(
    clipboard: &mut C,
    max_bytes: usize,
) -> ReadOutcome {
    if !clipboard.open() {
        // When: open gives up, another process held the clipboard throughout.
        return ReadOutcome::Failed;
    }
    let outcome = if clipboard.available(CF_UNICODETEXT) {
        read_open_text(clipboard, max_bytes)
    } else {
        // When: CF_UNICODETEXT is unavailable, check the raw and decoded sizes of literal text/plain.
        match read_registered(clipboard, "text/plain", max_bytes) {
            TypeRead::Bytes(bytes) => crate::app::osc52_read_source::bounded_utf8(bytes, max_bytes),
            TypeRead::Absent => ReadOutcome::Empty,
            TypeRead::TooLarge => ReadOutcome::TooLarge,
            TypeRead::Failed | TypeRead::Unsupported => ReadOutcome::Failed,
        }
    };
    clipboard.close();
    outcome
}

/// [`read_text_with`] on a clipboard this thread already opened.
fn read_open_text<C: Win32Clipboard>(clipboard: &mut C, max_bytes: usize) -> ReadOutcome {
    let Some(block_bytes) = clipboard.text_block_bytes() else {
        // When: text_block_bytes is None, the clipboard offers no CF_UNICODETEXT: no text.
        return ReadOutcome::Empty;
    };
    if block_bytes == 0 {
        // When: block_bytes is 0, GlobalSize could not size the handle, so nothing is readable.
        return ReadOutcome::Failed;
    }
    if !utf16_block_admits(block_bytes, max_bytes) {
        // When: utf16_block_admits refuses block_bytes, the block is never locked.
        return ReadOutcome::TooLarge;
    }
    // A block that cannot be locked, or changed size, leaves the read failed.
    let mut outcome = ReadOutcome::Failed;
    clipboard.lock_text(block_bytes, &mut |units| outcome = bounded_utf16(units, max_bytes));
    outcome
}

/// Read a listed MIME in one acquisition. Prefer its Windows encoding; a literal registration
/// is already MIME data and must not be interpreted as CF_HTML, DROPFILES or UTF-16.
pub(super) fn read_type_with<C: Win32Clipboard>(
    clipboard: &mut C,
    mime: &str,
    max_bytes: usize,
) -> TypeRead {
    let Some(format) = super::native_format(mime) else {
        // When: native_format rejects mime, an invalid request cannot name a clipboard format.
        return TypeRead::Absent;
    };
    if !clipboard.open() {
        // When: open fails, no stable format can be selected or read.
        return TypeRead::Failed;
    }
    let result = read_open_type(clipboard, &format, mime, max_bytes);
    clipboard.close();
    result
}

fn read_open_type<C: Win32Clipboard>(
    clipboard: &mut C,
    format: &NativeFormat,
    mime: &str,
    max_bytes: usize,
) -> TypeRead {
    let raw = |bytes: &[u8]| TypeRead::Bytes(bytes.to_vec());
    match format {
        NativeFormat::Text if clipboard.available(CF_UNICODETEXT) => {
            // When: CF_UNICODETEXT is available, its bounded conversion wins even if it returns no bytes.
            return read_open_text(clipboard, max_bytes).into();
        }
        NativeFormat::UriList if clipboard.available(CF_HDROP) => {
            // When: CF_HDROP exists, measure its UTF-16 paths before creating any URI output.
            return clipboard.read_block(
                CF_HDROP,
                max_bytes.saturating_mul(2).saturating_add(64),
                |bytes| hdrop_uri_list(bytes, max_bytes),
            );
        }
        NativeFormat::Html | NativeFormat::Png => {
            // When: format is Html or Png, prefer the Windows registration over any MIME alias.
            let html = *format == NativeFormat::Html;
            let preferred = if html { HTML_FORMAT_NAME } else { PNG_FORMAT_NAME };
            if let Some(id) = clipboard.registered(|name| name.eq_ignore_ascii_case(preferred)) {
                // When: registered returns the preferred ID, finish this read without probing other formats.
                return clipboard.read_block(id, max_bytes, |bytes| {
                    if html {
                        html_fragment(bytes).map_or(TypeRead::Absent, raw)
                    } else {
                        // When: html is false, PNG has no Windows envelope to strip.
                        raw(bytes)
                    }
                });
            }
        }
        _ => {
            // When: format is custom or its standard ID is unavailable, look up the advertised MIME below.
        }
    }
    let requested = if *format == NativeFormat::Text { "text/plain" } else { mime };
    read_registered(clipboard, requested, max_bytes)
}

/// A registered MIME block already contains the representation. Limit its raw size before locking.
fn read_registered(
    clipboard: &mut impl Win32Clipboard,
    requested: &str,
    max_bytes: usize,
) -> TypeRead {
    match clipboard.registered(|name| registered_mime_matches(name, requested)) {
        Some(id) => clipboard.read_block(id, max_bytes, |bytes| TypeRead::Bytes(bytes.to_vec())),
        None => TypeRead::Absent,
    }
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

impl<C: Win32Clipboard> WindowsOffer<C> {
    /// One clipboard acquisition: open, run `check` on the open clipboard, then empty it and set
    /// every staged block, then close. `check` holds the comparison and the commit gate, so no
    /// other process can change the clipboard between them and the replacement; a refusal closes
    /// it untouched.
    fn commit(&mut self, check: &mut dyn FnMut(&mut C) -> bool) -> Published {
        let Some(_owner) = self.clipboard.open_owned() else {
            // When: open_owned fails, no mutation is possible and the previous clipboard stays intact.
            self.discard();
            return Published::Failed;
        };
        if !check(&mut self.clipboard) {
            // When: check refuses on the open clipboard, it closes untouched.
            self.clipboard.close();
            self.discard();
            return Published::Unchanged;
        }
        if !self.clipboard.empty() {
            // When: empty fails, the clipboard was not taken, so the previous value stays.
            self.clipboard.close();
            self.discard();
            return Published::Failed;
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
        if published {
            Published::Written
        } else {
            // When: published is false, a format was refused and the clipboard was emptied.
            Published::Failed
        }
    }

    /// Replace the clipboard with `text` (an empty text clears it) in one acquisition: with
    /// `snapshot_max_bytes`, the value it replaces is read first, and `gate` is checked after that
    /// read, immediately before `EmptyClipboard`. Returns what was replaced when it was read.
    pub(super) fn write_text(
        &mut self,
        text: &str,
        snapshot_max_bytes: Option<usize>,
        gate: CommitGate<'_>,
    ) -> (Published, Option<Snapshot>) {
        if !text.is_empty() && !self.stage(&NativeFormat::Text, text.as_bytes()) {
            // When: the text block cannot be staged, the clipboard is never opened.
            return (Published::Failed, None);
        }
        let mut replaced = None;
        let published = self.commit(&mut |clipboard| {
            replaced = snapshot_max_bytes.and_then(|max_bytes| read_snapshot(clipboard, max_bytes));
            gate()
        });
        (published, replaced)
    }

    /// Write `text` again only while the clipboard holds exactly `previous` and its change counter
    /// stays put across that comparison, compared and replaced in one acquisition; `gate` is
    /// checked after the comparison, immediately before `EmptyClipboard`.
    pub(super) fn reassert_text(
        &mut self,
        text: &str,
        previous: &Snapshot,
        gate: CommitGate<'_>,
    ) -> Published {
        if !self.stage(&NativeFormat::Text, text.as_bytes()) {
            // When: the text block cannot be staged, the clipboard is never opened.
            return Published::Failed;
        }
        self.commit(&mut |clipboard| {
            let before = clipboard.sequence();
            // A delayed-rendering owner may still change the data while it is read, so the
            // counter must not move across the comparison either.
            holds_snapshot(clipboard, previous) && clipboard.sequence() == before && gate()
        })
    }
}

impl<C: Win32Clipboard> OfferBackend for WindowsOffer<C> {
    fn stage(&mut self, format: &NativeFormat, bytes: &[u8]) -> bool {
        let Some(id) = self.format_id(format) else {
            // When: format_id failed, the complete offer cannot be staged.
            return false;
        };
        if self.staged.iter().any(|(known, _)| *known == id) {
            // When: staged already contains id, the first spelling owns the native representation.
            return true;
        }
        let Some(data) = encode(format, bytes) else {
            // When: encode fails, no partial set of formats may be published.
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

    fn publish(&mut self, gate: CommitGate<'_>) -> Published {
        self.commit(&mut |_| gate())
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
