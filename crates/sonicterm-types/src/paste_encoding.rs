//! Validated, size-bounded encoding of user text and native paths for terminal paste.

use std::path::PathBuf;

use crate::{
    script_draft::write_shell_quote_powershell, shell_quote::write_shell_quote_posix, ShellDialect,
};

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// Caller-owned text or native paths awaiting terminal-paste encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UserPayload {
    /// Clipboard text, preserved byte-for-byte inside any paste guards.
    Text(String),
    /// Native paths, validated as a whole list before shell quoting.
    Paths(Vec<PathBuf>),
}

/// Paste protocol and shell syntax selected for the receiving pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PasteTarget {
    /// Whether the pane requested DECSET 2004 bracketed-paste guards.
    pub bracketed: bool,
    /// Shell syntax used only for path arguments, never for plain text.
    pub dialect: ShellDialect,
}

/// Why a user payload cannot be encoded for the selected paste target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasteRefusal {
    /// A native path cannot be represented without losing its original units.
    NonUnicodePath,
    /// A path contains a control character that could act as terminal input.
    ControlCharacter,
    /// A path contains a Command Prompt quote or expansion character.
    CmdUnsafeCharacter,
    /// The encoded payload exceeds the caller's byte limit or overflows its length.
    TooLarge {
        /// Exact required bytes, saturated at `usize::MAX` if arithmetic overflows.
        needed: usize,
    },
}

/// Validate and encode one complete paste, allocating only its admitted output buffer.
///
/// Text is unchanged apart from optional bracketed-paste guards. Paths are quoted for
/// the target shell and separated by one space; an empty list emits nothing. The
/// caller supplies its transport byte limit, including quotes, spaces, and guards.
pub fn encode_payload(
    payload: &UserPayload,
    target: PasteTarget,
    max_bytes: usize,
) -> Result<Vec<u8>, PasteRefusal> {
    let needed = encoded_len(payload, target)?;
    if needed > max_bytes {
        // When: `needed` exceeds transport admission, refuse before allocating or producing a partial paste.
        return Err(PasteRefusal::TooLarge { needed });
    }
    let mut output = Vec::with_capacity(needed);
    emit_payload(payload, target, |ch| {
        let mut utf8 = [0; 4];
        output.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
    });
    debug_assert_eq!(output.len(), needed);
    Ok(output)
}

fn checked_add_len(total: usize, additional: usize) -> Result<usize, PasteRefusal> {
    total.checked_add(additional).ok_or(PasteRefusal::TooLarge { needed: usize::MAX })
}

fn encoded_len(payload: &UserPayload, target: PasteTarget) -> Result<usize, PasteRefusal> {
    if let UserPayload::Paths(paths) = payload {
        // When: `payload` holds native Paths, validate the complete list before any shell syntax is emitted.
        for path in paths {
            let value = path.to_str().ok_or(PasteRefusal::NonUnicodePath)?;
            if value.chars().any(char::is_control) {
                // When: any path carries terminal controls, reject the entire list before measuring or quoting a prefix.
                return Err(PasteRefusal::ControlCharacter);
            }
            if target.dialect == ShellDialect::Cmd && value.contains(['"', '%', '!']) {
                // When: cmd would reinterpret a quote or expansion in `value`, no quoted prefix can make the list safe.
                return Err(PasteRefusal::CmdUnsafeCharacter);
            }
        }
    }
    let mut needed = Ok(0);
    emit_payload(payload, target, |ch| {
        needed = needed.and_then(|total| checked_add_len(total, ch.len_utf8()));
    });
    needed
}

// Both sizing and encoding traverse this exact stream after whole-list validation;
// borrowing UserPayload prevents native paths from changing between the two passes.
fn emit_payload(payload: &UserPayload, target: PasteTarget, mut emit: impl FnMut(char)) {
    if matches!(payload, UserPayload::Paths(paths) if paths.is_empty()) {
        // When: `matches!` identifies an empty path payload, suppress guards so a missing drop produces no terminal input.
        return;
    }
    if target.bracketed {
        for ch in PASTE_START.chars() {
            emit(ch);
        }
    }
    match payload {
        UserPayload::Text(text) => {
            for ch in text.chars() {
                emit(ch);
            }
        }
        UserPayload::Paths(paths) => {
            for (index, path) in paths.iter().enumerate() {
                if index != 0 {
                    emit(' ');
                }
                let value = path.to_str().expect("native paths were validated before encoding");
                match target.dialect {
                    ShellDialect::Posix | ShellDialect::Unknown => {
                        write_shell_quote_posix(value, &mut emit);
                    }
                    ShellDialect::PowerShell => write_shell_quote_powershell(value, &mut emit),
                    ShellDialect::Cmd => {
                        emit('"');
                        for ch in value.chars() {
                            emit(ch);
                        }
                        emit('"');
                    }
                }
            }
        }
    }
    if target.bracketed {
        for ch in PASTE_END.chars() {
            emit(ch);
        }
    }
}

#[cfg(test)]
#[path = "paste_encoding_tests.rs"]
mod paste_encoding_tests;
