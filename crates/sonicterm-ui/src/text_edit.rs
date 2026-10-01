//! Shared, renderer-independent single-line text editing primitives.

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Editing operations supported by SonicTerm-owned single-line text fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEdit {
    MoveStart,
    MoveEnd,
    MoveBackward,
    MoveForward,
    DeleteBackward,
    /// Remove the final scalar of the previous grapheme's canonical decomposition.
    DeleteBackwardDecomposing,
    DeleteForward,
    DeletePreviousWord,
    /// Delete to AppKit's previous word boundary on macOS, or the preceding Unicode segment elsewhere.
    DeletePreviousUnicodeWord,
    DeleteToStart,
    DeleteToEnd,
}

impl TextEdit {
    /// Whether the edit only moves the caret, so a selection may extend through it.
    #[must_use]
    pub const fn is_navigation(self) -> bool {
        matches!(self, Self::MoveStart | Self::MoveEnd | Self::MoveBackward | Self::MoveForward)
    }
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

/// Caret plus optional selection anchor for one externally owned single-line string.
///
/// Offsets are UTF-8 bytes into the owning field's text. Every read and edit
/// normalizes them against the current text, so an anchor left behind after
/// the owner replaced its string with a shorter one never slices mid-scalar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextSelection {
    caret: usize,
    anchor: Option<usize>,
}

impl TextSelection {
    /// A collapsed selection with its caret at `caret`.
    #[must_use]
    pub const fn collapsed(caret: usize) -> Self {
        Self { caret, anchor: None }
    }

    /// Caret offset normalized to a scalar boundary of `text`.
    #[must_use]
    pub fn caret(&self, text: &str) -> usize {
        normalize_cursor(text, self.caret)
    }

    /// Anchor offset normalized to a scalar boundary of `text`, when one is set.
    #[must_use]
    pub fn anchor(&self, text: &str) -> Option<usize> {
        self.anchor.map(|anchor| normalize_cursor(text, anchor))
    }

    /// Nonempty selected byte range in ascending order, whichever direction it was extended.
    #[must_use]
    pub fn range(&self, text: &str) -> Option<std::ops::Range<usize>> {
        let caret = self.caret(text);
        let anchor = self.anchor(text)?;
        // An anchor equal to the caret is a collapsed selection, which selects nothing.
        (anchor != caret).then(|| anchor.min(caret)..anchor.max(caret))
    }

