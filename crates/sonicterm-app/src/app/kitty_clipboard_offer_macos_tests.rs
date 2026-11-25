use super::super::{plan, publish_offer, NativeOffer, OfferPlan};
use super::*;
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
    match plan(entries, &[], &holds) {
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
        assert!(publish_offer(&mut Pasteboard::on(board.0.clone()), &offer));
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
        assert!(publish_offer(
            &mut Pasteboard::on(board.0.clone()),
            &offer(&[("text/plain", b"previous")])
        ));
        let before = board.0.changeCount();
        // A uri-list cannot be held here, so a fake plan that kept it fails at staging.
        let forced = match plan(
            vec![
                ("text/plain".into(), b"new".to_vec()),
                ("text/uri-list".into(), b"file:///".to_vec()),
            ],
            &[],
            &|_| true,
        ) {
            OfferPlan::Offer(offer) => offer,
            other => panic!("{other:?}"),
        };
        assert!(!publish_offer(&mut Pasteboard::on(board.0.clone()), &forced));
        assert_eq!(board.0.changeCount(), before, "nothing was cleared");
        assert_eq!(board.data("public.utf8-plain-text").as_deref(), Some(&b"previous"[..]));
    });
}
