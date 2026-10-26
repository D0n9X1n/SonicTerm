//! Regression contracts for source target detection.

use super::*;

/// Local line fragments identify source locations without becoming part of the filename.
#[test]
fn local_line_fragments_preserve_file_identity() {
    for uri in [
        "C:/work/CLAUDE.md#L3",
        "C:/work/CLAUDE.md#3",
        "file:c://work/CLAUDE.md#L3",
        "file:///C:/work/CLAUDE.md#3",
    ] {
        let target = local_link_target(uri, PathStyle::Windows).unwrap().unwrap();
        assert!(
            matches!(target, DetectedTarget::SourceReference(reference)
            if reference.path.ends_with("/CLAUDE.md") && reference.line == 3),
            "{uri}"
        );
    }
    for uri in
        ["file:c://work/main.rs#L0", "file:///C:/work/main.rs#Lbad", "file:c://work/main.rs#L9-L3"]
    {
        assert!(local_link_target(uri, PathStyle::Windows).is_err(), "{uri}");
    }
    assert_eq!(
        local_link_target("file:///C:/work/a%23L3.txt", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/work/a#L3.txt".into())))
    );
    assert_eq!(local_link_target("https://example.com/a#L3", PathStyle::Windows), Ok(None));
    assert_eq!(
        local_link_target("C:/work/a#Lemon.txt", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/work/a#Lemon.txt".into())))
    );
    assert_eq!(
        local_link_target("file:///C:/work/notes%233", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/work/notes#3".into())))
    );
    assert!(find_targets_for_style("C:/work/notes#3", PathStyle::Windows)
        .iter()
        .any(|target| target.target == DetectedTarget::PathCandidate("C:/work/notes#3".into())));
}

/// Drive-rooted file: links use file-URI decoding, never browser dispatch or drive-relative resolution.
#[test]
fn file_colon_drive_links_are_local_on_windows() {
    for (uri, path) in [
        ("file:c://work/a%20b.txt", "c://work/a b.txt"),
        ("FILE:C:/work/a%2520b.txt", "C:/work/a%20b.txt"),
        ("file:c://", "c://"),
    ] {
        assert_eq!(
            local_link_target(uri, PathStyle::Windows),
            Ok(Some(DetectedTarget::PathCandidate(path.into())))
        );
        assert!(local_link_target(uri, PathStyle::Posix).is_err());
    }
    for uri in [
        "file:c:relative.txt",
        "file:notes.txt",
        "file://server/share/file.txt",
        "file:c://work/%2e%2e/other.txt",
        "file:c://work/a%2fb.txt",
        "file:c://work/a%00b.txt",
        "file:c://work/a%3astream",
        "file:c://work/a%2",
    ] {
        assert!(local_link_target(uri, PathStyle::Windows).is_err(), "{uri}");
    }
}

/// Encoded path structure cannot redirect a displayed file URI into another directory.
#[test]
fn file_uri_rejects_encoded_separators_and_traversal() {
    for uri in [
        "file:///tmp/%2E%2E%2Fetc/passwd",
        "file:///tmp/%2e%2e/file",
        "file:///tmp/a%5cb",
        "file:///tmp/../file",
    ] {
        assert!(local_link_target(uri, PathStyle::Posix).is_err(), "{uri}");
    }
    assert!(local_link_target("file:///C:/Users/%2E%2E/Windows", PathStyle::Windows).is_err());
    assert_eq!(
        local_link_target("file:///", PathStyle::Posix),
        Ok(Some(DetectedTarget::PathCandidate("/".into())))
    );
    assert_eq!(
        local_link_target("file:///C:/", PathStyle::Windows),
        Ok(Some(DetectedTarget::PathCandidate("C:/".into())))
    );
}

/// Explicit link destinations are classified whole, never through prose trimming or label fallback.
#[test]
fn local_link_destinations_preserve_native_paths_and_decode_file_uris_once() {
    for (input, expected) in [
        ("C://work/main.rs", "C://work/main.rs"),
        (r"C:\work\a%20b.txt", r"C:\work\a%20b.txt"),
        ("file:///C:/work/a%20b.txt", "C:/work/a b.txt"),
        ("file://localhost/C:/work/a%2520b.txt", "C:/work/a%20b.txt"),
    ] {
        assert_eq!(
            local_link_target(input, PathStyle::Windows),
            Ok(Some(DetectedTarget::PathCandidate(expected.into())))
        );
    }
    assert!(matches!(local_link_target("C://work/main.rs:7", PathStyle::Windows),
        Ok(Some(DetectedTarget::SourceReference(reference))) if reference.path == "C://work/main.rs" && reference.line == 7));
    assert_eq!(
        local_link_target("file:///tmp/a%20b.txt", PathStyle::Posix),
        Ok(Some(DetectedTarget::PathCandidate("/tmp/a b.txt".into())))
    );
    for target in [
        "file://server/share/a",
        "file:///C:/bad%00.txt",
        "file:///C:/bad%2",
        "file:///C:/x%3Astream",
        "C://work/a.exe:bad:2",
    ] {
        assert!(local_link_target(target, PathStyle::Windows).is_err(), "{target}");
    }
    for target in ["https://example.com/?a=1&b=2", "vscode://file/C:/a.rs", "C:notes.txt"] {
        assert_eq!(local_link_target(target, PathStyle::Windows), Ok(None));
    }
    assert_eq!(local_link_target("C://work/main.rs", PathStyle::Posix), Ok(None));
}

/// Return the full-span source reference offered for `col`, if the scanner produced one.
fn source_reference_at(
    text: &str,
    col: usize,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<SourceReference> {
    target_candidates_at_char_col_for_style(text, col, style, include_bare_names)
        .into_iter()
        .find_map(|matched| match matched.target {
            DetectedTarget::SourceReference(reference)
                if matched.start == 0 && matched.end == text.len() =>
            {
                Some(reference)
            }
            _ => None,
        })
}

/// Every pointed cell of a source reference resolves to the same typed location and full span.
#[test]
fn source_reference_suffix_is_typed_at_every_column() {
    // `end_line` carries ranges, `column` carries `:line:column`; the two never both apply.
    for (style, text, path, line, column, end_line) in [
        (PathStyle::Posix, "src/main.rs:12", "src/main.rs", 12, None, None),
        (PathStyle::Posix, "src/main.rs:12:4", "src/main.rs", 12, Some(4), None),
        (PathStyle::Posix, "src/main.rs:12-20", "src/main.rs", 12, None, Some(20)),
        // An en dash is the range separator terminals produce when rendering prose output.
        (PathStyle::Posix, "src/main.rs:12\u{2013}20", "src/main.rs", 12, None, Some(20)),
        // A degenerate range still ascends and stays a valid single-line span.
        (PathStyle::Posix, "/tmp/a.rs:7-7", "/tmp/a.rs", 7, None, Some(7)),
        // Leading zeros are ordinary decimal syntax rather than a malformed suffix.
        (PathStyle::Posix, "./a.rs:012", "./a.rs", 12, None, None),
        (PathStyle::Windows, r"C:\work\main.rs:12", r"C:\work\main.rs", 12, None, None),
        (PathStyle::Windows, r"C:\work\main.rs:12:4", r"C:\work\main.rs", 12, Some(4), None),
    ] {
        for col in 0..text.chars().count() {
            let reference = source_reference_at(text, col, style, true)
                .unwrap_or_else(|| panic!("missing source reference at column {col} in {text:?}"));
            assert_eq!(reference.path, path);
            assert_eq!(reference.display, text, "display keeps the whole pointed span");
            assert_eq!(reference.line, line);
            assert_eq!(reference.column, column);
            assert_eq!(reference.end_line, end_line);
            assert!(reference.explicit_path);
        }
    }
}

/// A numeric-looking but invalid suffix stays inert instead of decaying to a literal filename.
#[test]
fn malformed_source_suffix_never_falls_back_to_literal_path() {
    for text in [
        // Zero, descending ranges, `u32` overflow, and an unparsable extra segment.
        "src/main.rs:0",
        "src/main.rs:12:0",
        "src/main.rs:0:12",
        "src/main.rs:20-12",
        "src/main.rs:20\u{2013}12",
        "src/main.rs:99999999999999",
        "src/main.rs:12:99999999999999",
        "src/main.rs:12-",
        "src/main.rs:12-20-30",
        "src/main.rs:1x",
        "src/main.rs:12:abc",
    ] {
        for col in 0..text.chars().count() {
            assert!(
                target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true)
                    .is_empty(),
                "malformed suffix produced a candidate at column {col} in {text:?}"
            );
        }
    }
}

/// Contextual source references obey the same bare-name switch as contextual paths.
#[test]
fn bare_source_reference_requires_bare_names_enabled() {
    let text = "main.rs:12";
    let reference = source_reference_at(text, 0, PathStyle::Posix, true).expect("bare reference");
    assert_eq!(reference.path, "main.rs");
    assert_eq!(reference.line, 12);
    assert!(!reference.explicit_path, "a bare component is not explicit path syntax");
    // With bare names disabled the same text has no filesystem provenance at all.
    assert!(target_candidates_at_char_col_for_style(text, 0, PathStyle::Posix, false).is_empty());
}

/// Drive letters and alternate-data-stream syntax are not read as location metadata.
#[test]
fn windows_drive_and_stream_colons_are_not_source_metadata() {
    // A lone drive letter keeps its existing reading rather than becoming file `C` line 12.
    for style in [PathStyle::Posix, PathStyle::Windows] {
        assert!(source_reference_at("C:12", 0, style, true).is_none());
    }
    // A drive-rooted path without a suffix stays an ordinary explicit path candidate.
    let matches =
        target_candidates_at_char_col_for_style(r"C:\work\main.rs", 0, PathStyle::Windows, true);
    assert!(matches
        .iter()
        .any(|matched| matched.target == DetectedTarget::PathCandidate(r"C:\work\main.rs".into())));
    // `file.txt:12` is read as a location; Windows stream syntax shares the grammar.
    let reference = source_reference_at("file.txt:12", 0, PathStyle::Windows, true).unwrap();
    assert_eq!((reference.path.as_str(), reference.line), ("file.txt", 12));
}

/// URI provenance still wins over source-reference parsing on the same cell.
#[test]
fn uri_precedence_outranks_source_reference_parsing() {
    let text = "https://example.com/a.rs:12";
    for col in 0..text.chars().count() {
        let matches = target_candidates_at_char_col_for_style(text, col, PathStyle::Posix, true);
        assert!(
            matches.iter().all(|matched| matches!(matched.target, DetectedTarget::Uri(_))),
            "non-URI provenance at column {col} in {text:?}: {matches:?}"
        );
    }
}
