//! Integration tests for the wezterm-style tab title formatter and the
//! OSC 7 → cwd pipeline that feeds it. These hit the public surface
//! (`sonic_shared::tab_title::format_tab_title` and
//! `sonic_core::vt::Parser::cwd`) so a regression in either layer is
//! caught here.

use sonic_core::grid::Grid;
use sonic_core::vt::{parse_osc7_cwd, Parser};
use sonic_shared::tab_title::format_tab_title;

#[test]
fn format_tab_title_nvim_two_components() {
    assert_eq!(
        format_tab_title(0, Some("/foo/bar/baz"), Some("nvim"), None),
        "#1 \u{E62B} bar/baz"
    );
}

#[test]
fn format_tab_title_no_process_uses_folder() {
    assert_eq!(format_tab_title(0, Some("/foo/bar"), None, None), "#1 \u{F07B} foo/bar");
}

#[test]
fn format_tab_title_ssh_with_raw_title() {
    assert_eq!(format_tab_title(0, None, Some("ssh"), Some("user@host")), "#1 \u{F08C0} user@host");
}

#[test]
fn format_tab_title_unknown_process_falls_back_to_folder() {
    assert_eq!(
        format_tab_title(0, Some("/Users/me/proj"), Some("htop"), None),
        "#1 \u{F07B} me/proj"
    );
}

#[test]
fn osc7_sets_cwd_on_grid_parser() {
    // OSC 7 ; file://localhost/tmp/work ST → cwd("/tmp/work").
    let mut parser = Parser::new(Grid::new(80, 24));
    let payload = b"\x1b]7;file://localhost/tmp/work\x1b\\";
    let _ = parser.advance(payload);
    assert_eq!(parser.cwd(), Some("/tmp/work"));
}

#[test]
fn osc7_decodes_percent_escapes() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let payload = b"\x1b]7;file://host/Users/me/My%20Code\x1b\\";
    let _ = parser.advance(payload);
    assert_eq!(parser.cwd(), Some("/Users/me/My Code"));
}

#[test]
fn osc7_accepts_payload_without_scheme() {
    // Some shells skip the scheme; parser must still capture the path.
    assert_eq!(parse_osc7_cwd("/raw/path"), "/raw/path");
    assert_eq!(parse_osc7_cwd("file:///abs/path"), "/abs/path");
    assert_eq!(parse_osc7_cwd("file://host/abs"), "/abs");
}

#[test]
fn format_tab_title_cargo_in_project() {
    assert_eq!(
        format_tab_title(1, Some("/proj/sonic"), Some("cargo"), None),
        "#2 \u{F1617} proj/sonic"
    );
}

#[test]
fn format_tab_title_shell_index_zero_to_one() {
    // Index is 0-based; rendered N is 1-based.
    assert_eq!(format_tab_title(0, Some("/tmp"), Some("zsh"), None), "#1 \u{F018D} tmp");
    assert_eq!(format_tab_title(4, Some("/tmp"), Some("zsh"), None), "#5 \u{F018D} tmp");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_proc_probe_returns_known_name_for_self() {
    use sonic_core::proc_info::foreground_process;
    // The test binary itself: probing our own pid + walking descendants
    // should at minimum return *some* process name (the deepest
    // descendant is often a temporary helper; we just assert non-empty
    // and not the pid stringified).
    let me = std::process::id();
    let name = foreground_process(me);
    // The deepest descendant may be us if no children — accept that too.
    if let Some(n) = name {
        assert!(!n.is_empty(), "process name should never be empty");
        assert!(n.parse::<u32>().is_err(), "process name should not be a stringified pid: got {n}");
        // Should not still have a login-shell dash prefix.
        assert!(!n.starts_with('-'), "leading - should be stripped, got {n}");
    }
}
