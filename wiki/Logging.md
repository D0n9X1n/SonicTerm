# Logging

[简体中文](Logging-zh-CN)

Start with the newest log below. Use `debug` for frame timing, `info` for memory
snapshots, and the bug-report checklist at the end. Crash and hang evidence has
its own section; a missing crash dump does not mean a clean exit.

## Paths

- Log files: `~/.sonicterm/logs/sonicterm.log.*`
- Fatal-signal fallback path: `~/.sonicterm/logs/sonicterm.log`
- Panic artifacts: `~/.sonicterm/logs/crashes/`
- Session markers: `~/.sonicterm/logs/sessions/`
- Bounded breadcrumbs: `~/.sonicterm/logs/breadcrumbs/`

`tracing-appender` uses daily names such as `sonicterm.log.YYYY-MM-DD`; the file
with the newest modification time is active. Size rotation may add a Unix-time
suffix. On Windows, `~` means the current user's profile directory. Native
runtime smokes on macOS, Windows, and Linux use the explicit `logs/` child of
`SONICTERM_RUNTIME_SMOKE_DIR` instead of the user log tree; their separate
`config/` child is used for config/reload state, and `HOME` is preserved. The
outer runner removes inherited `NO_COLOR` and retains failed stdout/stderr plus
SonicTerm logs for CI artifacts.

## Configuration and retention

```toml
[logging]
level = "warn"                    # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
max_crash_bytes = 10485760        # 10 MiB
max_breadcrumb_files = 10
max_breadcrumb_age_days = 2
max_breadcrumb_bytes = 1048576    # 1 MiB
```

`warn` is the default. SonicTerm reads `[logging]` before installing the tracing
subscriber, so the configured level applies to normal startup. `RUST_LOG`
overrides the configured filter for one run. Stderr follows that same filter
with an added global `warn` fallback; more specific target directives can still
emit `debug` lines there, so `warn` is not a hard stderr ceiling.

Before the appender opens, SonicTerm rotates an active log over
`max_file_size_mb` unless the value is `0`, then applies age and count limits to
older log files. The active file is never deleted. Crash and breadcrumb
artifacts are each bounded independently by count, age, and aggregate bytes;
oldest files are removed until all enabled limits are satisfied. Setting an age
or aggregate-byte limit to `0` disables that axis. Cleanup is fail-soft and
artifact cleanup runs on a background thread.

## Levels and diagnostic targets

| Level | What it admits |
| --- | --- |
| `error` | errors only |
| `warn` | warnings, errors, `sonic_exit`, and user-visible reclamation/exhaustion warnings |
| `info` | normal SonicTerm information plus the aggregate `memory snapshot` |
| `debug` | detailed SonicTerm diagnostics, pane/renderer memory lines, state-machine events, `render_timing`, and `tear_out_timing` |

`wgpu`, `naga`, `sonicterm-vt`, and `sonicterm-grid` remain warning-oriented in
the configured filters. Very hot font-shaper dumps are `trace`; no configured
level admits them. Use a targeted `RUST_LOG` directive only when investigating
that path.

## Local-path click diagnostics

Enable `[logging] level = "debug"` before reproducing an explicit local-path
click failure, or use `RUST_LOG=sonicterm_app::app::path_target=debug` for one run.
The `local path activation unverified` event records a click whose detected
explicit path has no current authorized filesystem selection. It uses the
immutable click snapshot, without re-reading the filesystem or the parser.

| Field | Meaning |
| --- | --- |
| `window_id`, `pane_id`, `pointed`, `view_top` | Clicked window/pane, absolute cell, and viewport origin |
| `screen_epoch`, `scrollback_evicted` | Screen and retained-history identity |
| `cwd`, `cwd_revision` | That pane's OSC 7 authority/path and revision, not the process CWD |
| `clicked_path` | Explicit path text associated with the click |
| `candidates` | Bounded candidate set with typed provenance, resolved paths, cell spans, and literal-missing prerequisites |
| `reason` | Current probe failure key, or `path-error-pending` when no matching failure is available |

