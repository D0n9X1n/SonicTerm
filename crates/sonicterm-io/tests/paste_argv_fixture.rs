//! Real-shell argument checks for the shared user-paste encoder and PTY input boundary.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;
use sonicterm_io::pty::{PtyHandle, ShellSpawnOpts, MAX_PTY_INPUT_MESSAGE_BYTES};
use sonicterm_types::{encode_payload, PasteTarget, ShellDialect, UserPayload};

// Reserve the remainder of each 30-second shell budget for bounded native PTY teardown.
const SHELL_WORK_TIMEOUT: Duration = Duration::from_secs(24);

struct ShellFixture {
    pty: Option<PtyHandle>,
    deadline: Instant,
}

impl ShellFixture {
    fn spawn(program: &str, dialect: ShellDialect) -> Result<Self, String> {
        let deadline = Instant::now() + SHELL_WORK_TIMEOUT;
        let args: Vec<String> = match dialect {
            ShellDialect::PowerShell => vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NoExit".into(),
                "-Command".into(),
                // Keep the native parser, but disable editor redraws and use UTF-8 at the ConPTY boundary.
                "[Console]::InputEncoding = New-Object System.Text.UTF8Encoding; \
                 [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding; \
                 Remove-Module PSReadLine -ErrorAction SilentlyContinue; function prompt { '' }"
                    .into(),
            ],
            ShellDialect::Cmd => vec!["/D".into(), "/Q".into(), "/V:OFF".into()],
            ShellDialect::Posix => Vec::new(),
            ShellDialect::Unknown => return Err("fixture needs a known shell dialect".into()),
        };
        let pty = PtyHandle::spawn_with_args_and_opts(
            program,
            &args,
            512,
            40,
            ShellSpawnOpts { clean_e2e: true, ..ShellSpawnOpts::default() },
        )
        .map_err(|error| format!("spawn {program}: {error}"))?;
        #[cfg(windows)]
        {
            // ConPTY's inherited-cursor query must receive a position reply before the console can render.
            pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).map_err(|error| error.to_string())?;
        }
        Ok(Self { pty: Some(pty), deadline })
    }

    fn send_line(&self, command: Vec<u8>) -> Result<(), String> {
        if Instant::now() >= self.deadline {
            return Err("shell work deadline expired before input".into());
        }
        let pty = self.pty.as_ref().expect("live fixture");
        pty.send_input_nonblocking(command).map_err(|error| error.to_string())?;
        // The encoder never submits a draft: the fixture sends the Enter keystroke separately.
        pty.send_input_nonblocking(b"\r".to_vec()).map_err(|error| error.to_string())
    }

    fn read_through_marker(&self, marker: &str) -> Result<(String, Vec<u8>), String> {
        let pty = self.pty.as_ref().expect("live fixture");
        let mut output = Vec::new();
        loop {
            let text = strip_vt(&output);
            if output_has_marker(&text, marker) {
                return Ok((text, output));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "shell never emitted {marker}; output: {text:?}; raw: {}",
                    output.escape_ascii()
                ));
            }
            match pty.out_rx.recv_timeout(remaining.min(Duration::from_millis(50))) {
                Ok(chunk) => output.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "PTY disconnected before {marker}; output: {text:?}; raw: {}",
                        output.escape_ascii()
                    ));
                }
            }
        }
    }

    fn close(&mut self) -> Result<(), String> {
        let Some(pty) = self.pty.take() else { return Ok(()) };
        let probe = pty.child_exit_probe();
        let killed = pty.kill();
        // Dropping the production handle closes the master, cancels I/O and reaps the child on bounded deadlines.
        drop(pty);
        killed.map_err(|error| format!("kill fixture shell: {error}"))?;
        if !probe.has_exited().map_err(|error| error.to_string())? {
            return Err("fixture shell survived PTY teardown".into());
        }
        Ok(())
    }
}

// Lifecycle: ShellFixture releases its PtyHandle on every return or unwind, including failed argument assertions.
impl Drop for ShellFixture {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            eprintln!("fixture cleanup failed: {error}");
        }
    }
}

