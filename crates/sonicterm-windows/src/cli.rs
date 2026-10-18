#![allow(dead_code)] // CLI helpers compile cross-host for tests; production runs on Windows.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sonicterm_app::launch_cli::{
    parse_launch_args, CliDialect, LaunchCli, TerminalCli, UsageError,
};
use sonicterm_app::os_drag::TabPayload;

/// Parsed CLI inputs for the Windows binary.
///
/// The launch grammar is the shared `launch_cli` one; this binary adds the
/// windowless `--refresh-shell-associations` and the private
/// `--tear-out-payload` that tab tear-out passes to a new process.
#[derive(Default, Debug)]
pub struct ParsedCli {
    pub tearout: Option<TabPayload>,
    pub open_script: Option<PathBuf>,
    pub refresh_shell_associations: bool,
    pub runtime_smoke: bool,
    /// `--help`: print usage and exit 0.
    pub help: bool,
    /// Terminal options and an optional command; `None` for the other modes.
    pub terminal: Option<TerminalCli>,
}

/// Why the command line was refused.
#[derive(Debug)]
pub enum CliError {
    /// The command line breaks the grammar; the binary exits 2.
    Usage(UsageError),
    /// The private tear-out payload is not valid JSON for a tab.
    Payload(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(error) => write!(formatter, "{error}"),
            Self::Payload(reason) => write!(formatter, "decode --tear-out-payload JSON: {reason}"),
        }
    }
}

impl std::error::Error for CliError {}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_tearout_payload_from<I, S>(args: I) -> Result<Option<TabPayload>, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    Ok(parse_cli_from(args)?.tearout)
}

#[cfg_attr(any(test, not(windows)), allow(dead_code))]
pub fn parse_tearout_payload_from_env() -> Result<Option<TabPayload>, CliError> {
    parse_tearout_payload_from(std::env::args_os())
}

/// Parse `args` (binary name first) with the process cwd for a relative `--dir`.
pub fn parse_cli_from<I, S>(args: I) -> Result<ParsedCli, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let cwd = std::env::current_dir().ok();
    parse_cli_from_with_cwd(args, cwd.as_deref())
}

/// Parse `args` (binary name first), resolving a relative `--dir` against `process_cwd`.
pub fn parse_cli_from_with_cwd<I, S>(
    args: I,
    process_cwd: Option<&Path>,
) -> Result<ParsedCli, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let cli = parse_launch_args(args, CliDialect::Windows, process_cwd).map_err(CliError::Usage)?;
    let mut out = ParsedCli::default();
    match cli {
        LaunchCli::Terminal(terminal) => out.terminal = Some(terminal),
        LaunchCli::OpenScript(path) => out.open_script = Some(path),
        LaunchCli::RuntimeSmoke => out.runtime_smoke = true,
        LaunchCli::Help => out.help = true,
        LaunchCli::RefreshShellAssociations => out.refresh_shell_associations = true,
        LaunchCli::TearOut(json) => {
            let payload = TabPayload::from_json(&json.to_string_lossy())
                .map_err(|error| CliError::Payload(error.to_string()))?;
            out.tearout = Some(payload);
        }
    }
    Ok(out)
}

#[cfg_attr(any(test, not(windows)), allow(dead_code))]
pub fn parse_cli_from_env() -> Result<ParsedCli, CliError> {
    parse_cli_from(std::env::args_os())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
