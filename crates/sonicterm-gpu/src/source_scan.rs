//! Test-only Rust source scanning for source checks that pin a call pattern in production code.
//!
//! [`code_only`] blanks comments and the contents of string and char literals, so prose and
//! literal text can neither satisfy nor fail a check, and [`token_matches`] finds a token sequence
//! whatever whitespace or line breaks sit between its tokens. Only this crate compiles it, so the
//! coverage report lists it once.

use std::ops::Range;

/// `source` with every comment and the contents of every string, byte-string, raw-string and char
/// literal replaced by spaces, keeping each line break and the byte length, so an offset or line
/// number in the result is the same one in `source`. Literal delimiters stay, so code after them is
/// kept; nested block comments, escapes and raw strings with any number of `#` are honoured, and a
/// lifetime such as `'a` is code, not a char literal.
pub(crate) fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut kept = bytes.to_vec();
    let mut index = 0;
    while index < bytes.len() {
        if let Some((body, end)) = raw_string_span(source, index) {
            // When: a raw string opens at `index`, blank its body between the quotes and hashes.
            blank(&mut kept, body);
            index = end;
            continue;
        }
        let next = bytes.get(index + 1).copied();
        match bytes[index] {
            b'/' if next == Some(b'/') => {
                // When: `next` starts a line comment, blank it up to its line break.
                let end = source[index..].find('\n').map_or(bytes.len(), |offset| index + offset);
                blank(&mut kept, index..end);
                index = end;
            }
            b'/' if next == Some(b'*') => {
                // When: `next` starts a block comment, blank it and every comment nested in it.
                let end = block_comment_end(bytes, index + 2);
                blank(&mut kept, index..end);
                index = end;
            }
            b'"' => {
                // A `b` or `c` prefix is an identifier byte passed over earlier, so `b"..."` lands here.
                let end = string_end(bytes, index + 1);
                blank(&mut kept, index + 1..end.saturating_sub(1).max(index + 1));
                index = end;
            }
            b'\'' => match char_literal_end(source, index) {
                Some(end) => {
                    // When: a char literal opens at `index`, blank what sits between its quotes.
                    blank(&mut kept, index + 1..end - 1);
                    index = end;
                }
                None => index += 1,
            },
            _ => index += 1,
        }
    }
    // Every blanked range starts and ends on an ASCII delimiter, so no multibyte char is split.
    String::from_utf8(kept).expect("blanking whole literals and comments keeps UTF-8 intact")
}

/// Replace every byte of `range` in `bytes` except line breaks with a space.
fn blank(bytes: &mut [u8], range: Range<usize>) {
    for byte in &mut bytes[range] {
        if *byte != b'\n' && *byte != b'\r' {
            // When: the byte is not a line break, it becomes a space so line numbers survive.
            *byte = b' ';
        }
    }
}

