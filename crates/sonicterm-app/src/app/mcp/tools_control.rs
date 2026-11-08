//! The control tools: `create_pane`, `send_text`, `send_keys`, `run_command` and `close_pane`.
//! Arguments are checked here, on the connection's worker; the loop rechecks scope and does the
//! work; `run_command` then waits here, off the loop.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use super::claims::ResumeToken;
use super::jsonrpc::{tool_error, tool_result};
use super::run::{wait_run, DEFAULT_RUN_WAIT, MAX_RUN_WAIT};
use super::tools_read::pane_id_arg;
use super::{keys, CallScope, LoopCall, LoopReply, McpShared};
use crate::app::pane_target::{AgentLaunch, LaunchOutcome, Placement};

/// The most text one `send_text` call may carry.
pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The control tools this server offers, appended to the read tools in `tools/list`.
pub(crate) fn control_tools() -> Vec<Value> {
    let pane_id = json!({ "type": "string", "description": "A pane id from list_panes." });
    // Every result is an object; a refusal carries `error` (a stable code) and `message` instead.
    let output = |properties: Value| {
        let mut properties = properties;
        properties["error"] = json!({ "type": "string" });
        properties["message"] = json!({ "type": "string" });
        json!({ "type": "object", "properties": properties })
    };
    let written = output(json!({
        "bytes": { "type": "integer", "minimum": 0 },
        "delivery": { "enum": ["written", "queued", "failed"] },
    }));
    vec![
        json!({
            "name": "create_pane",
            "description": "Open a helper pane beside anchor_pane_id: a split to its right or below, \
                            or a new tab in its window. Focus does not move. With argv the program \
                            runs directly and the pane stays open after it exits, so wait_for{exit} \
                            and read_pane can read how it ended.",
            "inputSchema": {
                "type": "object",
                "required": ["anchor_pane_id", "placement"],
                "properties": {
                    "anchor_pane_id": pane_id,
                    "placement": { "enum": ["right", "down", "tab"] },
                    "argv": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string" },
                    "title": { "type": "string" },
                },
            },
            "outputSchema": output(json!({
                "pane_id": { "type": "string" },
                "started": { "type": "boolean" },
                "resume_token": { "type": "string" },
            })),
            "annotations": { "destructiveHint": false },
        }),
        json!({
            "name": "send_text",
            "description": "Type text into a pane (bracketed paste when the program asked for it); \
                            submit adds Enter. At most 64 KiB.",
            "inputSchema": {
                "type": "object",
                "required": ["pane_id", "text"],
                "properties": {
                    "pane_id": pane_id,
                    "text": { "type": "string" },
                    "submit": { "type": "boolean" },
                },
            },
            "outputSchema": written.clone(),
            "annotations": { "destructiveHint": true },
        }),
        json!({
            "name": "send_keys",
            "description": "Send keys to a pane: Enter, Tab, Backspace, Escape, Space, Up, Down, \
                            Left, Right, Home, End, PageUp, PageDown, Insert, Delete, F1-F12, a \
                            single character, each optionally prefixed with Ctrl+, Alt+ or Shift+. \
                            At most 64 keys; one unknown name sends nothing.",
            "inputSchema": {
                "type": "object",
                "required": ["pane_id", "keys"],
                "properties": {
                    "pane_id": pane_id,
                    "keys": { "type": "array", "items": { "type": "string" }, "maxItems": 64 },
                },
            },
            "outputSchema": written,
            "annotations": { "destructiveHint": true },
        }),
        json!({
            "name": "run_command",
            "description": "Run one command line at a pane's shell prompt and wait for it to finish. \
                            Needs shell integration (OSC 133). Refused when text is typed at the \
                            prompt or anyone typed while the previous command ran; this check is \
                            best effort, so prefer panes this agent created. A timeout returns \
                            state 'running', never success. exit_verified true means markers \
                            carrying the pane's secret vouched for the exit code: SonicTerm's \
                            shell integration sends them, but so could a program the shell's \
                            startup files ran before loading it. When false the code was read \
                            from terminal output that any program in the pane could have printed. \
                            exit_code_source says what the code rests on: native, powershell or \
                            status as SonicTerm's integration named it, reported for an \
                            authenticated D naming no known source, unverified for \
                            unauthenticated markers, unknown for an authenticated D with no code, \
                            and null when the run did not complete or its final record could not \
                            be read.",
            "inputSchema": {
                "type": "object",
                "required": ["pane_id", "command"],
                "properties": {
                    "pane_id": pane_id,
                    "command": { "type": "string" },
                    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000 },
                },
            },
            "outputSchema": output(json!({
                "state": {
                    "enum": ["completed", "not_started", "interfered", "lost", "running", "revoked", "pane_closed"],
                },
                "exit_code": { "type": ["integer", "null"] },
                "exit_verified": { "type": "boolean" },
                "exit_code_source": {
                    "enum": ["native", "powershell", "status", "reported", "unverified", "unknown", null],
                },
                "generation": { "type": "integer" },
                "output_incomplete": { "type": "boolean" },
                "output": {
                    "type": "object",
                    "properties": {
                        "lines": { "type": "array", "items": { "type": "string" } },
                        "truncated": { "type": "boolean" },
                        "output_truncated": { "type": "boolean" },
                    },
                },
            })),
            "annotations": { "destructiveHint": true },
        }),
        json!({
            "name": "close_pane",
            "description": "Close one pane; a tab's only pane closes the tab. The main window's last \
                            tab cannot be closed.",
            "inputSchema": {
                "type": "object",
                "required": ["pane_id"],
                "properties": { "pane_id": pane_id },
            },
            "outputSchema": output(json!({ "pane_id": { "type": "string" } })),
            "annotations": { "destructiveHint": true },
        }),
        json!({
            "name": "session_token",
            "description": "This session's resume token, a bearer secret: anyone holding it can \
                            claim this session's panes after it disconnects. Keep it private. A \
                            revoked result means a reload issued a fresh token; read it again.",
            "inputSchema": { "type": "object", "properties": {} },
            "outputSchema": output(json!({ "resume_token": { "type": "string" } })),
            "annotations": { "destructiveHint": false, "readOnlyHint": true },
        }),
        json!({
            "name": "claim_panes",
            "description": "Take back the panes an earlier, disconnected session of this agent \
                            created, named by that session's resume_token. own_token is this \
                            session's current token (from session_token), proving it was received \
                            before ownership moves; the old token stops working.",
            "inputSchema": {
                "type": "object",
                "required": ["resume_token", "own_token"],
                "properties": {
                    "resume_token": { "type": "string" },
                    "own_token": { "type": "string" },
                },
            },
            "outputSchema": output(json!({ "claimed": { "type": "integer", "minimum": 0 } })),
            "annotations": { "destructiveHint": false },
        }),
    ]
}

