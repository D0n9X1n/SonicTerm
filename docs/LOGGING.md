# Logging

Developer documentation: [Architecture](ARCHITECTURE.md) · [Modules](MODULES.md) · **Logging** · [Packaging](packaging/README.md)

SonicTerm writes rolling logs through `sonicterm-logging`.

## Paths

- Logs on macOS and Windows: `~/.sonicterm/logs/sonicterm.log`
- Crash dumps on macOS and Windows: `~/.sonicterm/logs/crashes/`

Crash dumps and exit-path traces are written in the same directory when
available.

## Configuration

```toml
[logging]
level = "warn"          # error | warn | info | debug
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

SonicTerm supports four user-facing levels: `error`, `warn`, `info`, and
`debug`. Set `[logging].level = "debug"` to include performance diagnostics,
including `target="render_timing"` frame timing lines in `sonicterm.log`. The
renderer reports the main/child window label and phase timings such as grid walk,
overlay assembly, glyph upload, surface acquire, submit, and present. Each line
also includes `mode=full|partial` and `damaged_rows=<n>`; partial mode appears
when the software-render path reuses the retained frame and only assembles dirty
viewport rows. There is no separate render-timing config key or environment
variable.

## GPU / software-render diagnostics

At startup the renderer logs the selected wgpu adapter at `info` level,
including its `device_type` and a `software_rendering` flag. When there is no
usable GPU (RDP / VM / VDI falls back to a CPU rasterizer) it also logs a
`software-render degrade engaged` line showing the frame-cap change. If the
terminal feels heavy on a remote/virtual machine, check these lines first — and
see `[appearance].software_render_mode` in the Configuration wiki.

The Windows software presenter retains full-surface repaint semantics. Its
selection is controlled by `software_render_mode`: `auto` follows adapter
software detection, `force` selects it, and `off` disables it. Dirty-rectangle
messages in this path describe the area written into the software buffer; they
do not opt the presenter into retained GPU rendering.

## Redraw and window-lifecycle diagnostics

PTY output is coalesced on VT workers, but native redraw requests are delivered
as typed user events and executed on the winit event-loop thread. A worker must
never call `Window::request_redraw()` while holding a pane redraw-target mutex.
When a torn-out child is cleaned up, the app emits a line like:

```text
child window reaped after drag-merge; remaining children=0
```

Normal event-loop shutdown emits a `sonic_exit` line. A UI hang does not
necessarily produce a Rust panic or crash dump; capture a macOS process sample
before force-quitting:

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

For torn-out-window close reports, include whether multiple panes were producing
continuous output, whether the original window remained responsive, and whether
pane child processes were reaped after the child window closed.

## Bug report bundle

When reporting a bug, include:

1. SonicTerm version and OS version.
2. The last 200 lines of `sonicterm.log`.
3. A screenshot for rendering, font, VT, or pane-layout issues.
