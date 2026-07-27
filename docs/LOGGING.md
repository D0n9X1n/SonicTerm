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

At `debug` level, `target="memory"` records accepted surface dimensions, BGRA
byte counts, software-frame capacity changes, and rejected unsafe resize
requests. It also records renderer/window role and identity, software-presenter
state, CPU/GPU atlas dimensions and owned-payload estimates, glyph/image
resident counts, lazy image-atlas promotion, in-place resets, upload dirty-rect
counts/calls/bytes, retained inline-media bytes, and warm-renderer counts.
Payload fields estimate storage directly owned by these resources; they are not
process RSS, commit, or shared-GPU accounting and do not identify the allocator
behind a historical memory incident. Capture these lines when a report involves
a sudden memory jump; they distinguish window/frame/atlas allocations from PTY,
font, or inline-media growth without requiring a diagnostic build.

### Pane and session retention

Also at `debug` level, `target="memory"` samples what each pane retains, at
most once every 30 seconds, and emits one `pane retention` line per pane
followed by one `session retention` line:

```
pane retention    pane=WindowId(1)/7 total_bytes=15204320 grid_visible_bytes=8110080
                  grid_history_bytes=6405120 grid_alternate_bytes=0 parser_bytes=0
                  hyperlink_bytes=254240 inline_media_bytes=434880 pty_output_bytes=0
                  largest_seam=grid_visible largest_seam_bytes=8110080
session retention panes=12 total_bytes=182451840 grid_visible_bytes=97320960 ...
```

Seven seams meter their own memory and the figures are **disjoint** —
each counts only what it owns, so no allocation is charged twice:

| Field | Covers |
| --- | --- |
| `grid_visible_bytes` | cells and row storage in the visible primary grid |
| `grid_history_bytes` | retained scrollback history |
| `grid_alternate_bytes` | saved alternate-screen storage |
| `parser_bytes` | in-flight escape and media capture buffers |
| `hyperlink_bytes` | interned OSC 8 URI and id strings |
| `inline_media_bytes` | decoded inline images retained for display |
| `pty_output_bytes` | local PTY output queued or in flight |

`largest_seam` names the dominant subsystem. Read it first: a total alone says
a pane is large without saying where to look, and the remedy differs per seam —
a pane holding 60 MB of inline media is behaving as designed, while a pane
holding 60 MB of grid is not.

The `session retention` line is the one to check against a growth report.
Per-pane figures are each individually bounded, so a session can grow well past
any single ceiling while every pane remains compliant; only the session total
shows that. When investigating growth, compare successive `session retention`
lines rather than any single sample — the shape of the curve is what
distinguishes a working set that plateaus from retention that keeps climbing.

Two caveats when reading these:

- Panes whose parser lock is held by their VT thread are **skipped**, not
  waited on, so a sample taken while output is streaming may report fewer
  panes than the window has. `panes=` reports how many were actually measured.
- Not every seam plateaus, and that is intended. Grid reaches a true steady
  state once scrollback fills. Interned hyperlinks grow until their cap,
  because a link stays reachable for as long as the cells referencing it remain
  in retained scrollback — freeing it early would break a link the user can
  still scroll back to.

### Reclamation and eviction events

Five `target="memory"` events record memory being reclaimed or refused:

- **`inline media evicted to hold the process-wide ceiling`** (`warn`) — a pane
  dropped its oldest decoded image because the process-wide inline-media total
  would otherwise exceed its ceiling. Fields report the evicted size, the
  pane's remaining retention, the process total, and the ceiling. A pane only
  ever evicts its own images; one busy pane never blanks another.
- **`reclaimed unreferenced hyperlinks`** (`debug`) — the OSC 8 registry was
  full and entries whose cells had scrolled out of scrollback were freed so new
  links keep working. `freed` counts the entries dropped.
- **`OSC 8 hyperlink rejected by memory limits`** (`warn`) — a link was refused
  even after reclamation, meaning the retained links really are still on
  screen. The link renders as unlinked text.
- **`inline image atlas promoted`** (`debug`) — a window displayed inline media
  and allocated an image-capable atlas.
- **`image atlas released after sustained absence of inline media`** (`debug`)
  — that atlas was released after roughly four seconds without inline media.
  Promotion used to be one-way, so a window that displayed a single image held
  the atlas until it closed; this line is the reclamation that replaced it.

A burst of the first event means inline media is the dominant term. A steady
trickle of the second is normal on a link-heavy session and shows reclamation
working. Promotion without a matching release, across a window's whole
lifetime, is worth reporting.

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
