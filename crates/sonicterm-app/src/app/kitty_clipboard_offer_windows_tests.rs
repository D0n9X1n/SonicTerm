use super::super::{plan, publish_offer, NativeOffer, OfferPlan, Published};
use super::*;
use std::collections::HashMap;

/// A fake Win32 clipboard: registered names, the blocks SonicTerm still owns, what the clipboard
/// holds, and the calls one publish made; it fails the chosen allocation or `SetClipboardData`.
#[derive(Default)]
struct FakeWin32 {
    registered: Vec<String>,
    live: HashMap<u64, Vec<u8>>,
    next_block: u64,
    clipboard: Vec<(u32, Vec<u8>)>,
    opens: usize,
    owned_opens: usize,
    empties: usize,
    closes: usize,
    is_open: bool,
    refuse_open: bool,
    allocs: usize,
    fail_alloc_at: Option<usize>,
    sets: usize,
    fail_set_at: Option<usize>,
    /// The change counter: every empty, set and external copy advances it.
    sequence: u32,
    /// Advance the counter while the next text block is read, as a delayed-rendering owner does.
    race_read: bool,
    /// Another program's copy that tries to land while the clipboard is open: it waits for
    /// `CloseClipboard`, as `OpenClipboard` makes every other process wait.
    external_copy: Option<Vec<u8>>,
    /// Every clipboard call in order, shared so a test's gate can record itself between them.
    log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    /// The text blocks locked, as `GlobalLock` would.
    locks: usize,
}

struct WindowProbe(std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>);

// Lifecycle: WindowProbe records destruction after the mutating session closes.
impl Drop for WindowProbe {
    fn drop(&mut self) {
        self.0.borrow_mut().push("release window");
    }
}

impl Win32Clipboard for FakeWin32 {
    type Block = u64;

    fn register(&mut self, name: &str) -> Option<u32> {
        let index = match self.registered.iter().position(|known| known.eq_ignore_ascii_case(name))
        {
            Some(index) => index,
            None => {
                self.registered.push(name.to_owned());
                self.registered.len() - 1
            }
        };
        Some(0xC000 + u32::try_from(index).unwrap())
    }

    fn alloc(&mut self, byte_len: usize, fill: &dyn Fn(&mut [u8])) -> Option<u64> {
        let call = self.allocs;
        self.allocs += 1;
        if self.fail_alloc_at == Some(call) {
            return None;
        }
        // A sentinel, so a fill that skips a byte shows.
        let mut bytes = vec![0xAA; byte_len];
        fill(&mut bytes);
        self.next_block += 1;
        self.live.insert(self.next_block, bytes);
        Some(self.next_block)
    }

    fn free(&mut self, block: u64) {
        assert!(self.live.remove(&block).is_some(), "a block is freed once, only while owned");
    }

    fn open(&mut self) -> bool {
        if self.refuse_open {
            return false;
        }
        self.opens += 1;
        self.is_open = true;
        self.log.borrow_mut().push("open");
        true
    }

    type Owner = WindowProbe;
    fn open_owned(&mut self) -> Option<Self::Owner> {
        self.owned_opens += 1;
        self.open().then(|| WindowProbe(self.log.clone()))
    }

    fn empty(&mut self) -> bool {
        assert!(self.is_open, "EmptyClipboard only while open");
        self.empties += 1;
        self.clipboard.clear();
        self.sequence += 1;
        self.log.borrow_mut().push("empty");
        true
    }

    fn set(&mut self, format: u32, block: u64) -> Result<(), u64> {
        assert!(self.is_open, "SetClipboardData only while open");
        let call = self.sets;
        self.sets += 1;
        if self.fail_set_at == Some(call) {
            return Err(block);
        }
        let bytes = self.live.remove(&block).expect("a live block");
        self.clipboard.push((format, bytes));
        self.sequence += 1;
        self.log.borrow_mut().push("set");
        Ok(())
    }