/// Run the control tool `name`; `None` when no control tool has that name.
pub(crate) fn call(
    shared: &McpShared,
    name: &str,
    arguments: &Value,
    scope: &CallScope,
) -> Option<Value> {
    let result = match name {
        "create_pane" => create_pane(shared, arguments, scope),
        "send_text" => send_text(shared, arguments, scope),
        "send_keys" => send_keys(shared, arguments, scope),
        "run_command" => run_command(shared, arguments, scope),
        "close_pane" => close_pane(shared, arguments, scope),
        "session_token" => session_token(shared, scope),
        "claim_panes" => claim_panes(shared, arguments, scope),
        _ => {
            // When: the `name` is no control tool, the read tools answer for it.
            return None;
        }
    };
    Some(result)
}

/// An `invalid_arguments` tool error.
fn invalid(message: &str) -> Value {
    tool_error("invalid_arguments", message)
}

/// The pane id under `key`, from a string or a number.
fn pane_arg(arguments: &Value, key: &str) -> Option<u64> {
    pane_id_arg(&json!({ "pane_id": arguments.get(key)? }))
}

/// A loop reply that is not the expected kind, as a tool error.
fn reply_error(reply: LoopReply) -> Value {
    match reply {
        LoopReply::Refused(code, message) => tool_error(code, &message),
        _ => tool_error("internal", "unexpected reply"),
    }
}

/// The `placement` argument, if it names one.
fn placement_arg(arguments: &Value) -> Option<Placement> {
    match arguments.get("placement").and_then(Value::as_str)? {
        "right" => Some(Placement::Right),
        "down" => Some(Placement::Down),
        "tab" => Some(Placement::Tab),
        _ => None,
    }
}

/// The `argv` argument: empty when absent, `None` when it is not an array of strings.
fn argv_arg(arguments: &Value) -> Option<Vec<OsString>> {
    let Some(value) = arguments.get("argv") else {
        // When: `arguments` hold no `argv` `value`, the helper runs the configured shell.
        return Some(Vec::new());
    };
    let items = value.as_array()?;
    items.iter().map(|item| item.as_str().map(OsString::from)).collect()
}

