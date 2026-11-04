//! The `[mcp]` section: the local agent control server.

use serde::{Deserialize, Serialize};

/// Settings for the local MCP control server that lets agents such as Claude Code and Copilot
/// CLI inspect and drive SonicTerm's panes. Off by default.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Start the control server. Nothing listens and no endpoint exists while this is off.
    pub enabled: bool,
    /// Which panes control tools may act on.
    pub control: McpControl,
}

/// Which panes the control tools (`create_pane`, `run_command`, `send_text`, `send_keys`,
/// `close_pane`) may act on. Read tools work on every pane whenever the server is enabled.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpControl {
    /// Every control tool is refused.
    Off,
    /// Only panes an agent created; `create_pane` itself may anchor on any pane.
    #[default]
    Agent,
    /// Every pane.
    Any,
}

impl McpControl {
    /// The config spelling, for logs and templates.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Agent => "agent",
            Self::Any => "any",
        }
    }
}

#[cfg(test)]
#[path = "config_mcp_tests.rs"]
mod config_mcp_tests;
