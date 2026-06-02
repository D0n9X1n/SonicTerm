#![allow(dead_code)] // tearout CLI helpers used only via integration test; production wiring tracked separately

use anyhow::{bail, Context, Result};
use sonicterm_app::os_drag::TabPayload;

/// Parsed CLI inputs for the Windows binary. We still hand-roll the
/// parser to keep the production binary's diff surface minimal — clap
/// migration is tracked separately.
#[derive(Default, Debug)]
pub struct ParsedCli {
    pub tearout: Option<TabPayload>,
    /// Pipe-name request from `--harness-input-pipe <name>`. `"auto"`
    /// asks the harness module to pick a UUID-like suffix; any other
    /// value is taken as the literal stem appended after
    /// `\\.\pipe\sonicterm-harness-`. Only ever `Some` when the
    /// `harness` cargo feature is enabled — without it, the flag is
    /// rejected as unknown.
    #[cfg(feature = "harness")]
    pub harness_input_pipe: Option<String>,
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
/// parser) so we don't pull clap into a binary that's already on the
/// slow-link bench.
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
            "--harness-input-pipe" => {
                #[cfg(feature = "harness")]
                {
                    let Some(name) = args.next() else {
                        bail!("--harness-input-pipe requires a name argument (\"auto\" or stem)")
                    };
                    out.harness_input_pipe = Some(name);
                }
                #[cfg(not(feature = "harness"))]
                {
                    bail!("unknown flag: --harness-input-pipe (rebuild with `--features harness`)");
                }
            }
            _ => {
                // Mirror the previous lax behaviour for unrelated args
                // (some launch shims pass extras). We only hard-reject
                // `--harness-input-pipe` without the feature so a
                // stripped release exits noisily.
            }
        }
    }
    Ok(out)
}

#[cfg_attr(any(test, not(windows)), allow(dead_code))]
pub fn parse_cli_from_env() -> Result<ParsedCli> {
    parse_cli_from(std::env::args())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "harness"))]
    #[test]
    fn harness_flag_rejected_without_feature() {
        let err =
            parse_cli_from(["sonic", "--harness-input-pipe", "auto"]).expect_err("must reject");
        assert!(err.to_string().contains("--harness-input-pipe"));
    }

    #[cfg(feature = "harness")]
    #[test]
    fn harness_flag_accepted_with_feature() {
        let p = parse_cli_from(["sonic", "--harness-input-pipe", "auto"]).unwrap();
        assert_eq!(p.harness_input_pipe.as_deref(), Some("auto"));
    }

    #[cfg(feature = "harness")]
    #[test]
    fn harness_flag_requires_value() {
        let err = parse_cli_from(["sonic", "--harness-input-pipe"]).expect_err("must require arg");
        assert!(err.to_string().contains("requires"));
    }
}
