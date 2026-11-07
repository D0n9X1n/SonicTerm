//! Pins the `[mcp]` defaults and parsing: off, agent-only control, and a load failure for an
//! unknown control value or key.

use super::*;
use crate::config::Config;

/// The server is off and control limited to agent-created panes when nothing is configured.
#[test]
fn mcp_defaults_to_off_with_agent_control() {
    let config = Config::default();
    assert_eq!(
        config.mcp,
        McpConfig { enabled: false, control: McpControl::Agent, ..McpConfig::default() }
    );
    let parsed: Config = toml::from_str("").expect("empty config");
    assert_eq!(parsed.mcp, McpConfig::default());
}

/// Each control value parses from its lowercase spelling and round-trips through `as_str`.
#[test]
fn every_control_value_parses() {
    for control in [McpControl::Off, McpControl::Agent, McpControl::Any] {
        let text = format!("[mcp]\nenabled = true\ncontrol = \"{}\"\n", control.as_str());
        let parsed: Config = toml::from_str(&text).expect("valid [mcp]");
        assert_eq!(parsed.mcp, McpConfig { enabled: true, control, ..McpConfig::default() });
    }
}

/// An unknown control value or an unknown key in `[mcp]` fails the load instead of being
/// ignored, so a typo never widens or silently drops the user's choice.
#[test]
fn an_unknown_value_or_key_fails_the_load() {
    assert!(toml::from_str::<Config>("[mcp]\ncontrol = \"all\"\n").is_err());
    assert!(toml::from_str::<Config>("[mcp]\nenable = true\n").is_err());
}

/// A config with no `read` key sees only agent-created panes, and remembers that the key was
/// missing so the running server can say so; an explicit `"any"` is kept exactly.
#[test]
fn a_missing_read_key_reads_agent_panes_only() {
    let parsed: Config = toml::from_str("[mcp]\nenabled = true\ncontrol = \"agent\"\n").unwrap();
    assert_eq!(parsed.mcp.read, None);
    assert_eq!(parsed.mcp.read(), McpRead::Agent);
    assert_eq!(McpConfig::default().read(), McpRead::Agent);
    let explicit: Config = toml::from_str("[mcp]\nread = \"any\"\n").unwrap();
    assert_eq!(explicit.mcp.read, Some(McpRead::Any));
    assert_eq!(explicit.mcp.read(), McpRead::Any);
}

/// Each read value parses from its lowercase spelling and round-trips through `as_str`; an
/// unknown one fails the load rather than being read as either.
#[test]
fn every_read_value_parses_and_an_unknown_one_fails() {
    for read in [McpRead::Agent, McpRead::Any] {
        let text = format!("[mcp]\nread = \"{}\"\n", read.as_str());
        let parsed: Config = toml::from_str(&text).expect("valid [mcp] read");
        assert_eq!(parsed.mcp.read, Some(read));
    }
    assert!(toml::from_str::<Config>("[mcp]\nread = \"off\"\n").is_err());
}

/// A new config file starts from the template, which limits read tools to agent-created panes;
/// the template still parses, and only its `read` line differs from the defaults.
#[test]
fn the_new_config_template_reads_agent_panes_only() {
    let template = crate::config::default_config_template();
    assert!(template.contains("\nread = \"agent\"\n"), "the template sets read");
    let parsed: Config = toml::from_str(&template).expect("the template parses");
    assert_eq!(parsed.mcp.read, Some(McpRead::Agent));
    assert!(!parsed.mcp.enabled, "the server stays off by default");
}

/// `max_connections` defaults to 4, parses, and is clamped to 1..=16 when read.
#[test]
fn max_connections_defaults_and_clamps() {
    assert_eq!(McpConfig::default().max_connections(), 4);
    let parsed: Config = toml::from_str("[mcp]\nmax_connections = 9\n").unwrap();
    assert_eq!(parsed.mcp.max_connections(), 9);
    let zero: Config = toml::from_str("[mcp]\nmax_connections = 0\n").unwrap();
    assert_eq!(zero.mcp.max_connections(), 1);
    let many: Config = toml::from_str("[mcp]\nmax_connections = 200\n").unwrap();
    assert_eq!(many.mcp.max_connections(), 16);
}
