use super::*;

/// A fake native clipboard: what it holds, what one publish staged, and where it fails.
#[derive(Default)]
struct Fake {
    clipboard: Vec<(NativeFormat, Vec<u8>)>,
    staged: Vec<(NativeFormat, Vec<u8>)>,
    /// The zero-based stage call that fails.
    fail_stage_at: Option<usize>,
    fail_publish: bool,
    stage_calls: usize,
    publishes: usize,
}

impl OfferBackend for Fake {
    fn stage(&mut self, format: &NativeFormat, bytes: &[u8]) -> bool {
        let call = self.stage_calls;
        self.stage_calls += 1;
        if self.fail_stage_at == Some(call) {
            return false;
        }
        self.staged.push((format.clone(), bytes.to_vec()));
        true
    }

    fn publish(&mut self, gate: CommitGate<'_>) -> Published {
        self.publishes += 1;
        if !gate() {
            // The gate refused at the commit point: the staged buffers drop, nothing changes.
            self.staged.clear();
            return Published::Unchanged;
        }
        if self.fail_publish {
            // A failed publish empties the clipboard, as Windows' recovery does.
            self.clipboard.clear();
            self.staged.clear();
            return Published::Failed;
        }
        self.clipboard = std::mem::take(&mut self.staged);
        Published::Written
    }

    fn discard(&mut self) {
        self.staged.clear();
    }
}

/// `(name, bytes)` entries from string pairs.
fn entries(pairs: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
    pairs.iter().map(|(name, bytes)| ((*name).to_owned(), bytes.as_bytes().to_vec())).collect()
}

/// The offer `pairs` plan to under `holds`; panics when the plan is not an offer.
fn offer(pairs: &[(&str, &str)], holds: &dyn Fn(&NativeFormat, &[u8]) -> bool) -> NativeOffer {
    match plan(entries(pairs), &[], holds) {
        OfferPlan::Offer(offer) => offer,
        other => panic!("expected an offer, got {other:?}"),
    }
}

/// A fake holding one previous text.
fn previous() -> Fake {
    Fake { clipboard: vec![(NativeFormat::Text, b"previous".to_vec())], ..Fake::default() }
}

/// Contract: only RFC 6838 `type/subtype` names of restricted-name characters, at most 255 bytes,
/// are MIME names; the X11 text atoms are the only names without a slash that map.
#[test]
fn invalid_mime_name_rejected() {
    for name in ["", "text", "/plain", "text/", "text/pl ain", "te\u{e9}xt/plain", "a/b/c", "-x/y"]
    {
        assert_eq!(native_format(name), None, "{name:?}");
    }
    let long = format!("application/{}", "x".repeat(MAX_MIME_NAME_BYTES));
    assert_eq!(native_format(&long), None, "past 255 bytes");
    let custom = "application/vnd.sonicterm+json";
    assert_eq!(native_format(custom), Some(NativeFormat::Custom(custom.to_owned())));
}

/// Contract: the well-known types map to their native formats and every text alias is text.
#[test]
fn well_known_and_text_alias_mapping() {
    for name in ["text/plain", "text/plain;charset=utf-8", "UTF8_STRING", "TEXT", "STRING"] {
        assert_eq!(native_format(name), Some(NativeFormat::Text), "{name}");
    }
    assert_eq!(native_format("text/html"), Some(NativeFormat::Html));
    assert_eq!(native_format("image/png"), Some(NativeFormat::Png));
    assert_eq!(native_format("text/uri-list"), Some(NativeFormat::UriList));
}

/// Contract: when the second native format cannot be staged, nothing is published (the previous
/// clipboard stays) and the result is a failure, which the commit answers `EIO`.
#[test]
fn second_native_format_failure_publishes_nothing_eio() {
    let offer = offer(&[("text/plain", "t"), ("text/html", "<b>h</b>")], &|_, _| true);
    let mut fake = Fake { fail_stage_at: Some(1), ..previous() };
    assert_eq!(publish_offer(&mut fake, &offer, &|| true), Published::Failed);
    assert_eq!(fake.publishes, 0, "no publish after a staging failure");
    assert!(fake.staged.is_empty(), "the staged first format is discarded");
    assert_eq!(fake.clipboard, [(NativeFormat::Text, b"previous".to_vec())]);
}

/// Contract: a staging failure of the first format keeps the previous clipboard as well.
#[test]
fn stage_failure_keeps_previous_clipboard() {
    let offer = offer(&[("image/png", "png")], &|_, _| true);
    let mut fake = Fake { fail_stage_at: Some(0), ..previous() };
    assert_eq!(publish_offer(&mut fake, &offer, &|| true), Published::Failed);
    assert_eq!((fake.publishes, fake.clipboard.len()), (0, 1));
}

