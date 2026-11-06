//! Pins the shell integration the bridge prints: the snippet text is fixed and never holds a token,
//! and, where the shell is installed, a real interactive shell in a pseudo-terminal that loads it
//! emits OSC 133 A, B, C and D in order, each carrying the pane's token, reports a failing
//! command's status, keeps the token from the commands it runs, and leaves an existing prompt
//! hook running.

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

/// A token the tests give the shell; any 32 hex digits would do.
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn shell_names_map_to_snippets() {
    // Only the supported shells are accepted, by their exact names.
    assert_eq!(Shell::from_name("zsh"), Some(Shell::Zsh));
    assert_eq!(Shell::from_name("bash"), Some(Shell::Bash));
    assert_eq!(Shell::from_name("fish"), None);
    assert_eq!(Shell::from_name("ZSH"), None);
}

#[test]
fn snippets_read_and_unset_the_token_and_never_hold_one() {
    // The token is read at run time and removed from the environment; the printed text is the
    // same in every pane, so it can live in a dotfile.
    for shell in [Shell::Zsh, Shell::Bash] {
        let text = shell.snippet();
        assert!(text.contains("SONICTERM_SHELL_TOKEN"), "{shell:?} reads the token");
        assert!(text.contains("unset SONICTERM_SHELL_TOKEN"), "{shell:?} unsets it");
        assert!(text.contains("sonicterm_token="), "{shell:?} appends it to markers");
        assert!(!text.contains(TOKEN));
    }
}

/// A pseudo-terminal pair: the controlling side the test reads and writes, and the terminal side
/// the shell gets as its stdin, stdout and stderr.
fn open_pty() -> (std::fs::File, OwnedFd) {
    let mut controller = -1;
    let mut terminal = -1;
    let opened =
        // SAFETY: both out-pointers are valid; the name, termios and window size are optional.
        unsafe {
            libc::openpty(
                &mut controller,
                &mut terminal,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
    assert_eq!(opened, 0, "openpty failed");
    let controller =
        // SAFETY: `openpty` returned `controller` open, and this `File` takes sole ownership.
        unsafe { std::fs::File::from_raw_fd(controller) };
    let terminal =
        // SAFETY: `openpty` returned `terminal` open, and this `OwnedFd` takes sole ownership.
        unsafe { OwnedFd::from_raw_fd(terminal) };
    (controller, terminal)
}

/// Read what arrives on `controller` for `wait`, without blocking past it.
fn read_for(controller: &mut std::fs::File, wait: Duration) -> Vec<u8> {
    use std::os::fd::AsRawFd;
    let descriptor = controller.as_raw_fd();
    let mut collected = Vec::new();
    let deadline = Instant::now() + wait;
    let mut chunk = [0u8; 8192];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let mut poll = libc::pollfd { fd: descriptor, events: libc::POLLIN, revents: 0 };
        let timeout_ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
        let ready =
            // SAFETY: `poll` points at one valid `pollfd`, and the count passed is 1.
            unsafe { libc::poll(&mut poll, 1, timeout_ms) };
        if ready <= 0 {
            // When: `poll` timed out or failed, no more output is coming in time.
            break;
        }
        match controller.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => collected.extend_from_slice(&chunk[..count]),
        }
    }
    collected
}

/// The OSC 133 markers in `output`, in order, with the test token shown as `<T>`.
fn markers(output: &str) -> Vec<String> {
    output
        .split("\u{1b}]133;")
        .skip(1)
        .filter_map(|rest| rest.split('\u{7}').next())
        .map(|marker| marker.replace(TOKEN, "<T>"))
        .collect()
}

/// Run `shell` interactively in a pseudo-terminal: optionally `prehook`, then load `snippet`,
/// then run each of `commands`, and return everything it wrote. `None` when `shell` is absent.
fn drive(
    shell: &str,
    args: &[&str],
    prehook: &str,
    snippet: &str,
    commands: &[&str],
) -> Option<String> {
    let path = which(shell)?;
    let (mut controller, terminal) = open_pty();
    let stdio = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup the terminal side"));
    let mut child = Command::new(&path)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::temp_dir())
        .env("TERM", "xterm-256color")
        .env("SONICTERM_SHELL_TOKEN", TOKEN)
        .stdin(stdio(&terminal))
        .stdout(stdio(&terminal))
        .stderr(stdio(&terminal))
        .spawn()
        .ok()?;
    drop(terminal);
    let mut output = read_for(&mut controller, Duration::from_millis(800));
    let send = |line: &str, controller: &mut std::fs::File, output: &mut Vec<u8>| {
        controller.write_all(line.as_bytes()).unwrap();
        controller.write_all(b"\n").unwrap();
        output.extend(read_for(controller, Duration::from_millis(700)));
    };
    if !prehook.is_empty() {
        send(prehook, &mut controller, &mut output);
    }
    // A shell given by path names its temporary file by its file name only.
    let name =
        std::path::Path::new(shell).file_name().and_then(|name| name.to_str()).unwrap_or("sh");
    let file =
        std::env::temp_dir().join(format!("sonicterm-shellint-{}-{name}.sh", std::process::id()));
    std::fs::write(&file, snippet).unwrap();
    send(&format!("eval \"$(cat '{}')\"", file.display()), &mut controller, &mut output);
    for command in commands {
        send(command, &mut controller, &mut output);
    }
    send("exit", &mut controller, &mut output);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&file);
    Some(String::from_utf8_lossy(&output).into_owned())
}

