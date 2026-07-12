use super::*;

#[test]
fn display_labels_prefix_only_tabs_after_the_first() {
    assert_eq!(tab_display_label(0, "first"), "first");
    assert_eq!(tab_display_label(1, "second"), "│ second");
    assert_eq!(tab_display_label(12, "later"), "│ later");
}

#[test]
fn cwd_takes_precedence_and_is_reduced_to_two_components() {
    let title =
        format_tab_title(2, Some("/Users/alice/project/"), Some("NVIM"), Some("ignored raw title"));

    assert_eq!(title, "#3 \u{e62b} alice/project");
}

#[test]
fn cwd_reduction_handles_root_single_and_repeated_separators() {
    assert_eq!(cwd_two_components("/"), "/");
    assert_eq!(cwd_two_components("////"), "/");
    assert_eq!(cwd_two_components("/tmp/"), "tmp");
    assert_eq!(cwd_two_components("//srv///repo//src///"), "repo/src");
    assert_eq!(cwd_two_components("relative/path"), "relative/path");
}

#[test]
fn raw_titles_are_trimmed_and_blank_titles_fall_back_to_shell() {
    assert_eq!(format_tab_title(0, None, None, Some("  build log  ")), "#1 \u{f489} build log");
    assert_eq!(format_tab_title(0, None, None, Some(" \t\n ")), "#1 \u{f489} shell");
    assert_eq!(format_tab_title(0, None, None, None), "#1 \u{f489} shell");
}

#[test]
fn unknown_processes_use_context_sensitive_fallback_icons() {
    assert_eq!(
        format_tab_title(0, Some("/work/tree"), Some("tool"), None),
        "#1 \u{f07b} work/tree"
    );
    assert_eq!(format_tab_title(0, None, Some("tool"), Some("remote")), "#1 \u{f489} remote");
}

#[test]
fn process_families_map_case_insensitively_to_their_icons() {
    let cases = [
        ("ZSH", '\u{f018d}'),
        ("vi", '\u{e62b}'),
        ("mosh", '\u{f08c0}'),
        ("lazygit", '\u{f1d3}'),
        ("rust-analyzer", '\u{f1617}'),
        ("PNPM", '\u{f1842}'),
        ("python3", '\u{f0320}'),
        ("podman", '\u{f0867}'),
        ("ninja", '\u{f05b4}'),
    ];

    for (process, expected) in cases {
        assert_eq!(icon_for_process(Some(process), false), expected, "process {process}");
    }
}
