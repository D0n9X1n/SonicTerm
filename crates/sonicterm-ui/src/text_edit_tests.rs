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

/// Apply `operation` to `text` from `selection`, returning the edited text, outcome, and state.
fn edit_selection(
    text: &str,
    mut selection: TextSelection,
    operation: TextEdit,
    extending: bool,
) -> (String, EditOutcome, TextSelection) {
    let mut text = text.to_string();
    let outcome = if extending {
        selection.apply_extending(&mut text, operation)
    } else {
        // When: extending is false, the plain collapse/delete-selection contract applies.
        selection.apply(&mut text, operation)
    };
    (text, outcome, selection)
}

/// A selection spanning `start..end` with the caret at `end`, extended through the public API.
fn selected(text: &str, start: usize, end: usize) -> TextSelection {
    let mut selection = TextSelection::collapsed(start);
    selection.extend_to(text, end);
    selection
}

#[test]
fn selection_ranges_normalize_direction_and_scalar_boundaries() {
    // Reversed selections report ascending ranges, and mid-scalar offsets snap backward.
    let text = "a你🙂z";
    let forward = selected(text, 1, "a你🙂".len());
    let reversed = selected(text, "a你🙂".len(), 1);
    assert_eq!(forward.range(text), Some(1.."a你🙂".len()));
    assert_eq!(reversed.range(text), forward.range(text));
    assert_eq!(reversed.selected_text(text), Some("你🙂"));
    assert_eq!(reversed.caret(text), 1);

    let mid_scalar = selected(text, 2, "a你".len() + 2);
    assert_eq!(mid_scalar.range(text), Some(1.."a你".len()));
    assert_eq!(mid_scalar.selected_text(text), Some("你"));

    // A collapsed anchor selects nothing, so copy has no text to offer.
    let collapsed = selected(text, 1, 1);
    assert_eq!(collapsed.range(text), None);
    assert_eq!(collapsed.selected_text(text), None);
    assert_eq!(TextSelection::default().selected_text(text), None);
}

#[test]
fn stale_selection_offsets_normalize_against_replaced_text() {
    // An owner that replaces its string must never make a stored anchor slice past the end or mid-scalar.
    let mut selection = TextSelection::default();
    selection.select_all("hello world");
    for replacement in ["", "你", "ab"] {
        let caret = selection.caret(replacement);
        assert!(replacement.is_char_boundary(caret));
        assert_eq!(caret, replacement.len());
        let expected = (!replacement.is_empty()).then_some(0..replacement.len());
        assert_eq!(selection.range(replacement), expected);
    }
    // Offsets 2 and 4 both lie inside 你, so they clamp to 0 and 3 and still bound one scalar.
    let stale = selected("abcdef", 4, 2);
    assert_eq!(stale.range("你"), Some(0.."你".len()));
    assert_eq!(stale.caret("你"), 0);
    let mut shorter = String::from("你");
    let mut editing = stale;
    let outcome = editing.apply(&mut shorter, TextEdit::DeleteBackward);
    assert_eq!((shorter.as_str(), outcome), ("", EditOutcome { cursor: 0, changed: true }));
}

#[test]
fn every_deletion_removes_a_nonempty_selection_first() {
    // Each deletion variant deletes exactly the selected range, in either direction, and leaves the caret at its start.
    let text = "keep 你🙂 tail";
    let (start, end) = ("keep ".len(), "keep 你🙂".len());
    for operation in [
        TextEdit::DeleteBackward,
        TextEdit::DeleteBackwardDecomposing,
        TextEdit::DeleteForward,
        TextEdit::DeletePreviousWord,
        TextEdit::DeletePreviousUnicodeWord,
        TextEdit::DeleteToStart,
        TextEdit::DeleteToEnd,
    ] {
        for selection in [selected(text, start, end), selected(text, end, start)] {
            let (edited, outcome, after) = edit_selection(text, selection, operation, false);
            assert_eq!(edited, "keep  tail", "{operation:?}");
            assert_eq!(outcome, EditOutcome { cursor: start, changed: true }, "{operation:?}");
            assert_eq!(after.anchor(&edited), None, "{operation:?} clears the anchor");
        }
        // Extending a deletion is not meaningful, so it follows the plain selection-first contract.
        let (edited, _, _) = edit_selection(text, selected(text, start, end), operation, true);
        assert_eq!(edited, "keep  tail", "extending {operation:?}");
    }
}

