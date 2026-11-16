//! Drives a real PowerShell 7 with PSReadLine in a pseudo-terminal, loading the pwsh integration:
//! a failing child's code, a failing cmdlet, a cancelled line and a successful command each end
//! as expected, every marker carries the token, and no command inherits it. Skipped when `pwsh`
//! is not installed, unless `SONICTERM_REQUIRE_PWSH` is set, as CI sets it where PowerShell is.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;

/// How long PowerShell may take to show its first prompt; a loaded runner can take many seconds.
const FIRST_PROMPT_WAIT: Duration = Duration::from_secs(30);

/// Each pwsh run's number, so concurrent tests in one process load their own snippet file.
static NEXT_RUN: AtomicUsize = AtomicUsize::new(0);

/// The pwsh to test: `SONICTERM_TEST_PWSH` when set, else `pwsh` on `PATH`.
pub(super) fn pwsh() -> Option<String> {
    let candidate = std::env::var("SONICTERM_TEST_PWSH").unwrap_or_else(|_| "pwsh".to_owned());
    let found = Command::new(&candidate)
        .args(["-NoLogo", "-NoProfile", "-Command", "exit 0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    found.then_some(candidate)
}

/// Run `pwsh` interactively in a pseudo-terminal, load `snippet` from a file, type each of
/// `lines` (raw bytes, so a test can send Ctrl+C), and return everything it wrote.
pub(super) fn drive_pwsh(pwsh: &str, prehook: &str, snippet: &str, lines: &[&[u8]]) -> String {
    let (mut controller, terminal) = open_pty();
    set_window_size(&terminal);
    let stdio = |fd: &OwnedFd| Stdio::from(fd.try_clone().expect("dup the terminal side"));
    // Ordering: the counter only makes names unique; no other memory is published through it.
    let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let file = std::env::temp_dir()
        .join(format!("sonicterm-shellint-{}-{run}-pwsh.ps1", std::process::id()));
    std::fs::write(&file, snippet).unwrap();
    let mut child = Command::new(pwsh)
        .args(["-NoLogo", "-NoProfile", "-NoExit", "-Command"])
        .arg(format!("{prehook}. '{}'", file.display()))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::temp_dir())
        // .NET names its mutexes and PSReadLine its history under the temporary directory.
        .env("TMPDIR", std::env::temp_dir())
        .env("TERM", "xterm-256color")
        .env("COLUMNS", "120")
        .env("LINES", "40")
        .env("SONICTERM_SHELL_TOKEN", TOKEN)
        .stdin(stdio(&terminal))
        .stdout(stdio(&terminal))
        .stderr(stdio(&terminal))
        .spawn()
        .expect("pwsh starts");
    drop(terminal);
    // PowerShell starts slowly; the first prompt must be up before anything is typed.
    let mut output = first_prompt(&mut controller, &mut child);
    for line in lines {
        type_line(&mut controller, &mut child, line, &output);
        output.extend(answer_for(&mut controller, Duration::from_millis(1500)));
    }
    type_line(&mut controller, &mut child, b"exit\r", &output);
    output.extend(answer_for(&mut controller, Duration::from_millis(1500)));
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&file);
    String::from_utf8_lossy(&output).into_owned()
}

/// Read until the integration's first tokened B marker, the prompt PSReadLine reads keys after,
/// then a little longer for its cursor position query. Panics with what pwsh wrote, and whether it
/// exited, when no prompt comes within [`FIRST_PROMPT_WAIT`].
fn first_prompt(controller: &mut std::fs::File, child: &mut Child) -> Vec<u8> {
    let prompt = format!("\u{1b}]133;B;sonicterm_token={TOKEN}");
    let deadline = Instant::now() + FIRST_PROMPT_WAIT;
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains(&prompt) {
        if Instant::now() >= deadline || matches!(child.try_wait(), Ok(Some(_))) {
            // When: the `deadline` passed or pwsh exited, no prompt is coming.
            panic!("{}", pwsh_failure(child, "showed no first prompt", &output));
        }
        output.extend(answer_for(controller, Duration::from_millis(200)));
    }
    output.extend(answer_for(controller, Duration::from_millis(500)));
    output
}

