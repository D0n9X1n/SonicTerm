//! `sonicterm-mcp`: the stdio MCP server agents start, relaying to a running SonicTerm.
//!
//! Claude Code and Copilot CLI start MCP servers as console child processes speaking JSON-RPC
//! over stdin and stdout. SonicTerm's own binaries are GUI programs, so this small bridge finds
//! the running instance's control endpoint, checks it belongs to the same user, and relays lines
//! both ways. stdout carries only what SonicTerm sends; diagnostics go to stderr.

#![warn(clippy::min_ident_chars)]

use std::process::ExitCode;

#[cfg(unix)]
#[path = "connect_unix.rs"]
mod connect;
#[cfg(windows)]
#[path = "connect_windows.rs"]
mod connect;
mod discovery;
mod relay;
mod shell_integration;
#[cfg(unix)]
mod stdout_unix;

/// The usage text for `--help`.
const USAGE: &str = "\
sonicterm-mcp: connect an MCP client to a running SonicTerm

Usage: sonicterm-mcp [--endpoint <path-or-pipe>]
       sonicterm-mcp --shell-integration <zsh|bash|pwsh>

The endpoint is, in order: --endpoint, $SONICTERM_CONTROL (set in every SonicTerm pane), or the
only running SonicTerm with [mcp] enabled = true. stdin and stdout carry MCP JSON-RPC lines.

--shell-integration prints SonicTerm's own OSC 133 shell integration for zsh (5.3+), bash (4.4+)
or PowerShell 7+ with PSReadLine (pwsh).
Load it from your shell's startup file, before any line that starts a program, so run_command
can verify exit codes:
  eval \"$(sonicterm-mcp --shell-integration zsh)\"
  sonicterm-mcp --shell-integration pwsh | Out-String | Invoke-Expression
";

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// Relay, optionally to an explicit endpoint.
    Relay(Option<String>),
    /// Print the version.
    Version,
    /// Print usage.
    Help,
    /// Print the OSC 133 shell integration for a shell.
    ShellIntegration(shell_integration::Shell),
}

/// Parse the arguments after the program name.
fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut endpoint = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "--endpoint" => endpoint = Some(rest.next().ok_or("--endpoint needs a value")?.clone()),
            "--shell-integration" => {
                let name = rest.next().ok_or("--shell-integration needs zsh, bash or pwsh")?;
                let shell = shell_integration::Shell::from_name(name).ok_or_else(|| {
                    format!("no shell integration for {name:?}; use zsh, bash or pwsh")
                })?;
                return Ok(Command::ShellIntegration(shell));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Command::Relay(endpoint))
}

/// The live candidates in this user's endpoint directory.
fn candidates() -> (Vec<discovery::Candidate>, Option<std::path::PathBuf>) {
    #[cfg(unix)]
    {
        let dir = connect::endpoint_dir();
        let found = discovery::scan(
            &dir,
            |path| Some(path.to_string_lossy().into_owned()),
            connect::process_alive,
            connect::listening,
        );
        (found, Some(dir))
    }
    #[cfg(windows)]
    {
        let Some(dir) = connect::endpoint_dir() else {
            return (Vec::new(), None);
        };
        let found = discovery::scan(
            &dir,
            connect::record_address,
            connect::process_alive,
            connect::listening,
        );
        (found, Some(dir))
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let endpoint = match parse_args(&args) {
        Ok(Command::Version) => {
            println!("sonicterm-mcp {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Command::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::ShellIntegration(shell)) => {
            print!("{}", shell.snippet());
            return ExitCode::SUCCESS;
        }
        Ok(Command::Relay(endpoint)) => endpoint,
        Err(message) => {
            eprintln!("sonicterm-mcp: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let (found, searched) = candidates();
    let chosen =
        match discovery::choose(endpoint, std::env::var("SONICTERM_CONTROL").ok(), found, searched)
        {
            Ok(chosen) => chosen,
            Err(error) => {
                eprintln!("sonicterm-mcp: {error}");
                return ExitCode::from(1);
            }
        };
    let (from_server, to_server) = match connect::connect(&chosen.address) {
        Ok(halves) => halves,
        Err(error) => {
            eprintln!("sonicterm-mcp: cannot use {}: {error}", chosen.address);
            return ExitCode::from(1);
        }
    };
    let preamble = relay::preamble(std::env::var("SONICTERM_PANE_ID").ok().as_deref());
    #[cfg(unix)]
    let output = stdout_unix::Stdout::new();
    #[cfg(windows)]
    let output = std::io::stdout();
    let ended = relay::relay(&preamble, std::io::stdin(), output, from_server, to_server);
    match ended {
        relay::Ended::InputClosed | relay::Ended::EndpointClosed | relay::Ended::OutputClosed => {
            // When: the agent or SonicTerm closed its side (`InputClosed`, `EndpointClosed`,
            // `OutputClosed`), the session ended normally.
            ExitCode::SUCCESS
        }
        relay::Ended::Stalled => {
            eprintln!("sonicterm-mcp: the client stopped reading SonicTerm's output for 5 s");
            ExitCode::from(1)
        }
        relay::Ended::LineTooLong => {
            eprintln!("sonicterm-mcp: a line passed 1 MiB");
            ExitCode::from(1)
        }
    }
}