/// `create_pane`.
fn create_pane(shared: &McpShared, arguments: &Value, scope: &CallScope) -> Value {
    let Some(anchor) = pane_arg(arguments, "anchor_pane_id") else {
        // When: `pane_arg` found no usable `anchor` id, nothing is created.
        return invalid("anchor_pane_id is required");
    };
    let Some(placement) = placement_arg(arguments) else {
        // When: `placement_arg` found no `right`, `down` or `tab`, nothing is created.
        return invalid("placement must be right, down or tab");
    };
    let Some(argv) = argv_arg(arguments) else {
        // When: `argv_arg` found something other than an array of strings, nothing is created.
        return invalid("argv must be an array of strings");
    };
    let launch = AgentLaunch {
        argv,
        cwd: arguments.get("cwd").and_then(Value::as_str).map(PathBuf::from),
        title: arguments.get("title").and_then(Value::as_str).map(str::to_owned),
    };
    match shared.ask(LoopCall::CreatePane { anchor, placement, launch }, scope) {
        LoopReply::Created(created) => {
            let started = created.launch == LaunchOutcome::Started;
            let mut result = json!({ "pane_id": created.pane_id.to_string(), "started": started });
            // The token that claims this pane later; absent when the session has none.
            if let Some(token) = shared.sessions.tokens.token_of(scope.session) {
                result["resume_token"] = json!(token.to_hex());
            }
            tool_result(&format!("created pane {}", created.pane_id), result)
        }
        other => reply_error(other),
    }
}

/// `send_text`.
fn send_text(shared: &McpShared, arguments: &Value, scope: &CallScope) -> Value {
    let Some(pane_id) = pane_id_arg(arguments) else {
        // When: no usable `pane_id` was given, nothing is written.
        return invalid("pane_id is required");
    };
    let Some(text) = arguments.get("text").and_then(Value::as_str) else {
        // When: no `text` string was given, nothing is written.
        return invalid("text is required");
    };
    if text.len() > MAX_TEXT_BYTES {
        // When: the `text` is over `MAX_TEXT_BYTES`, it is refused whole.
        return invalid("text is over 64 KiB");
    }
    let submit = arguments.get("submit").and_then(Value::as_bool).unwrap_or(false);
    let call = LoopCall::SendText { pane_id, text: text.to_owned(), submit };
    written(shared.ask(call, scope))
}

/// `send_keys`.
fn send_keys(shared: &McpShared, arguments: &Value, scope: &CallScope) -> Value {
    let Some(pane_id) = pane_id_arg(arguments) else {
        // When: no usable `pane_id` was given, nothing is written.
        return invalid("pane_id is required");
    };
    // Every element must be a name: one that is not refuses the call rather than being skipped.
    let names: Option<Vec<String>> = arguments
        .get("keys")
        .and_then(Value::as_array)
        .and_then(|items| items.iter().map(|item| item.as_str().map(str::to_owned)).collect());
    let Some(names) = names else {
        // When: `keys` is not an array of names, nothing is written.
        return invalid("keys must be an array of key names");
    };
    let keys = match keys::parse_keys(&names) {
        Ok(keys) => keys,
        Err(keys::KeysError::TooMany) => {
            // When: `TooMany` keys were given, the call is refused whole.
            return invalid("at most 64 keys");
        }
        Err(error) => {
            // When: a key name is unknown (`error`), nothing is sent.
            return invalid(&format!("{error:?}"));
        }
    };
    written(shared.ask(LoopCall::SendKeys { pane_id, keys }, scope))
}

/// How long a write waits for the pane's writer to write what it queued.
const DELIVERY_WAIT: std::time::Duration = std::time::Duration::from_millis(250);
/// How often that wait checks.
const DELIVERY_POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// A write's reply as a tool result, with `delivery`: `"written"` once the pane's writer wrote and
/// flushed it to the PTY (never proof the program read it), `"queued"` if it was still waiting
/// after [`DELIVERY_WAIT`], or `"failed"` if the writer stopped first.
fn written(reply: LoopReply) -> Value {
    match reply {
        LoopReply::Written(bytes, receipt) => {
            let delivery = receipt.map_or("queued", |(ticket, probe)| delivery_of(&probe, ticket));
            tool_result(
                &format!("{bytes} bytes, {delivery}"),
                json!({ "bytes": bytes, "delivery": delivery }),
            )
        }
        other => reply_error(other),
    }
}