A pending result is not evidence that the file is missing. The event contains
paths, which can be sensitive, but no whole terminal rows or environment dump;
review it before sharing. Paths use escaped debug formatting, and one event may
be large even though candidate enumeration is bounded. Default `warn` logging
does not emit it. Hover and unverified bare-name clicks do not emit it either.

An absent event proves nothing about success or failure: logging may be disabled,
no target may have been detected, or a rejected target/native-open failure may
have followed another branch. Native-open failures retain their separate
`path open failed` warning. To investigate a relative-path failure, compare the
same file's relative path, absolute path, and local file URI in the same pane;
retain the matching click identity and do not infer its CWD from another shell.

## PTY input rejection diagnostics

The default `warn` level reports input that was refused, including terminal
parser replies. The producer assigns `source`: `Keyboard`, `Paste`, `FileDrop`,
`Ime`, `PointerButton`, `PointerMotion`, `Wheel`, `FocusReport`, `TerminalReply`,
`ScriptDraft`, or `StateMachine`. Sources are never guessed from payload bytes.

| Field | Meaning |
| --- | --- |
| `pane_id` | Stable pane identity supplied at the producer |
| `window_id` | The pane's current window when the event loop handles the rejection; absent after pane closure or when the event loop is unavailable |
| `source`, `rejected_bytes`, `reason` | Input category, refused byte length, and payload-free reason |
| `observation="concurrent"` | Queue and writer fields are independent observations, not one rejection-time transaction |
| `queued_messages`, `queued_bytes`, `queue_capacity` | Waiting message count, payload byte count, and four-slot limit; excludes the active native write |
| `writer_phase` | `Idle`, `Writing`, `Flushing`, or `Stopped`; a boundary observation, not a child-health verdict |
| `in_flight_bytes`, `in_flight_millis` | Active message size and time spent in the observed write or flush; time is absent when idle/stopped |
| `completed_messages` | Native writes whose `write_all` and `flush` both succeeded |

The event carries no rejected payload. Its debug representation, warnings, and
notification never include typed text, commands, paths, or clipboard content.
Notification follows the pane's current window after tab transfers; a closed
pane produces a warning but no notification on an unrelated window. When the
proxy is absent or event delivery fails, the producer logs the same metadata
without a current-window identity.

Production terminal replies enter a separate FIFO with 64 KiB RAM and private
temporary-file spill, outside parser and side-effect locks. UI queue saturation
does not discard replies, trigger rejection warnings, or stop output processing.
Storage/native failure is latched and reported once as `terminal reply delivery
failed` with pane identity and byte count; output, redraw, and exit observation
continue. Native writer/spool read errors are independently logged. Intentional
teardown releases spill storage without a rejection notice. See
[Terminal IO and VT](Terminal-IO-and-VT) for storage and delivery limits.

Four small messages can fill the channel before a healthy writer is scheduled.
Controlled tests use the production admission and writer loop to demonstrate
that burst draining preserves order. Separate blocked-write and blocked-flush
fixtures demonstrate zero queued bytes with one in-flight message, followed by
four additional queued messages and explicit refusal of the next message.
These fixtures distinguish mechanisms; they do not retrospectively identify
which producer or native condition caused an older un-attributed warning.

Queue capacity and per-message limits are unchanged. Each pane coalesces native
pointer motion into one fixed 64-byte slot before queue admission. A full queue
keeps the latest position pending and retries after 10 ms without requesting a
frame; a later position replaces it. Discrete input bundles preceding pending
motion into the same queue message when it fits, preserving byte order without
using an extra slot. A cap-sized or refused discrete message supersedes pending
motion rather than replaying it afterward. Disconnected motion is reported once
and cleared. A changed mouse-tracking/encoding/screen profile invalidates deferred
motion. A busy parser defers motion-only retries until its profile can be checked;
a following discrete input supersedes unvalidated motion rather than delaying the
key or replaying stale bytes. Pane teardown releases its slot. Other UI input still refuses
rather than blocking; worker-owned replies use the spill FIFO instead.
Interpret repeated observations of the same pane and progress counter rather than
a single `QueueFull` warning.

## PTY resize failure diagnostics

The default `warn` level reports a PTY resize the native layer refused, as one
`pty resize failed` event.

