# Logging

SonicTerm writes rolling logs through `sonicterm-logging`.

## Paths

- Logs on macOS and Windows: `~/.sonicterm/logs/sonicterm.log`
- Crash dumps on macOS and Windows: `~/.sonicterm/logs/crashes/`

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

## Render timing diagnostics

Set `[logging].level = "debug"` to include `target="render_timing"` frame timing
lines in `sonicterm.log`. The renderer reports the main/child window label and
phase timings such as grid walk, overlay assembly, glyph upload, surface acquire,
submit, and present. There is no separate render-timing config key or environment
variable.

## GPU / software-render diagnostics

At startup the renderer logs the selected wgpu adapter at `info` level,
including its `device_type` and a `software_rendering` flag. When there is no
usable GPU (RDP / VM / VDI falls back to a CPU rasterizer) it also logs a
`software-render degrade engaged` line showing the frame-cap change. If the
terminal feels heavy on a remote/virtual machine, check these lines first — and
see `[appearance].software_render_mode` in the Configuration wiki.

## Bug report bundle

When reporting a bug, include:

1. SonicTerm version and OS version.
2. The last 200 lines of `sonicterm.log`.
3. A screenshot for rendering, font, VT, or pane-layout issues.