/// The absolute path of `program` on `PATH`, if it is installed.
fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| std::path::Path::new(dir).join(program))
        .find(|candidate| candidate.is_file())
}

/// The bash to test: `SONICTERM_TEST_BASH` when set (a local bash 4.4+ build), else `bash` on
/// `PATH`, when it is 4.4 or newer, the oldest the snippet supports.
fn supported_bash() -> Option<String> {
    let candidate = std::env::var("SONICTERM_TEST_BASH").unwrap_or_else(|_| "bash".to_owned());
    let output = Command::new(&candidate)
        .args(["-c", "echo ${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.split_whitespace().filter_map(|part| part.parse::<u32>().ok());
    let supported = matches!(
        (parts.next(), parts.next()),
        (Some(major), Some(minor)) if major > 4 || (major == 4 && minor >= 4)
    );
    supported.then_some(candidate)
}

/// Only the markers that carry the test token. Tokenless markers from an existing integration
/// may appear too; SonicTerm drops those once the pane is authenticated.
fn tokened(output: &str) -> Vec<String> {
    markers(output).into_iter().filter(|marker| marker.ends_with("sonicterm_token=<T>")).collect()
}

/// A command a child runs to count environment variables holding the token; the pattern is
/// written in two quoted halves so the echoed command line itself never contains the token.
const LEAK_PROBE: &str =
    "sh -c 'printf \"leak-%s\\n\" \"$(env | grep -c 0123456789abcdef\"\"0123456789abcdef)\"'";

/// The tokened markers form complete A B [C D] cycles: every C is followed by its D before the
/// next A, every A by a B; the D statuses are `statuses`, in order.
fn check_cycles(shell: &str, output: &str, statuses: &[i32]) {
    let found = tokened(output);
    let kinds: String = found.iter().map(|marker| &marker[..1]).collect();
    // Each command's cycle is C then D, and each prompt is A then B.
    assert!(!kinds.contains("CA"), "{shell}: a command ended without its D: {kinds}");
    assert!(!kinds.contains("CC"), "{shell}: two commands started without a D: {kinds}");
    assert!(!kinds.contains("AA"), "{shell}: a prompt had no B: {kinds}");
    let ends: Vec<i32> = found
        .iter()
        .filter_map(|marker| marker.strip_prefix("D;"))
        .filter_map(|rest| rest.split(';').next()?.parse().ok())
        .collect();
    assert_eq!(ends, statuses, "{shell}: the D statuses: {found:?}");
    assert!(output.contains("leak-0"), "{shell}: the leak probe did not run");
    assert!(!output.contains("leak-1"), "{shell}: a command inherited the token");
}

#[test]
fn an_interactive_zsh_emits_tokened_markers_in_order() {
    // zsh with allexport on and a token name already exported, beside an existing OSC 133
    // integration whose tokenless A and B keep printing: every command still gets its tokened C
    // and D with the right status, and no command inherits the token.
    let prehook = "setopt allexport; export __sonicterm_token __sonicterm_ran; \
                   autoload -Uz add-zsh-hook add-zle-hook-widget; \
                   theirs_a() { printf '\\e]133;A\\a' }; theirs_b() { printf '\\e]133;B\\a' }; \
                   add-zsh-hook precmd theirs_a; add-zle-hook-widget line-init theirs_b";
    let commands = ["false", "false", LEAK_PROBE, "true"];
    let Some(output) = drive("zsh", &["-f", "-i"], prehook, Shell::Zsh.snippet(), &commands) else {
        eprintln!("skipped: zsh is not installed");
        return;
    };
    check_cycles("zsh", &output, &[1, 1, 0, 0]);
    assert!(output.contains("\u{1b}]133;A\u{7}"), "the existing integration stopped running");
}

#[test]
fn an_interactive_bash_emits_tokened_markers_in_order() {
    // bash 4.4+ with `set -a`, history filtering that drops repeated and space-prefixed lines, an
    // existing tokenless B in PS1 and an existing PROMPT_COMMAND: a repeated command, a
    // space-prefixed one and an empty line are told apart from the prompt's command number, our
    // tokened B is still installed, and no command inherits the token.
    let Some(bash) = supported_bash() else {
        eprintln!("skipped: bash 4.4 or newer is not installed (set SONICTERM_TEST_BASH)");
        return;
    };
    let prehook = "set -a; HISTCONTROL=ignoreboth; PROMPT_COMMAND='printf \"[theirs]\"'; \
                   PS1='\\[\\e]133;B\\a\\]$ '";
    let commands = ["false", "false", " false", "", LEAK_PROBE, "true"];
    let args = ["--norc", "--noprofile", "-i"];
    let output = drive(&bash, &args, prehook, Shell::Bash.snippet(), &commands)
        .expect("bash was found above");
    check_cycles("bash", &output, &[1, 1, 1, 0, 0]);
    let after_load = output.rsplit("sonicterm-shellint").next().unwrap_or("");
    assert!(after_load.contains("[theirs]"), "the existing PROMPT_COMMAND stopped running");
}
