//! The `[mcp]` section: the local agent control server.

use serde::{Deserialize, Serialize};

/// The most connections `[mcp] max_connections` may allow; a larger value is clamped.
pub const MAX_MCP_CONNECTIONS: u8 = 16;
/// The default connection limit.
pub const DEFAULT_MCP_CONNECTIONS: u8 = 4;

/// Settings for the local MCP control server that lets agents such as Claude Code and Copilot
/// CLI inspect and drive SonicTerm's panes. Off by default.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Start the control server. Nothing listens and no endpoint exists while this is off.
    pub enabled: bool,
    /// Which panes control tools may act on.
    pub control: McpControl,
    /// Which panes read tools may see, as written. A missing key (`None`) means `"agent"`; the
    /// running server tells the user once that the key is unset. Read it through [`Self::read`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<McpRead>,
    /// Connections served at once. Each runs one tool call at a time, so a long `wait_for` holds
    /// its connection's slot. Read through [`Self::max_connections`], which clamps it.
    pub max_connections: u8,
    /// Whether an agent session's `"agent"` scopes cover every agent's panes or only its own.
    pub isolation: McpIsolation,
    /// Which panes `create_pane` may open a helper beside under `control = "agent"`.
    pub create_anchor: McpCreateAnchor,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            control: McpControl::default(),
            read: None,
            max_connections: DEFAULT_MCP_CONNECTIONS,
            isolation: McpIsolation::default(),
            create_anchor: McpCreateAnchor::default(),
        }
    }
}

impl McpConfig {
    /// The connection limit in force: `max_connections` clamped to `1..=16`.
    #[must_use]
    pub fn max_connections(&self) -> usize {
        usize::from(self.max_connections.clamp(1, MAX_MCP_CONNECTIONS))
    }

    /// The read scope in force: the configured one, or `"agent"` when the key is missing.
    #[must_use]
    pub fn read(&self) -> McpRead {
        self.read.unwrap_or_default()
    }
}

/// Which panes the read tools (`list_panes`, `read_pane`, `wait_for`) may see. Pane identity
/// comes only from SonicTerm's own records (`agent_owned`), never from anything a client says.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpRead {
    /// Only panes an agent created; the default when `read` is not set.
    #[default]
    Agent,
    /// Every pane.
    Any,
}

impl McpRead {
    /// The config spelling, for logs and templates.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Any => "any",
        }
    }
}

/// Which panes the control tools (`create_pane`, `run_command`, `send_text`, `send_keys`,
/// `close_pane`) may act on; [`McpRead`] sets what the read tools may see.
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

/// What `"agent"` in `control` and `read` covers when several agents connect. An agent session is
/// one connection; a reconnecting bridge is a new session.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpIsolation {
    /// Every agent-created pane, whichever session created it.
    #[default]
    Shared,
    /// Only panes the calling session created; another session's panes are out of scope.
    Session,
}

impl McpIsolation {
    /// The config spelling, for logs and templates.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Session => "session",
        }
    }
}

/// Which panes `create_pane` may anchor a helper on under `control = "agent"`; `"any"` always
/// allows every pane.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpCreateAnchor {
    /// Any pane, the user's included: a helper never touches its anchor's input.
    #[default]
    Any,
    /// Only a pane the agent could control, so an agent cannot place panes beside the user's.
    Agent,
}

impl McpCreateAnchor {
    /// The config spelling, for logs and templates.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Agent => "agent",
        }
    }
}

#[cfg(test)]
#[path = "config_mcp_tests.rs"]
mod config_mcp_tests;
