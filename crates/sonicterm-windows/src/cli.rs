#![allow(dead_code)] // tearout CLI helpers used only via integration test; production wiring tracked separately

use anyhow::{bail, Context, Result};
use sonicterm_app::os_drag::TabPayload;

/// Parsed CLI inputs for the Windows binary. We still hand-roll the
/// parser to keep the production binary's diff surface minimal — clap
/// migration is tracked separately.
#[derive(Default, Debug)]
pub struct ParsedCli {
    pub tearout: Option<TabPayload>,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_tearout_payload_from<I, S>(args: I) -> Result<Option<TabPayload>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Ok(parse_cli_from(args)?.tearout)
}

#[cfg_attr(any(test, not(windows)), allow(dead_code))]
pub fn parse_tearout_payload_from_env() -> Result<Option<TabPayload>> {
    parse_tearout_payload_from(std::env::args())
}

/// Full CLI parse. Hand-rolled (same style as the existing tearout
/// parser) so we don't pull clap into a startup-sensitive binary.
pub fn parse_cli_from<I, S>(args: I) -> Result<ParsedCli>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let _program = args.next();
    let mut out = ParsedCli::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tear-out-payload" => {
                let Some(json) = args.next() else {
                    bail!("--tear-out-payload requires a JSON argument")
                };
                let parsed =
                    TabPayload::from_json(&json).context("decode --tear-out-payload JSON")?;
                out.tearout = Some(parsed);
            }
            _ => {
                // Mirror the previous lax behaviour for unrelated args
                // (some launch shims pass extras).
            }
        }
    }
    Ok(out)
}

#[cfg_attr(any(test, not(windows)), allow(dead_code))]
pub fn parse_cli_from_env() -> Result<ParsedCli> {
    parse_cli_from(std::env::args())
}