/// Contract: a failed publish operation is never success, even after every format was staged.
#[test]
fn publish_failure_is_never_done() {
    let offer = offer(&[("text/plain", "t"), ("image/png", "p")], &|_, _| true);
    let mut fake = Fake { fail_publish: true, ..previous() };
    assert_eq!(publish_offer(&mut fake, &offer, &|| true), Published::Failed);
    assert_eq!(fake.publishes, 1);
}

/// Contract: when every name is invalid, unmapped or not holdable, the plan is unavailable (the
/// commit answers `ENOSYS`) and nothing would reach the clipboard.
#[test]
fn all_omitted_enosys() {
    let pairs = [("image/png", "p"), ("not a mime", "x"), ("application/x-y", "y")];
    let text_only = |format: &NativeFormat, _: &[u8]| *format == NativeFormat::Text;
    assert!(matches!(plan(entries(&pairs), &[], &text_only), OfferPlan::Unavailable));
}

/// Contract: omitted types are counted, the holdable ones are all published, and that is success.
#[test]
fn omitted_plus_published_done() {
    let holds =
        |format: &NativeFormat, _: &[u8]| matches!(format, NativeFormat::Text | NativeFormat::Html);
    let pairs = [("image/png", "p"), ("text/html", "h"), ("bad", "b"), ("text/plain", "t")];
    let offer = offer(&pairs, &holds);
    assert_eq!(offer.omitted(), 2);
    let mut fake = previous();
    assert_eq!(publish_offer(&mut fake, &offer, &|| true), Published::Written);
    let html = (NativeFormat::Html, b"h".to_vec());
    assert_eq!(fake.clipboard, [html, (NativeFormat::Text, b"t".to_vec())]);
}

/// Contract: aliases resolve over every staged name before mapping; each format is kept once,
/// first first, and an alias shares its target's buffer instead of copying it.
#[test]
fn aliases_resolve_before_mapping_and_first_format_wins() {
    let aliases = [("text/plain".to_owned(), "x/source".to_owned())];
    let pairs = entries(&[("text/html", "h"), ("x/source", "src"), ("text/plain", "late")]);
    let OfferPlan::Offer(offer) = plan(pairs, &aliases, &|_, _| true) else { panic!("an offer") };
    assert_eq!(offer.text(), Some(&b"src"[..]), "the alias overrode the staged text");
    let formats: Vec<_> = offer.representations().map(|(format, _)| format.clone()).collect();
    let source = NativeFormat::Custom("x/source".to_owned());
    assert_eq!(formats, [NativeFormat::Html, source, NativeFormat::Text]);
    assert_eq!(offer.entries.len(), 2, "the overridden text buffer is dropped");
}

/// Contract: a commit that staged nothing clears the clipboard.
#[test]
fn empty_commit_clears() {
    assert!(matches!(plan(Vec::new(), &[], &|_, _| true), OfferPlan::Clear));
}

/// Contract: an offer reserves its kept buffers plus one native copy per representation, and a
/// text-only offer is recognized so it can take the text path.
#[test]
fn reservation_counts_native_copies_and_text_only() {
    let aliases = [("text/html".to_owned(), "text/plain".to_owned())];
    let OfferPlan::Offer(shared) = plan(entries(&[("text/plain", "abcd")]), &aliases, &|_, _| true)
    else {
        panic!("an offer")
    };
    assert!(shared.reserved_bytes() >= 4 + 2 * 4, "{}", shared.reserved_bytes());
    assert_eq!(shared.byte_len(), 8, "two representations of four bytes");
    assert!(!shared.is_text_only());
    let text_only = |format: &NativeFormat, _: &[u8]| *format == NativeFormat::Text;
    let text = offer(&[("text/plain", "t"), ("text/html", "h")], &text_only);
    assert!(text.is_text_only());
    assert_eq!(text.into_text(), b"t".to_vec());
}

/// Each plain-text MIME parameter has a token name and either a token or quoted value.
/// Quoted delimiters, spaces and quoted-pair escapes are data, never parameter separators.
#[test]
fn text_plain_parameters_are_validated() {
    use super::super::kitty_clipboard_tx::is_text;
    for name in [
        "text/plain",
        "text/plain;charset=utf-8",
        "text/plain;x=1;y=2",
        r#"text/plain;note="one; two";x="a\"b""#,
        r#"text/plain;x="""#,
    ] {
        assert!(is_text(name), "{name}");
    }
    for name in [
        "text/plain;",
        "text/plain;a b",
        "text/plain;\u{1b}",
        "text/plainx",
        "text/plain\n",
        "text/plain;name",
        "text/plain;=x",
        "text/plain;x=",
        "text/plain;x=a/b",
        "text/plain;x=a=b",
        "text/plain;x=1;",
        "text/plain;x*=invalid?",
        r#"text/plain;x="value"tail"#,
        r#"text/plain;x="unterminated"#,
        "text/plain;x=\"a\n\"",
    ] {
        assert!(!is_text(name), "{name:?}");
    }
}
