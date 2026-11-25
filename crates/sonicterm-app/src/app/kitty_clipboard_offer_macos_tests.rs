use super::super::{plan, publish_offer, NativeOffer, OfferPlan, Published};
use super::*;
use crate::app::osc52_read_source::{TypeListing, TypeRead};
use objc2::runtime::AnyObject;

/// A private pasteboard with a unique name, released when dropped; never the user's clipboard.
struct Private(Retained<NSPasteboard>);

impl Private {
    fn new() -> Self {
        Self(NSPasteboard::pasteboardWithUniqueName())
    }

    /// The bytes the pasteboard holds under `kind`.
    fn data(&self, kind: &str) -> Option<Vec<u8>> {
        self.0.dataForType(&NSString::from_str(kind)).map(|data| data.to_vec())
    }
}

impl Drop for Private {
    // Lifecycle: a unique pasteboard lives in the pasteboard server until it is released.
    fn drop(&mut self) {
        // SAFETY: `releaseGlobally` takes no arguments and returns nothing; the pasteboard is ours.
        unsafe { msg_send![&*self.0, releaseGlobally] }
    }
}

/// The offer `pairs` plan to on this platform.
fn offer(pairs: &[(&str, &[u8])]) -> NativeOffer {
    let entries = pairs.iter().map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec())).collect();
    match plan(entries, &[], &|format, bytes| holds(format, bytes)) {
        OfferPlan::Offer(offer) => offer,
        other => panic!("expected an offer, got {other:?}"),
    }
}

/// Contract: text, HTML, PNG and a custom MIME type are published as one item in one operation:
/// the change count moves once (`clearContents`) and every type reads back byte for byte, text
/// included even when it is not UTF-8.
#[test]
fn macos_png_html_text_custom_mime_one_change_count() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let custom = "application/x-sonicterm-test";
        let text: &[u8] = b"text \xff";
        let offer = offer(&[
            ("text/plain", text),
            ("text/html", b"<b>h</b>"),
            ("image/png", b"\x89PNG"),
            (custom, b"custom"),
        ]);
        assert_eq!(offer.omitted(), 0);
        let before = board.0.changeCount();
        let published = publish_offer(&mut Pasteboard::on(board.0.clone()), &offer, &|| true);
        assert_eq!(published, Published::Written);
        assert_eq!(board.0.changeCount(), before + 1, "one publish, one change");
        assert_eq!(board.data("public.utf8-plain-text").as_deref(), Some(text));
        assert_eq!(board.data("public.html").as_deref(), Some(&b"<b>h</b>"[..]));
        assert_eq!(board.data("public.png").as_deref(), Some(&b"\x89PNG"[..]));
        let custom_type = uniform_type(custom).expect("a uniform type").to_string();
        assert_eq!(board.data(&custom_type).as_deref(), Some(&b"custom"[..]));
    });
}

/// Contract: a MIME name with no system type gets a dynamic uniform type that maps back to the same
/// MIME name, so a reader can list it.
#[test]
fn dynamic_uti_round_trips_mime() {
    objc2::rc::autoreleasepool(|_| {
        let mime = "application/x-sonicterm-test";
        let identifier = uniform_type(mime).expect("a dynamic uniform type");
        let class = AnyClass::get(c"UTType").expect("UTType");
        let back: Option<Retained<NSString>>;
        // SAFETY: `typeWithIdentifier:` takes an NSString and returns a nullable UTType;
        // `preferredMIMEType` returns a nullable NSString.
        unsafe {
            let uniform: Option<Retained<AnyObject>> =
                msg_send![class, typeWithIdentifier: &*identifier];
            back = msg_send![&*uniform.expect("the type resolves"), preferredMIMEType];
        }
        assert_eq!(back.map(|name| name.to_string()).as_deref(), Some(mime));
    });
}

/// Contract: when a representation cannot be staged, the pasteboard keeps its previous offer and
/// its change count: nothing was cleared.
#[test]
fn stage_failure_leaves_previous_offer() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let previous = offer(&[("text/plain", b"previous")]);
        let published = publish_offer(&mut Pasteboard::on(board.0.clone()), &previous, &|| true);
        assert_eq!(published, Published::Written);
        let before = board.0.changeCount();
        // A uri-list line that is not a URI cannot be held, so a fake plan that kept it fails at
        // staging.
        let forced = match plan(
            vec![
                ("text/plain".into(), b"new".to_vec()),
                ("text/uri-list".into(), b"not a uri".to_vec()),
            ],
            &[],
            &|_, _| true,
        ) {
            OfferPlan::Offer(offer) => offer,
            other => panic!("{other:?}"),
        };
        let published = publish_offer(&mut Pasteboard::on(board.0.clone()), &forced, &|| true);
        assert_eq!(published, Published::Failed);
        assert_eq!(board.0.changeCount(), before, "nothing was cleared");
        assert_eq!(board.data("public.utf8-plain-text").as_deref(), Some(&b"previous"[..]));
    });
}

