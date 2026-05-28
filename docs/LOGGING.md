# Logging in Sonic

Sonic uses [`tracing`] for all in-process logging. The logging subsystem
is initialised at the very top of `main()` so even early bootstrap
errors (config parse, theme load, panic during init) end up on disk.

## Where logs live

| Platform | Log directory                              |
|----------|---------------------------------------------|
| macOS    | `~/Library/Logs/Sonic/`                    |
| Windows  | `%LOCALAPPDATA%\Sonic\Logs\`               |
| Linux/dev| `$XDG_STATE_HOME/sonic/logs/` (fallback `~/.local/state/sonic/logs/`) |

Inside that directory you will find:

- `sonic.log.YYYY-MM-DD` — current day's events. `tracing-appender`
  rotates daily; the freshest file is the one being actively written.
- `sonic.log.YYYY-MM-DD` (older) — previous days, capped by retention.
- `crashes/crash-<utc-iso8601>.log` — per-panic dump (see Crash dumps).

The path can be overridden by setting the `SONIC_LOG_DIR` env var
before launching Sonic — useful in CI and ops.

## Retention

All five knobs are exposed in `sonic.toml` under `[logging]`. Defaults:

```toml
[logging]
max_file_size_mb    = 10   # soft cap per active log file
max_rotated_files   = 5    # delete older rotated logs beyond this
max_age_days        = 14   # delete rotated logs older than this (0 = off)
max_crash_dumps     = 10   # delete oldest crash dumps beyond this
max_crash_age_days  = 30   # delete crash dumps older than this (0 = off)
```

Total worst-case disk usage:
`max_file_size_mb * (max_rotated_files + 1)` ≈ **~60 MB** for logs,
plus the size of up to `max_crash_dumps` crash dumps (typically a few
kilobytes each).

Cleanup runs in a background thread at startup so it can never stall
the GUI; it is also re-runnable from **Help → Clear Old Logs**.

## How to change the log level

Two equivalent options (env var wins if both are set):

1. `RUST_LOG=sonic=debug ./sonic` — standard `tracing_subscriber`
   syntax. Multiple targets: `RUST_LOG=sonic=debug,wgpu=info`.
2. In `sonic.toml`:

   ```toml
   [logging]
   level = "sonic=debug,sonic_vt=info"
   ```

The default per-target filter is:

```
sonic=info,sonic_vt=warn,sonic_grid=warn,wgpu=warn,naga=warn,cosmic_text=warn,glyphon=warn
```

The stderr sink is always pinned to `WARN+` no matter how verbose the
file filter is, so `RUST_LOG=debug` won't drown the terminal you
launched Sonic from.

## Crash dumps

A `tracing_subscriber::Layer` keeps a fixed-size ring of the most
recent 200 events. On panic, Sonic's panic hook writes
`crashes/crash-<utc-iso8601>.log` containing:

- header (timestamp, version, panic location, panic message);
- a full `std::backtrace::Backtrace` (force-captured regardless of
  `RUST_BACKTRACE`);
- the 200-event ring snapshot.

The hook then chains to the previously installed (default) panic
hook, so normal abort behaviour is preserved.

## Filing a bug report

Please attach:

1. The last 200 lines of the most recent log file. On macOS:
   ```sh
   tail -200 ~/Library/Logs/Sonic/sonic.log.* | pbcopy
   ```
   On Windows:
   ```powershell
   Get-Content "$env:LOCALAPPDATA\Sonic\Logs\sonic.log.*" -Tail 200 | Set-Clipboard
   ```
2. Any matching `crashes/crash-*.log` for the same timeframe.
3. If you cleared logs already, mention the rough time the bug
   occurred so we can correlate against your shell history.

## Clearing logs

- **Help → Show Logs in Finder/Explorer** opens the log directory in
  the platform file browser.
- **Help → Clear Old Logs** runs an aggressive cleanup pass that
  removes every rotated log file (preserving only the active one) and
  every crash dump. A native notification reports the count and bytes
  freed.
- Manual nuke: `rm -rf ~/Library/Logs/Sonic/*` (or the platform
  equivalent) — Sonic recreates the directory on next launch.