/// Byte index just past the `*/` closing a block comment whose body starts at `start`. Rust block
/// comments nest, so each inner `/*` needs its own `*/` before the outer comment ends.
fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 1_usize;
    let mut index = start;
    while index < bytes.len() {
        match (bytes[index], bytes.get(index + 1).copied()) {
            (b'/', Some(b'*')) => {
                depth += 1;
                index += 2;
            }
            (b'*', Some(b'/')) => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    // When: `depth` reaches zero, the outermost comment has closed.
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    bytes.len()
}

/// The body and the end of a raw string opening at `index` (`r"..."`, `r#"..."#`, `br#"..."#` or
/// `cr#"..."#`): the byte range between its quotes, and the byte index just past its last `#`.
/// `None` when no raw string opens there, as for an identifier such as `r#type` or `bar`.
fn raw_string_span(source: &str, index: usize) -> Option<(Range<usize>, usize)> {
    let bytes = source.as_bytes();
    if index > 0 && is_ident_byte(bytes[index - 1]) {
        // When: the previous byte continues an identifier, this letter is no literal prefix.
        return None;
    }
    let prefix_len = match (bytes[index], bytes.get(index + 1).copied()) {
        (b'r', _) => 1,
        (b'b' | b'c', Some(b'r')) => 2,
        _ => {
            // When: the bytes are no `r`, `br` or `cr` prefix, no raw string opens here.
            return None;
        }
    };
    let hash_count = bytes[index + prefix_len..].iter().take_while(|byte| **byte == b'#').count();
    if bytes.get(index + prefix_len + hash_count) != Some(&b'"') {
        // When: no `"` follows the prefix and hashes, this is an identifier such as `r#type`.
        return None;
    }
    let closing = format!("\"{}", "#".repeat(hash_count));
    let body_start = index + prefix_len + hash_count + 1;
    match source[body_start..].find(&closing) {
        Some(body_len) => {
            let body_end = body_start + body_len;
            Some((body_start..body_end, body_end + closing.len()))
        }
        None => Some((body_start..bytes.len(), bytes.len())),
    }
}

/// Byte index just past the `"` closing a string whose body starts at `start`, honouring escapes.
fn string_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// Byte index just past a char or byte-char literal whose opening `'` is at `quote`, or `None`
/// when the quote starts a lifetime or label such as `'a` or `'outer:` instead.
fn char_literal_end(source: &str, quote: usize) -> Option<usize> {
    let rest = &source[quote + 1..];
    if let Some(escape_body) = rest.strip_prefix('\\') {
        // When: the literal is escaped (`'\''`, `'\n'`, `'\u{41}'`), its quote follows the escape.
        let escape_len = escape_body.chars().next()?.len_utf8();
        let close = escape_body[escape_len..].find('\'')?;
        return Some(quote + 2 + escape_len + close + 1);
    }
    let first = rest.chars().next()?;
    let after_first = quote + 1 + first.len_utf8();
    // A single char then a quote is a literal; `'a,` or `'a>` is a lifetime and stays code.
    (source.as_bytes().get(after_first) == Some(&b'\'') && first != '\'').then_some(after_first + 1)
}

/// Whether `byte` can continue an identifier, so an `r` after it is not a raw-string prefix.
fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// One token of code: an identifier or number, or a single punctuation char, with its byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token<'source> {
    /// The token's text.
    pub(crate) text: &'source str,
    /// Byte offset of the token's first char in the scanned text.
    pub(crate) offset: usize,
}

/// The tokens of `code`, skipping whitespace, so a match never depends on spacing or line breaks.
pub(crate) fn tokens(code: &str) -> Vec<Token<'_>> {
    let mut found = Vec::new();
    let mut chars = code.char_indices().peekable();
    while let Some((start, current)) = chars.next() {
        if current.is_whitespace() {
            // When: `current` is whitespace, it separates tokens and is none itself.
            continue;
        }
        let mut end = start + current.len_utf8();
        if current.is_alphanumeric() || current == '_' {
            // When: `current` starts a word, the token runs to the end of the word.
            while let Some(&(next_start, next)) = chars.peek() {
                if !(next.is_alphanumeric() || next == '_') {
                    // When: `next` cannot continue the word, the word token ends before it.
                    break;
                }
                end = next_start + next.len_utf8();
                chars.next();
            }
        }
        found.push(Token { text: &code[start..end], offset: start });
    }
    found
}

/// Byte offsets in `code` where the token sequence of `pattern` starts. Whole tokens are compared,
/// so `grid.row(` matches `frame.grid\n    .row(0)` but not `my_grid.row(` or `grid.rows(`.
pub(crate) fn token_matches(code: &str, pattern: &str) -> Vec<usize> {
    let wanted = tokens(pattern);
    let present = tokens(code);
    if wanted.is_empty() {
        // When: `pattern` has no token, it can match nothing.
        return Vec::new();
    }
    present
        .windows(wanted.len())
        .filter(|window| window.iter().zip(&wanted).all(|(have, want)| have.text == want.text))
        .map(|window| window[0].offset)
        .collect()
}

/// The 1-based line of byte `offset` in `text`.
pub(crate) fn line_number(text: &str, offset: usize) -> usize {
    1 + text.as_bytes()[..offset.min(text.len())].iter().filter(|byte| **byte == b'\n').count()
}

/// The byte range of the braced block whose `{` is the first at or after `from` in `code`, through
/// its matching `}`; `None` when there is no `{` or the block never closes. `code` must come from
/// [`code_only`], so a brace inside a comment or literal is already blanked and never counted.
pub(crate) fn block_span(code: &str, from: usize) -> Option<Range<usize>> {
    let open = from + code[from..].find('{')?;
    let mut depth = 0_usize;
    for (offset, byte) in code.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // When: `depth` returns to zero, this brace closes the block opened at `open`.
                    return Some(open..open + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
#[path = "source_scan_tests.rs"]
mod source_scan_tests;
