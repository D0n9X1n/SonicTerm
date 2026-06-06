# Logging

SonicTerm writes rolling logs through `sonicterm-logging`.

## Paths

- Logs: `~/.sonicterm/logs/sonicterm.log`
- Crash dumps: `~/.sonicterm/logs/crashes/`

Crash dumps and exit-path traces are written in the same directory when
available.

## Configuration

```toml
[logging]
level = "info"          # trace | debug | info | warn | error
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
```

Logging is initialized after `sonicterm.toml` is loaded so the configured level
is honored from startup onward. Log files and crash dumps older than 2 days are
cleaned asynchronously by default.

## Bug report bundle

When reporting a bug, include:

1. SonicTerm version and OS version.
2. The last 200 lines of `sonicterm.log`.
3. A screenshot for rendering, font, VT, or pane-layout issues.
