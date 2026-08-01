use super::*;
use std::path::Path;

fn request(file: &str) -> OpenScriptRequest {
    let root = if cfg!(windows) { Path::new(r"C:\work") } else { Path::new("/work") };
    OpenScriptRequest::resolve(PathBuf::from(file), root).unwrap()
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
