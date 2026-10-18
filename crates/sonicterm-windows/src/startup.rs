use std::path::PathBuf;

use anyhow::{anyhow, Result};
use sonicterm_app::launch_cli::{terminal_launch, CliDialect};
use sonicterm_app::launch_queue::{self, LaunchRequest};
use sonicterm_types::{OpenScriptRequest, OpenScriptResolveError, TerminalLaunch};

use crate::cli::ParsedCli;

fn startup_open_script<F>(parsed: &ParsedCli, cwd_lookup: F) -> Result<Option<OpenScriptRequest>>
where
    F: FnOnce() -> Option<PathBuf>,
{
    let Some(original_path) = parsed.open_script.clone() else {
        // When: `open_script` is absent, startup has no script request to resolve or queue.
        return Ok(None);
    };
    let display = original_path.display().to_string();
    OpenScriptRequest::resolve_with_cwd_lookup(original_path, cwd_lookup)
        .map(Some)
        .map_err(|error| match error {
            OpenScriptResolveError::InitialCwdUnavailable => anyhow!(
                "cannot resolve relative --open-script path {display:?}: process current directory is unavailable"
            ),
            OpenScriptResolveError::InitialCwdNotAbsolute => anyhow!(
                "cannot resolve relative --open-script path {display:?}: process current directory is not absolute"
            ),
        })
}

fn queue_startup_open_script_with<F, S>(parsed: &ParsedCli, cwd_lookup: F, sink: S) -> Result<bool>
where
    F: FnOnce() -> Option<PathBuf>,
    S: FnOnce(OpenScriptRequest),
{
    let Some(request) = startup_open_script(parsed, cwd_lookup)? else {
        // When: startup produced no `request`, leave the bridge untouched and report that nothing was queued.
        return Ok(false);
    };
    sink(request);
    Ok(true)
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn queue_startup_open_script<F>(parsed: &ParsedCli, cwd_lookup: F) -> Result<bool>
where
    F: FnOnce() -> Option<PathBuf>,
{
    queue_startup_open_script_with(parsed, cwd_lookup, |request| {
        let _ = launch_queue::push_scripts(vec![request]);
    })
}

/// The terminal launch to queue, or `None` for a plain launch that keeps the default first tab.
///
/// A plain shell keeps today's `%USERPROFILE%` default, because the installer's
/// shortcuts start the process in the install folder; only a command launch
/// defaults to the process cwd.
fn startup_terminal(
    parsed: &ParsedCli,
    process_cwd: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<TerminalLaunch> {
    let terminal = parsed.terminal.clone()?;
    if terminal == Default::default() {
        // When: `terminal` has no option or command, the default first tab is identical; queue nothing.
        return None;
    }
    Some(terminal_launch(terminal, CliDialect::Windows, process_cwd, home))
}

/// Queue the startup terminal launch, if any, before the shell runs; returns whether one was queued.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn queue_startup_terminal(parsed: &ParsedCli) -> bool {
    let process_cwd = std::env::current_dir().ok();
    let home = sonicterm_app::launch_cli::home_dir();
    let Some(launch) = startup_terminal(parsed, process_cwd, home) else {
        // When: `startup_terminal` returns None, the default first tab needs no request.
        return false;
    };
    let _ = launch_queue::push_requests(vec![LaunchRequest::Terminal(launch)]);
    true
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod startup_tests;
