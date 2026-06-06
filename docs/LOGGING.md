# Logging

SonicTerm writes rolling logs through `sonicterm-logging`.

## Paths

- macOS: `~/Library/Logs/SonicTerm/sonicterm.log`
- Windows: `%LOCALAPPDATA%\SonicTerm\logs\sonicterm.log`

Crash dumps and exit-path traces are written in the same directory when
available.

## Configuration

```toml
[logging]
level = "info"       # trace | debug | info | warn | error
max_files = 8
max_bytes = 1048576
```

Logging is initialized after `sonicterm.toml` is loaded so the configured level
is honored from startup onward. Old log files are cleaned asynchronously.

## Bug report bundle

When reporting a bug, include:

1. SonicTerm version and OS version.
2. The last 200 lines of `sonicterm.log`.
3. A screenshot for rendering, font, VT, or pane-layout issues.
