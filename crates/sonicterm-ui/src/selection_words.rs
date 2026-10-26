//! Grid-independent word-boundary rules shared by point and drag selection.

/// Connector characters that count as part of a word in addition to
/// alphanumerics. These are common in filesystem paths and identifiers
/// (`foo-bar`, `a.b.c`, `/usr/local`, `http://`, `~/.config`), so a
/// double-click grabs the whole token rather than stopping at the first
/// punctuation. Tweak this set to adjust double-click word semantics.
/// Mirrors WezTerm's default `selection_word_boundary` spirit.
const WORD_CONNECTORS: &[char] = &['_', '-', '.', '/', ':', '~'];

/// True when `character` should be treated as part of a word for double-click
/// selection: any Unicode alphanumeric, or one of `WORD_CONNECTORS`.
/// Whitespace and other punctuation are word boundaries.
pub fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || WORD_CONNECTORS.contains(&character)
}

/// Find the inclusive `[left, right]` column span of the word containing
/// `col` in `chars`. Pure and grid-free so it is trivially unit-testable.
///
/// - If `chars[col]` is a word char, expands left and right over the
///   maximal run of word chars.
/// - If `chars[col]` is a boundary (space / punctuation), returns
///   `(col, col)` — a single cell, never expanding across whitespace.
/// - An empty slice returns `(0, 0)`.
pub fn word_bounds(chars: &[char], col: usize) -> (usize, usize) {
    if chars.is_empty() {
        // When: chars is empty, there is no cell to expand from, so report the
        // degenerate span at the origin.
        return (0, 0);
    }
    let col = col.min(chars.len() - 1);
    if !is_word_char(chars[col]) {
        // When: is_word_char rejects the clicked cell, the span stays that one
        // cell instead of swallowing the surrounding whitespace.
        return (col, col);
    }
    let mut left = col;
    while left > 0 && is_word_char(chars[left - 1]) {
        left -= 1;
    }
    let mut right = col;
    while right + 1 < chars.len() && is_word_char(chars[right + 1]) {
        right += 1;
    }
    (left, right)
}

#[cfg(test)]
#[path = "selection_words_tests.rs"]
mod selection_words_tests;