/// Type `line` into pwsh. A failed write means pwsh closed its terminal, so the panic names the
/// exit status and everything pwsh wrote rather than the bare I/O error.
fn type_line(controller: &mut std::fs::File, child: &mut Child, line: &[u8], output: &[u8]) {
    if let Err(error) = controller.write_all(line) {
        // When: the write failed, pwsh's side of the terminal is gone.
        let what =
            format!("closed its terminal ({error}) before {:?}", String::from_utf8_lossy(line));
        panic!("{}", pwsh_failure(child, &what, output));
    }
}

/// A failure message naming what went wrong, pwsh's exit status if it has exited, and its output.
fn pwsh_failure(child: &mut Child, what: &str, output: &[u8]) -> String {
    // Give an exiting pwsh a moment to be reaped, so its status is reported rather than unknown.
    std::thread::sleep(Duration::from_millis(200));
    let status = match child.try_wait() {
        Ok(Some(status)) => status.to_string(),
        Ok(None) => "still running".to_owned(),
        Err(error) => format!("unknown ({error})"),
    };
    format!("pwsh {what}; status {status}; output: {:?}", String::from_utf8_lossy(output))
}

/// [`read_for`] in short slices, answering each cursor position query (`CSI 6 n`) PSReadLine
/// sends as a terminal would, since it waits for the reply before reading keys.
fn answer_for(controller: &mut std::fs::File, wait: Duration) -> Vec<u8> {
    let deadline = std::time::Instant::now() + wait;
    let mut output = Vec::new();
    while std::time::Instant::now() < deadline {
        let chunk = read_for(controller, Duration::from_millis(50));
        let queries = chunk.windows(4).filter(|window| window == b"\x1b[6n").count();
        for _ in 0..queries {
            controller.write_all(b"\x1b[1;1R").unwrap();
        }
        output.extend(chunk);
    }
    output
}

/// Give the terminal side a 120x40 size: PSReadLine renders against the window and fails on 0x0.
fn set_window_size(terminal: &OwnedFd) {
    use std::os::fd::AsRawFd;
    let size = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    let resized =
        // SAFETY: `terminal` is an open terminal descriptor and `size` a valid `winsize`.
        unsafe { libc::ioctl(terminal.as_raw_fd(), libc::TIOCSWINSZ, &size) };
    assert_eq!(resized, 0, "TIOCSWINSZ failed");
}

