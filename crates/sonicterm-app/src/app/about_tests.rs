use std::path::{Path, PathBuf, MAIN_SEPARATOR};

use super::*;

/// The English labels, as the en locale spells them.
fn english() -> AboutLabels {
    AboutLabels {
        version: "Version".into(),
        system: "System".into(),
        renderer: "Renderer".into(),
        font: "Font".into(),
        theme: "Theme".into(),
        keymap: "Keymap".into(),
        config: "Config".into(),
        logs: "Logs".into(),
        gpu: "GPU".into(),
        software: "Software".into(),
        software_forced: "Software (forced)".into(),
        gdi: "GDI (software)".into(),
        not_started: "not started".into(),
        config_loaded: "loaded".into(),
        config_missing: "not found, using defaults".into(),
        copy_button: "[ Copy ]".into(),
        hint: "Cmd+Shift+C copies · Esc closes".into(),
    }
}

/// The maintainer's example: a Mac with a GPU and a loaded config.
fn mac_facts() -> AboutFacts {
    AboutFacts {
        version: "1.3.11".into(),
        commit: "a17ee368".into(),
        date: "2026-10-24".into(),
        os: "macOS 27.0".into(),
        arch: "arm64".into(),
        renderer: RendererFacts {
            kind: RendererKind::Gpu,
            adapter: "Apple M4 Max".into(),
            backend: "Metal".into(),
        },
        font_family: "Menlo".into(),
        font_size_pt: 13.5,
        theme: "apollo".into(),
        theme_error: None,
        keymap: "sonicterm-macos".into(),
        keymap_error: None,
        config_path: "~/.sonicterm/sonicterm.toml".into(),
        config_status: ConfigStatus::Loaded,
        log_dir: "~/.sonicterm/logs".into(),
    }
}

/// The content model lays fixed facts out as the maintainer's target: version with build commit
/// and date, OS and architecture, then labelled rows aligned in one column, then the right-aligned
/// Copy button and its hint.
#[test]
fn the_about_rows_render_from_fixed_facts() {
    let view = render(&mac_facts(), &english(), 100);
    let expected = "\
SonicTerm 1.3.11 (a17ee368, 2026-10-24)
macOS 27.0 · arm64
Renderer   GPU · Apple M4 Max (Metal)
Font       Menlo 13.5 pt
Theme      apollo   Keymap  sonicterm-macos
Config     ~/.sonicterm/sonicterm.toml  ✓ loaded
Logs       ~/.sonicterm/logs

                                        [ Copy ]
Cmd+Shift+C copies · Esc closes
";
    assert_eq!(view.text, expected);
    assert_eq!(view.button_row, 8);
    assert_eq!(view.button_cols, (40, 48), "right-aligned to the widest row, the Config row");
    assert_eq!(view.width, 48);
}

/// The Config row reports a load error with its message (first line, control characters made
/// inert, cut at 120 characters) and a missing file as defaults in use.
#[test]
fn the_config_row_reports_a_load_error_or_a_missing_file() {
    let mut facts = mac_facts();
    facts.config_status =
        ConfigStatus::Failed("TOML parse error at line 3, column 1\nexpected `=`".into());
    let text = render(&facts, &english(), 100).text;
    assert!(
        text.contains(
            "Config     ~/.sonicterm/sonicterm.toml  ✗ TOML parse error at line 3, column 1\n"
        ),
        "{text}"
    );
    facts.config_status = ConfigStatus::Failed(format!("bad\x1b[31m{}", "x".repeat(200)));
    let text = render(&facts, &english(), 100).text;
    let row = text.lines().find(|line| line.starts_with("Config")).unwrap();
    assert!(!row.contains('\x1b'), "an escape in the error never reaches the parser: {row:?}");
    let message = row.split("✗ ").nth(1).unwrap();
    assert_eq!(message.chars().count(), CONFIG_ERROR_CHARS + 1, "{message}");
    assert!(message.ends_with('…'));
    facts.config_status = ConfigStatus::Missing;
    let text = render(&facts, &english(), 100).text;
    assert!(text.contains("~/.sonicterm/sonicterm.toml  – not found, using defaults\n"), "{text}");
}

