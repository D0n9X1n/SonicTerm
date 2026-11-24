use super::*;
use std::path::Path;

fn request(file: &str) -> OpenScriptRequest {
    let root = if cfg!(windows) { Path::new(r"C:\work") } else { Path::new("/work") };
    OpenScriptRequest::resolve(PathBuf::from(file), root).unwrap()
}

/// Only local, bounded native absolute directories may replace ordinary shell defaults.
#[test]
fn inherited_cwd_validates_local_authority_and_native_paths() {
    for authority in ["", "localhost", "MY-HOST"] {
        for (style, path, expected) in [
            (PathStyle::Posix, "/", "/"),
            (PathStyle::Posix, "/work/a b;c", "/work/a b;c"),
            (PathStyle::Windows, "/C:/", "C:\\"),
            (PathStyle::Windows, "/c:/work/a b;c", r"C:\work\a b;c"),
        ] {
            let cwd = Osc7Cwd { authority: authority.into(), path: path.into(), uri_path: true };
            let launch = PaneLaunch::default().inherit_cwd(Some(&cwd), style, "my-host");
            assert_eq!(launch.cwd, Some(PathBuf::from(expected)));
        }
    }
    for (style, authority, path) in [
        (PathStyle::Posix, "remote", "/work"),
        (PathStyle::Posix, "localhost", "relative"),
        (PathStyle::Posix, "", r"C:\work"),
        (PathStyle::Windows, "", "/work"),
        (PathStyle::Windows, "", "C:"),
        (PathStyle::Windows, "", r"\\server\share"),
        (PathStyle::Posix, "", "//server/share"),
        (PathStyle::Posix, "", "/work\nunsafe"),
    ] {
        let cwd = Osc7Cwd { authority: authority.into(), path: path.into(), uri_path: true };
        assert_eq!(PaneLaunch::default().inherit_cwd(Some(&cwd), style, "my-host").cwd, None);
    }
    let huge = Osc7Cwd {
        authority: String::new(),
        path: format!("/{}", "x".repeat(4096)),
        uri_path: true,
    };
    assert_eq!(PaneLaunch::default().inherit_cwd(Some(&huge), PathStyle::Posix, "host").cwd, None);
}

/// Explicit launch directories, script parents and command launches outrank a source pane's OSC 7.
#[test]
fn inherited_cwd_never_overrides_explicit_or_script_launch() {
    let inherited = Osc7Cwd { authority: String::new(), path: "/different".into(), uri_path: true };
    let script = PaneLaunch::for_script(request("scripts/build.sh"));
    let explicit = PaneLaunch { cwd: Some(PathBuf::from("/explicit")), ..PaneLaunch::default() };
    let command = PaneLaunch {
        command: Some(CommandLaunch { program: "ls".into(), args: Vec::new() }),
        ..PaneLaunch::default()
    };
    for launch in [script, explicit, command] {
        assert_eq!(launch.clone().inherit_cwd(Some(&inherited), PathStyle::Posix, "host"), launch);
    }
}

/// Window snapshots stay pane-local and fail closed on parser contention or invalid OSC 7.
#[test]
fn source_window_selects_only_its_active_pane_and_never_waits() {
    use crate::app::App;
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child", "other"]);
    let prefix = if cfg!(windows) { "/C:" } else { "" };
    app.__test_advance_pane_parser(
        main,
        format!("\x1b]7;file://localhost{prefix}/main\x07").as_bytes(),
    );
    let pane_ids: Vec<_> =
        app.registry.windows[&child].tab_states.iter().map(|tab| tab.active_pane).collect();
    for (pane, name) in pane_ids.iter().zip(["child", "other"]) {
        app.__test_advance_child_pane_parser(
            child,
            *pane,
            format!("\x1b]7;file://localhost{prefix}/{name}\x07").as_bytes(),
        );
    }
    app.registry.windows.get_mut(&child).unwrap().tabs.activate(0);
    let expected = if cfg!(windows) { r"C:\child" } else { "/child" };
    let selected = PaneLaunch::from_window(app.registry.windows.get(&child), "host");
    assert_eq!(selected.cwd, Some(PathBuf::from(expected)));
    assert_ne!(selected.cwd, PaneLaunch::from_window(app.main(), "host").cwd);
    // Same-tab splits must use active_pane, not the first tree leaf or another tab.
    let window = app.registry.windows.get_mut(&child).unwrap();
    let inactive = window.tab_states.remove(1).active_pane;
    let tab = &mut window.tab_states[0];
    assert!(tab.tree.split(tab.active_pane, sonicterm_cfg::keymap::Direction::Right, inactive));
    tab.active_pane = inactive;
    let other = if cfg!(windows) { r"C:\other" } else { "/other" };
    assert_eq!(
        PaneLaunch::from_window(app.registry.windows.get(&child), "host").cwd,
        Some(PathBuf::from(other))
    );
    app.registry.windows.get_mut(&child).unwrap().tab_states[0].active_pane = pane_ids[0];
    let parser = app.registry.windows[&child].panes[&pane_ids[0]].parser.clone();
    let guard = parser.lock();
    assert_eq!(PaneLaunch::from_window(app.registry.windows.get(&child), "host").cwd, None);
    drop(guard);
    parser.lock().advance(b"\x1b]7;file://remote/tmp\x07");
    assert_eq!(PaneLaunch::from_window(app.registry.windows.get(&child), "host").cwd, None);
    parser.lock().advance(b"\x1b]7;file://localhost/bad%xx\x07");
    assert_eq!(PaneLaunch::from_window(app.registry.windows.get(&child), "host").cwd, None);
}

