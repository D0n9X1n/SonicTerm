//! `ShellDialect` — per-shell command emitter for the e2e gate examples
//! (`pty_dump`, `pty_dump_unicode`) and the Windows integration test.
//!
//! Gated behind `cfg(feature = "test_support")` so production builds don't
//! see this module. Examples and integration tests enable the feature in
//! their `[dev-dependencies]` / build config.
//!
//! Background (#457): the e2e gate examples send POSIX `printf '\033...'`
//! commands to whatever shell `PtyHandle::spawn_default_shell` resolved.
//! On Windows, that's PowerShell — which can't parse POSIX `printf`, so
//! the examples silently produced zero output. This module dispatches
//! shell-family-specific emissions per `PtyHandle::shell_program_path()`.

use std::path::Path;

/// Per-shell-family command emitter. Implementations encode the SAME
/// semantic output (marker lines + payload + ANSI SGR colored text) in
/// the syntax that family understands.
pub trait ShellDialect {
    /// Human-readable name for diagnostics. e.g. "posix" or "powershell".
    fn name(&self) -> &'static str;

    /// Emit `text` followed by a newline. No additional formatting.
    fn emit_text(&self, text: &str) -> Vec<u8>;

    /// Emit three lines: `BEGIN_UNICODE`, the payload codepoints, `END_UNICODE`.
    /// Used by `pty_dump_unicode` to bracket Unicode shibboleths so the scrape
    /// can find them regardless of shell prompt position.
    fn emit_unicode_markers(&self, payload: &str) -> Vec<u8>;

    /// Emit `text` styled with non-default fg color (ANSI SGR).
    fn emit_colored_text(&self, text: &str) -> Vec<u8>;

    /// Emit `text` styled bold (ANSI SGR).
    fn emit_bold_text(&self, text: &str) -> Vec<u8>;

    /// Emit a command that prints the shell's version string.
    /// `parse_version_output` decodes the resulting line.
    fn version_check_command(&self) -> Vec<u8>;

    /// Parse the output of `version_check_command` into `(major, minor, patch)`,
    /// returning `None` if the output doesn't look like a recognizable version.
    fn parse_version_output(&self, output: &str) -> Option<(u32, u32, u32)>;
}

/// Error returned by `dialect_for_shell` when the resolved shell has no
/// `ShellDialect` implementation in this crate (cmd.exe, fish, the
/// test sentinel, anything we haven't added explicit support for yet).
#[derive(Debug, Clone)]
pub struct UnsupportedShellError {
    pub shell_path: String,
}

impl std::fmt::Display for UnsupportedShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "e2e gate doesn't support shell at {:?} — see issue #457", self.shell_path)
    }
}

impl std::error::Error for UnsupportedShellError {}

/// Picks the right `ShellDialect` for the resolved shell program path.
/// Returns `Err` for unsupported shells (cmd.exe, fish, test sentinel,
/// unknown) so callers fail loudly instead of silently sending POSIX
/// commands to a shell that can't parse them.
pub fn dialect_for_shell(
    shell_program_path: &str,
) -> Result<Box<dyn ShellDialect>, UnsupportedShellError> {
    let name = Path::new(shell_program_path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match name.as_str() {
        "pwsh.exe" | "powershell.exe" | "pwsh" | "powershell" => Ok(Box::new(PowerShellDialect)),
        "bash" | "bash.exe" | "zsh" | "zsh.exe" | "sh" | "sh.exe" => Ok(Box::new(PosixDialect)),
        _ => Err(UnsupportedShellError { shell_path: shell_program_path.to_string() }),
    }
}

/// POSIX shells (bash, zsh, sh) — emits via `printf` with raw `\033` escapes.
#[derive(Debug, Default)]
pub struct PosixDialect;

impl ShellDialect for PosixDialect {
    fn name(&self) -> &'static str {
        "posix"
    }

    fn emit_text(&self, text: &str) -> Vec<u8> {
        format!("printf '%s\\n' {}\n", posix_quote(text)).into_bytes()
    }

    fn emit_unicode_markers(&self, payload: &str) -> Vec<u8> {
        format!("printf 'BEGIN_UNICODE\\n%s\\nEND_UNICODE\\n' {}\n", posix_quote(payload))
            .into_bytes()
    }

    fn emit_colored_text(&self, text: &str) -> Vec<u8> {
        // SGR 31 = red fg, 0 = reset.
        format!("printf '\\033[31m%s\\033[0m\\n' {}\n", posix_quote(text)).into_bytes()
    }

    fn emit_bold_text(&self, text: &str) -> Vec<u8> {
        format!("printf '\\033[1m%s\\033[0m\\n' {}\n", posix_quote(text)).into_bytes()
    }

    fn version_check_command(&self) -> Vec<u8> {
        // bash --version / zsh --version both print "X.Y.Z" in first line.
        b"echo $BASH_VERSION$ZSH_VERSION\n".to_vec()
    }

    fn parse_version_output(&self, output: &str) -> Option<(u32, u32, u32)> {
        parse_dotted_version(output)
    }
}

