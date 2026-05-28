//! Wezterm-style tab title formatter.
//!
//! Produces a string of the form `#N{icon} {parent}/{leaf}` where:
//! - `N`  — 1-based tab index (the user-visible position in the bar).
//! - `{icon}` — a Nerd Font glyph picked from the foreground process name
//!   (zsh → console, nvim → vim icon, ssh → ssh icon, …). Falls back to a
//!   folder glyph when the process is unknown but a cwd is reported.
//! - `{parent}/{leaf}` — the last two path components of the pane's cwd.
//!   A single-component path (e.g. `/tmp`) shows as just that component.
//!
//! The OSC 0/2 "raw" window title is used as a last-resort body when no cwd
//! is available — handy for commands like `ssh user@host` which set the
//! title themselves but never report a cwd.

/// Wezterm's "fancy mode" vertical separator drawn between tabs.
/// U+2502 BOX DRAWINGS LIGHT VERTICAL, followed by a single space of
/// padding (~6–8px at typical monospace cell widths) to keep the
/// separator visually clear of the next tab's title.
pub const TAB_SEPARATOR_PREFIX: &str = "\u{2502} ";

/// Build the on-screen label for a tab. Mirrors wezterm fancy-mode:
/// every tab except the first is prefixed by `│ ` so a thin divider
/// appears between adjacent tab titles. Callers that render the
/// separator in a distinct color should use [`TAB_SEPARATOR_PREFIX`]
/// directly and split the returned string on its length — or, more
/// commonly, look at the tab `index` themselves.
#[must_use]
pub fn tab_display_label(index: usize, title: &str) -> String {
    if index == 0 {
        title.to_string()
    } else {
        format!("{TAB_SEPARATOR_PREFIX}{title}")
    }
}

/// Format a tab title in wezterm style. See module docs for the contract.
///
/// All inputs are optional so that the function works regardless of which
/// signals the pty has produced so far. The return value is always a
/// non-empty `String` (a bare `#N` shell fallback is the worst case).
#[must_use]
pub fn format_tab_title(
    index: usize,
    cwd: Option<&str>,
    process: Option<&str>,
    raw_title: Option<&str>,
) -> String {
    let n = index + 1;
    let icon = icon_for_process(process, cwd.is_some());

    let body = if let Some(c) = cwd {
        cwd_two_components(c)
    } else if let Some(t) = raw_title.map(str::trim).filter(|s| !s.is_empty()) {
        t.to_string()
    } else {
        "shell".to_string()
    };

    format!("#{n} {icon} {body}")
}

/// Pick the Nerd Font glyph for a process name. Returns the folder icon
/// when `has_cwd` is true and the process is unknown / absent. Returns a
/// terminal icon when neither is known.
fn icon_for_process(process: Option<&str>, has_cwd: bool) -> char {
    if let Some(p) = process {
        match p.to_ascii_lowercase().as_str() {
            "zsh" | "bash" | "fish" | "sh" | "dash" => return '\u{F018D}', // md_console
            "nvim" | "vim" | "vi" | "nvi" => return '\u{E62B}',            // custom_vim
            "ssh" | "mosh" => return '\u{F08C0}',                          // md_ssh
            "git" | "lazygit" | "tig" => return '\u{F1D3}',                // fa_git
            "cargo" | "rustc" | "rust-analyzer" => return '\u{F1617}',     // md_language_rust
            "node" | "npm" | "npx" | "yarn" | "pnpm" | "deno" | "bun" => return '\u{F1842}', // md_nodejs
            "python" | "python3" | "ipython" | "pip" | "pip3" => return '\u{F0320}', // fa_python
            "docker" | "podman" => return '\u{F0867}',                               // md_docker
            "make" | "gmake" | "cmake" | "ninja" => return '\u{F05B4}', // md_hammer_wrench
            _ => {}
        }
    }
    if has_cwd {
        '\u{F07B}' // fa_folder
    } else {
        '\u{F489}' // nf-oct-terminal — generic shell fallback
    }
}

/// Take the trailing two components of a cwd, separated by `/`. Trailing
/// slashes are stripped. A single-component path returns just that
/// component. The empty / root path returns `/`.
fn cwd_two_components(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    let comps: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    match comps.as_slice() {
        [] => "/".to_string(),
        [only] => (*only).to_string(),
        [.., parent, leaf] => format!("{parent}/{leaf}"),
    }
}

// Unit tests live in `tests/src_tab_title.rs`.
