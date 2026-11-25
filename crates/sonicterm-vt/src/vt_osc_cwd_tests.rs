//! Pins the working-directory sources: OSC 7, ConEmu OSC 9;9 and OSC 1337 `CurrentDir`, each with
//! `file://` and `kitty-shell-cwd://` URIs, bare paths, empty resets, revocation of earlier trust,
//! and the collector's ownership of every form.

use super::*;
use crate::vt::{CaptureStagingPool, Parser};

/// A parser with a private staging pool, as the crate's unit tests require.
fn parser() -> Parser {
    Parser::new_with_staging_pool(Grid::new(40, 4), None, CaptureStagingPool::new())
}

/// A URI-path snapshot (OSC 7, or a URI from any source) with `authority` and `path`.
fn snapshot(authority: &str, path: &str) -> Osc7Cwd {
    Osc7Cwd { authority: authority.into(), path: path.into(), uri_path: true }
}

/// The snapshot of a bare 9;9 `path`: no authority, and not a URI path.
fn bare(path: &str) -> Osc7Cwd {
    Osc7Cwd { authority: String::new(), path: path.into(), uri_path: false }
}

/// A trusted local directory, then the OSC `report` (without its introducer and BEL).
fn trusted_then(report: &[u8]) -> Parser {
    let mut parser = parser();
    parser.advance(b"\x1b]7;file:///home/a\x07");
    assert_eq!(parser.osc7_cwd(), Some(&snapshot("", "/home/a")));
    parser.advance(&[b"\x1b]".as_slice(), report, b"\x07"].concat());
    parser
}

/// An empty OSC 7 or 9;9 (`9;9;""` included) clears the displayed directory and the trusted
/// snapshot together and moves the revision; an explicit OSC 2 title survives the clear.
#[test]
fn an_empty_osc7_or_osc9_9_clears_display_and_snapshot_but_not_the_title() {
    for empty in [b"7;".as_slice(), b"9;9;", b"9;9;\"\""] {
        let mut parser = parser();
        parser.advance(b"\x1b]2;my title\x07\x1b]7;file:///home/a\x07");
        let revision = parser.cwd_revision();
        parser.advance(&[b"\x1b]".as_slice(), empty, b"\x07"].concat());
        assert_eq!(parser.cwd(), None, "{empty:?}");
        assert_eq!(parser.osc7_cwd(), None, "{empty:?}");
        assert_ne!(parser.cwd_revision(), revision, "{empty:?}");
        assert_eq!(parser.title(), Some("my title"), "{empty:?}");
    }
}

/// An empty `CurrentDir=` is ignored, as in Ghostty: display, snapshot and revision stay.
#[test]
fn an_empty_current_dir_is_ignored() {
    let mut parser = parser();
    parser.advance(b"\x1b]7;file:///home/a\x07");
    let revision = parser.cwd_revision();
    parser.advance(b"\x1b]1337;CurrentDir=\x07");
    assert_eq!(parser.cwd(), Some("/home/a"));
    assert_eq!(parser.osc7_cwd(), Some(&snapshot("", "/home/a")));
    assert_eq!(parser.cwd_revision(), revision);
}

/// A bare 9;9 path is stored literally with an empty authority after one pair of surrounding
/// double quotes is removed; an unmatched quote stays literal. Native validation decides later
/// whether the path can be used, so drive and UNC forms are stored as sent.
#[test]
fn a_bare_osc9_9_path_is_stored_literally_after_one_quote_pair() {
    for (payload, path) in [
        (b"9;9;/tmp".as_slice(), "/tmp"),
        (b"9;9;\"/a b\"", "/a b"),
        (b"9;9;\"/x", "\"/x"),
        (b"9;9;/x\"", "/x\""),
        (b"9;9;\"\"/x\"\"", "\"/x\""),
        (b"9;9;C:\\Users\\x", "C:\\Users\\x"),
        (b"9;9;\"C:\\fixture\"", "C:\\fixture"),
        (b"9;9;C:foo", "C:foo"),
        (b"9;9;\\\\host\\share", "\\\\host\\share"),
        (b"9;9;/a;b%20c", "/a;b%20c"),
    ] {
        let mut parser = parser();
        let revision = parser.cwd_revision();
        parser.advance(&[b"\x1b]".as_slice(), payload, b"\x07"].concat());
        assert_eq!(parser.cwd(), Some(path), "{payload:?}");
        assert_eq!(parser.osc7_cwd(), Some(&bare(path)), "{payload:?}");
        assert_ne!(parser.cwd_revision(), revision, "{payload:?}");
    }
}