    fn close(&mut self) {
        assert!(self.is_open, "CloseClipboard only while open");
        self.closes += 1;
        self.is_open = false;
        self.log.borrow_mut().push("close");
        if let Some(bytes) = self.external_copy.take() {
            // The other program was waiting for the clipboard; it takes it now.
            self.clipboard = vec![(CF_UNICODETEXT, bytes)];
            self.sequence += 1;
        }
    }

    fn sequence(&mut self) -> u32 {
        self.sequence
    }

    fn text_block_bytes(&mut self) -> Option<usize> {
        assert!(self.is_open, "GetClipboardData only while open");
        self.log.borrow_mut().push("read");
        let block_bytes =
            self.clipboard.iter().find(|(format, _)| *format == CF_UNICODETEXT)?.1.len();
        if std::mem::take(&mut self.race_read) {
            self.sequence += 1;
        }
        Some(block_bytes)
    }

    fn lock_text(&mut self, block_bytes: usize, read: &mut dyn FnMut(&[u16])) {
        assert!(self.is_open, "GlobalLock only while the clipboard is open");
        let Some((_, bytes)) = self.clipboard.iter().find(|(format, _)| *format == CF_UNICODETEXT)
        else {
            return;
        };
        if bytes.len() != block_bytes {
            return;
        }
        self.locks += 1;
        let units: Vec<u16> =
            bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let text_units = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
        read(&units[..text_units]);
    }
}

impl FakeWin32 {
    /// The text the clipboard holds now, decoded from its `CF_UNICODETEXT` block.
    fn text(&self) -> Option<String> {
        let bytes = &self.clipboard.iter().find(|(format, _)| *format == CF_UNICODETEXT)?.1;
        let units: Vec<u16> =
            bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let text_units = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..text_units]))
    }
}

/// A fake whose clipboard holds one previous text.
fn previous() -> FakeWin32 {
    FakeWin32 { clipboard: vec![(CF_UNICODETEXT, b"o\0\0\0".to_vec())], ..FakeWin32::default() }
}

/// The offer `pairs` plan to when every format is held.
fn offer(pairs: &[(&str, &[u8])]) -> NativeOffer {
    let entries = pairs.iter().map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec())).collect();
    match plan(entries, &[], &|_, _| true) {
        OfferPlan::Offer(offer) => offer,
        other => panic!("expected an offer, got {other:?}"),
    }
}