#[test]
fn default_launch_preserves_the_existing_shell_defaults() {
    let launch = PaneLaunch::default();
    let opts = launch.shell_spawn_opts("SonicTerm".to_string(), Some("zsh".to_string()));

    assert_eq!(launch.cwd, None);
    assert_eq!(launch.script, None);
    assert_eq!(opts.cwd, None);
    assert_eq!(opts.term_program, "SonicTerm");
    assert_eq!(opts.shell.as_deref(), Some("zsh"));
}

#[test]
fn script_launch_uses_the_absolute_paths_parent_as_cwd() {
    let request = request("scripts/build.sh");
    let launch = PaneLaunch::for_script(request.clone());
    let opts = launch.shell_spawn_opts("SonicTerm".to_string(), None);

    assert_eq!(launch.cwd.as_deref(), request.launch_path.parent());
    assert_eq!(opts.cwd.as_deref(), request.launch_path.parent());
    assert_eq!(launch.script, Some(request));
}

#[test]
fn draft_uses_the_resolved_shell_and_absolute_launch_path() {
    let launch = PaneLaunch::for_script(request("scripts/build.sh"));
    let draft = launch.draft_for_shell("/bin/zsh").unwrap().unwrap();

    assert!(draft.starts_with("sh "));
    assert!(draft.contains("scripts/build.sh"));
    assert!(!draft.chars().any(char::is_control));
}

#[test]
fn unsupported_shell_pair_returns_a_typed_rejection() {
    let launch = PaneLaunch::for_script(request("scripts/build.sh"));

    assert_eq!(launch.draft_for_shell("pwsh.exe"), Err(DraftRejection::UnsupportedPair));
}