/// A `file://` URI is percent-decoded and a `kitty-shell-cwd://` URI kept raw, with its host kept
/// as the authority, and every source reads them the same way; `file:///p` has an empty (local)
/// authority. A malformed URI with a known scheme updates the display but stores no snapshot.
#[test]
fn every_source_reads_file_and_kitty_shell_cwd_uris_alike() {
    for prefix in [b"7;".as_slice(), b"9;9;", b"1337;CurrentDir="] {
        for (uri, display, expected) in [
            (b"file://host/My%20Dir".as_slice(), "/My Dir", Some(snapshot("host", "/My Dir"))),
            (b"file:///p", "/p", Some(snapshot("", "/p"))),
            (b"kitty-shell-cwd://host/100%", "/100%", Some(snapshot("host", "/100%"))),
            (b"kitty-shell-cwd://localhost/a%20b", "/a%20b", Some(snapshot("localhost", "/a%20b"))),
            (b"file:///bad%GG", "/bad%GG", None),
            (b"kitty-shell-cwd://bad host/x", "/x", None),
        ] {
            let mut parser = parser();
            let revision = parser.cwd_revision();
            parser.advance(&[b"\x1b]".as_slice(), prefix, uri, b"\x07"].concat());
            assert_eq!(parser.cwd(), Some(display), "{prefix:?} {uri:?}");
            assert_eq!(parser.osc7_cwd(), expected.as_ref(), "{prefix:?} {uri:?}");
            assert_ne!(parser.cwd_revision(), revision, "{prefix:?} {uri:?}");
        }
    }
}

/// The `CurrentDir` key matches ASCII case-insensitively, as Ghostty's 1337 keys do.
#[test]
fn the_current_dir_key_matches_case_insensitively() {
    for key in [b"currentdir=".as_slice(), b"CURRENTDIR=", b"cUrReNtDiR="] {
        let mut parser = parser();
        parser.advance(&[b"\x1b]1337;".as_slice(), key, b"file://localhost/w\x07"].concat());
        assert_eq!(parser.osc7_cwd(), Some(&snapshot("localhost", "/w")), "{key:?}");
    }
}

/// A bare `CurrentDir` path shows in the title but is never trusted: iTerm2 pairs it with a
/// `RemoteHost` SonicTerm does not read, so it may name another machine. It revokes earlier trust.
#[test]
fn a_bare_current_dir_is_display_only_and_revokes_trust() {
    let parser = trusted_then(b"1337;CurrentDir=/c");
    assert_eq!(parser.cwd(), Some("/c"));
    assert_eq!(parser.osc7_cwd(), None);
}

/// A remote host is kept with its authority, so later local-host checks reject it; it and every
/// other malformed report replace an earlier trusted directory, with a new revision.
#[test]
fn foreign_or_malformed_reports_replace_earlier_trust() {
    for (report, expected) in [
        (b"7;file://otherhost/b".as_slice(), Some(snapshot("otherhost", "/b"))),
        (b"9;9;kitty-shell-cwd://otherhost/b", Some(snapshot("otherhost", "/b"))),
        (b"9;9;C:foo", Some(bare("C:foo"))),
        (b"9;9;file:///bad%GG", None),
        (b"1337;CurrentDir=file://bad host/b", None),
        (b"7;\xff", None),
        (b"9;9;/b\x01", None),
        (b"1337;CurrentDir=/b\x7f", None),
    ] {
        let mut parser = parser();
        parser.advance(b"\x1b]7;file:///home/a\x07");
        let revision = parser.cwd_revision();
        parser.advance(&[b"\x1b]".as_slice(), report, b"\x07"].concat());
        assert_eq!(parser.osc7_cwd(), expected.as_ref(), "{report:?}");
        assert_ne!(parser.cwd_revision(), revision, "{report:?}");
    }
}

/// A 9;9 or `CurrentDir` report cut by CAN, an ESC that does not complete ST, the over-cap refusal
/// or host cancellation revokes trust as a cut OSC 7 does; a cut notification or `SetUserVar`
/// leaves the directory alone.
#[test]
fn a_cut_directory_report_revokes_trust_and_other_cuts_do_not() {
    for (pending, revokes) in [
        (b"\x1b]9;9;/pending".as_slice(), true),
        (b"\x1b]1337;CurrentDir=/pending", true),
        (b"\x1b]9;hello", false),
        (b"\x1b]1337;SetUserVar=a=MQ==", false),
    ] {
        for cut in [b"\x18".as_slice(), b"\x1b[0m"] {
            let mut parser = parser();
            parser.advance(b"\x1b]7;file:///home/a\x07");
            parser.advance(pending);
            parser.advance(cut);
            assert_eq!(parser.osc7_cwd().is_none(), revokes, "{pending:?} {cut:?}");
            assert_eq!(parser.cwd(), Some("/home/a"), "{pending:?} {cut:?}");
        }
        let mut parser = parser();
        parser.advance(b"\x1b]7;file:///home/a\x07");
        parser.advance(pending);
        assert!(parser.cancel_capture() > 0 || !revokes, "{pending:?}");
        parser.advance(b"\x07");
        assert_eq!(parser.osc7_cwd().is_none(), revokes, "{pending:?}");
    }
    let mut parser = parser();
    parser.advance(b"\x1b]7;file:///home/a\x07\x1b]9;9;/");
    parser.advance(&vec![b'a'; MAX_NOTIFY_BYTES]);
    parser.advance(b"\x07X");
    assert_eq!(parser.osc7_cwd(), None);
    assert_eq!(parser.grid().row(0)[0].ch, 'X');
}

