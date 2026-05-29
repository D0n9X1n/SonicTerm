#![allow(dead_code)] // tearout CLI helpers used only via integration test; production wiring tracked separately

use anyhow::{bail, Context, Result};
use sonic_app::os_drag::TabPayload;

pub fn parse_tearout_payload_from<I, S>(args: I) -> Result<Option<TabPayload>>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let _program = args.next();
    let mut payload = None;
    while let Some(arg) = args.next() {
        if arg == "--tear-out-payload" {
            let Some(json) = args.next() else {
                bail!("--tear-out-payload requires a JSON argument")
            };
            let parsed = TabPayload::from_json(&json).context("decode --tear-out-payload JSON")?;
            payload = Some(parsed);
        }
    }
    Ok(payload)
}

#[cfg_attr(test, allow(dead_code))]
pub fn parse_tearout_payload_from_env() -> Result<Option<TabPayload>> {
    parse_tearout_payload_from(std::env::args())
}