/// Wait up to [`DELIVERY_WAIT`] for `ticket` to be written, on the connection's worker.
fn delivery_of(probe: &sonicterm_io::pty::PtyDeliveryProbe, ticket: u64) -> &'static str {
    use sonicterm_io::pty::PtyDelivery;
    let deadline = std::time::Instant::now() + DELIVERY_WAIT;
    loop {
        let status = probe.status(ticket);
        if status == PtyDelivery::Written {
            // When: the `status` is Written, the writer flushed everything up to the ticket.
            return "written";
        }
        if status == PtyDelivery::Failed {
            // When: the `status` is Failed, the writer stopped before reaching the ticket.
            return "failed";
        }
        if std::time::Instant::now() >= deadline {
            // When: still queued at the `deadline`, the write is reported as still waiting.
            return "queued";
        }
        std::thread::sleep(DELIVERY_POLL);
    }
}

/// `run_command`.
// Ordering: output_generation Acquire pairs with the VT worker's Release publish.
fn run_command(shared: &McpShared, arguments: &Value, scope: &CallScope) -> Value {
    let Some(pane_id) = pane_id_arg(arguments) else {
        // When: no usable `pane_id` was given, nothing runs.
        return invalid("pane_id is required");
    };
    let Some(command) = arguments.get("command").and_then(Value::as_str) else {
        // When: no `command` string was given, nothing runs.
        return invalid("command is required");
    };
    let timeout = arguments
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_RUN_WAIT, |millis| Duration::from_millis(millis).min(MAX_RUN_WAIT));
    let started =
        match shared.ask(LoopCall::RunStart { pane_id, command: command.to_owned() }, scope) {
            LoopReply::RunStarted(started) => started,
            other => {
                // When: the loop answered `other` than a started run, its refusal is the result.
                return reply_error(other);
            }
        };
    let output_generation = std::sync::Arc::clone(&started.entry.output_generation);
    let outcome = wait_run(shared, *started, timeout, scope);
    let generation = output_generation.load(std::sync::atomic::Ordering::Acquire);
    tool_result(
        outcome.state.as_str(),
        json!({
            "state": outcome.state.as_str(),
            "exit_code": outcome.exit_code,
            "exit_verified": outcome.exit_verified,
            "exit_code_source": outcome.exit_code_source,
            "generation": generation,
            // The returned lines may be missing some of the command's output.
            "output_incomplete": outcome.truncated || outcome.output_truncated,
            "output": {
                "lines": outcome.output,
                "truncated": outcome.truncated,
                "output_truncated": outcome.output_truncated,
            },
        }),
    )
}

/// `close_pane`.
fn close_pane(shared: &McpShared, arguments: &Value, scope: &CallScope) -> Value {
    let Some(pane_id) = pane_id_arg(arguments) else {
        // When: no usable `pane_id` was given, nothing closes.
        return invalid("pane_id is required");
    };
    match shared.ask(LoopCall::ClosePane { pane_id }, scope) {
        LoopReply::Closed => tool_result("closed", json!({ "pane_id": pane_id.to_string() })),
        other => reply_error(other),
    }
}

/// `session_token`: the caller's own resume token, or `unavailable` when it has none.
fn session_token(shared: &McpShared, scope: &CallScope) -> Value {
    match shared.sessions.tokens.token_of(scope.session) {
        Some(token) => tool_result("resume token", json!({ "resume_token": token.to_hex() })),
        None => tool_error("unavailable", "this session has no resume token"),
    }
}

/// `claim_panes`.
fn claim_panes(shared: &McpShared, arguments: &Value, scope: &CallScope) -> Value {
    let token = |key: &str| arguments.get(key).and_then(Value::as_str).and_then(ResumeToken::parse);
    let (Some(resume), Some(own)) = (token("resume_token"), token("own_token")) else {
        // When: either token is missing or not 32 hex digits, nothing moves.
        return invalid("resume_token and own_token must be 32 hex digits");
    };
    match shared.ask(LoopCall::ClaimPanes { resume, own }, scope) {
        LoopReply::Claimed(count) => {
            tool_result(&format!("claimed {count} panes"), json!({ "claimed": count }))
        }
        other => reply_error(other),
    }
}

#[cfg(test)]
#[path = "tools_control_tests.rs"]
mod tools_control_tests;