/// The draft reaches the shell through the real PTY without its Enter, and, since it runs
/// nothing, it leaves the pane's shell token trusted: the fake shell's tokened prompt, printed
/// after the draft was queued, still latches the pane.
#[cfg(unix)]
#[test]
fn spawned_pane_sends_the_unterminated_draft_through_the_real_pty() {
    use crate::app::App;
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use sonicterm_types::{format_script_draft, ShellDialect};
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let dir = std::env::temp_dir().join(format!(
        "sonicterm-pane-launch-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let shell = dir.join("sh");
    let fake_shell = "#!/bin/sh\nsleep 0.3\n\
                      printf '\\033]133;A;sonicterm_token=%s\\007' \"$SONICTERM_SHELL_TOKEN\"\n\
                      exec cat\n";
    std::fs::write(&shell, fake_shell).unwrap();
    let mut permissions = std::fs::metadata(&shell).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&shell, permissions).unwrap();

    let script = dir.join("draft target.sh");
    let request = OpenScriptRequest::resolve(script.clone(), Path::new("/")).unwrap();
    let launch = PaneLaunch::for_script(request);
    let expected = format_script_draft(ShellDialect::Posix, &script).unwrap();

    let mut config = Config::default();
    config.terminal.shell = Some(shell.to_string_lossy().into_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_synthetic_main();
    app.new_tab_with_launch("script", launch);

    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let (text, authenticated) = {
            let window = app.main().unwrap();
            let pane_id = window.tab_states[0].active_pane;
            let parser = window.panes[&pane_id].parser.lock();
            let text = parser
                .grid()
                .rows_iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect::<String>();
            (text, parser.shell_markers_authenticated())
        };
        if text.contains(&expected) && authenticated {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "PTY never echoed draft {expected:?} and latched (latched: {authenticated}); grid={text:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Identity matching accepts only the same regular file, never a replacement or a symlink to it.
#[cfg(unix)]
#[test]
fn file_identity_matches_only_the_classified_regular_file() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("tool");
    std::fs::write(&file, b"one").unwrap();
    let metadata = std::fs::metadata(&file).unwrap();
    let identity = sonicterm_types::FileIdentity { device: metadata.dev(), inode: metadata.ino() };
    assert!(file_identity_matches(&file, identity));

    let link = root.path().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(!file_identity_matches(&link, identity), "a symlink is refused even to the same file");

    // The replacement is written while the original still exists, so the two hold distinct
    // inodes; deleting first lets ext4 or tmpfs hand the freed inode straight to the new file.
    // The rename then swaps the path atomically.
    let other = root.path().join("other");
    std::fs::write(&other, b"two").unwrap();
    std::fs::rename(&other, &file).unwrap();
    assert!(!file_identity_matches(&file, identity), "a replaced inode is refused");
    assert!(!file_identity_matches(&root.path().join("missing"), identity));
}

/// The log kind names who asked for a command, and only a link launch carries a scheme.
#[test]
fn kind_and_scheme_follow_the_launch_origin() {
    let identity = sonicterm_types::FileIdentity { device: 1, inode: 2 };
    let command = Some(CommandLaunch { program: "tool".into(), args: Vec::new() });
    let with = |origin: LaunchOrigin| PaneLaunch {
        command: command.clone(),
        origin,
        ..PaneLaunch::default()
    };
    assert_eq!(with(LaunchOrigin::Launcher).kind(), "command");
    assert_eq!(with(LaunchOrigin::Executable { identity }).kind(), "executable");
    assert_eq!(with(LaunchOrigin::Url { scheme: "ssh" }).kind(), "url");
    assert_eq!(with(LaunchOrigin::Url { scheme: "ssh" }).scheme(), Some("ssh"));
    assert_eq!(with(LaunchOrigin::Launcher).scheme(), None);
    assert!(with(LaunchOrigin::Launcher).executable_still_matches());
    let no_command =
        PaneLaunch { origin: LaunchOrigin::Executable { identity }, ..PaneLaunch::default() };
    assert!(!no_command.executable_still_matches(), "an executable origin without a command fails");
}

/// The launch directory `report` (an OSC without its introducer and BEL) gives a new shell, read
/// through a real parser and the native validation seam for `style`, on host `my-host`.
fn launch_cwd_after(report: &[u8], style: PathStyle) -> Option<PathBuf> {
    let mut parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(20, 2));
    parser.advance(&[b"\x1b]".as_slice(), report, b"\x07"].concat());
    PaneLaunch::default().inherit_cwd(parser.osc7_cwd(), style, "my-host").cwd
}

/// A bare native-absolute 9;9 path, a local `CurrentDir` URI, a `kitty-shell-cwd://` OSC 7 and
/// WezTerm's two Windows OSC 7 forms (any host case) give a launch directory; drive-relative,
/// bare-drive, UNC, relative, foreign-OS, remote and bare `CurrentDir` reports give none.
#[test]
fn directory_reports_reach_the_launch_cwd_only_through_native_validation() {
    for (report, style, expected) in [
        (b"9;9;/tmp".as_slice(), PathStyle::Posix, "/tmp"),
        (b"9;9;\"/a b\"", PathStyle::Posix, "/a b"),
        (b"1337;CurrentDir=file://MY-HOST/w", PathStyle::Posix, "/w"),
        (b"7;kitty-shell-cwd://localhost/w%20x", PathStyle::Posix, "/w%20x"),
        (b"9;9;C:\\x", PathStyle::Windows, r"C:\x"),
        (b"9;9;C:/x", PathStyle::Windows, r"C:\x"),
        (b"9;9;\"C:\\fixture\"", PathStyle::Windows, r"C:\fixture"),
        (b"7;file://localhost/C:\\Users\\x", PathStyle::Windows, r"C:\Users\x"),
        (b"7;file://My-Host/C:/Users/x", PathStyle::Windows, r"C:\Users\x"),
        (b"1337;CurrentDir=file://localhost/C:/fixture", PathStyle::Windows, r"C:\fixture"),
        (b"7;kitty-shell-cwd://localhost/C:/fixture", PathStyle::Windows, r"C:\fixture"),
    ] {
        assert_eq!(
            launch_cwd_after(report, style),
            Some(PathBuf::from(expected)),
            "{report:?} {style:?}"
        );
    }
    for (report, style) in [
        (b"9;9;C:foo".as_slice(), PathStyle::Windows),
        (b"9;9;C:", PathStyle::Windows),
        (b"9;9;\\\\host\\share", PathStyle::Windows),
        (b"9;9;//host/x", PathStyle::Windows),
        (b"9;9;//host/x", PathStyle::Posix),
        (b"9;9;rel/dir", PathStyle::Posix),
        (b"9;9;rel\\dir", PathStyle::Windows),
        (b"9;9;/tmp", PathStyle::Windows),
        (b"9;9;C:\\x", PathStyle::Posix),
        (b"9;9;file://otherhost/b", PathStyle::Posix),
        (b"1337;CurrentDir=/c", PathStyle::Posix),
    ] {
        assert_eq!(launch_cwd_after(report, style), None, "{report:?} {style:?}");
    }
}

/// A trusted `/home/a` followed by a remote URI, a bare `CurrentDir`, a drive-relative 9;9 path or
/// a malformed URI leaves no usable launch directory and a new revision, so the relative-path
/// hover cache keyed by that revision is invalidated too.
#[test]
fn a_later_untrusted_report_revokes_the_launch_cwd() {
    for (report, style) in [
        (b"7;file://otherhost/b".as_slice(), PathStyle::Posix),
        (b"1337;CurrentDir=/c", PathStyle::Posix),
        (b"9;9;C:foo", PathStyle::Windows),
        (b"9;9;file:///bad%GG", PathStyle::Posix),
    ] {
        let mut parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(20, 2));
        let trusted = if style == PathStyle::Windows { "/C:/home/a" } else { "/home/a" };
        parser.advance(format!("\x1b]7;file://{trusted}\x07").as_bytes());
        let launch = |parser: &sonicterm_vt::vt::Parser| {
            PaneLaunch::default().inherit_cwd(parser.osc7_cwd(), style, "my-host").cwd
        };
        assert!(launch(&parser).is_some(), "{report:?}");
        let revision = parser.cwd_revision();
        parser.advance(&[b"\x1b]".as_slice(), report, b"\x07"].concat());
        assert_eq!(launch(&parser), None, "{report:?}");
        assert_ne!(parser.cwd_revision(), revision, "{report:?}");
    }
}

/// Only a URI path may write a Windows drive after a separator (`/C:/x`); a bare 9;9 path is read
/// as sent, so one that was not drive-rooted gives neither a launch directory nor a link base.
#[test]
fn only_a_uri_path_drops_the_separator_before_a_windows_drive() {
    let link_base = |report: &[u8]| {
        let mut parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(20, 2));
        parser.advance(&[b"\x1b]".as_slice(), report, b"\x07"].concat());
        let target = sonicterm_cfg::url_scan::DetectedTarget::PathCandidate(r".\f".into());
        super::super::path_target::resolve_detected_path(
            &target,
            PathStyle::Windows,
            parser.osc7_cwd(),
            None,
            "my-host",
        )
    };
    for report in [
        b"7;file:///C:/x".as_slice(),
        b"7;/C:/x",
        b"9;9;file:///C:/x",
        b"1337;CurrentDir=kitty-shell-cwd://localhost/C:/x",
    ] {
        assert_eq!(launch_cwd_after(report, PathStyle::Windows), Some(PathBuf::from(r"C:\x")));
        assert_eq!(link_base(report), Some(PathBuf::from(r"C:\x\f")), "{report:?}");
    }
    for report in [b"9;9;/C:/x".as_slice(), b"9;9;\\C:\\x", b"9;9;\"/C:/x\""] {
        assert_eq!(launch_cwd_after(report, PathStyle::Windows), None, "{report:?}");
        assert_eq!(link_base(report), None, "{report:?}");
    }
}
