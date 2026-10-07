//! Test-only source pins: read a production Rust file as code only and extract one item's body, so a test can
//! assert how production code is wired without a comment, a literal or a second copy of the item standing in.
//!
//! The masker walks the source by `char`, never by byte, so a multi-byte character or identifier anywhere in the
//! file is kept intact and can never be split by a slice.

/// Whether `character` can continue an identifier, so a following `r`, `b` or `c` is part of that identifier and
/// not a literal prefix. Unicode identifiers count, as they do for the Rust lexer.
fn continues_identifier(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// The char index just past a block comment opening at `start`. Block comments nest, so the comment ends where its
/// depth returns to zero; an unterminated comment runs to the end of the source.
fn block_comment_end(chars: &[char], start: usize) -> usize {
    let mut depth = 0_usize;
    let mut cursor = start;
    while cursor + 1 < chars.len() {
        match (chars[cursor], chars[cursor + 1]) {
            ('/', '*') => {
                depth += 1;
                cursor += 2;
            }
            ('*', '/') => {
                depth -= 1;
                cursor += 2;
                // When: the depth returns to zero, the outermost `*/` closed the comment.
                if depth == 0 {
                    return cursor;
                }
            }
            _ => cursor += 1,
        }
    }
    chars.len()
}

/// The char index just past a raw string whose `r` is at `raw_at`: `r`, then any number of `#`, then `"`, closed by
/// `"` and the same number of `#`. None when `#` is not followed by `"`, which makes `r#name` a raw identifier.
fn raw_string_end(chars: &[char], raw_at: usize) -> Option<usize> {
    let hashes = chars[raw_at + 1..].iter().take_while(|character| **character == '#').count();
    let quote = raw_at + 1 + hashes;
    // When: the hashes are not followed by a quote, this is a raw identifier, which is code.
    if chars.get(quote) != Some(&'"') {
        return None;
    }
    let mut cursor = quote + 1;
    while cursor < chars.len() {
        let closes = chars[cursor] == '"'
            && chars[cursor + 1..]
                .iter()
                .take(hashes)
                .filter(|character| **character == '#')
                .count()
                == hashes;
        // When: a quote is followed by the opening's hash count, the raw string ends after those hashes.
        if closes {
            return Some(cursor + 1 + hashes);
        }
        cursor += 1;
    }
    Some(chars.len())
}

/// The char index just past an escaped (`"…"`, `b"…"`, `c"…"`) string whose opening quote is at `quote_at`; a
/// backslash escapes the next character, whatever its width. An unterminated string runs to the end.
fn quoted_string_end(chars: &[char], quote_at: usize) -> usize {
    let mut cursor = quote_at + 1;
    while cursor < chars.len() && chars[cursor] != '"' {
        cursor += if chars[cursor] == '\\' { 2 } else { 1 };
    }
    (cursor + 1).min(chars.len())
}

/// The char index just past a character literal opening at `quote_at`, or None when the quote begins a lifetime or
/// label such as `'a`, which is code. An escape (`'\''`, `'\u{754c}'`) ends at the next quote after the escaped
/// character; any other literal is one character, of any width, then a quote.
fn char_literal_end(chars: &[char], quote_at: usize) -> Option<usize> {
    match chars.get(quote_at + 1) {
        Some('\\') => {
            let after_escape = (quote_at + 3).min(chars.len());
            let closing = chars[after_escape..].iter().position(|character| *character == '\'');
            Some(closing.map_or(chars.len(), |offset| after_escape + offset + 1))
        }
        Some(_) if chars.get(quote_at + 2) == Some(&'\'') => Some(quote_at + 3),
        _ => None,
    }
}

/// The char index just past the literal or comment starting at `start`, or None when `start` begins code.
/// Literal prefixes (`b`, `c`, `r`, `br`, `cr`) count only at the start of a token, never inside an identifier.
fn masked_span_end(chars: &[char], start: usize) -> Option<usize> {
    let at_token_start = start == 0 || !continues_identifier(chars[start - 1]);
    let next = chars.get(start + 1).copied();
    match chars[start] {
        '/' if next == Some('/') => Some(
            chars[start..]
                .iter()
                .position(|character| *character == '\n')
                .map_or(chars.len(), |offset| start + offset),
        ),
        '/' if next == Some('*') => Some(block_comment_end(chars, start)),
        'r' if at_token_start && matches!(next, Some('"' | '#')) => raw_string_end(chars, start),
        'b' | 'c'
            if at_token_start
                && next == Some('r')
                && matches!(chars.get(start + 2), Some('"' | '#')) =>
        {
            raw_string_end(chars, start + 1)
        }
        'b' | 'c' if at_token_start && next == Some('"') => {
            Some(quoted_string_end(chars, start + 1))
        }
        'b' if at_token_start && next == Some('\'') => char_literal_end(chars, start + 1),
        '"' => Some(quoted_string_end(chars, start)),
        '\'' => char_literal_end(chars, start),
        _ => None,
    }
}

/// `source` with every comment (line, nested block and doc) and every string, raw string (`r`, `br`, `cr`, any
/// number of hashes), C string and character literal replaced by spaces of the same UTF-8 byte length. Line breaks
/// are kept, so byte offsets and line numbers still match the source and a search over it sees only code. A
/// lifetime, a raw identifier and a Unicode identifier are code and are kept.
pub(crate) fn code_only(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut hidden = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        match masked_span_end(&chars, index) {
            Some(end) => {
                hidden[index..end].fill(true);
                index = end.max(index + 1);
            }
            None => index += 1,
        }
    }
    let mut masked = String::with_capacity(source.len());
    for (character, is_hidden) in chars.iter().zip(&hidden) {
        // When: a hidden character is not a line break, it becomes spaces of its own byte width, so offsets hold.
        if *is_hidden && *character != '\n' {
            masked.extend(std::iter::repeat_n(' ', character.len_utf8()));
        } else {
            masked.push(*character);
        }
    }
    masked
}

/// The body of the item starting at `head` in `source`, searched and returned as code only (`code_only`, read as
/// LF) and ending at its first closing brace at the item's own indentation, so neither a comment nor a literal
/// counts. The signature must occur exactly once in code: a second copy, in an unused macro or a cfg-disabled
/// impl, makes the source ambiguous, and the pin fails closed rather than guess which one compiles.
pub(crate) fn item_body(source: &str, head: &str) -> Result<String, String> {
    let code = code_only(&source.replace("\r\n", "\n"));
    let starts: Vec<usize> = code.match_indices(head).map(|(at, _)| at).collect();
    let &[start] = starts.as_slice() else {
        return Err(if starts.is_empty() {
            format!("{head} is not defined")
        } else {
            format!("ambiguous source: {head} occurs {} times", starts.len())
        });
    };
    let line_start = code[..start].rfind('\n').map_or(0, |at| at + 1);
    let indent = &code[line_start..start];
    let rest = &code[start..];
    let end =
        rest.find(&format!("\n{indent}}}\n")).ok_or_else(|| format!("{head} does not end"))?;
    Ok(rest[..end].to_owned())
}

#[cfg(test)]
#[path = "source_pin_tests.rs"]
mod source_pin_tests;
