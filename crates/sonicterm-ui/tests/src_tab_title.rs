//! Tests for the wezterm-style tab-title formatter.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/tab_title.rs`.
//! Named `src_tab_title.rs` to avoid colliding with the existing
//! `crates/sonicterm-shared/tests/tab_title.rs` integration test
//! (different crate, but the convention keeps it visually distinct).

use sonicterm_ui::tab_title::{format_tab_title, tab_display_label, TAB_SEPARATOR_PREFIX};

#[test]
fn wezterm_parity_space_between_index_and_icon() {
    let s = format_tab_title(0, Some("/Users/d0n9x1n"), Some("zsh"), None);
    assert_eq!(s, "#1 \u{F018D} Users/d0n9x1n");
    assert!(s.starts_with("#1 "), "must have a space between #N and icon");
    let space_count = s.chars().filter(|c| *c == ' ').count();
    assert_eq!(space_count, 2, "one space after #N and one after icon");
}

#[test]
fn nvim_with_cwd_two_components() {
    assert_eq!(
        format_tab_title(0, Some("/foo/bar/baz"), Some("nvim"), None),
        "#1 \u{E62B} bar/baz"
    );
}

#[test]
fn no_process_falls_back_to_folder() {
    assert_eq!(format_tab_title(0, Some("/foo/bar"), None, None), "#1 \u{F07B} foo/bar");
}

#[test]
fn ssh_with_raw_title_no_cwd() {
    assert_eq!(format_tab_title(0, None, Some("ssh"), Some("user@host")), "#1 \u{F08C0} user@host");
}

#[test]
fn unknown_process_with_cwd_uses_folder() {
    assert_eq!(format_tab_title(2, Some("/tmp"), Some("htop"), None), "#3 \u{F07B} tmp");
}

#[test]
fn login_shell_dash_stripped_by_caller_zsh_matches() {
    assert_eq!(format_tab_title(0, Some("/Users/me"), Some("zsh"), None), "#1 \u{F018D} Users/me");
}

#[test]
fn single_component_cwd() {
    assert_eq!(format_tab_title(0, Some("/tmp"), Some("bash"), None), "#1 \u{F018D} tmp");
}

#[test]
fn root_cwd() {
    assert_eq!(format_tab_title(0, Some("/"), Some("bash"), None), "#1 \u{F018D} /");
}

#[test]
fn cwd_with_trailing_slash() {
    assert_eq!(format_tab_title(0, Some("/foo/bar/"), None, None), "#1 \u{F07B} foo/bar");
}

#[test]
fn nothing_known_renders_shell_fallback() {
    assert_eq!(format_tab_title(0, None, None, None), "#1 \u{F489} shell");
}

#[test]
fn process_case_insensitive() {
    assert_eq!(format_tab_title(0, None, Some("NVIM"), Some("editor")), "#1 \u{E62B} editor");
}

#[test]
fn cargo_icon() {
    assert_eq!(
        format_tab_title(0, Some("/proj/sonic"), Some("cargo"), None),
        "#1 \u{F1617} proj/sonic"
    );
}

#[test]
fn git_icon() {
    assert_eq!(
        format_tab_title(0, Some("/proj/sonic"), Some("git"), None),
        "#1 \u{F1D3} proj/sonic"
    );
}

#[test]
fn python_icon() {
    assert_eq!(format_tab_title(0, Some("/x/y"), Some("python3"), None), "#1 \u{F0320} x/y");
}

#[test]
fn docker_icon() {
    assert_eq!(format_tab_title(0, Some("/x/y"), Some("docker"), None), "#1 \u{F0867} x/y");
}

#[test]
fn make_icon() {
    assert_eq!(format_tab_title(0, Some("/x/y"), Some("make"), None), "#1 \u{F05B4} x/y");
}

#[test]
fn node_icon() {
    assert_eq!(format_tab_title(0, Some("/x/y"), Some("node"), None), "#1 \u{F1842} x/y");
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
    assert_eq!(rendered[0], "#1 zsh");
    assert!(rendered[1].starts_with(TAB_SEPARATOR_PREFIX));
    assert!(rendered[2].starts_with(TAB_SEPARATOR_PREFIX));
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
    assert_eq!(TAB_SEPARATOR_PREFIX, "\u{2502} ");
    assert_eq!(TAB_SEPARATOR_PREFIX.chars().next(), Some('│'));
}
