//! Pins which panes get a shell token: a shell pane gets a fresh one in its environment, the same
//! token in its parser and the input flag its writer sets; a direct command launch gets none, so
//! the program it runs cannot forge authenticated OSC 133 markers.

use std::sync::atomic::Ordering;

use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::ShellSpawnOpts;
use sonicterm_types::CommandLaunch;

use super::*;
use crate::app::pane_launch::PaneLaunch;

/// A fresh parser behind the lock the app keeps it in.
fn parser() -> Arc<Mutex<Parser>> {
    Arc::new(Mutex::new(Parser::new(Grid::new(20, 4))))
}

/// OSC 133 A carrying `token`, as a SonicTerm shell integration prints it.
fn tokened_prompt(token: &str) -> Vec<u8> {
    format!("\x1b]133;A;sonicterm_token={token}\x07").into_bytes()
}

#[test]
fn a_shell_pane_gets_a_token_its_parser_checks() {
    // The token in the shell's environment is the one the parser accepts, it is 32 hex digits,
    // and the input flag the writer will set is the one the parser reads.
    let parser = parser();
    let mut opts = ShellSpawnOpts::default();
    arm_shell_token(&parser, &mut opts, &PaneLaunch::default());
    let token = opts
        .extra_env
        .iter()
        .find(|(name, _)| name == SHELL_TOKEN_ENV)
        .map(|(_, value)| value.clone())
        .expect("a shell pane gets a token");
    assert_eq!(token.len(), 32);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let input_seen = opts.input_seen.clone().expect("the writer gets the input flag");
    parser.lock().advance(&tokened_prompt(&token));
    assert!(parser.lock().shell_markers_authenticated(), "the parser holds the same token");

    // A second pane gets its own token, and input before its first marker stops it latching.
    let second = self::parser();
    let mut second_opts = ShellSpawnOpts::default();
    arm_shell_token(&second, &mut second_opts, &PaneLaunch::default());
    let second_token = second_opts.extra_env.last().map(|(_, value)| value.clone()).unwrap();
    assert_ne!(token, second_token, "every pane gets its own token");
    second_opts.input_seen.as_ref().unwrap().store(true, Ordering::SeqCst);
    second.lock().advance(&tokened_prompt(&second_token));
    assert!(!second.lock().shell_markers_authenticated(), "a token after input never latches");
    assert!(!input_seen.load(Ordering::SeqCst), "each pane has its own flag");
}

#[test]
fn a_direct_command_launch_gets_no_token() {
    // `-e` runs a program with no shell integration to hide the token from it, so the program
    // gets none, its parser expects none, and no marker it prints is ever verified.
    let parser = parser();
    let mut opts = ShellSpawnOpts::default();
    let launch = PaneLaunch {
        command: Some(CommandLaunch { program: "tool".into(), args: Vec::new() }),
        ..PaneLaunch::default()
    };
    arm_shell_token(&parser, &mut opts, &launch);
    assert!(opts.extra_env.iter().all(|(name, _)| name != SHELL_TOKEN_ENV));
    assert!(opts.input_seen.is_none());
    parser.lock().advance(&tokened_prompt("0123456789abcdef0123456789abcdef"));
    assert!(!parser.lock().shell_markers_authenticated());
}

/// End to end through a real pane: a shell with no SonicTerm integration keeps the token in its
/// environment, so a command typed into it can print a marker carrying the right token. Because
/// the command arrived as input, the marker is admitted unverified and never latches the pane.
#[cfg(unix)]
#[test]
fn a_command_in_an_unintegrated_shell_cannot_forge_a_verified_marker() {
    use crate::app::App;
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use std::time::{Duration, Instant};

    let mut config = Config::default();
    config.terminal.shell = Some("/bin/sh".to_owned());
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_synthetic_main();
    app.new_tab_with_launch("plain", PaneLaunch::default());
    let (pane_id, parser) = {
        let window = app.main().unwrap();
        let pane_id = window.tab_states.last().unwrap().active_pane;
        (pane_id, Arc::clone(&window.panes[&pane_id].parser))
    };
    // The command prints OSC 133 A with the pane's own token, read from the environment.
    let forge =
        b"printf '\\033]133;A;sonicterm_token=%s\\007' \"$SONICTERM_SHELL_TOKEN\"\r".to_vec();
    {
        let window = app.main().unwrap();
        let pty = window.panes[&pane_id].pty.as_ref().expect("a live shell");
        assert!(crate::app::App::queue_pty_input(
            None,
            pty,
            pane_id,
            crate::app::PtyInputSource::Keyboard,
            forge,
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while parser.lock().grid().prompts_len() == 0 {
        assert!(Instant::now() < deadline, "the forged marker never arrived");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !parser.lock().shell_markers_authenticated(),
        "a typed command forged a verified marker"
    );
    drop(app);
}