| Field | Meaning |
| --- | --- |
| `pane_id` | Stable pane identity of the pane whose PTY refused the resize |
| `cols`, `rows` | Requested geometry, not the geometry the pty currently holds |
| `error` | The failure rendered by `Display`. A native refusal shows the platform text; a zero axis shows `refusing pty resize to <cols>x<rows>` |

The event carries no terminal content: only the pane id, the requested columns
and rows, and the error.

Each pane logs only the first resize failure until a success resets its warning
latch; this avoids input-rate logs during tab activation or window drags.
Attempts still run: only invalid sizes and successful duplicates are skipped by
the IO boundary. The grid keeps the requested geometry. There is no rollback,
retry timer, or failure heartbeat.

## Render and performance diagnostics

Set `level = "debug"`, restart, and reproduce the problem. The
`render_timing` target records frame phases including grid walking, overlay
assembly, glyph upload, surface acquisition, submission, and presentation. It
identifies main or child renderers, `mode=full`, and `damaged_rows`. No-op frames
return before completed-frame timing is emitted. Partial GPU damage limits the
draw scissor, not frame assembly. There is no separate render-timing option.

Startup logs the selected wgpu adapter, device type, and software-adapter
classification. On RDP, VM, or VDI hosts, look for `software-render degrade
engaged` and compare it with `[appearance].software_render_mode` on
[Configuration](Configuration). At `level = "debug"`, each renderer also writes
`renderer LCD subpixel policy` at startup and whenever mode, opacity, theme, or
presenter state changes. Its `requested`, `effective`, `windows_host`,
`opaque_target`, `software_presenter`, and `dual_source_supported` fields explain
every LCD-to-grayscale fallback without relying on a screenshot.

At `info`, `DPI transition synchronized` records `window_id`, `old_scale`,
`new_scale`, `native_scale`, and `size_scale` alongside `old_inner`, `suggested`,
`minimum`, `available`, and `target`. `renderer_before`/`renderer_after` are
physical surface pixels; `cell_before`/`cell_after` are raster-pixel cell extents.
On macOS, `size_scale` follows the native backing scale because `old_inner` is
already reported in that domain. The other platforms use the stored old scale.
These paired inputs/outputs distinguish double scaling from a surface or cell
mismatch without recording terminal content.

## Memory diagnostics

### Aggregate snapshot at `info`

At `level = "info"`, `target="memory"` writes one `memory snapshot` at most
every 30 seconds. It combines OS process figures, all sampled pane seams, visible
and warm renderers, and one shared-device allocator reading:

The following log examples are schematic, not captured measurements. Angle-bracket
values stand for runtime fields; `<metric>` can be a byte count or `unsupported`,
and `<delta>` can be a signed change or `unavailable`. Lines wrap for readability.

```text
memory snapshot process_private_committed_bytes=<metric> process_resident_bytes=<metric>
                process_virtual_bytes=<metric> process_private_committed_delta=<delta>
                process_resident_delta=<delta> process_virtual_delta=<delta>
                session_total_bytes=<bytes> session_delta=<delta>
                grid_visible_bytes=<bytes> grid_history_bytes=<bytes> grid_alternate_bytes=<bytes>
                parser_bytes=<bytes> hyperlink_bytes=<bytes> inline_media_bytes=<bytes>
                pty_output_bytes=<bytes> pty_input_bytes=<bytes> panes_total=<count> panes_sampled=<count> panes_contended=<count>
                renderer_total_bytes=<bytes> renderer_total_items=<count>
                renderer_row_glyph_cache_bytes=<bytes> renderer_row_glyph_cache_items=<count>
                renderer_row_quad_cache_bytes=<bytes> renderer_row_quad_cache_items=<count> renderer_delta=<delta>
                live_renderers=<count> renderers="visible[<window-id>] glyph=<bytes>/<items> image=<bytes>/<items> row_glyph=<bytes>/<items> row_quad=<bytes>/<items> software=<bytes>/<items> total=<bytes>/<items>; warm[<slot>] glyph=<bytes>/<items> image=<bytes>/<items> row_glyph=<bytes>/<items> row_quad=<bytes>/<items> software=<bytes>/<items> total=<bytes>/<items>"
                allocator_state=measured allocator_source=main allocator_label=<window-id>
                allocator_allocated_bytes=<bytes> allocator_reserved_bytes=<bytes>
                allocator_allocations=<count> allocator_blocks=<count> allocator_largest_block_bytes=<bytes>
```