/// Only the collector applies a 9;9 or `CurrentDir` report, whole across every chunk boundary; a
/// spelling it does not own (`01337`, a C1 introducer) reaches vte and changes nothing, and a
/// bare `9;9` without its separator is no directory report.
#[test]
fn the_collector_owns_every_directory_report_across_chunks() {
    for (input, expected) in [
        (b"\x1b]9;9;/a;b\x1b\\X".as_slice(), bare("/a;b")),
        (b"\x1b]1337;CurrentDir=file:///a;b\x07X", snapshot("", "/a;b")),
    ] {
        for split in 0..=input.len() {
            let mut parser = parser();
            parser.advance(&input[..split]);
            parser.advance(&input[split..]);
            assert_eq!(parser.osc7_cwd(), Some(&expected), "{input:?} at {split}");
            assert_eq!(parser.grid().row(0)[0].ch, 'X', "{input:?} at {split}");
        }
    }
    for ignored in
        [b"\x1b]01337;CurrentDir=/x\x07".as_slice(), b"\x9d1337;CurrentDir=/x\x07", b"\x1b]9;9\x07"]
    {
        let mut parser = parser();
        parser.advance(ignored);
        assert_eq!(parser.cwd(), None, "{ignored:?}");
        assert_eq!(parser.osc7_cwd(), None, "{ignored:?}");
    }
}

/// A 9;9 report is classified on the bytes it arrived with: a C0 control anywhere in it, the
/// subcommand included, refuses it as a directory and revokes earlier trust, and stripping the
/// control never turns it into a trusted report. The displayed path stays.
#[test]
fn a_control_inside_a_9_9_report_refuses_it_and_revokes_trust() {
    for report in
        [b"9;\x019;/home/x".as_slice(), b"9;9\x01;/home/x", b"9;0\x019;/home/x", b"9;9;/home\x01/x"]
    {
        let parser = trusted_then(report);
        assert_eq!(parser.osc7_cwd(), None, "{report:?}");
        assert_eq!(parser.cwd(), Some("/home/a"), "{report:?}");
    }
}

/// The collector is OSC 7's only owner in every spelling its probe reads (`7`, `07`, `+7`): a
/// report cut by CAN, SUB or an ESC that does not complete ST installs nothing and revokes earlier
/// trust. A spelling it cannot read (a control inside the number, a number past the probe, a C1
/// introducer) reaches vte, which dispatches it complete or cut, so vte's OSC 7 changes nothing.
#[test]
fn only_the_collector_applies_osc7_and_a_cut_report_installs_nothing() {
    for spelling in [b"7;".as_slice(), b"07;", b"+7;"] {
        for cut in [b"\x18".as_slice(), b"\x1a", b"\x1b[0m"] {
            let report = [b"\x1b]".as_slice(), spelling, b"kitty-shell-cwd://localhost/x", cut];
            let mut fresh = parser();
            fresh.advance(&report.concat());
            assert_eq!(fresh.osc7_cwd(), None, "{spelling:?} {cut:?}");
            assert_eq!(fresh.cwd(), None, "{spelling:?} {cut:?}");
            let mut trusted = parser();
            trusted.advance(b"\x1b]7;file:///home/a\x07");
            trusted.advance(&report.concat());
            assert_eq!(trusted.osc7_cwd(), None, "{spelling:?} {cut:?}");
            assert_eq!(trusted.cwd(), Some("/home/a"), "{spelling:?} {cut:?}");
        }
    }
    for unowned in [
        b"\x1b]7\x01;file:///x\x07".as_slice(),
        b"\x1b]00000000007;file:///x\x07",
        b"\x9d7;file:///x\x07",
    ] {
        let mut parser = parser();
        parser.advance(unowned);
        assert_eq!(parser.osc7_cwd(), None, "{unowned:?}");
        assert_eq!(parser.cwd(), None, "{unowned:?}");
    }
}

/// A complete OSC 7 in any spelling the collector reads is applied whole across every chunk
/// boundary, a `;` in the path included; a bare one with no payload is an empty report and clears
/// the directory and the snapshot, as kitty reads it.
#[test]
fn the_collector_applies_every_osc7_spelling_whole_and_a_bare_one_clears() {
    for spelling in [b"7;".as_slice(), b"07;", b"+7;"] {
        let input = [b"\x1b]".as_slice(), spelling, b"file:///a;b\x1b\\X"].concat();
        for split in 0..=input.len() {
            let mut parser = parser();
            parser.advance(&input[..split]);
            parser.advance(&input[split..]);
            assert_eq!(parser.osc7_cwd(), Some(&snapshot("", "/a;b")), "{input:?} at {split}");
            assert_eq!(parser.grid().row(0)[0].ch, 'X', "{input:?} at {split}");
        }
    }
    for bare in [b"\x1b]7\x07".as_slice(), b"\x1b]07\x07", b"\x1b]+7\x1b\\"] {
        let mut parser = parser();
        parser.advance(b"\x1b]7;file:///home/a\x07");
        parser.advance(bare);
        assert_eq!(parser.cwd(), None, "{bare:?}");
        assert_eq!(parser.osc7_cwd(), None, "{bare:?}");
    }
}