/// A path under the home directory is shown with `~`; home itself is `~`; a path elsewhere, a
/// sibling that only shares the home's spelling as a prefix, and a path with no home stay whole.
#[test]
fn paths_under_home_are_shown_with_a_tilde() {
    let home = PathBuf::from("/Users/me");
    let logs = home.join(".sonicterm").join("logs");
    assert_eq!(
        home_relative(&logs, Some(&home)),
        format!("~{MAIN_SEPARATOR}.sonicterm{MAIN_SEPARATOR}logs")
    );
    assert_eq!(home_relative(&home, Some(&home)), "~");
    let elsewhere = Path::new("/tmp/sonic-test-config/sonicterm.toml");
    assert_eq!(home_relative(elsewhere, Some(&home)), elsewhere.display().to_string());
    let sibling = Path::new("/Users/meow/sonicterm.toml");
    assert_eq!(home_relative(sibling, Some(&home)), sibling.display().to_string());
    assert_eq!(home_relative(&logs, None), logs.display().to_string());
}

/// Copy puts every report row on the clipboard, ending in a newline, without the button or hint.
#[test]
fn the_copy_text_is_the_report_rows_only() {
    let view = render(&mac_facts(), &english(), 100);
    let expected = "\
SonicTerm 1.3.11 (a17ee368, 2026-10-24)
macOS 27.0 · arm64
Renderer   GPU · Apple M4 Max (Metal)
Font       Menlo 13.5 pt
Theme      apollo   Keymap  sonicterm-macos
Config     ~/.sonicterm/sonicterm.toml  ✓ loaded
Logs       ~/.sonicterm/logs
";
    assert_eq!(view.copy_text, expected);
    assert!(view.text.starts_with(&view.copy_text));
}

/// The button is right-aligned inside the pane, so it never wraps, and it is found on a drawn row
/// only when that row is exactly the button, never on a row that merely mentions it.
#[test]
fn the_copy_button_fits_the_pane_and_is_found_where_drawn() {
    let wide = render(&mac_facts(), &english(), 100);
    assert_eq!(wide.button_cols, (40, 48), "right-aligned to the widest row");
    let narrow = render(&mac_facts(), &english(), 30);
    assert_eq!(narrow.button_cols, (22, 30), "right-aligned inside a narrower pane");
    let row = narrow.text.lines().nth(usize::from(narrow.button_row)).unwrap();
    assert_eq!(button_span(row, "[ Copy ]"), Some((22, 30)));
    assert_eq!(button_span("Config  [ Copy ] here", "[ Copy ]"), None);
    assert_eq!(button_span("", "[ Copy ]"), None);
}

/// The version row names the build commit and date, either alone when the other is unknown, and
/// neither in parentheses when both are unknown.
#[test]
fn the_version_row_names_what_the_build_knows() {
    assert_eq!(
        version_line("1.3.11", "a17ee368", "2026-10-24"),
        "SonicTerm 1.3.11 (a17ee368, 2026-10-24)"
    );
    assert_eq!(version_line("1.3.11", "a17ee368", ""), "SonicTerm 1.3.11 (a17ee368)");
    assert_eq!(version_line("1.3.11", "", "2026-10-24"), "SonicTerm 1.3.11 (2026-10-24)");
    assert_eq!(version_line("1.3.11", "", ""), "SonicTerm 1.3.11");
}