Process figures come from the OS, so they include allocator fragmentation,
retired pages, mapped files, GPU-driver mappings, and thread stacks that
SonicTerm's own seams do not count.

| Field | Meaning |
| --- | --- |
| `process_private_committed_bytes` | memory charged to this process alone; Windows reports `PrivateUsage`, macOS and Linux report `unsupported` |
| `process_resident_bytes` | pages currently resident in physical memory; macOS reports resident size, Windows reports `WorkingSetSize`, and Linux reports `unsupported` |
| `process_virtual_bytes` | reserved address space; macOS and Windows measure it, Linux reports `unsupported`; measured values can be hundreds of gigabytes without representing consumed memory |
| `*_delta` | change since the preceding snapshot; `+0` is measured, while `unavailable` means no comparable sample |
| `panes_total` | all panes visited |
| `panes_sampled` | panes included in `session_total_bytes` |
| `panes_contended` | panes skipped because a parser or inline-image lock was busy; non-zero makes the session total partial |
| `renderer_total_bytes` / `renderer_total_items` | CPU-side storage across visible and warm renderers |
| `renderer_row_glyph_cache_bytes` / `renderer_row_glyph_cache_items` | per-row glyph-instance and decoration cache storage and cached row count across renderers |
| `renderer_row_quad_cache_bytes` / `renderer_row_quad_cache_items` | per-row background/decoration quad cache storage and cached row count across renderers |
| `live_renderers` | process-wide renderer count; a count above the `renderers` entries can expose an unreachable live renderer |
| `renderers` | per-renderer role and glyph/image/row-cache/software storage breakdown |
| `allocator_state` | `measured`, `unsupported` for a backend without a report, or `none` before a renderer exists |
| `allocator_source` / `allocator_label` | renderer class and identifier used for the one shared-device reading |
| `allocator_allocated_bytes` | bytes assigned to live wgpu allocations |
| `allocator_reserved_bytes` | bytes reserved in wgpu allocator blocks |
| `allocator_allocations` | live allocation count |
| `allocator_blocks` | allocator block count |
| `allocator_largest_block_bytes` | largest allocator block in bytes |

The allocator is reported once per shared device/context, not once per renderer.
Sampling shares the retention cadence. An idle session wakes for a due sample,
but that wake suppresses redraw and draws no frame.

### Pane and session detail at `debug`

At `level = "debug"`, the same cadence writes one `pane retention` line for each
sampled pane, then one `session retention` line. A pane whose parser or
inline-image lock is busy is skipped, not waited on.

```text
pane retention pane="<window-id>/<pane-id>" total_bytes=<bytes>
               grid_visible_bytes=<bytes> grid_history_bytes=<bytes>
               grid_alternate_bytes=<bytes> parser_bytes=<bytes> hyperlink_bytes=<bytes>
               inline_media_bytes=<bytes> pty_output_bytes=<bytes> pty_input_bytes=<bytes>
               largest_seam="<seam-name>" largest_seam_bytes=<bytes>
session retention panes=<count> total_bytes=<bytes> grid_visible_bytes=<bytes>
                  grid_history_bytes=<bytes> grid_alternate_bytes=<bytes> parser_bytes=<bytes>
                  hyperlink_bytes=<bytes> inline_media_bytes=<bytes>
                  pty_output_bytes=<bytes> pty_input_bytes=<bytes>
```

The eight seam fields are disjoint and sum to `total_bytes`:

| Field | What it owns | First response |
| --- | --- | --- |
| `grid_visible_bytes` | current screen rows, prompt storage, rare attributes, and grid-container overhead | no action; this includes the screen |
| `grid_history_bytes` | retained scrollback | lower `scrollback` if needed |
| `grid_alternate_bytes` | saved primary-screen rows and history while the alternate screen is active | leave the full-screen program |
| `parser_bytes` | in-flight escape or media-capture buffers | recheck the next sample; normally transient |
| `hyperlink_bytes` | interned OSC 8 URI and id strings | no action; bounded and reclaimed when links leave retained history |
| `inline_media_bytes` | decoded inline images retained by panes | display fewer images or close image-heavy panes |
| `pty_output_bytes` | local PTY output queued or in flight | let output drain |
| `pty_input_bytes` | input queued toward the shell, usually a large paste | let the shell drain it |

