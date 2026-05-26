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

    format!("#{n}{icon} {body}")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wezterm_parity_no_space_between_index_and_icon() {
        // Wezterm fancy-mode pins the index directly against the icon
        // (`#1{icon} body`) — one space between icon and body, none
        // between `#N` and icon. Pre-fix had `#N {icon} {body}` which
        // looked like two visual spaces because the Nerd Font glyph is
        // half-width on most monospaces.
        let s = format_tab_title(0, Some("/Users/d0n9x1n"), Some("zsh"), None);
        assert_eq!(s, "#1\u{F018D} Users/d0n9x1n");
        // No space between `#1` and the icon.
        assert!(!s.starts_with("#1 "), "must not have a space between #N and icon");
        // Exactly one space between icon and body.
        let space_count = s.chars().filter(|c| *c == ' ').count();
        assert_eq!(space_count, 1, "exactly one space between icon and body");
    }

    #[test]
    fn nvim_with_cwd_two_components() {
        assert_eq!(
            format_tab_title(0, Some("/foo/bar/baz"), Some("nvim"), None),
            "#1\u{E62B} bar/baz"
        );
    }

    #[test]
    fn no_process_falls_back_to_folder() {
        assert_eq!(format_tab_title(0, Some("/foo/bar"), None, None), "#1\u{F07B} foo/bar");
    }

    #[test]
    fn ssh_with_raw_title_no_cwd() {
        assert_eq!(
            format_tab_title(0, None, Some("ssh"), Some("user@host")),
            "#1\u{F08C0} user@host"
        );
    }

    #[test]
    fn unknown_process_with_cwd_uses_folder() {
        assert_eq!(format_tab_title(2, Some("/tmp"), Some("htop"), None), "#3\u{F07B} tmp");
    }

    #[test]
    fn login_shell_dash_stripped_by_caller_zsh_matches() {
        // proc_info normalizes "-zsh" → "zsh"; format_tab_title matches
        // on the already-normalized name.
        assert_eq!(
            format_tab_title(0, Some("/Users/me"), Some("zsh"), None),
            "#1\u{F018D} Users/me"
        );
    }

    #[test]
    fn single_component_cwd() {
        assert_eq!(format_tab_title(0, Some("/tmp"), Some("bash"), None), "#1\u{F018D} tmp");
    }

    #[test]
    fn root_cwd() {
        assert_eq!(format_tab_title(0, Some("/"), Some("bash"), None), "#1\u{F018D} /");
    }

    #[test]
    fn cwd_with_trailing_slash() {
        assert_eq!(format_tab_title(0, Some("/foo/bar/"), None, None), "#1\u{F07B} foo/bar");
    }

    #[test]
    fn nothing_known_renders_shell_fallback() {
        assert_eq!(format_tab_title(0, None, None, None), "#1\u{F489} shell");
    }

    #[test]
    fn process_case_insensitive() {
        assert_eq!(format_tab_title(0, None, Some("NVIM"), Some("editor")), "#1\u{E62B} editor");
    }

    #[test]
    fn cargo_icon() {
        assert_eq!(
            format_tab_title(0, Some("/proj/sonic"), Some("cargo"), None),
            "#1\u{F1617} proj/sonic"
        );
    }

    #[test]
    fn git_icon() {
        assert_eq!(
            format_tab_title(0, Some("/proj/sonic"), Some("git"), None),
            "#1\u{F1D3} proj/sonic"
        );
    }

    #[test]
    fn python_icon() {
        assert_eq!(format_tab_title(0, Some("/x/y"), Some("python3"), None), "#1\u{F0320} x/y");
    }

    #[test]
    fn docker_icon() {
        assert_eq!(format_tab_title(0, Some("/x/y"), Some("docker"), None), "#1\u{F0867} x/y");
    }

    #[test]
    fn make_icon() {
        assert_eq!(format_tab_title(0, Some("/x/y"), Some("make"), None), "#1\u{F05B4} x/y");
    }

    #[test]
    fn node_icon() {
        assert_eq!(format_tab_title(0, Some("/x/y"), Some("node"), None), "#1\u{F1842} x/y");
    }

    #[test]
    fn first_tab_has_no_separator_prefix() {
        assert_eq!(tab_display_label(0, "#1 zsh"), "#1 zsh");
    }

    #[test]
    fn second_and_later_tabs_get_separator_prefix() {
        assert_eq!(tab_display_label(1, "#2 nvim"), "\u{2502} #2 nvim");
        assert_eq!(tab_display_label(4, "#5 cargo"), "\u{2502} #5 cargo");
    }

    #[test]
    fn three_tab_list_has_separator_between_each() {
        let titles = ["#1 zsh", "#2 nvim", "#3 cargo"];
        let rendered: Vec<String> =
            titles.iter().enumerate().map(|(i, t)| tab_display_label(i, t)).collect();
        // First tab: bare title. Subsequent tabs each start with the separator.
        assert_eq!(rendered[0], "#1 zsh");
        assert!(rendered[1].starts_with(TAB_SEPARATOR_PREFIX));
        assert!(rendered[2].starts_with(TAB_SEPARATOR_PREFIX));
        // Concatenated, the strip contains the separator exactly (n - 1) times.
        let joined = rendered.join("");
        assert_eq!(joined.matches('\u{2502}').count(), 2);
    }

    #[test]
    fn single_tab_has_no_separator_anywhere() {
        let only = tab_display_label(0, "#1 shell");
        assert!(!only.contains('\u{2502}'));
    }

    #[test]
    fn separator_prefix_constant_matches_wezterm() {
        // U+2502 BOX DRAWINGS LIGHT VERTICAL + a single space of padding.
        assert_eq!(TAB_SEPARATOR_PREFIX, "\u{2502} ");
        assert_eq!(TAB_SEPARATOR_PREFIX.chars().next(), Some('│'));
    }
}