/// `text` as `CF_UNICODETEXT` bytes: UTF-16LE and a terminator.
fn utf16_block(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// The native block `format` gives `bytes`.
fn block(format: &NativeFormat, bytes: &[u8]) -> Vec<u8> {
    let data = encode(format, bytes).expect("an encodable representation");
    let mut out = vec![0xAA; data.byte_len()];
    data.fill(&mut out);
    out
}

/// Contract: when `SetClipboardData` fails for the second format, the open clipboard is emptied
/// again before it closes, every block SonicTerm still owns is freed, and the publish is a
/// failure (`EIO`), never success, so no other process sees a partial offer.
#[test]
fn set_clipboard_data_failure_empties_and_eio_never_done() {
    let offer = offer(&[("text/plain", b"t"), ("text/html", b"<b>h</b>"), ("image/png", b"png")]);
    let mut backend = WindowsOffer::new(FakeWin32 { fail_set_at: Some(1), ..previous() });
    assert_eq!(publish_offer(&mut backend, &offer, &|| true), Published::Failed);
    let fake = &backend.clipboard;
    assert!(fake.clipboard.is_empty(), "the first format was emptied again");
    assert_eq!((fake.opens, fake.empties, fake.closes), (1, 2, 1));
    assert!(fake.live.is_empty(), "every block SonicTerm owned was freed");
    assert!(backend.staged.is_empty());
}

/// Contract: one publish opens the clipboard once, empties it once and sets every staged format
/// in the commit's order: `CF_UNICODETEXT`, the registered `HTML Format` and `PNG`, a format
/// registered under the custom MIME name, and `CF_HDROP`; the clipboard then owns every block.
#[test]
fn windows_publish_every_format_in_one_session() {
    let custom = "application/x-sonicterm-test";
    let offer = offer(&[
        ("text/plain", "h\u{e9}llo".as_bytes()),
        ("text/html", b"<b>h</b>"),
        ("image/png", b"\x89PNG"),
        (custom, b"custom"),
        ("text/uri-list", b"file:///C:/a%20b.txt\r\n"),
    ]);
    let mut backend = WindowsOffer::new(previous());
    assert_eq!(publish_offer(&mut backend, &offer, &|| true), Published::Written);
    let fake = &backend.clipboard;
    assert_eq!((fake.opens, fake.empties, fake.closes), (1, 1, 1));
    assert_eq!(fake.registered, [HTML_FORMAT_NAME, PNG_FORMAT_NAME, custom]);
    let formats: Vec<u32> = fake.clipboard.iter().map(|(format, _)| *format).collect();
    assert_eq!(formats, [CF_UNICODETEXT, 0xC000, 0xC001, 0xC002, CF_HDROP]);
    assert_eq!(fake.clipboard[0].1, utf16_block("h\u{e9}llo"));
    assert_eq!(fake.clipboard[2].1, b"\x89PNG");
    assert!(fake.live.is_empty(), "the clipboard owns every block");
}

/// Contract: a custom MIME type is published under the format `RegisterClipboardFormatW` gives its
/// own name, with its bytes exactly as staged.
#[test]
fn windows_registered_custom_mime_round_trip() {
    let custom = "application/vnd.sonicterm+json";
    let mut backend = WindowsOffer::new(previous());
    let custom_offer = offer(&[(custom, b"{\"a\":1}")]);
    assert_eq!(publish_offer(&mut backend, &custom_offer, &|| true), Published::Written);
    assert_eq!(backend.clipboard.registered, [custom]);
    assert_eq!(backend.clipboard.clipboard, [(0xC000, b"{\"a\":1}".to_vec())]);
}

/// Contract: a staging failure (an allocation) never opens the clipboard: the previous value stays
/// and every block already staged is freed; a clipboard another process holds fails the same way.
#[test]
fn stage_or_open_failure_keeps_previous_clipboard() {
    let offer = offer(&[("text/plain", b"t"), ("text/html", b"h")]);
    let mut backend = WindowsOffer::new(FakeWin32 { fail_alloc_at: Some(1), ..previous() });
    assert_eq!(publish_offer(&mut backend, &offer, &|| true), Published::Failed);
    assert_eq!(backend.clipboard.opens, 0);
    assert!(backend.clipboard.live.is_empty());
    assert_eq!(backend.clipboard.clipboard, previous().clipboard);
    let mut backend = WindowsOffer::new(FakeWin32 { refuse_open: true, ..previous() });
    assert_eq!(publish_offer(&mut backend, &offer, &|| true), Published::Failed);
    assert!(backend.clipboard.live.is_empty());
    assert_eq!(backend.clipboard.clipboard, previous().clipboard);
}

/// Contract: registered names are session-global and never freed, so at most 128 distinct custom
/// names are admitted per process; a later new name is omitted at preflight, a known name (case
/// insensitively, as Windows matches them) stays admitted, and an empty custom payload is omitted.
#[test]
fn registration_and_atom_caps_omit_extra_names() {
    let names = CustomNames::new();
    for index in 0..MAX_CUSTOM_FORMAT_NAMES {
        assert!(names.admit(&format!("application/x-{index}")), "{index}");
    }
    assert!(!names.admit("application/x-extra"), "past the cap");
    assert!(names.admit("APPLICATION/X-0"), "a known name");
    let extra = NativeFormat::Custom("application/x-extra".to_owned());
    assert!(!holds_with(&names, &extra, b"x"));
    let known = NativeFormat::Custom("application/x-1".to_owned());
    assert!(holds_with(&names, &known, b"x"));
    assert!(!holds_with(&names, &known, b""), "an empty custom payload");
    assert!(!holds_with(&names, &NativeFormat::Png, b""), "an empty PNG");
    assert!(holds_with(&names, &NativeFormat::Text, b""), "text, even empty, has a terminator");
}

/// Contract: CF_HTML carries the five-line header with ten-digit byte offsets: `StartHTML` right
/// after the header, `StartFragment`/`EndFragment` exactly around the fragment (counted in bytes,
/// so non-ASCII text shifts them), `EndHTML` before the terminating NUL; a read strips the block
/// back to the fragment, and a header with missing or out-of-range offsets has none.
#[test]
fn windows_cf_html_offsets_round_trip() {
    let fragment = "<b>h\u{e9}</b>".as_bytes();
    let html = block(&NativeFormat::Html, fragment);
    let header = "Version:0.9\r\nStartHTML:0000000105\r\nEndHTML:0000000187\r\n\
                  StartFragment:0000000141\r\nEndFragment:0000000151\r\n";
    assert_eq!(&html[..105], header.as_bytes());
    assert_eq!(&html[105..141], b"<html>\r\n<body>\r\n<!--StartFragment-->");
    assert_eq!(&html[141..151], fragment);
    assert_eq!(&html[151..187], b"<!--EndFragment-->\r\n</body>\r\n</html>");
    assert_eq!(html.len(), 188, "a terminating NUL");
    assert_eq!(html[187], 0);
    assert_eq!(html_fragment(&html), Some(fragment));
    let foreign = b"Version:1.0\r\nStartFragment:50\r\nEndFragment:52\r\n<x>hi</x>";
    assert_eq!(html_fragment(foreign), Some(&b"hi"[..]), "unpadded offsets from another program");
    assert_eq!(html_fragment(b"Version:0.9\r\nStartFragment:0000000090\r\n<p>"), None);
    let past = b"Version:0.9\r\nStartFragment:0000000010\r\nEndFragment:0000009999\r\n<p>";
    assert_eq!(html_fragment(past), None);
}

/// Contract: `text/uri-list` is `CF_HDROP` only when every item is a `file:` URI with a Windows
/// path: a DROPFILES header (`pFiles` 20, `fWide` 1), each path NUL-terminated as UTF-16, and a
/// final NUL. A read turns a `CF_HDROP` back into `file:` URIs within the cap; a malformed or
/// ANSI list reads as absent.
#[test]
fn windows_hdrop_file_uris_only() {
    let list = b"file:///C:/a%20b.txt\r\n# a comment\r\nfile://server/share/x\r\n";
    let paths = uri_list_windows_paths(list).expect("every item is a file path");
    assert_eq!(paths, ["C:\\a b.txt", "\\\\server\\share\\x"]);
    let names = CustomNames::new();
    for refused in [&b"file:///C:/a\r\nhttps://example.com/\r\n"[..], b"# only a comment\r\n", b""]
    {
        assert_eq!(uri_list_windows_paths(refused), None, "{refused:?}");
        assert!(!holds_with(&names, &NativeFormat::UriList, refused));
    }
    let hdrop = block(&NativeFormat::UriList, list);
    assert_eq!(&hdrop[..4], &20u32.to_le_bytes());
    assert_eq!(&hdrop[4..16], &[0; 12], "pt and fNC");
    assert_eq!(&hdrop[16..20], &1u32.to_le_bytes(), "fWide");
    let units: Vec<u8> =
        "C:\\a b.txt\0\\\\server\\share\\x\0\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(&hdrop[20..], units.as_slice());
    let uris = b"file:///C:/a%20b.txt\r\nfile://server/share/x\r\n".to_vec();
    assert_eq!(hdrop_uri_list(&hdrop, 1 << 20), TypeRead::Bytes(uris));
    assert_eq!(hdrop_uri_list(&hdrop, 10), TypeRead::TooLarge);
    assert_eq!(hdrop_uri_list(&hdrop[..10], 1 << 20), TypeRead::Absent, "a short header");
    let mut ansi = hdrop.clone();
    ansi[16] = 0;
    assert_eq!(hdrop_uri_list(&ansi, 1 << 20), TypeRead::Absent, "an ANSI list");
    let mut past = hdrop;
    past[..4].copy_from_slice(&999u32.to_le_bytes());
    assert_eq!(hdrop_uri_list(&past, 1 << 20), TypeRead::Absent, "pFiles past the block");
}

/// Contract: a `file:` URI maps to a Windows path only when it names a drive (empty or
/// `localhost` host) or a UNC share (another host), percent-decoded to UTF-8 with no controls and
/// no query or fragment; a path maps back to a percent-encoded `file:` URI.
#[test]
fn windows_file_uri_paths() {
    for (uri, path) in [
        ("file:///C:/a%20b.txt", Some("C:\\a b.txt")),
        ("FILE://localhost/D:/x/y", Some("D:\\x\\y")),
        ("file://server/share/dir", Some("\\\\server\\share\\dir")),
        ("file:///C:/%E2%82%AC", Some("C:\\\u{20ac}")),
        ("file:///home/user", None),
        ("file:///C:/x?y", None),
        ("file:///C:/x#y", None),
        ("file:///C:/%00", None),
        ("file:///C:/%zz", None),
        ("file:///C:/%+1", None),
        ("file:///C:/%ff", None),
        ("file://C:/x", None),
        ("file://server", None),
        ("https://example.com/C:/x", None),
    ] {
        assert_eq!(file_uri_windows_path(uri).as_deref(), path, "{uri}");
    }
    for (path, uri) in [
        ("C:\\a b.txt", Some("file:///C:/a%20b.txt")),
        ("\\\\server\\share\\x", Some("file://server/share/x")),
        ("C:\\\u{20ac}", Some("file:///C:/%E2%82%AC")),
        ("relative\\x", None),
    ] {
        let mut encoded = Vec::new();
        let result = emit_file_uri(path.chars().map(Some), &mut |part| {
            encoded.extend_from_slice(part);
            Ok(())
        });
        assert_eq!(result.ok().and_then(|_| std::str::from_utf8(&encoded).ok()), uri, "{path}");
    }
}

/// Contract: text converts to UTF-16 with each invalid UTF-8 sequence as one U+FFFD and a
/// terminator, and every native block fits the copy reserved for it before it is posted.
#[test]
fn native_copies_fit_their_reservation() {
    assert_eq!(block(&NativeFormat::Text, b"a\xffb"), utf16_block("a\u{fffd}b"));
    let custom = NativeFormat::Custom("application/x-y".to_owned());
    for (format, bytes) in [
        (NativeFormat::Text, &b""[..]),
        (NativeFormat::Text, b"\xff\xfe\xfd"),
        (NativeFormat::Text, "\u{1f600}".as_bytes()),
        (NativeFormat::Html, b""),
        (NativeFormat::Html, b"<b>x</b>"),
        (NativeFormat::Png, b"\x89PNG"),
        (custom, b"c"),
        (NativeFormat::UriList, b"file:///C:/%F0%9F%98%80\r\n"),
        (NativeFormat::UriList, b"file:///C:/a\r\nfile:///D:/b"),
    ] {
        let native = encode(&format, bytes).expect("encodable").byte_len();
        assert!(native <= copy_bound(&format, bytes.len()), "{format:?} {bytes:?}: {native}");
    }
}

/// The fake holding `text` as its only `CF_UNICODETEXT` block.
fn holding_text(text: &str) -> FakeWin32 {
    FakeWin32 { clipboard: vec![(CF_UNICODETEXT, utf16_block(text))], ..FakeWin32::default() }
}

/// The snapshot a write over `text` reads.
fn snapshot_of(text: &str) -> Snapshot {
    let units: Vec<u16> = text.encode_utf16().collect();
    Snapshot { block_bytes: units.len() * 2 + 2, units }
}

/// Contract: a program text write reads the value it replaces, checks its gate and replaces the
/// clipboard in one acquisition: one open, the read, the gate, then `EmptyClipboard`, the set and
/// one close, so nothing can change the clipboard between the check and the replacement.
#[test]
fn text_write_reads_gates_and_replaces_in_one_acquisition() {
    let mut backend = WindowsOffer::new(holding_text("old"));
    let log = backend.clipboard.log.clone();
    let gate = || {
        log.borrow_mut().push("gate");
        true
    };
    let (published, replaced) = backend.write_text("program", Some(1 << 20), &gate);
    assert_eq!(published, Published::Written);
    let replaced = replaced.expect("the replaced value was read");
    assert_eq!(replaced.units, "old".encode_utf16().collect::<Vec<_>>());
    assert_eq!(*log.borrow(), ["open", "read", "gate", "empty", "set", "close", "release window"]);
    assert_eq!(backend.clipboard.text().as_deref(), Some("program"));
}

/// Contract: another program's copy that tries to land between the read of the replaced value
/// and the write waits for the acquisition to end, so it lands after the write and wins; the
/// program write never overwrites it.
#[test]
fn external_copy_during_text_write_lands_after_it() {
    let mut backend = WindowsOffer::new(holding_text("old"));
    backend.clipboard.external_copy = Some(utf16_block("external"));
    let (published, _) = backend.write_text("program", Some(1 << 20), &|| true);
    assert_eq!(published, Published::Written);
    assert_eq!(backend.clipboard.text().as_deref(), Some("external"));
}

/// Contract: a gate that refuses at the commit point, after the replaced value was read, closes
/// the clipboard untouched and frees the staged block: `Unchanged`, nothing emptied or set.
#[test]
fn refused_gate_leaves_the_clipboard_untouched() {
    let mut backend = WindowsOffer::new(holding_text("old"));
    let (published, replaced) = backend.write_text("program", Some(1 << 20), &|| false);
    assert_eq!(published, Published::Unchanged);
    assert!(replaced.is_some(), "the read ran before the gate");
    let fake = &backend.clipboard;
    assert_eq!((fake.opens, fake.empties, fake.sets, fake.closes), (1, 0, 0, 1));
    assert_eq!(fake.text().as_deref(), Some("old"));
    assert!(fake.live.is_empty(), "the staged block was freed");
    let offer = offer(&[("text/plain", b"t"), ("text/html", b"h")]);
    let mut backend = WindowsOffer::new(holding_text("old"));
    assert_eq!(publish_offer(&mut backend, &offer, &|| false), Published::Unchanged);
    assert_eq!((backend.clipboard.empties, backend.clipboard.sets), (0, 0));
    assert!(backend.clipboard.live.is_empty());
}

/// Contract: a reassertion writes the program's text again only while the clipboard holds exactly
/// the value the write replaced, its change counter stays put across that comparison, and its gate
/// still passes after it; compare and replace share one acquisition, so a newer value always wins.
#[test]
fn reassert_compares_and_replaces_in_one_acquisition() {
    let previous = snapshot_of("old");
    let mut backend = WindowsOffer::new(holding_text("old"));
    assert_eq!(backend.reassert_text("program", &previous, &|| true), Published::Written);
    let fake = &backend.clipboard;
    assert_eq!((fake.opens, fake.closes), (1, 1));
    assert_eq!(fake.text().as_deref(), Some("program"));

    let mut backend = WindowsOffer::new(holding_text("newer"));
    assert_eq!(backend.reassert_text("program", &previous, &|| true), Published::Unchanged);
    assert_eq!(backend.clipboard.text().as_deref(), Some("newer"), "a newer value wins");

    let mut backend = WindowsOffer::new(holding_text("old"));
    backend.clipboard.race_read = true;
    let published = backend.reassert_text("program", &previous, &|| true);
    assert_eq!(published, Published::Unchanged, "the counter moved during the comparison");
    assert_eq!(backend.clipboard.text().as_deref(), Some("old"));

    let mut backend = WindowsOffer::new(holding_text("old"));
    assert_eq!(backend.reassert_text("program", &previous, &|| false), Published::Unchanged);
    assert_eq!(backend.clipboard.text().as_deref(), Some("old"), "a revoked gate writes nothing");
    assert!(backend.clipboard.live.is_empty());

    let mut backend = WindowsOffer::new(holding_text("old"));
    backend.clipboard.external_copy = Some(utf16_block("external"));
    assert_eq!(backend.reassert_text("program", &previous, &|| true), Published::Written);
    let text = backend.clipboard.text();
    assert_eq!(text.as_deref(), Some("external"), "a copy at the handoff lands after the write");
}

/// Contract: an empty program write clears the clipboard in one acquisition: opened, emptied and
/// closed, with nothing set.
#[test]
fn empty_text_write_clears_in_one_acquisition() {
    let mut backend = WindowsOffer::new(holding_text("old"));
    assert_eq!(backend.write_text("", None, &|| true).0, Published::Written);
    let fake = &backend.clipboard;
    assert_eq!((fake.opens, fake.empties, fake.sets, fake.closes), (1, 1, 0, 1));
    assert!(fake.clipboard.is_empty());
}

/// Contract: an OSC 52 read sizes the `CF_UNICODETEXT` block with `GlobalSize` before it locks it:
/// a block holding more UTF-16 units than the cap plus a terminator is `TooLarge` and never locked,
/// one within the cap is locked once and converted, and the clipboard is closed either way.
#[test]
fn global_size_refuses_before_lock() {
    let mut fake = holding_text("hello");
    assert_eq!(read_text_with(&mut fake, 4), ReadOutcome::TooLarge);
    assert_eq!(fake.locks, 0, "an oversized block is never locked");
    assert_eq!((fake.opens, fake.closes), (1, 1));
    let mut fake = holding_text("hello");
    assert_eq!(read_text_with(&mut fake, 5), ReadOutcome::Text("hello".into()));
    assert_eq!((fake.locks, fake.closes), (1, 1));
    let mut fake = FakeWin32::default();
    assert_eq!(read_text_with(&mut fake, 5), ReadOutcome::Empty, "no text block");
    let mut fake = FakeWin32 { refuse_open: true, ..holding_text("x") };
    assert_eq!(read_text_with(&mut fake, 5), ReadOutcome::Failed, "the clipboard stayed held");
}

/// RegisterClipboardFormatW ignores ASCII case. Two valid spellings that return one format ID
/// must publish only the first representation and allocate exactly one native block.
#[test]
fn clipboard_name_windows_duplicate_format_id_keeps_first() {
    let custom = offer(&[("application/x-case", b"first"), ("APPLICATION/X-CASE", b"second")]);
    let mut backend = WindowsOffer::new(previous());
    assert_eq!(publish_offer(&mut backend, &custom, &|| true), Published::Written);
    assert_eq!((backend.clipboard.sets, backend.clipboard.allocs), (1, 1));
    assert_eq!(backend.clipboard.clipboard, [(0xC000, b"first".to_vec())]);
}

/// Many short paths stress per-item containers. Admission uses no heap, and the production
/// encoder including its destination must fit the pre-post native reservation.
#[test]
fn uri_batch_windows_preflight_and_encoding_fit_reservation() {
    let list = "file:///C:/x\n".repeat(256);
    let names = CustomNames::new();
    let counter = crate::lib_tests::CountAllocations::start();
    let held = holds_with(&names, &NativeFormat::UriList, list.as_bytes());
    let preflight = counter.peak_bytes();
    drop(counter);
    assert!(held);
    assert_eq!(preflight, 0);
    let counter = crate::lib_tests::CountAllocations::start();
    let encoded = block(&NativeFormat::UriList, list.as_bytes());
    let peak = counter.peak_bytes();
    drop(counter);
    assert!(peak <= copy_bound(&NativeFormat::UriList, list.len()), "peak {peak}");
    assert_eq!(
        hdrop_uri_list(&encoded, 1 << 20),
        TypeRead::Bytes("file:///C:/x\r\n".repeat(256).into_bytes())
    );
}

/// A drive-relative path, a decoded UNC authority with a separator/colon, and excess file
/// counts cannot enter CF_HDROP, even when the first 256 lines were individually valid.
#[test]
fn uri_batch_windows_rejects_malformed_or_excess_items() {
    let names = CustomNames::new();
    for uri in [
        "file:///C:",
        "file://bad%3Ahost/share",
        "file://bad%2Fhost/share",
        "file://bad%5Chost/share",
        "file://./share",
        "file://host//path",
    ] {
        assert!(!holds_with(&names, &NativeFormat::UriList, uri.as_bytes()), "{uri}");
    }
    assert!(!holds_with(&names, &NativeFormat::UriList, "file:///C:/x\n".repeat(257).as_bytes()));
}

/// A single path terminator does not terminate the list. A huge path whose escaped URI would
/// exceed the cap is sized before allocation, not built and then discarded.
#[test]
fn uri_batch_windows_read_refuses_before_allocating() {
    let list = format!("file:///C:/{}", "%20".repeat(8192));
    let encoded = block(&NativeFormat::UriList, list.as_bytes());
    let counter = crate::lib_tests::CountAllocations::start();
    let read = hdrop_uri_list(&encoded, 1024);
    let peak = counter.peak_bytes();
    drop(counter);
    assert_eq!(read, TypeRead::TooLarge);
    assert_eq!(peak, 0, "no path or URI scratch");
    assert_eq!(hdrop_uri_list(&encoded[..encoded.len() - 2], 1 << 20), TypeRead::Absent);
    let too_many = block(&NativeFormat::UriList, b"file:///C:/x");
    let mut packed = too_many[..20].to_vec();
    for _ in 0..257 {
        packed.extend_from_slice(&too_many[20..too_many.len() - 2]);
    }
    packed.extend([0, 0]);
    assert_eq!(hdrop_uri_list(&packed, 1 << 20), TypeRead::TooLarge);
}

/// Only reads may use the ownerless open. Every mutation, including a clear, must request an
/// owner-backed acquisition before it may empty the clipboard.
#[test]
fn windows_mutations_use_worker_owned_acquisition() {
    let mut backend = WindowsOffer::new(previous());
    let _ = read_text_with(&mut backend.clipboard, 1024);
    assert_eq!(backend.clipboard.owned_opens, 0);
    assert_eq!(backend.write_text("", None, &|| true).0, Published::Written);
    assert_eq!(backend.clipboard.owned_opens, 1);
    assert_eq!(
        publish_offer(&mut backend, &offer(&[("application/x-own", b"data")]), &|| true),
        Published::Written
    );
    assert_eq!(backend.clipboard.owned_opens, 2);
}

/// A successful publish closes the open session and releases its owning window before the worker
/// can park. The log observes destruction of the acquisition token, not just a CloseClipboard call.
#[test]
fn windows_mutation_releases_window_after_close() {
    let mut backend = WindowsOffer::new(previous());
    assert_eq!(backend.write_text("text", None, &|| true).0, Published::Written);
    assert!(backend.clipboard.log.borrow().ends_with(&["close", "release window"]));
}

/// Use the name the listing actually emits to look up the same registered format. Enumeration
/// may return either a MIME registration or a legacy name; neither lookup registers new names.
#[test]
fn listed_windows_legacy_names_read_back() {
    use crate::app::osc52_read_source::{list_native_types, TypeListing};
    for registered in ["Rich Text Format", "JFIF", "application/x-test"] {
        let TypeListing::Types(names) = list_native_types([registered.to_owned()]) else {
            panic!("listing")
        };
        assert_eq!(names.len(), 1);
        assert!(registered_mime_matches(registered, &names[0]));
        assert!(!registered_mime_matches(registered, "application/x-absent"));
    }
}