/// The strings each item of `board` holds under `kind`, in item order.
fn item_strings(board: &Private, kind: &str) -> Vec<Option<String>> {
    let kind = NSString::from_str(kind);
    let items = board.0.pasteboardItems().expect("items");
    (0..items.count())
        .map(|index| items.objectAtIndex(index).stringForType(&kind).map(|text| text.to_string()))
        .collect()
}

/// Contract: a `text/uri-list` of `file:` URIs publishes one `public.file-url` item per file in the
/// same operation (the first item also carrying the other types); one non-file URL publishes as
/// `public.url`; a list mixing them, or of several non-file URLs, is not held. Reading
/// `text/uri-list` joins the items' URLs with CRLF.
#[test]
fn macos_uri_list_file_and_url_items() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let list: &[u8] = b"file:///tmp/a%20b\r\n# note\r\nfile:///tmp/c\r\n";
        let files_offer = offer(&[("text/plain", b"t"), ("text/uri-list", list)]);
        assert_eq!(files_offer.omitted(), 0);
        let before = board.0.changeCount();
        let board_writer = &mut Pasteboard::on(board.0.clone());
        assert_eq!(publish_offer(board_writer, &files_offer, &|| true), Published::Written);
        assert_eq!(board.0.changeCount(), before + 1, "one publish, one change");
        let files = item_strings(&board, "public.file-url");
        assert_eq!(files, [Some("file:///tmp/a%20b".into()), Some("file:///tmp/c".into())]);
        assert_eq!(item_strings(&board, "public.utf8-plain-text")[0].as_deref(), Some("t"));
        let joined = b"file:///tmp/a%20b\r\nfile:///tmp/c\r\n".to_vec();
        assert_eq!(read_type(&board.0, "text/uri-list", 1 << 20), TypeRead::Bytes(joined));
        assert_eq!(read_type(&board.0, "text/uri-list", 10), TypeRead::TooLarge);

        let url = Private::new();
        let url_offer = offer(&[("text/uri-list", b"https://example.com/x\r\n")]);
        let url_writer = &mut Pasteboard::on(url.0.clone());
        assert_eq!(publish_offer(url_writer, &url_offer, &|| true), Published::Written);
        assert_eq!(item_strings(&url, "public.url"), [Some("https://example.com/x".into())]);
        for refused in [
            &b"file:///tmp/a\r\nhttps://example.com/\r\n"[..],
            b"https://a.example/\r\nhttps://b.example/\r\n",
            b"# only a comment\r\n",
            b"not a uri",
        ] {
            assert!(!holds(&NativeFormat::UriList, refused), "{refused:?}");
        }
    });
}

/// Contract: HTML, PNG and a custom MIME type read back byte for byte through their pasteboard
/// types within the cap; one over the cap is refused by its length before the copy, a type the
/// board does not hold is absent, and the listing names the custom type by its MIME name.
#[test]
fn read_and_list_non_text_types() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let custom = "application/x-sonicterm-test";
        let typed = offer(&[
            ("text/plain", b"t"),
            ("text/html", b"<b>h</b>"),
            ("image/png", b"\x89PNG"),
            (custom, b"custom"),
        ]);
        let published = publish_offer(&mut Pasteboard::on(board.0.clone()), &typed, &|| true);
        assert_eq!(published, Published::Written);
        let read = |mime: &str, cap: usize| read_type(&board.0, mime, cap);
        assert_eq!(read("text/html", 64), TypeRead::Bytes(b"<b>h</b>".to_vec()));
        assert_eq!(read("image/png", 64), TypeRead::Bytes(b"\x89PNG".to_vec()));
        assert_eq!(read(custom, 64), TypeRead::Bytes(b"custom".to_vec()));
        assert_eq!(read(custom, 5), TypeRead::TooLarge);
        assert_eq!(read("text/uri-list", 64), TypeRead::Absent);
        assert_eq!(read("application/x-absent", 64), TypeRead::Absent);
        let TypeListing::Types(names) = list_types(&board.0) else { panic!("a listing") };
        for expected in ["text/plain", "text/html", "image/png", custom] {
            assert!(names.iter().any(|name| name == expected), "{expected} in {names:?}");
        }
    });
}

/// Contract: an OSC 52 write whose text is not UTF-8 reaches the macOS pasteboard byte for byte,
/// through the same raw text offer and publish an OSC 5522 text write uses; nothing becomes U+FFFD.
#[test]
fn osc52_non_utf8_text_published_byte_for_byte() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let staging = crate::app::osc52_stream::ClipboardStaging::with_budget(1 << 20, 1 << 20);
        let raw: &[u8] = b"caf\xe9 \xff";
        let mut lease = staging.lease();
        assert!(lease.grow_to(raw.len()));
        let targets = crate::app::osc52_stream::ClipTargets { clipboard: true, primary: false };
        let job = crate::app::osc52_stream::ClipboardJob::program_text(
            &staging,
            raw.to_vec(),
            lease,
            targets,
            None,
        )
        .expect("admitted");
        let offer = job.offer.as_ref().expect("a raw text offer");
        let published = publish_offer(&mut Pasteboard::on(board.0.clone()), offer, &|| true);
        assert_eq!(published, Published::Written);
        assert_eq!(board.data("public.utf8-plain-text").as_deref(), Some(raw));
    });
}