#[test]
fn deletions_without_a_selection_keep_the_core_edit_behavior() {
    // A collapsed selection, including one with an equal anchor, edits exactly like apply_edit.
    let text = "keep foo.bar  suffix";
    let caret = "keep foo.bar  ".len();
    for operation in [
        TextEdit::DeleteBackward,
        TextEdit::DeleteBackwardDecomposing,
        TextEdit::DeleteForward,
        TextEdit::DeletePreviousWord,
        TextEdit::DeletePreviousUnicodeWord,
        TextEdit::DeleteToStart,
        TextEdit::DeleteToEnd,
    ] {
        let expected = edit(text, caret, operation);
        for selection in [TextSelection::collapsed(caret), selected(text, caret, caret)] {
            let (edited, outcome, after) = edit_selection(text, selection, operation, false);
            assert_eq!((edited.clone(), outcome), expected, "{operation:?}");
            assert_eq!(after.caret(&edited), outcome.cursor);
        }
    }
}

#[test]
fn plain_moves_collapse_selections_and_clear_the_anchor() {
    // Left/right collapse to the range edge; Home/End go to the field boundary; none keeps the anchor.
    let text = "ab你cd";
    let (start, end) = (1, "ab你".len());
    for selection in [selected(text, start, end), selected(text, end, start)] {
        for (operation, expected) in [
            (TextEdit::MoveBackward, start),
            (TextEdit::MoveForward, end),
            (TextEdit::MoveStart, 0),
            (TextEdit::MoveEnd, text.len()),
        ] {
            let (edited, outcome, after) = edit_selection(text, selection, operation, false);
            assert_eq!(edited, text);
            assert_eq!(outcome, EditOutcome { cursor: expected, changed: false }, "{operation:?}");
            assert_eq!(after.anchor(text), None, "{operation:?}");
            assert_eq!(after.caret(text), expected);
        }
    }
    // Without a selection, plain moves step one scalar as before.
    let collapsed = TextSelection::collapsed(end);
    let (_, outcome, _) = edit_selection(text, collapsed, TextEdit::MoveBackward, false);
    assert_eq!(outcome.cursor, "ab".len());
}

#[test]
fn extending_moves_keep_the_anchor_through_reversal_and_collapse() {
    // Shift-navigation anchors once; crossing back over the anchor reverses and collapsing keeps it.
    let mut text = String::from("a你b");
    let mut selection = TextSelection::collapsed(1);
    selection.apply_extending(&mut text, TextEdit::MoveForward);
    assert_eq!(selection.selected_text(&text), Some("你"));
    selection.apply_extending(&mut text, TextEdit::MoveBackward);
    assert_eq!(selection.range(&text), None, "collapsed onto the anchor");
    assert_eq!(selection.anchor(&text), Some(1), "the anchor survives collapse");
    selection.apply_extending(&mut text, TextEdit::MoveBackward);
    assert_eq!(selection.selected_text(&text), Some("a"));
    selection.apply_extending(&mut text, TextEdit::MoveEnd);
    assert_eq!(selection.selected_text(&text), Some("你b"));
    assert_eq!(text, "a你b", "navigation never edits text");
    selection.apply(&mut text, TextEdit::MoveEnd);
    assert_eq!(selection.anchor(&text), None, "a plain edit clears the anchor");
}

#[test]
fn select_all_set_cursor_and_replace_follow_the_field_contract() {
    // Replacement substitutes the range once and collapses after the inserted text.
    let mut text = String::from("alpha 你 omega");
    let mut selection = TextSelection::default();
    selection.select_all(&text);
    assert_eq!(selection.selected_text(&text), Some("alpha 你 omega"));
    let outcome = selection.replace(&mut text, "新名");
    let expected = EditOutcome { cursor: "新名".len(), changed: true };
    assert_eq!((text.as_str(), outcome), ("新名", expected));
    assert_eq!(selection.range(&text), None);

    selection.set_cursor(&text, 1);
    assert_eq!(selection.caret(&text), 0, "set_cursor normalizes a mid-scalar offset");
    let outcome = selection.replace(&mut text, "x");
    assert_eq!((text.as_str(), outcome.cursor), ("x新名", 1));
    assert!(
        !selection.replace(&mut text, "").changed,
        "empty insertion without a range is a no-op"
    );

    let mut empty = String::new();
    selection.select_all(&empty);
    assert_eq!(selection.range(&empty), None, "select-all of an empty field selects nothing");
    assert!(!selection.replace(&mut empty, "").changed);
}
