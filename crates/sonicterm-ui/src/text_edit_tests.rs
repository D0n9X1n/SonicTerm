use super::*;

fn edit(text: &str, cursor: usize, operation: TextEdit) -> (String, EditOutcome) {
    let mut text = text.to_string();
    let outcome = apply_edit(&mut text, cursor, operation);
    (text, outcome)
}

#[test]
fn movement_uses_utf8_boundaries() {
    let text = "a你🙂z";
    assert_eq!(edit(text, text.len(), TextEdit::MoveStart).1.cursor, 0);
    assert_eq!(edit(text, 0, TextEdit::MoveEnd).1.cursor, text.len());
    assert_eq!(edit(text, "a你🙂".len(), TextEdit::MoveBackward).1.cursor, "a你".len());
    assert_eq!(edit(text, "a".len(), TextEdit::MoveForward).1.cursor, "a你".len());
}

#[test]
fn character_deletion_preserves_multibyte_neighbors() {
    let (text, outcome) = edit("a你🙂z", "a你🙂".len(), TextEdit::DeleteBackward);
    assert_eq!(text, "a你z");
    assert_eq!(outcome, EditOutcome { cursor: "a你".len(), changed: true });

    let (text, outcome) = edit("a你🙂z", "a".len(), TextEdit::DeleteForward);
    assert_eq!(text, "a🙂z");
    assert_eq!(outcome, EditOutcome { cursor: 1, changed: true });
}

#[test]
fn previous_word_deletes_whitespace_then_one_non_whitespace_run() {
    let (text, outcome) =
        edit("keep foo.bar  suffix", "keep foo.bar  ".len(), TextEdit::DeletePreviousWord);
    assert_eq!(text, "keep suffix");
    assert_eq!(outcome, EditOutcome { cursor: "keep ".len(), changed: true });

    let (text, outcome) =
        edit("你好 🙂世界  tail", "你好 🙂世界  ".len(), TextEdit::DeletePreviousWord);
    assert_eq!(text, "你好 tail");
    assert_eq!(outcome, EditOutcome { cursor: "你好 ".len(), changed: true });
}