fn exercise_shell(program: &str, dialect: ShellDialect, expected: &[&str]) -> Result<(), String> {
    let mut fixture = ShellFixture::spawn(program, dialect)?;
    let result = (|| {
        let ready = match dialect {
            // Empty prompts keep later ARG rows unprefixed; only this reply can share a prompt's row.
            ShellDialect::Posix => "PS1= PS2=; echo __READY''__",
            ShellDialect::PowerShell => "'__READY' + '__'",
            ShellDialect::Cmd => "echo __READY^__",
            ShellDialect::Unknown => unreachable!(),
        };
        fixture.send_line(ready.as_bytes().to_vec())?;
        fixture.read_through_marker("__READY__")?;
        let (mut actual, mut output) = capture_paths(&fixture, dialect, expected)?;
        if dialect == ShellDialect::Cmd && actual != expected {
            // cmd's default console code page can lose Unicode before ConPTY emits UTF-8.
            eprintln!("{program} did not preserve non-ASCII output; running chcp 65001 >nul first");
            fixture.send_line(b"chcp 65001 >nul & echo __UTF8^__".to_vec())?;
            fixture.read_through_marker("__UTF8__")?;
            (actual, output) = capture_paths(&fixture, dialect, expected)?;
        }
        if actual != expected {
            return Err(format!(
                "{program} arguments changed: expected {expected:?}, got {actual:?}; output: {output:?}"
            ));
        }
        for argument in actual {
            eprintln!("{program}: ARG[{argument}]");
        }
        Ok(())
    })();
    let cleanup = fixture.close();
    result.and(cleanup)
}

fn capture_paths(
    fixture: &ShellFixture,
    dialect: ShellDialect,
    paths: &[&str],
) -> Result<(Vec<String>, String), String> {
    let payload = UserPayload::Paths(paths.iter().map(PathBuf::from).collect());
    let encoded = encode_payload(
        &payload,
        PasteTarget { bracketed: false, dialect },
        MAX_PTY_INPUT_MESSAGE_BYTES,
    )
    .map_err(|error| format!("encode fixture paths: {error:?}"))?;
    let (prefix, suffix) = match dialect {
        ShellDialect::Posix => ("printf 'ARG[%s]\\n' ", "; echo __END''__"),
        ShellDialect::PowerShell => {
            ("& { foreach ($a in $args) { \"ARG[$a]\" } } ", "; '__END' + '__'")
        }
        ShellDialect::Cmd => ("for %a in (", ") do @echo ARG[%a] & echo __END^__"),
        ShellDialect::Unknown => unreachable!(),
    };
    let mut command = prefix.as_bytes().to_vec();
    command.extend_from_slice(&encoded);
    command.extend_from_slice(suffix.as_bytes());
    fixture.send_line(command)?;
    let (output, raw) = fixture.read_through_marker("__END__")?;
    // Whole-line matching excludes echoed commands; unexpected or duplicate ARG lines fail the vector comparison.
    let arguments = output
        .split(['\r', '\n'])
        .filter_map(|line| line.trim().strip_prefix("ARG[")?.strip_suffix(']'))
        .map(|argument| {
            if dialect == ShellDialect::Cmd {
                argument
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or(argument)
            } else {
                argument
            }
            .to_owned()
        })
        .collect();
    Ok((arguments, format!("{output:?}; raw: {}", raw.escape_ascii())))
}

