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
pub fn shell_quote_posix(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    write_shell_quote_posix(value, |character| out.push(character));
    out
}

/// Emit POSIX quoting without allocating an intermediate argument string.
pub(crate) fn write_shell_quote_posix(value: &str, mut emit: impl FnMut(char)) {
    emit('\'');
    for character in value.chars() {
        if character == '\'' {
            // Close the POSIX quoted word, escape its literal apostrophe, and reopen the word.
            for escaped in "'\\''".chars() {
                emit(escaped);
            }
        } else {
            // When: `character` is not a quote delimiter, it is literal inside the surrounding single quotes.
            emit(character);
        }
    }
    emit('\'');
}

#[cfg(test)]
#[path = "shell_quote_tests.rs"]
mod shell_quote_tests;
