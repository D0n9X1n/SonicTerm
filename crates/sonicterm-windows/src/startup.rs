use std::path::PathBuf;

use anyhow::{anyhow, Result};
use sonicterm_app::open_script_bridge;
use sonicterm_types::{OpenScriptRequest, OpenScriptResolveError};

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
        let _ = open_script_bridge::push_requests(vec![request]);
    })
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod startup_tests;
