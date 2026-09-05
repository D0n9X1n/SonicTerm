#![allow(dead_code)] // CLI helpers compile cross-host for tests; production runs on Windows.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use sonicterm_app::os_drag::TabPayload;

/// Parsed CLI inputs for the Windows binary. We still hand-roll the
/// parser to keep the production binary's diff surface minimal — clap
/// migration is tracked separately.
#[derive(Default, Debug)]
pub struct ParsedCli {
    pub tearout: Option<TabPayload>,
    pub open_script: Option<PathBuf>,
    pub refresh_shell_associations: bool,
    pub runtime_smoke: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_tearout_payload_from<I, S>(args: I) -> Result<Option<TabPayload>>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    Ok(parse_cli_from(args)?.tearout)
}

#[cfg_attr(any(test, not(windows)), allow(dead_code))]
pub fn parse_tearout_payload_from_env() -> Result<Option<TabPayload>> {
    parse_tearout_payload_from(std::env::args_os())
}

/// Full CLI parse. Hand-rolled so we don't pull clap into a startup-sensitive binary.
pub fn parse_cli_from<I, S>(args: I) -> Result<ParsedCli>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let _program = args.next();
    let mut out = ParsedCli::default();
    let mut unknown_args = 0usize;
    let mut runtime_smoke_flags = 0usize;
    while let Some(arg) = args.next() {
        if arg == OsStr::new("--tear-out-payload") {
            let Some(json) = args.next() else {
                bail!("--tear-out-payload requires a JSON argument")
            };
            let parsed = TabPayload::from_json(&json.to_string_lossy())
                .context("decode --tear-out-payload JSON")?;
            out.tearout = Some(parsed);
        } else if arg == OsStr::new("--open-script") {
            // When: `arg` selects script opening after the tear-out option was rejected; parse exactly one path.
            let Some(path) = args.next() else { bail!("--open-script requires a path argument") };
            if out.open_script.is_some() {
                bail!("--open-script may be provided only once")
            }
            out.open_script = Some(PathBuf::from(path));
        } else if arg == OsStr::new("--refresh-shell-associations") {
            // When: `arg` selects association refresh after both payload options were rejected; no path value is consumed.
            out.refresh_shell_associations = true;
        } else if arg == OsStr::new("--runtime-smoke") {
            // When: `arg` equals `--runtime-smoke`, count the one permitted hidden-mode flag.
            runtime_smoke_flags = runtime_smoke_flags.saturating_add(1);
            out.runtime_smoke = true;
        } else {
            // When: `arg` is not SonicTerm-owned, ordinary startup tolerates launch-shim metadata.
            unknown_args = unknown_args.saturating_add(1);
        }
    }
    if out.open_script.is_some() && out.tearout.is_some() {
        bail!("--open-script cannot be combined with --tear-out-payload")
    }
    if out.runtime_smoke
        && (out.open_script.is_some()
            || out.tearout.is_some()
            || out.refresh_shell_associations
            || unknown_args > 0
            || runtime_smoke_flags != 1)
    {
        // When: `runtime_smoke` accompanies launch state, `unknown_args`, or duplicate flags, reject it.
        bail!("--runtime-smoke cannot be combined with user launch options")
    }
    Ok(out)
}

#[cfg_attr(any(test, not(windows)), allow(dead_code))]
pub fn parse_cli_from_env() -> Result<ParsedCli> {
    parse_cli_from(std::env::args_os())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