fn strip_vt(bytes: &[u8]) -> String {
    let mut clean = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            clean.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        match bytes.get(index) {
            Some(b'[') => {
                index += 1;
                while let Some(byte) = bytes.get(index) {
                    index += 1;
                    if (0x40..=0x7e).contains(byte) {
                        // ConPTY can separate output rows with cursor positioning instead of CR LF.
                        if matches!(*byte, b'H' | b'f') {
                            clean.push(b'\n');
                        }
                        break;
                    }
                }
            }
            Some(b']' | b'P' | b'_' | b'^' | b'X') => {
                index += 1;
                while let Some(byte) = bytes.get(index) {
                    if *byte == 0x07 {
                        index += 1;
                        break;
                    }
                    if *byte == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            Some(_) => {
                while bytes.get(index).is_some_and(|byte| (0x20..=0x2f).contains(byte)) {
                    index += 1;
                }
                index += usize::from(index < bytes.len());
            }
            None => {}
        }
    }
    String::from_utf8_lossy(&clean).into_owned()
}

fn assert_strip_vt_splits_rows_at_cursor_moves() {
    // ConPTY cursor-position moves must preserve row boundaries for exact marker and argument matching.
    for raw in [b"__READY__\x1b[7;1HC:\x3e".as_slice(), b"__READY__\x1b[7;1fC:\x3e".as_slice()] {
        let text = strip_vt(raw);
        assert_eq!(text, "__READY__\nC:>", "strip_vt must split cursor-positioned rows");
        assert!(
            text.lines().any(|line| line == "__READY__"),
            "strip_vt joined the marker to the prompt"
        );
    }
}

/// Whether stripped shell output printed `marker` on a row of its own or after the shell's prompt.
fn output_has_marker(text: &str, marker: &str) -> bool {
    text.split(['\r', '\n']).any(|line| line.trim_end().ends_with(marker))
}

fn assert_marker_survives_a_shared_prompt_row() {
    // A command typed before an interactive shell's first prompt leaves that prompt on the marker's row.
    let prompt_row = "echo __READY''__\r\n# __READY__\r\n# ";
    assert!(output_has_marker(prompt_row, "__READY__"), "missed a marker after a prompt");
    // An echoed command never ends with its marker, so it still cannot count as the shell's output.
    assert!(!output_has_marker("echo __READY''__\r\n# ", "__READY__"), "counted an echoed command");
}

#[cfg(windows)]
fn pwsh_on_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join("pwsh.exe"))
        .find(|candidate| candidate.is_file())
}

#[cfg(windows)]
#[test]
fn paste_paths_arrive_as_single_arguments_in_windows_shells() {
    // Required Windows shells and an installed pwsh must parse every encoded native path unchanged.
    assert_strip_vt_splits_rows_at_cursor_moves();
    assert_marker_survives_a_shared_prompt_row();
    let paths = [
        r"C:\My Files\a.txt",
        r"C:\O'Brien\a.txt",
        r"C:\O’Brien\a.txt",
        r"C:\‘a’\b.txt",
        r"C:\‚b‛\c.txt",
        r"C:\a&b\c.txt",
        r"C:\日本\ü.txt",
    ];
    exercise_shell("powershell.exe", ShellDialect::PowerShell, &paths)
        .expect("required powershell.exe argument round-trip");
    if let Some(program) = pwsh_on_path() {
        exercise_shell(
            program.to_str().expect("Unicode pwsh program path"),
            ShellDialect::PowerShell,
            &paths,
        )
        .expect("installed pwsh argument round-trip");
    } else {
        eprintln!("pwsh is absent from PATH; optional shell not exercised");
    }
    exercise_shell("cmd.exe", ShellDialect::Cmd, &paths)
        .expect("required cmd.exe argument round-trip");
}

#[cfg(unix)]
#[test]
fn paste_paths_arrive_as_single_arguments_in_posix_shell() {
    // The required POSIX shell receives one exact argument per encoded path, including quote and UTF-8 cases.
    assert_strip_vt_splits_rows_at_cursor_moves();
    assert_marker_survives_a_shared_prompt_row();
    exercise_shell(
        "/bin/sh",
        ShellDialect::Posix,
        &[
            "/tmp/my files/a.txt",
            "/tmp/it's/a.txt",
            "/tmp/‘a’/b.txt",
            "/tmp/a&b/c.txt",
            "/tmp/日本/ü.txt",
        ],
    )
    .expect("required /bin/sh argument round-trip");
}