/// Contract: two MIME names the system maps to one pasteboard type (`image/jpeg` and `image/jpg`
/// are both `public.jpeg`) publish that type once, with the first name's bytes; the second never
/// silently replaces it while the write reports every type written.
#[test]
fn names_sharing_one_pasteboard_type_publish_it_once() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let offer = offer(&[("image/jpeg", b"first"), ("image/jpg", b"second")]);
        let published = publish_offer(&mut Pasteboard::on(board.0.clone()), &offer, &|| true);
        assert_eq!(published, Published::Written);
        assert_eq!(board.data("public.jpeg").as_deref(), Some(&b"first"[..]));
    });
}

/// An external pasteboard type can advertise a MIME name without being the UTI a new registration
/// of that MIME would produce. Read the listed representation under its actual pasteboard type.
#[test]
fn clipboard_name_macos_reads_the_actual_listed_type() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let name = "application/x-listed!type";
        board.0.clearContents();
        assert!(board
            .0
            .setData_forType(Some(&NSData::with_bytes(&[0xa5])), &NSString::from_str(name)));
        assert_eq!(list_types(&board.0), TypeListing::Types(vec![name.into()]));
        assert_eq!(read_type(&board.0, name, 8), TypeRead::Bytes(vec![0xa5]));
    });
}

/// File URI preflight is allocation-free regardless of the number of short lines and reserves
/// native item workspace, not just the short path bytes, before staging the offer.
#[test]
fn uri_batch_macos_preflight_and_item_reservation() {
    let list = "file:///x\n".repeat(256);
    let count = crate::lib_tests::CountAllocations::start();
    let admitted = holds(&NativeFormat::UriList, list.as_bytes());
    let peak = count.peak_bytes();
    drop(count);
    assert!(admitted);
    assert_eq!(peak, 0);
    let offer = offer(&[("text/uri-list", list.as_bytes())]);
    assert!(
        offer.reserved_bytes() > 2 * list.len() + 256 * size_of::<Retained<NSPasteboardItem>>()
    );
}

/// Reject the entire write if any file URI lacks an absolute path or contains malformed escapes,
/// or if there would be too many pasteboard items.
#[test]
fn uri_batch_macos_refuses_invalid_or_excess_items() {
    for uri in [
        "file:relative",
        "file://",
        "file://host",
        "file:///x%zz",
        "file:///x%00",
        "file://bad%2Fhost/path",
        "file:///x?query",
    ] {
        assert!(!holds(&NativeFormat::UriList, format!("file:///good\n{uri}").as_bytes()), "{uri}");
    }
    assert!(!holds(&NativeFormat::UriList, "file:///x\n".repeat(257).as_bytes()));
}

/// A private pasteboard containing more than the supported item count is not read as a prefix.
#[test]
fn uri_batch_macos_read_never_returns_a_prefix() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let kind = NSString::from_str(FILE_URL_TYPE);
        let file = NSString::from_str("file:///x");
        let items: Vec<_> = (0..257)
            .map(|_| {
                let item = NSPasteboardItem::new();
                assert!(item.setString_forType(&file, &kind));
                ProtocolObject::<dyn NSPasteboardWriting>::from_retained(item)
            })
            .collect();
        board.0.clearContents();
        assert!(board.0.writeObjects(&NSArray::from_retained_slice(&items)));
        assert_eq!(read_uri_list(&board.0, 1 << 20), TypeRead::TooLarge);
    });
}

/// A refusal from the writeObjects call, after the gate and clear on a private pasteboard, must
/// return Failed (EIO). Real staged items reach the injected call, so this is not a stage failure.
#[test]
fn macos_writeobjects_no_answers_eio() {
    objc2::rc::autoreleasepool(|_| {
        let board = Private::new();
        let mut pasteboard = Pasteboard::on(board.0.clone());
        let before = board.0.changeCount();
        let staged = offer(&[("text/plain", b"data"), ("text/html", b"<p/>")]);
        pasteboard.write_objects = |_, items| {
            assert_eq!(items.count(), 1);
            false
        };
        assert_eq!(publish_offer(&mut pasteboard, &staged, &|| true), Published::Failed);
        assert_ne!(board.0.changeCount(), before, "publish reached clearContents");
        assert!(board.data("public.utf8-plain-text").is_none());
    });
}

/// macOS file items represent local paths only. A remote authority, even in the second item,
/// omits the entire representation. A percent-encoded localhost is still the local authority.
#[test]
fn macos_file_items_refuse_nonlocal_authorities() {
    assert!(!holds(&NativeFormat::UriList, b"file:///ok\nfile://remote.example/share/path"));
    assert!(!holds(&NativeFormat::UriList, b"file://%72emote/path"));
    for uri in ["file:///x", "file:/x", "file://LOCALHOST/x", "file://%6cocalhost/x"] {
        assert!(holds(&NativeFormat::UriList, uri.as_bytes()), "{uri}");
    }
}
