//! Shared, renderer-independent single-line text editing primitives.

/// Core terminal-style edits supported by SonicTerm-owned text fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEdit {
    MoveStart,
    MoveEnd,
    MoveBackward,
    MoveForward,
    DeleteBackward,
    DeleteForward,
    DeletePreviousWord,
    DeleteToStart,
    DeleteToEnd,
}

/// Result of applying one [`TextEdit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditOutcome {
    /// UTF-8 byte offset of the caret after the edit.
    pub cursor: usize,
    /// Whether the text, rather than only the caret, changed.
    pub changed: bool,
}

/// Clamp `caret` to the string and move it backward to a UTF-8 boundary.
#[must_use]
pub fn normalize_cursor(text: &str, caret: usize) -> usize {
    let mut caret = caret.min(text.len());
    while !text.is_char_boundary(caret) {
        caret -= 1;
    }
    caret
}

/// Apply one core terminal-style edit to `text` at a UTF-8 byte caret.
///
/// `DeletePreviousWord` follows shell/readline behavior: it removes whitespace
/// immediately left of the caret, then the preceding contiguous non-whitespace
/// run. Invalid or mid-codepoint carets are normalized backward first.
#[must_use]
pub fn apply_edit(text: &mut String, caret: usize, edit: TextEdit) -> EditOutcome {
    let cursor = normalize_cursor(text, caret);
    match edit {
        TextEdit::MoveStart => outcome(0, false),
        TextEdit::MoveEnd => outcome(text.len(), false),
        TextEdit::MoveBackward => outcome(previous_boundary(text, cursor), false),
        TextEdit::MoveForward => outcome(next_boundary(text, cursor), false),
        TextEdit::DeleteBackward => {
            let start = previous_boundary(text, cursor);
            if start < cursor {
                text.drain(start..cursor);
                outcome(start, true)
            } else {
                // When: `start` equals `cursor`, there is no previous character to delete.
                outcome(cursor, false)
            }
        }
        TextEdit::DeleteForward => {
            let end = next_boundary(text, cursor);
            if cursor < end {
                text.drain(cursor..end);
                outcome(cursor, true)
            } else {
                // When: `cursor` equals `end`, there is no following character to delete.
                outcome(cursor, false)
            }
        }
        TextEdit::DeletePreviousWord => {
            let prefix = &text[..cursor];
            let word_end = prefix.trim_end_matches(char::is_whitespace).len();
            let word_start = prefix[..word_end]
                .char_indices()
                .rev()
                .find_map(|(index, ch)| ch.is_whitespace().then_some(index + ch.len_utf8()))
                .unwrap_or(0);
            if word_start < cursor {
                text.drain(word_start..cursor);
                outcome(word_start, true)
            } else {
                // When: `word_start` equals `cursor`, no preceding word or whitespace remains to remove.
                outcome(cursor, false)
            }
        }
        TextEdit::DeleteToStart => {
            if cursor > 0 {
                text.drain(..cursor);
                outcome(0, true)
            } else {
                // When: `cursor` is already zero, deleting to start leaves text and caret unchanged.
                outcome(0, false)
            }
        }
        TextEdit::DeleteToEnd => {
            if cursor < text.len() {
                text.truncate(cursor);
                outcome(cursor, true)
            } else {
                // When: `cursor` equals `text.len()`, deleting to end leaves the buffer unchanged.
                outcome(cursor, false)
            }
        }
    }
}

fn previous_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor].char_indices().next_back().map(|(index, _)| index).unwrap_or(0)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..].char_indices().nth(1).map(|(index, _)| cursor + index).unwrap_or(text.len())
}

const fn outcome(cursor: usize, changed: bool) -> EditOutcome {
    EditOutcome { cursor, changed }
}

#[cfg(test)]
#[path = "text_edit_tests.rs"]
mod text_edit_tests;
