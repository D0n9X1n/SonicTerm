//! The control tools: creating panes and sending them input. Their list and dispatch live here,
//! apart from the read tools, because every one of them is authorized against `[mcp] control`.

use serde_json::Value;

use super::{CallScope, McpShared};

/// The control tools this server offers, appended to the read tools in `tools/list`.
pub(crate) fn control_tools() -> Vec<Value> {
    Vec::new()
}

/// Run the control tool `name`; `None` when no control tool has that name.
pub(crate) fn call(
    _shared: &McpShared,
    _name: &str,
    _arguments: &Value,
    _scope: &CallScope,
) -> Option<Value> {
    None
}