#[test]
fn previous_word_handles_empty_and_whitespace_only_inputs() {
    assert_eq!(
        edit("", 0, TextEdit::DeletePreviousWord),
        (String::new(), EditOutcome { cursor: 0, changed: false })
    );
    assert_eq!(
        edit("   ", 3, TextEdit::DeletePreviousWord),
        (String::new(), EditOutcome { cursor: 0, changed: true })
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unicode_word_deletion_retains_punctuation_boundaries_and_suffix() {
    // The portable operation uses Unicode segments without changing Ctrl+W's whitespace-run contract.
    for (text, prefix, expected) in [
        ("keep foo/bar  suffix", "keep foo/bar  ", "keep foo/suffix"),
        ("keep can't suffix", "keep can't", "keep  suffix"),
        ("保留你好后文", "保留你好", "保留你后文"),
        ("keep cafe\u{301} suffix", "keep cafe\u{301}", "keep  suffix"),
        ("keep foo/bar!!! suffix", "keep foo/bar!!!", "keep foo/bar!! suffix"),
    ] {
        let (edited, outcome) = edit(text, prefix.len(), TextEdit::DeletePreviousUnicodeWord);
        assert_eq!(edited, expected, "{text:?}");
        assert!(outcome.changed);
        assert!(edited.is_char_boundary(outcome.cursor));
        assert_eq!(&edited[outcome.cursor..], &text[prefix.len()..]);
    }
    assert_eq!(edit("keep foo/bar", "keep foo/bar".len(), TextEdit::DeletePreviousWord).0, "keep ",);
}

#[cfg(target_os = "macos")]
#[test]
fn mac_word_deletion_matches_native_text_view_boundaries() {
    // AppKit selector measurements pin punctuation, dictionary-based CJK, and emoji word boundaries.
    for (prefix, retained) in [
        ("keep foo/bar!!!", "keep foo/"),
        ("keep foo/bar  ", "keep foo/"),
        ("keep foo/", "keep "),
        ("keep foo.bar", "keep "),
        ("keep foo_bar", "keep "),
        ("keep foo!!!", "keep "),
        ("keep !!!", ""),
        ("!!!", ""),
        ("保留你好", "保留"),
        ("keep \u{1f469}\u{200d}\u{1f4bb}!!!", "keep "),
        ("keep é", "keep "),
        ("keep ấ", "keep "),
        ("keep e\u{301}", "keep "),
        ("keep \u{1f44d}\u{1f3fd}", "keep "),
        ("   ", ""),
        ("", ""),
    ] {
        let text = format!("{prefix} tail");
        assert_eq!(
            edit(&text, prefix.len(), TextEdit::DeletePreviousUnicodeWord),
            (
                format!("{retained} tail"),
                EditOutcome { cursor: retained.len(), changed: retained != prefix },
            ),
            "{prefix:?}",
        );
    }
    assert_eq!(edit("keep foo/bar", "keep foo/bar".len(), TextEdit::DeletePreviousWord).0, "keep ");
}

#[cfg(target_os = "macos")]
#[test]
fn native_word_boundary_conversion_rejects_surrogate_interiors() {
    // Native UTF-16 offsets must resolve exactly, never rounding through an astral character or past the caret.
    let text = "a\u{1f642}你";
    for (utf16, utf8) in [(0, 0), (1, 1), (3, 5), (4, 8)] {
        assert_eq!(utf16_boundary_to_utf8(text, utf16), Some(utf8));
    }
    assert_eq!(utf16_boundary_to_utf8(text, 2), None);
    assert_eq!(utf16_boundary_to_utf8(text, 5), None);
    assert_eq!(utf16_boundary_to_utf8(text, usize::MAX), None);
}

#[test]
fn unicode_word_deletion_handles_empty_whitespace_and_mid_codepoint_carets() {
    // Empty prefixes are no-ops; malformed carets normalize before choosing a Unicode word boundary.
    for text in ["", "suffix"] {
        assert_eq!(
            edit(text, 0, TextEdit::DeletePreviousUnicodeWord),
            (text.to_owned(), EditOutcome { cursor: 0, changed: false }),
        );
    }
    assert_eq!(
        edit(" \t\u{3000}suffix", " \t\u{3000}".len(), TextEdit::DeletePreviousUnicodeWord),
        ("suffix".to_owned(), EditOutcome { cursor: 0, changed: true }),
    );
    assert_eq!(
        edit("a你b", 2, TextEdit::DeletePreviousUnicodeWord),
        ("你b".to_owned(), EditOutcome { cursor: 0, changed: true }),
    );
}

#[test]
fn decomposing_delete_removes_one_canonical_component_and_preserves_suffix() {
    // Precomposed and combining spellings have the same deletion result without normalizing their neighbors.
    for (previous, remaining) in [
        ("é", "e"),
        ("e\u{301}", "e"),
        ("ấ", "a\u{302}"),
        ("각", "\u{1100}\u{1161}"),
        ("x", ""),
        ("中", ""),
    ] {
        let text = format!("a\u{301}{previous}tail");
        let cursor = "a\u{301}".len() + previous.len();
        assert_eq!(
            edit(&text, cursor, TextEdit::DeleteBackwardDecomposing),
            (
                format!("a\u{301}{remaining}tail"),
                EditOutcome { cursor: "a\u{301}".len() + remaining.len(), changed: true },
            ),
            "{previous:?}",
        );
    }
}

#[test]
fn decomposing_delete_preserves_joiners_as_individual_components() {
    // AppKit removes the final scalar only; the retained ZWJ belongs to the next decomposing deletion.
    let joined = "\u{1f469}\u{200d}\u{1f4bb}";
    let remaining = "\u{1f469}\u{200d}";
    let text = format!("{joined}tail");
    assert_eq!(
        edit(&text, joined.len(), TextEdit::DeleteBackwardDecomposing),
        (format!("{remaining}tail"), EditOutcome { cursor: remaining.len(), changed: true }),
    );
    assert_eq!(
        edit(&format!("{remaining}tail"), remaining.len(), TextEdit::DeleteBackwardDecomposing),
        ("\u{1f469}tail".to_owned(), EditOutcome { cursor: '\u{1f469}'.len_utf8(), changed: true }),
    );
    assert_eq!(edit("\u{1f44d}\u{1f3fd}", 8, TextEdit::DeleteBackwardDecomposing).0, "\u{1f44d}",);
}

#[test]
fn decomposing_delete_handles_empty_and_mid_codepoint_carets() {
    // Decomposition respects the existing UTF-8 caret normalization and the immutable right-hand suffix.
    assert_eq!(
        edit("", 0, TextEdit::DeleteBackwardDecomposing),
        (String::new(), EditOutcome { cursor: 0, changed: false }),
    );
    assert_eq!(
        edit("éz", 1, TextEdit::DeleteBackwardDecomposing),
        ("éz".to_owned(), EditOutcome { cursor: 0, changed: false }),
    );
    assert_eq!(
        edit("xéz", 2, TextEdit::DeleteBackwardDecomposing),
        ("éz".to_owned(), EditOutcome { cursor: 0, changed: true }),
    );
    assert_eq!(
        edit("é", usize::MAX, TextEdit::DeleteBackwardDecomposing),
        ("e".to_owned(), EditOutcome { cursor: 1, changed: true }),
    );
}

#[test]
fn line_kills_preserve_the_other_side_of_the_caret() {
    let (text, outcome) = edit("alpha中omega", "alpha中".len(), TextEdit::DeleteToStart);
    assert_eq!(text, "omega");
    assert_eq!(outcome, EditOutcome { cursor: 0, changed: true });

    let (text, outcome) = edit("alpha中omega", "alpha".len(), TextEdit::DeleteToEnd);
    assert_eq!(text, "alpha");
    assert_eq!(outcome, EditOutcome { cursor: "alpha".len(), changed: true });
}

#[test]
fn boundary_noops_report_no_text_change() {
    assert!(!edit("abc", 0, TextEdit::DeleteBackward).1.changed);
    assert!(!edit("abc", 3, TextEdit::DeleteForward).1.changed);
    assert!(!edit("abc", 0, TextEdit::DeleteToStart).1.changed);
    assert!(!edit("abc", 3, TextEdit::DeleteToEnd).1.changed);
}

#[test]
fn malformed_carets_normalize_backward_before_editing() {
    let text = "a你b";
    assert_eq!(normalize_cursor(text, usize::MAX), text.len());
    assert_eq!(normalize_cursor(text, 2), 1);

    let (text, outcome) = edit(text, 2, TextEdit::DeleteForward);
    assert_eq!(text, "ab");
    assert_eq!(outcome, EditOutcome { cursor: 1, changed: true });
}