/// PowerShell (pwsh.exe + powershell.exe) — emits via `[Console]::Out.WriteLine`
/// with `$e=[char]27` ANSI escapes. Avoids `Write-Host` (which goes through
/// the formatter) and avoids POSIX `printf` (which PowerShell can't parse).
#[derive(Debug, Default)]
pub struct PowerShellDialect;

impl ShellDialect for PowerShellDialect {
    fn name(&self) -> &'static str {
        "powershell"
    }

    fn emit_text(&self, text: &str) -> Vec<u8> {
        format!("[Console]::Out.WriteLine({})\r\n", ps_quote(text)).into_bytes()
    }

    fn emit_unicode_markers(&self, payload: &str) -> Vec<u8> {
        // Force UTF-8 console output then emit the three marker lines.
        let mut cmd = String::new();
        cmd.push_str("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\r\n");
        cmd.push_str("[Console]::Out.WriteLine('BEGIN_UNICODE')\r\n");
        cmd.push_str(&format!("[Console]::Out.WriteLine({})\r\n", ps_quote(payload)));
        cmd.push_str("[Console]::Out.WriteLine('END_UNICODE')\r\n");
        cmd.into_bytes()
    }

    fn emit_colored_text(&self, text: &str) -> Vec<u8> {
        // SGR via `$e = [char]27`. Concatenate into one WriteLine call.
        format!(
            "$e = [char]27; [Console]::Out.WriteLine($e + '[31m' + {} + $e + '[0m')\r\n",
            ps_quote(text)
        )
        .into_bytes()
    }

    fn emit_bold_text(&self, text: &str) -> Vec<u8> {
        format!(
            "$e = [char]27; [Console]::Out.WriteLine($e + '[1m' + {} + $e + '[0m')\r\n",
            ps_quote(text)
        )
        .into_bytes()
    }

    fn version_check_command(&self) -> Vec<u8> {
        b"$PSVersionTable.PSVersion.ToString()\r\n".to_vec()
    }

    fn parse_version_output(&self, output: &str) -> Option<(u32, u32, u32)> {
        parse_dotted_version(output)
    }
}

/// Escape `s` for POSIX single-quoted string literal.
fn posix_quote(s: &str) -> String {
    // Standard POSIX trick: close quote, emit literal escaped single quote, reopen.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Escape `s` for PowerShell single-quoted string literal (doubles single quotes).
fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Parse the first "X.Y[.Z]" pattern found in `output`. Returns `(major, minor, patch)`
/// with missing components defaulting to 0.
fn parse_dotted_version(output: &str) -> Option<(u32, u32, u32)> {
    for line in output.lines() {
        let mut chars = line.chars().peekable();
        let mut numbers: Vec<u32> = Vec::new();
        let mut current = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                current.push(c);
                chars.next();
            } else if !current.is_empty() {
                // Non-digit terminates the current run. Flush whether it's
                // a '.' (in-version separator) or any other delimiter —
                // both cases close the current number and advance.
                if let Ok(n) = current.parse() {
                    numbers.push(n);
                }
                current.clear();
                chars.next();
            } else {
                chars.next();
            }
        }
        if !current.is_empty() {
            if let Ok(n) = current.parse() {
                numbers.push(n);
            }
        }
        if numbers.len() >= 2 {
            let major = numbers[0];
            let minor = numbers.get(1).copied().unwrap_or(0);
            let patch = numbers.get(2).copied().unwrap_or(0);
            return Some((major, minor, patch));
        }
    }
    None
}
