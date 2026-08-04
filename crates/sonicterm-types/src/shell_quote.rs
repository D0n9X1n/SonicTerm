//! Shared POSIX shell quoting for file-drop paste.
//!
//! File drops on macOS and Windows must paste the same bytes into a
//! POSIX-style shell prompt, so the quoting rule lives here in the
//! contract crate and both `sonicterm-app` and `sonicterm-windows`
//! re-export it rather than keeping parallel copies.

/// Quote a single path or word for POSIX-shell paste.
///
/// Single-quotes everything and escapes an embedded `'` as `'\''`.
/// Empty input becomes `''`. Pure function.
pub fn shell_quote_posix(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            // When: `ch` is not a quote delimiter, it is literal inside the surrounding single quotes.
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
#[path = "shell_quote_tests.rs"]
mod shell_quote_tests;