Read `largest_seam` first, then compare at least five consecutive samples. A
large flat working set is different from a value that rises every sample. Pane
labels contain window and pane identifiers; a pane keeps its pane id after a tab
moves even though the window id changes.

### Renderer detail at `debug`

Each visible or warm renderer also writes one `renderer retention` line:

```text
renderer retention window="<window-id>" role="visible" total_bytes=<bytes>
                   glyph_atlas_bytes=<bytes> glyph_atlas_items=<count>
                   image_atlas_bytes=<bytes> image_atlas_items=<count>
                   row_glyph_cache_bytes=<bytes> row_glyph_cache_items=<count>
                   row_quad_cache_bytes=<bytes> row_quad_cache_items=<count> software_frame_bytes=<bytes>
renderer retention window="warm[<slot>]" role="warm" total_bytes=<bytes>
                   glyph_atlas_bytes=<bytes> glyph_atlas_items=<count>
                   image_atlas_bytes=<bytes> image_atlas_items=<count>
                   row_glyph_cache_bytes=<bytes> row_glyph_cache_items=<count>
                   row_quad_cache_bytes=<bytes> row_quad_cache_items=<count> software_frame_bytes=<bytes>
```

| Field | What it owns | First response |
| --- | --- | --- |
| `glyph_atlas_bytes` | CPU glyph-atlas capacity for this renderer | bounded; a warm entry is controlled by `warm_window_pool` |
| `glyph_atlas_items` | glyph entries in that atlas | use with bytes to distinguish occupancy from capacity |
| `image_atlas_bytes` | CPU inline-image atlas pixel-buffer capacity, including the nonempty 1×1 placeholder allocation | reduce image use or renderer count |
| `image_atlas_items` | inline-image atlas entries | use with bytes to identify image occupancy |
| `row_glyph_cache_bytes` | hash-table backing plus cached glyph, underline, tofu, and missing-character vector capacities | compare with cached rows; pane departure releases that pane's payload while table capacity can remain at its high-water mark |
| `row_glyph_cache_items` | cached glyph rows | a falling count with flat bytes can mean reusable table capacity remains |
| `row_quad_cache_bytes` | hash-table backing plus cached background/decoration quad vector capacities | compare with cached rows and pane/window churn |
| `row_quad_cache_items` | cached quad rows | a falling count confirms row eviction even when table capacity is sticky |
| `software_frame_bytes` | full-window Windows software-present buffer | reduce window size; zero outside that path |

`role="warm"` means the renderer belongs to the standby pool, not a visible
window; closing a window does not release it. Renderer figures are host memory,
not GPU video memory.

### Reclamation warnings

The default `warn` filter admits warnings on `memory::reclaimed` because they
explain visible loss:

```sh
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log*
```

- `cancelled a media capture that stopped receiving` means no bytes arrived for
  two 30-second samples. The incomplete image will not appear and staging was
  reclaimed.
- `discarded inline images from idle panes to stay within the process ceiling`
  means older decoded images were removed; resend any still needed.

The default filter also admits `inline image atlas full; skipped older images
without evicting text glyphs` on `sonic::glyph_atlas`; renderer atlas pressure
prevented older images from being uploaded. `inline media evicted to hold the
process-wide ceiling` is a warning on the `memory` target and therefore appears
when `level` is `info` or `debug`; it means a pane removed older images while
retaining at least its newest image.

## Crash, hang, and exit evidence

The panic hook runs on every thread and writes a session-tagged
`crashes/crash-<timestamp>.log` containing version, panic payload, source
location, forced backtrace, and up to 50 admitted tracing events. Normal shutdown
writes `sonic_exit` warning lines. On Unix, SIGSEGV, SIGBUS, SIGILL, SIGABRT, and
SIGFPE append a fixed `FATAL: SIG…` line through an async-signal-safe path, then
re-raise the signal for OS diagnostics. Windows relies on WER or LocalDumps when
the system is configured to create them.

