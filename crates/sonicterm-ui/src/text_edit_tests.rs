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
