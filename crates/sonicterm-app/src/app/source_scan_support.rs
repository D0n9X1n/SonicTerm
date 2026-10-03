//! Test-only source scanning: blank comments and literal contents out of Rust source, so a
//! source-scan test matches only real code.

/// Blank `range` of `view` with spaces, keeping newlines so line numbers survive.
fn blank_span(view: &mut [u8], range: std::ops::Range<usize>) {
    for byte in &mut view[range] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Whether `byte` can continue a Rust identifier.
pub(super) fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The quote offset and `#` count of a raw string literal (`r"`, `r#"`, `br#"`) starting at
/// `offset`, or `None` when no raw string starts there.
fn raw_string_opener(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    if offset > 0 && is_ident_byte(bytes[offset - 1]) {
        // When: the `r` continues an identifier such as `parser`, so no literal starts here.
        return None;
    }
    let after_prefix = match bytes.get(offset..offset + 2) {
        Some([b'b', b'r']) => offset + 2,
        Some([b'r', _]) => offset + 1,
        _ => return None,
    };
    let hash_count = bytes[after_prefix..].iter().take_while(|byte| **byte == b'#').count();
    (bytes.get(after_prefix + hash_count) == Some(&b'"'))
        .then_some((after_prefix + hash_count, hash_count))
}

/// Two views of `text`, with `\r\n` normalized to `\n`, sharing byte offsets: `code` blanks
/// comments (nested block comments included) and keeps literals, so bytes inside a call's
/// argument stay visible; `bare` also blanks the contents of string, raw string and char
/// literals (escaped ones included), so only real code can name a call, a binding or a variant.
pub(super) fn code_views(text: &str) -> (String, String) {
    let normalized = text.replace("\r\n", "\n");
    let text = normalized.as_str();
    let bytes = text.as_bytes();
    let mut code = bytes.to_vec();
    let mut bare = bytes.to_vec();
    let mut offset = 0;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        if rest.starts_with(b"//") {
            let end =
                rest.iter().position(|byte| *byte == b'\n').map_or(bytes.len(), |len| offset + len);
            blank_span(&mut code, offset..end);
            blank_span(&mut bare, offset..end);
            offset = end;
        } else if rest.starts_with(b"/*") {
            // Block comments nest in Rust, so the scan counts openers and closers.
            let mut depth = 0usize;
            let mut cursor = offset;
            while cursor < bytes.len() {
                if bytes[cursor..].starts_with(b"/*") {
                    depth += 1;
                    cursor += 2;
                } else if bytes[cursor..].starts_with(b"*/") {
                    depth -= 1;
                    cursor += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    cursor += 1;
                }
            }
            blank_span(&mut code, offset..cursor);
            blank_span(&mut bare, offset..cursor);
            offset = cursor;
        } else if let Some((quote, hash_count)) = raw_string_opener(bytes, offset) {
            let closer: Vec<u8> =
                std::iter::once(b'"').chain(std::iter::repeat_n(b'#', hash_count)).collect();
            let body = quote + 1;
            let close = bytes[body..]
                .windows(closer.len())
                .position(|window| window == closer.as_slice())
                .map_or(bytes.len(), |len| body + len);
            blank_span(&mut bare, body..close);
            offset = (close + closer.len()).min(bytes.len());
        } else if bytes[offset] == b'"' {
            let body = offset + 1;
            let mut cursor = body;
            while cursor < bytes.len() && bytes[cursor] != b'"' {
                // An escape consumes the next byte, which may be a quote.
                cursor += if bytes[cursor] == b'\\' { 2 } else { 1 };
            }
            let close = cursor.min(bytes.len());
            blank_span(&mut bare, body..close);
            offset = close + 1;
        } else if bytes[offset] == b'\'' {
            // A char literal closes within a few bytes; anything else is a lifetime or label.
            let body = offset + 1;
            let close = if bytes.get(body) == Some(&b'\\') {
                bytes[body..(body + 12).min(bytes.len())]
                    .iter()
                    .skip(2)
                    .position(|byte| *byte == b'\'')
                    .map(|len| body + 2 + len)
            } else {
                text[body..]
                    .chars()
                    .next()
                    .map(|first| body + first.len_utf8())
                    .filter(|after| bytes.get(*after) == Some(&b'\''))
            };
            if let Some(close) = close {
                blank_span(&mut bare, body..close);
                offset = close + 1;
            } else {
                offset += 1;
            }
        } else {
            offset += 1;
        }
    }
    // Every blanked span covers whole characters, so both views stay valid UTF-8.
    (String::from_utf8(code).unwrap(), String::from_utf8(bare).unwrap())
}

#[cfg(test)]
#[path = "source_scan_support_tests.rs"]
mod source_scan_support_tests;