Crash history uses the selected `RUST_LOG`/configured filter without widening
it, plus a DEBUG ceiling and an explicit persistence predicate. TRACE is never
retained there, even when an output sink opts in. Font shaping text and
collections use `sonicterm_font::payload`, which is excluded from crash history
at every level; normal warning/error targets retain safe stage/count diagnostics.
Routine white-text and untinted-color-glyph emission is not a warning.

Each record owns at most 4 KiB of variable payload, including at most 256 bytes
of target. The ring also enforces a 64 KiB aggregate variable-capacity bound and
the 50-record limit, evicting oldest records. Formatting uses bounded storage
and UTF-8-safe truncation; `[truncated]` fits inside each cap. Fixed metadata is
bounded separately by record count. Panic payload text and the rendered summary
each have an independent 4 KiB bound, read from borrowed panic data.

Backtrace capture/output and a chained panic hook are separate surfaces. These
limits cover recorder-controlled formatting/retention, not allocations inside
arbitrary producer `Debug` implementations or a guarantee that arbitrary logs
contain no secrets. Payload TRACE remains opt-in normal-sink evidence; structured
breadcrumbs keep their separate metadata-only contract.

A hang may produce no panic artifact. On macOS, sample before force-quitting:

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

`SIGKILL`, Force Quit, `TerminateProcess`, power loss, and a hard OOM run no
cleanup code. SonicTerm cannot write a final line or post-failure dump in those
cases. Instead it leaves two pre-failure records:

1. `sessions/session-<id>.marker` records only session id, pid, version,
   platform, start time, and state. A stale marker proves shutdown was not
   reached; it does not identify the cause. Live sibling processes are skipped,
   damaged markers still count as evidence, and each prior marker is reported
   once on the next launch.
2. `breadcrumbs/breadcrumbs-<id>.log` is a bounded atomic snapshot with no
   terminal text, commands, environment values, tokens, or credentials. It pins
   the latest version, platform, renderer, counts, full process resource sample,
   retention, allocator state, and bounded lifecycle transitions. Its
   `event=retention` record includes `renderer_bytes`,
   `row_glyph_cache_bytes` / `row_glyph_cache_items`, and
   `row_quad_cache_bytes` / `row_quad_cache_items`, so the last complete
   pre-failure snapshot preserves both row-cache size and occupancy. A separate
   fixed-cost `event=resource_history private_committed=... resident=...` sample
   is taken immediately and every 5 seconds, retaining at most 48 samples.
   Virtual address space appears only in the full `event=resource` record.

A breadcrumb rewrite replaces the old file atomically. The surviving file after
a hard kill is the latest complete pre-failure snapshot, not a dump and not
proof of cause. The default file budget is 64 KiB; configured limits must be able
to hold all mandatory records, lifecycle capacity, and one maximum-width history
line. The absolute minimum is 4096 bytes.

On the next launch, SonicTerm also checks OS records by conservative filename
convention:

| Platform | Locations checked |
| --- | --- |
| macOS | `~/Library/Logs/DiagnosticReports`, `/Library/Logs/DiagnosticReports` (`.ips`) |
| Windows | `%LOCALAPPDATA%\CrashDumps`, `%LOCALAPPDATA%\Microsoft\Windows\WER\ReportQueue`, `...\ReportArchive` |

A match only “may relate to” SonicTerm. The Windows check does not read WER
registry configuration, so no file means only that the standard locations held
no match.

## Bug-report bundle

Include:

1. SonicTerm and OS versions.
2. The last 200 lines of the newest `sonicterm.log*` file.
3. The relevant panic artifact, OS record, or process sample, if present.
4. Exact reproduction steps and a screenshot or short recording for visual,
   input, VT, font, or layout defects.
5. Hardware/software adapter information for rendering problems.
6. For memory growth, at least five consecutive `memory snapshot` records; with
   `debug`, also include the identified pane's `pane retention` lines and all
   `session retention` and relevant `renderer retention` lines over the same
   interval. State the pane's `largest_seam` and what the session was doing.

Do not post secrets, tokens, full environment dumps, terminal output, or
sensitive command data.
