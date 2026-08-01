//! Safe command-draft formatting for script-file open requests.

use std::path::Path;

use crate::shell_quote_posix;

/// Shell families whose input syntax SonicTerm knows how to quote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellDialect {
    /// POSIX-compatible shells.
    Posix,
    /// Windows PowerShell or PowerShell Core.
    PowerShell,
    /// Windows Command Prompt.
    Cmd,
    /// A shell whose quoting rules are unknown.
    Unknown,
}

/// Why a script path cannot be represented as a safe command draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DraftRejection {
    /// The platform path cannot be represented as Unicode text.
    NonUnicodePath,
    /// The path is not absolute.
    NonAbsolutePath,
    /// The path contains a control character.
    ControlCharacter,
    /// The shell and script extension are not a supported pair.
    UnsupportedPair,
    /// A path character would be expanded by Command Prompt.
    CmdUnsafeCharacter,
}

/// Classify a resolved shell program path by its executable basename.
#[must_use]
pub fn classify_shell(program_path: &str) -> ShellDialect {
    let basename = program_path.rsplit(['/', '\\']).next().unwrap_or_default().to_ascii_lowercase();
    let stem = basename.strip_suffix(".exe").unwrap_or(&basename);
    match stem {
        "sh" | "bash" | "zsh" | "dash" | "ksh" => ShellDialect::Posix,
        "pwsh" | "powershell" => ShellDialect::PowerShell,
        "cmd" => ShellDialect::Cmd,
        _ => ShellDialect::Unknown,
    }
}

/// Quote one PowerShell argument with single quotes.
#[must_use]
pub fn shell_quote_powershell(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push('\'');
        }
        quoted.push(ch);
    }
    quoted.push('\'');
    quoted
}

/// Format an absolute script path as an unsubmitted shell command draft.
pub fn format_script_draft(dialect: ShellDialect, path: &Path) -> Result<String, DraftRejection> {
    let value = path.to_str().ok_or(DraftRejection::NonUnicodePath)?;
    if !path.is_absolute() {
        return Err(DraftRejection::NonAbsolutePath);
    }
    if value.chars().any(char::is_control) {
        return Err(DraftRejection::ControlCharacter);
    }

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match (dialect, extension.as_str()) {
        (ShellDialect::Posix, "sh" | "command" | "tool") => {
            Ok(format!("sh {}", shell_quote_posix(value)))
        }
        (ShellDialect::PowerShell, "ps1" | "cmd" | "bat") => {
            Ok(format!("& {}", shell_quote_powershell(value)))
        }
        (ShellDialect::Cmd, "cmd" | "bat") => {
            if value.contains(['%', '!', '"']) {
                Err(DraftRejection::CmdUnsafeCharacter)
            } else {
                Ok(format!("\"{value}\""))
            }
        }
        _ => Err(DraftRejection::UnsupportedPair),
    }
}

#[cfg(test)]
#[path = "script_draft_tests.rs"]
mod script_draft_tests;