/// The renderer row names how frames are drawn, then the adapter and its API whenever they are
/// known, GDI included; an adapter with no name shows only its API.
#[test]
fn the_renderer_row_names_the_selected_path_adapter_and_api() {
    let labels = english();
    let row = |kind: RendererKind, adapter: &str, backend: &str| {
        renderer_value(
            &RendererFacts { kind, adapter: adapter.into(), backend: backend.into() },
            &labels,
        )
    };
    assert_eq!(row(RendererKind::Gpu, "Apple M4 Max", "Metal"), "GPU · Apple M4 Max (Metal)");
    assert_eq!(row(RendererKind::Gpu, "", "Metal"), "GPU (Metal)");
    assert_eq!(
        row(RendererKind::SoftwareAdapter, "llvmpipe", "Vulkan"),
        "Software · llvmpipe (Vulkan)"
    );
    assert_eq!(
        row(RendererKind::SoftwareForced, "NVIDIA RTX", "DirectX 12"),
        "Software (forced) · NVIDIA RTX (DirectX 12)"
    );
    assert_eq!(
        row(RendererKind::Gdi, "Microsoft Basic Render Driver", "DirectX 12"),
        "GDI (software) · Microsoft Basic Render Driver (DirectX 12)"
    );
    assert_eq!(row(RendererKind::Gdi, "", ""), "GDI (software)");
    assert_eq!(row(RendererKind::NotStarted, "", ""), "not started");
}

/// Labels align by display width, so wide CJK labels still line their values up in one column.
#[test]
fn wide_labels_keep_the_value_column_aligned() {
    let mut labels = english();
    labels.renderer = "渲染器".into();
    labels.logs = "日志".into();
    let text = render(&mac_facts(), &labels, 100).text;
    let column = |prefix: &str| {
        let line = text.lines().find(|line| line.starts_with(prefix)).unwrap();
        let label_end = line.find(char::is_whitespace).unwrap();
        let value_start = label_end
            + line[label_end..].find(|character: char| !character.is_whitespace()).unwrap();
        UnicodeWidthStr::width(&line[..value_start])
    };
    assert_eq!(column("渲染器"), column("Font"));
    assert_eq!(column("日志"), column("Font"));
}

/// The OS row's architecture uses each OS's own name, and its OS name comes from the
/// distribution's os-release or the Windows registry, with Windows 11 named from its build.
#[test]
fn os_and_architecture_use_each_platforms_names() {
    assert_eq!(arch_label("macos", "aarch64"), "arm64");
    assert_eq!(arch_label("macos", "x86_64"), "x86_64");
    assert_eq!(arch_label("windows", "x86_64"), "x64");
    assert_eq!(arch_label("windows", "aarch64"), "ARM64");
    assert_eq!(arch_label("linux", "x86_64"), "x86_64");
    let pretty = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n";
    assert_eq!(linux_label(pretty).as_deref(), Some("Ubuntu 24.04.1 LTS"));
    assert_eq!(linux_label("NAME=Fedora\nVERSION_ID=41\n").as_deref(), Some("Fedora 41"));
    assert_eq!(linux_label("ID=custom\n"), None);
    assert_eq!(
        windows_label("Windows 10 Pro", "24H2", "26100"),
        "Windows 11 Pro 24H2 (build 26100)"
    );
    assert_eq!(
        windows_label("Windows 10 Pro", "22H2", "19045"),
        "Windows 10 Pro 22H2 (build 19045)"
    );
    assert_eq!(windows_label("Windows 10 Pro", "", ""), "Windows 10 Pro");
}

/// A failed theme or keymap reload splits their shared row: each shows the active name, and the
/// one that failed adds its error, so the window never names a theme or keymap that is not in use.
#[test]
fn a_failed_theme_or_keymap_reload_shows_the_active_name_and_its_error() {
    let mut facts = mac_facts();
    facts.keymap_error = Some("keymap \"no-such-keymap\" not found".into());
    let text = render(&facts, &english(), 100).text;
    assert!(text.contains("\nTheme      apollo\n"), "{text}");
    assert!(
        text.contains("\nKeymap     sonicterm-macos  ✗ keymap \"no-such-keymap\" not found\n"),
        "{text}"
    );
    assert!(!text.contains("Keymap  sonicterm-macos"), "the shared row is gone: {text}");
}