    /// Selected text, or `None` when the selection is empty.
    #[must_use]
    pub fn selected_text<'text>(&self, text: &'text str) -> Option<&'text str> {
        self.range(text).map(|range| &text[range])
    }

    /// Move the caret to `caret` and clear the anchor.
    pub fn set_cursor(&mut self, text: &str, caret: usize) {
        self.caret = normalize_cursor(text, caret);
        self.anchor = None;
    }

    /// Move the caret to `caret`, anchoring at the current caret if no anchor exists.
    pub fn extend_to(&mut self, text: &str, caret: usize) {
        self.anchor = Some(self.anchor(text).unwrap_or_else(|| self.caret(text)));
        self.caret = normalize_cursor(text, caret);
    }

    /// Select the whole string, leaving the caret at its end.
    pub fn select_all(&mut self, text: &str) {
        self.anchor = Some(0);
        self.caret = text.len();
    }

    /// Apply one plain edit: moves collapse a selection and deletions remove it first.
    ///
    /// `MoveBackward` and `MoveForward` collapse a nonempty selection to its
    /// start or end; `MoveStart` and `MoveEnd` go to the field boundaries.
    /// Every deletion variant removes a nonempty selection and leaves the caret
    /// at its start. Without a selection the edit is [`apply_edit`]. The anchor
    /// is always cleared.
    pub fn apply(&mut self, text: &mut String, edit: TextEdit) -> EditOutcome {
        let range = self.range(text);
        let caret = self.caret(text);
        self.anchor = None;
        let result = match (range, edit) {
            (None, _) => apply_edit(text, caret, edit),
            (Some(range), TextEdit::MoveBackward) => outcome(range.start, false),
            (Some(range), TextEdit::MoveForward) => outcome(range.end, false),
            (Some(_), TextEdit::MoveStart | TextEdit::MoveEnd) => apply_edit(text, caret, edit),
            (
                Some(range),
                TextEdit::DeleteBackward
                | TextEdit::DeleteBackwardDecomposing
                | TextEdit::DeleteForward
                | TextEdit::DeletePreviousWord
                | TextEdit::DeletePreviousUnicodeWord
                | TextEdit::DeleteToStart
                | TextEdit::DeleteToEnd,
            ) => {
                let start = range.start;
                text.drain(range);
                outcome(start, true)
            }
        };
        self.caret = result.cursor;
        result
    }

    /// Apply a selection-extending edit: navigation moves only the caret around a kept anchor.
    ///
    /// The anchor is set at the current caret when absent and persists through
    /// reversal or collapse. Deletions are not extensible and behave as [`Self::apply`].
    pub fn apply_extending(&mut self, text: &mut String, edit: TextEdit) -> EditOutcome {
        if !edit.is_navigation() {
            // When: edit deletes text, extension has no meaning, so the plain deletion contract applies.
            return self.apply(text, edit);
        }
        let caret = self.caret(text);
        self.anchor = Some(self.anchor(text).unwrap_or(caret));
        let result = apply_edit(text, caret, edit);
        self.caret = result.cursor;
        result
    }

    /// Replace the selection, or insert at the caret, with `replacement`, then collapse after it.
    ///
    /// Reports `changed` when a nonempty range or nonempty replacement altered the text.
    pub fn replace(&mut self, text: &mut String, replacement: &str) -> EditOutcome {
        let caret = self.caret(text);
        let range = self.range(text).unwrap_or(caret..caret);
        let changed = !range.is_empty() || !replacement.is_empty();
        let start = range.start;
        text.replace_range(range, replacement);
        self.anchor = None;
        self.caret = start + replacement.len();
        outcome(self.caret, changed)
    }
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
        TextEdit::DeleteBackwardDecomposing => {
            // When: DeleteBackwardDecomposing owns the edit, preserve native canonical components instead of whole-character deletion.
            let Some((start, previous)) = text[..cursor].grapheme_indices(true).next_back() else {
                // When: cursor has no preceding grapheme, decomposition leaves the text unchanged.
                return outcome(cursor, false);
            };
            let mut decomposed: String = previous.nfd().collect();
            let _ = decomposed.pop();
            text.replace_range(start..cursor, &decomposed);
            outcome(start + decomposed.len(), true)
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
                .find_map(|(index, character)| {
                    character.is_whitespace().then_some(index + character.len_utf8())
                })
                .unwrap_or(0);
            if word_start < cursor {
                text.drain(word_start..cursor);
                outcome(word_start, true)
            } else {
                // When: `word_start` equals `cursor`, no preceding word or whitespace remains to remove.
                outcome(cursor, false)
            }
        }
        TextEdit::DeletePreviousUnicodeWord => {
            // When: DeletePreviousUnicodeWord owns the edit, native word boundaries must resolve before changing text.
            let Some(start) = previous_unicode_word_boundary(text, cursor) else {
                // When: previous_unicode_word_boundary cannot map the native offset, preserve text rather than split a character.
                return outcome(cursor, false);
            };
            if start < cursor {
                text.drain(start..cursor);
                outcome(start, true)
            } else {
                // When: start equals cursor, no previous Unicode segment or whitespace remains.
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

#[cfg(target_os = "macos")]
fn previous_unicode_word_boundary(text: &str, cursor: usize) -> Option<usize> {
    use objc2_app_kit::NSAttributedStringAppKitAdditions;
    use objc2_foundation::{NSAttributedString, NSString};

    // AppKit owns punctuation and dictionary-based word boundaries; this string query creates no native view.
    let string = NSString::from_str(text);
    let attributed = NSAttributedString::from_nsstring(&string);
    let prefix = &text[..cursor];
    let boundary = attributed.nextWordFromIndex_forward(prefix.encode_utf16().count(), false);
    utf16_boundary_to_utf8(prefix, boundary)
}

#[cfg(target_os = "macos")]
fn utf16_boundary_to_utf8(text: &str, boundary: usize) -> Option<usize> {
    let mut utf16_index = 0;
    for (byte_index, character) in text.char_indices() {
        if utf16_index == boundary {
            // When: boundary matches a scalar start, byte_index cannot split a surrogate or UTF-8 character.
            return Some(byte_index);
        }
        utf16_index += character.len_utf16();
        if utf16_index > boundary {
            // When: boundary splits a surrogate pair, refuse deletion instead of truncating its character.
            return None;
        }
    }
    (utf16_index == boundary).then_some(text.len())
}

#[cfg(not(target_os = "macos"))]
fn previous_unicode_word_boundary(text: &str, cursor: usize) -> Option<usize> {
    let prefix = &text[..cursor];
    let word_end = prefix.trim_end_matches(char::is_whitespace).len();
    Some(prefix[..word_end].split_word_bound_indices().next_back().map_or(0, |(start, _)| start))
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