#[test]
fn an_interactive_pwsh_emits_tokened_markers_in_order() {
    // Under a custom prompt: a child exiting 3 twice (a repeated code is still 3), a command reading
    // `$LASTEXITCODE` (it sees 3, untouched), a failing cmdlet (1), a cmdlet error then a native
    // failure on one line, twice with the same code (no code either time), a cmdlet failing with
    // no error record (1), a line opened with `{` then cancelled with Ctrl+C (a D with no C:
    // `not_started`), the leak probe and a success.
    let Some(pwsh) = pwsh() else {
        assert!(
            std::env::var_os("SONICTERM_REQUIRE_PWSH").is_none(),
            "SONICTERM_REQUIRE_PWSH is set but pwsh is not installed"
        );
        eprintln!("skipped: pwsh is not installed (set SONICTERM_TEST_PWSH)");
        return;
    };
    let child_exit = format!("& '{pwsh}' -NoProfile -Command 'exit 3'\r");
    let lines: [&[u8]; 11] = [
        child_exit.as_bytes(),
        child_exit.as_bytes(),
        // The next command reads the code the child left, as it would without integration.
        b"Write-Output ('seen=' + '[' + $LASTEXITCODE + ']')\r",
        b"Get-Item /no/such/path\r",
        // A cmdlet error and then a native failure on one line: which ended it is unknown, so D
        // carries no code rather than a guessed 1, whether the native code changed (3 to 4) or
        // repeated (4 again).
        b"Get-Item /no/such/path -ErrorAction SilentlyContinue; sh -c 'exit 4'\r",
        b"Get-Item /no/such/path -ErrorAction SilentlyContinue; sh -c 'exit 4'\r",
        // A cmdlet failing with no error record after that native failure: the line can run no
        // native program, so it is 1, not the stale 4.
        b"Get-Item /no/such/path -ErrorAction Ignore\r",
        b"if ($true) {\r",
        b"\x03",
        b"sh -c 'printf \"leak-%s\\n\" \"$(env | grep -c 0123456789abcdef\"\"0123456789abcdef)\"'\r",
        b"$null = 1\r",
    ];
    // The custom prompt shows `$LASTEXITCODE`, so each prompt proves which code the user sees.
    let prehook = "function global:prompt { \"custom[$LASTEXITCODE]> \" }; ";
    let output = drive_pwsh(&pwsh, prehook, Shell::Pwsh.snippet(), &lines);
    let found = tokened(&output);
    let kinds: String = found.iter().map(|marker| &marker[..1]).collect();
    assert!(!kinds.contains("CA"), "a command ended without its D: {kinds} {found:?}");
    assert!(!kinds.contains("CC"), "two commands started without a D: {kinds}");
    // Each D's code, `None` for a D that carries none.
    let ends: Vec<Option<i32>> = found
        .iter()
        .filter_map(|marker| marker.strip_prefix('D'))
        .map(|rest| rest.strip_prefix(';').and_then(|rest| rest.split(';').next()?.parse().ok()))
        .collect();
    assert_eq!(
        ends,
        [Some(3), Some(3), Some(0), Some(1), None, None, Some(1), Some(0), Some(0), Some(0)],
        "the D statuses: {found:?}"
    );
    // Each D names which reading gave its code: the child's native code, PowerShell's own
    // failure, `$?` holding, and none for a D with no code.
    let sources: Vec<Option<&str>> =
        found.iter().filter(|marker| marker.starts_with('D')).map(|end| source(end)).collect();
    assert_eq!(
        sources,
        [
            Some("native"),
            Some("native"),
            Some("status"),
            Some("powershell"),
            None,
            None,
            Some("powershell"),
            Some("status"),
            Some("status"),
            Some("status"),
        ],
        "the D sources: {found:?}"
    );
    assert!(output.contains("seen=[3]"), "a command saw a changed $LASTEXITCODE: {output:?}");
    // The cancelled line's D follows its prompt's A B with no C between: `run_command` reads it
    // as `not_started`.
    assert!(kinds.contains("ABDAB"), "a cancelled line ends without a C: {kinds}");
    // The custom prompt still renders between A and B, showing the code the user sees: none
    // before any native command, 3 after each child, still 3 after the read and the failing cmdlet
    // (nothing changes it), 4 after the mixed lines' `sh` and through the silent cmdlet and the
    // cancelled line, 0 once the leak probe (`sh`, a native command) exits 0, and 0 after
    // `$null = 1`.
    let prompts: Vec<&str> =
        output.split("custom[").skip(1).filter_map(|rest| rest.split("]> ").next()).collect();
    let distinct: Vec<&str> = prompts.iter().copied().fold(Vec::new(), |mut seen, code| {
        if seen.last() != Some(&code) {
            seen.push(code);
        }
        seen
    });
    assert_eq!(
        distinct,
        ["", "3", "4", "0"],
        "the user's $LASTEXITCODE at each prompt: {prompts:?}"
    );
    assert!(prompts.iter().filter(|code| **code == "3").count() >= 4, "{prompts:?}");
    assert!(output.contains("leak-0"), "the leak probe did not run: {output:?}");
    assert!(!output.contains("leak-1"), "a command inherited the token");
}
