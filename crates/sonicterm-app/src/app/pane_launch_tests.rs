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
            let cwd = Osc7Cwd { authority: authority.into(), path: path.into() };
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
        let cwd = Osc7Cwd { authority: authority.into(), path: path.into() };
        assert_eq!(PaneLaunch::default().inherit_cwd(Some(&cwd), style, "my-host").cwd, None);
    }
    let huge = Osc7Cwd { authority: String::new(), path: format!("/{}", "x".repeat(4096)) };
    assert_eq!(PaneLaunch::default().inherit_cwd(Some(&huge), PathStyle::Posix, "host").cwd, None);
}

/// Explicit launch directories and script parents outrank a source pane's OSC 7.
#[test]
fn inherited_cwd_never_overrides_explicit_or_script_launch() {
    let inherited = Osc7Cwd { authority: String::new(), path: "/different".into() };
    let script = PaneLaunch::for_script(request("scripts/build.sh"));
    let explicit = PaneLaunch { cwd: Some(PathBuf::from("/explicit")), script: None };
    for launch in [script, explicit] {
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
        app.windows[&child].tab_states.iter().map(|tab| tab.active_pane).collect();
    for (pane, name) in pane_ids.iter().zip(["child", "other"]) {
        app.__test_advance_child_pane_parser(
            child,
            *pane,
            format!("\x1b]7;file://localhost{prefix}/{name}\x07").as_bytes(),
        );
    }
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    let expected = if cfg!(windows) { r"C:\child" } else { "/child" };
    let selected = PaneLaunch::from_window(app.windows.get(&child), "host");
    assert_eq!(selected.cwd, Some(PathBuf::from(expected)));
    assert_ne!(selected.cwd, PaneLaunch::from_window(app.main(), "host").cwd);
    // Same-tab splits must use active_pane, not the first tree leaf or another tab.
    let window = app.windows.get_mut(&child).unwrap();
    let inactive = window.tab_states.remove(1).active_pane;
    let tab = &mut window.tab_states[0];
    assert!(tab.tree.split(tab.active_pane, sonicterm_cfg::keymap::Direction::Right, inactive));
    tab.active_pane = inactive;
    let other = if cfg!(windows) { r"C:\other" } else { "/other" };
    assert_eq!(
        PaneLaunch::from_window(app.windows.get(&child), "host").cwd,
        Some(PathBuf::from(other))
    );
    app.windows.get_mut(&child).unwrap().tab_states[0].active_pane = pane_ids[0];
    let parser = app.windows[&child].panes[&pane_ids[0]].parser.clone();
    let guard = parser.lock();
    assert_eq!(PaneLaunch::from_window(app.windows.get(&child), "host").cwd, None);
    drop(guard);
    parser.lock().advance(b"\x1b]7;file://remote/tmp\x07");
    assert_eq!(PaneLaunch::from_window(app.windows.get(&child), "host").cwd, None);
    parser.lock().advance(b"\x1b]7;file://localhost/bad%xx\x07");
    assert_eq!(PaneLaunch::from_window(app.windows.get(&child), "host").cwd, None);
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
    std::fs::write(&shell, "#!/bin/sh\nexec cat\n").unwrap();
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

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let text = {
            let window = app.main().unwrap();
            let pane_id = window.tab_states[0].active_pane;
            let parser = window.panes[&pane_id].parser.lock();
            parser
                .grid()
                .rows_iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect::<String>()
        };
        if text.contains(&expected) {
            break;
        }
        assert!(Instant::now() < deadline, "PTY never echoed draft {expected:?}; grid={text:?}");
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(app);
    std::fs::remove_dir_all(dir).unwrap();
}
