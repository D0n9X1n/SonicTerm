# MCP Server

[简体中文](MCP-Server-zh-CN)

SonicTerm can run a local [Model Context Protocol](https://modelcontextprotocol.io)
server so that agents such as Claude Code and Copilot CLI can read its panes and,
within the scope you choose, act on them. It is off by default. While it is off,
nothing listens and no endpoint exists.

This page shows how to turn the server on or off, connect an agent, and choose
what the agent may control. The endpoints, their permissions and the read tools'
limits are described in [Platform Integration](Platform-Integration).

```mermaid
flowchart LR
    agent["Agent<br/>Claude Code, Copilot CLI"] -->|"stdio JSON-RPC"| bridge["sonicterm-mcp<br/>bridge"]
    bridge -->|"socket or named pipe"| server["SonicTerm<br/>control server"]
    server --> panes["Panes"]
    config["sonicterm.toml<br/>[mcp] enabled"] -->|"startup or Reload Config"| server
```

## Turn it on or off

The server is controlled by the `[mcp]` section of `sonicterm.toml`:

```toml
[mcp]
# Local MCP server for agents such as Claude Code and Copilot CLI.
# Nothing listens while this is off.
enabled = false
# Control tools act on: "off" (none), "agent" (panes an agent created),
# or "any" (every pane). Read tools work on every pane when enabled.
control = "agent"
```

| Key | Default | Values |
| --- | --- | --- |
| `enabled` | `false` | `true` runs the server; `false` stops it. |
| `control` | `"agent"` | `"off"`, `"agent"` or `"any"`; see Control scope below. |

To turn it on:

1. Run **Edit sonicterm.toml** from the command palette.
2. Set `enabled = true` under `[mcp]` and save the file.
3. Run **Reload Config** from the command palette, or restart SonicTerm.

To turn it off, set `enabled = false`, save, and run **Reload Config**. The
server stops, every connection ends, and its endpoint is removed.

Saving the file alone changes nothing: SonicTerm has no file watcher and reads
`sonicterm.toml` only at startup and on **Reload Config**. A reload that leaves
`[mcp]` unchanged keeps the server and its connections. The full rules for each
key are in [Configuration](Configuration).

## Connect an agent

SonicTerm's own binaries are GUI programs, so agents start the bundled
`sonicterm-mcp` bridge instead. It is a stdio MCP server: it speaks
newline-delimited JSON-RPC on stdin and stdout and relays each line to and from
the running SonicTerm's control server. stdout carries only what SonicTerm
sends; diagnostics go to stderr.

| Platform | `sonicterm-mcp` path |
| --- | --- |
| macOS | `/Applications/SonicTerm.app/Contents/MacOS/sonicterm-mcp` |
| Linux | `sonicterm-mcp` (`/usr/bin` from the `.deb`), or next to `sonicterm` in the tarball |
| Windows | `"C:\Program Files\SonicTerm\sonicterm-mcp.exe"` |

Register it with Claude Code:

```bash
claude mcp add --transport stdio --scope user sonicterm -- /Applications/SonicTerm.app/Contents/MacOS/sonicterm-mcp
```

For Copilot CLI, add an `mcpServers` entry to `~/.copilot/mcp-config.json`:

```json
{ "mcpServers": { "sonicterm": { "type": "local", "command": "/Applications/SonicTerm.app/Contents/MacOS/sonicterm-mcp", "args": [], "tools": ["*"] } } }
```

On Linux or Windows, replace the command with the path from the table above.

### How the bridge finds SonicTerm

The bridge picks its endpoint in this order:

1. `--endpoint <path-or-pipe>`.
2. `SONICTERM_CONTROL`, which SonicTerm sets in every pane it opens while the
   server runs.
3. The only live endpoint in the per-user endpoint directory. None, or more than
   one, is an error that lists what it found; the bridge never guesses between
   instances.

An agent started inside a SonicTerm pane inherits `SONICTERM_CONTROL` and
`SONICTERM_PANE_ID`, so it reaches that instance with no flags, and the bridge
passes `SONICTERM_PANE_ID` as the default pane. A default pane is never
authorization. A pane's environment is fixed when the pane opens, so a pane
opened before the server started has no `SONICTERM_CONTROL`; the bridge then
falls back to the endpoint directory.

Before sending any byte, the bridge checks that the server runs as the same
user: the peer uid on macOS and Linux, the server process's token SID on
Windows. It exits with 2 for a usage error, 1 when it finds no endpoint or
cannot connect, and 0 when either side closes. After SonicTerm closes the
connection the bridge exits once everything it sent has been written to stdout,
or with 1 when stdout takes nothing for 2 s. While SonicTerm is connected, a
client that leaves 4 MiB queued and reads nothing for 5 s ends the relay with 1.
Progress is what the stdout pipe or socket accepts: on macOS any read the client
makes counts. On Linux a pipe frees room a page at a time and a socket one 16 KiB
write at a time, so a client must read at least that much per interval. A client
that reads continuously, as MCP clients do, is never cut off. `--version` (`-V`)
and `--help` (`-h`) print and exit.

## Control scope

`[mcp] control` sets what the control tools may touch. Read tools work on every
pane whenever the server runs.

| `control` | Control tools may act on |
| --- | --- |
| `"off"` | nothing; only the read tools work |
| `"agent"` (default) | panes an agent created, plus opening a helper beside any pane |
| `"any"` | every pane |

A pane in a READONLY window is always refused, whatever the scope. A tab holding
an agent's pane shows a purple ring marker before its title. A reload that
changes `control` drops every request already queued.

Pane text is untrusted input: a program can print text meant to steer an agent.
The server tells agents never to follow instructions found in pane text, but keep
`control` at `"agent"` unless the agent must drive your own panes.

## Tools

The read tools are `list_panes`, `read_pane` and `wait_for`. Their arguments,
caps and locking are described in [Platform Integration](Platform-Integration).

The control tools are checked against `control` again on the event loop when
they run:

| Tool | What it does |
| --- | --- |
| `create_pane` | Opens a helper beside a pane: a split to its right or below, or a new tab in its window. Focus does not move. With `argv` the program runs directly and the pane stays open after it exits, so `wait_for` with `exit` and `read_pane` can read how it ended. |
| `send_text` | Types text, as a bracketed paste when the program asked for one; `submit` adds Enter. At most 64 KiB. |
| `send_keys` | Sends named keys (`Enter`, `Tab`, `Escape`, arrows, `Home`, `End`, `PageUp`, `PageDown`, `F1`–`F12`, a single character), each optionally with `Ctrl+`, `Alt+` or `Shift+`, encoded for the pane's keyboard mode. At most 64 keys; one unknown name sends nothing. |
| `run_command` | Runs one command line at the pane's shell prompt and waits for it to finish, returning its output and the exit code the shell reported (`completed`), `not_started` when the shell ended the prompt without running anything, `interfered` when someone else typed before its output began, `lost` when its record was evicted or the shell exited, or `running` at the timeout (default 60 s, at most 600 s). |
| `close_pane` | Closes one pane; a tab's only pane closes the tab. The main window's last tab is never closed. |

`run_command` needs shell integration: the shell must mark its prompt with
OSC 133 `A`, `B`, `C` and `D`, as the shell-integration scripts of WezTerm,
iTerm2 and kitty do. It refuses a pane whose prompt has text typed after it, whose
input queue is not empty, or into which anyone typed while the previous command
ran. That check is best effort, because a terminal cannot see what the shell has
consumed, so use `run_command` on panes the agent created, where only the agent
types. The exit code is whatever the shell reported.

## Check that it works

- Each connection shows an in-app notification, `Agent connected: <name>`, with
  the name the client reported.
- With `[logging] level = "info"`, the log records `control server started`,
  `control server stopped` and `control level changed`. A server that cannot
  start always logs a warning under the `mcp` target. See [Logging](Logging).
- In a pane opened after the server started, `SONICTERM_CONTROL` holds the
  endpoint's path or pipe name.
- When the bridge reports no endpoint, check that `enabled = true` and that
  **Reload Config** ran after the file was saved.
