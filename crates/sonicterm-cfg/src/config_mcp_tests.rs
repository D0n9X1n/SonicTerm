//! Pins the `[mcp]` defaults and parsing: off, agent-only control, and a load failure for an
//! unknown control value or key.

use super::*;
use crate::config::Config;

/// The server is off and control limited to agent-created panes when nothing is configured.
#[test]
fn mcp_defaults_to_off_with_agent_control() {
    let config = Config::default();
    assert_eq!(config.mcp, McpConfig { enabled: false, control: McpControl::Agent });
    let parsed: Config = toml::from_str("").expect("empty config");
    assert_eq!(parsed.mcp, McpConfig::default());
}

/// Each control value parses from its lowercase spelling and round-trips through `as_str`.
#[test]
fn every_control_value_parses() {
    for control in [McpControl::Off, McpControl::Agent, McpControl::Any] {
        let text = format!("[mcp]\nenabled = true\ncontrol = \"{}\"\n", control.as_str());
        let parsed: Config = toml::from_str(&text).expect("valid [mcp]");
        assert_eq!(parsed.mcp, McpConfig { enabled: true, control });
    }
}

/// An unknown control value or an unknown key in `[mcp]` fails the load instead of being
/// ignored, so a typo never widens or silently drops the user's choice.
#[test]
fn an_unknown_value_or_key_fails_the_load() {
    assert!(toml::from_str::<Config>("[mcp]\ncontrol = \"all\"\n").is_err());
    assert!(toml::from_str::<Config>("[mcp]\nenable = true\n").is_err());
}
