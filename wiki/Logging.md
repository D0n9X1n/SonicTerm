# Logging / 日志

## English

### Paths

- Active log: `~/.sonicterm/logs/sonicterm.log`
- Rotated logs: `~/.sonicterm/logs/sonicterm.log.*`
- Crash dumps and exit traces: `~/.sonicterm/logs/crashes/`
- Session markers: `~/.sonicterm/logs/sessions/`
- Diagnostic breadcrumbs: `~/.sonicterm/logs/breadcrumbs/`

On Windows, `~` means the current user's profile directory.

### Configuration

```toml
[logging]
level = "warn"          # error | warn | info | debug
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

`warn` is the default level. SonicTerm loads `sonicterm.toml` before installing
the tracing subscriber, so the configured level applies from normal startup
onward. `RUST_LOG` can override the configured filter for a diagnostic run.
Stderr remains warning-oriented so a broad debug filter does not flood the
console.

Cleanup runs asynchronously. By default, rotated logs and crash dumps older
than two days are removed, at most three rotated logs and ten crash dumps are
kept, and the active log is never deleted by the retention pass. Set an age to
`0` to disable age-based eviction for that file class.

Crash dumps and breadcrumbs are additionally bounded by **aggregate bytes**, so
a burst of large artifacts cannot fill the disk while staying inside the count
and age limits. Each class enforces count, age, and total size independently,
and **the first limit reached wins**. Set a `*_bytes` key to `0` to disable
that axis without disabling the other two.

### Render and performance diagnostics

Set the level to `debug` and reproduce the problem:

```toml
[logging]
level = "debug"
```

The `render_timing` target records per-frame phases such as grid walking,
overlay assembly, glyph upload, surface acquisition, submission, and present.
It also identifies the main or child window and records whether the frame used
full or partial assembly. There is no separate render-timing option.

At startup, the renderer logs the selected wgpu adapter, device type, and
whether it appears to be a software adapter. Under RDP, a VM, or VDI, look for a
`software-render degrade engaged` line and review
`[appearance].software_render_mode` on the [Configuration](Configuration) page.

### Memory diagnostics

#### The aggregate snapshot, at `info`

The single most useful memory record does **not** require `debug`. At `info`
level, `target="memory"` emits one `memory snapshot` line at most every 30
seconds carrying the whole picture — process, session, and renderers — in one
record:

```toml
[logging]
level = "info"
```

```
memory snapshot process_private_committed_bytes=unsupported process_resident_bytes=412876800
                process_virtual_bytes=419923525632 process_private_committed_delta=unavailable
                process_resident_delta=+1841152 process_virtual_delta=+0
                session_total_bytes=182451840 session_delta=+1048576
                grid_visible_bytes=97320960 grid_history_bytes=76841472 grid_alternate_bytes=0
                parser_bytes=0 hyperlink_bytes=3050880 inline_media_bytes=5238528
                pty_output_bytes=0 pty_input_bytes=0 panes_total=12 panes_sampled=12 panes_contended=0
                renderer_total_bytes=35651584 renderer_total_items=1042 renderer_delta=+0
                live_renderers=2
                renderers=visible[WindowId(1)] glyph=16777216/1038 image=2097152/4 software=0/0
                          total=18874368/1042; warm[0] glyph=16777216/0 image=0/0 software=0/0
                          total=16777216/0
```

This line exists for the case where nobody predicted the problem. A session
that grew to several gigabytes and was then killed leaves only whatever the log
already contained, and a user who had to know to set `debug` *beforehand* has
already lost the session they wanted to explain. `info` admits this aggregate,
including its per-renderer breakdown. The per-pane and allocation/release
detail described in the next section stays at `debug`.

**Process figures come from the OS**, not from SonicTerm's own accounting, so
they include everything the seams below cannot see: allocator fragmentation,
retired pages not yet returned, mapped files, GPU driver mappings, thread
stacks.

| Field | Meaning |
| --- | --- |
| `process_private_committed_bytes` | memory charged to this process alone; what an out-of-memory kill is usually applied against |
| `process_resident_bytes` | pages currently in physical memory (macOS resident size, Windows working set) |
| `process_virtual_bytes` | reserved address space |

Read all three together. **Virtual bytes are routinely enormous and routinely
harmless** — a figure in the hundreds of gigabytes is normal for a GPU process
and is reserved address space, not consumption. Treating it as consumption is
the single most common misreading of this line.

Platform coverage differs, and the line says so rather than guessing:

| Figure | macOS | Windows |
| --- | --- | --- |
| private/committed | `unsupported` | `PrivateUsage` |
| resident / working set | resident size | `WorkingSetSize` |
| virtual | task virtual size | summed non-free regions |

`unsupported` is never a zero. macOS's meaningful private figure is
`phys_footprint`, which is not reachable through the APIs SonicTerm builds
against; reporting a substitute would put an invented measurement in the one
report whose value is being trustworthy.

**Deltas** compare against the preceding snapshot. `+0` is a measurement — the
figure did not move. `unavailable` is not: it means the first sample of the
session, or a figure this platform does not expose. The two are deliberately
distinguishable, because acting on "did not move" would rule out the wrong
subsystem.

`panes_total` is every pane visited, `panes_sampled` is the subset included in
`session_total_bytes`, and `panes_contended` is the subset skipped because its
parser lock was held. A non-zero contended count means the session total is
partial. A busy pane holds more memory than an idle one, so a silently omitted
pane would understate the session at exactly the moment it is largest.

`live_renderers` is read from the renderer's own process-wide counter rather
than derived from the `renderers=` breakdown. The two agreeing is the useful
signal: a leaked renderer is alive without being reachable from any window, so
it raises the count while contributing no entry to the breakdown.

The `renderers=` field covers **both** visible and warm renderers. A warm
renderer in the standby pool holds a full-size glyph atlas exactly like a
visible one, so a report omitting them would understate the process — and the
remedy the visible entries imply, closing a window, cannot reach a warm
renderer at all. The lever for those is the warm-pool size.

Sampling shares the existing 30-second retention cadence rather than adding a
timer, and an otherwise idle session wakes on that cadence to take the sample.
**That wake draws no frame.** The wake suppresses redraw in `NewEvents`, then
records the due sample in `AboutToWait`, so an idle session is measured without
repainting.

The aggregate also reports the shared wgpu allocator **once per device/context**,
not once per visible or warm renderer. A measured report contains
`allocator_allocated_bytes`, `allocator_reserved_bytes`,
`allocator_allocations`, `allocator_blocks`, and
`allocator_largest_block_bytes`. `allocator_state` explicitly distinguishes
`measured`, `unsupported` when the selected backend exposes no report, and
`none` when no renderer exists. It remains on this 30-second aggregate cadence; it
is neither per-frame data nor part of the 5-second process-history sampler.

#### Pane and session retention

At `debug` level, `target="memory"` samples what each pane retains, at most
once every 30 seconds. It emits one `pane retention` line per pane, followed by
one `session retention` line:

```
pane retention    pane=WindowId(1)/7 total_bytes=15204320 grid_visible_bytes=8110080
                  grid_history_bytes=6405120 grid_alternate_bytes=0 parser_bytes=0
                  hyperlink_bytes=254240 inline_media_bytes=434880 pty_output_bytes=0
                  pty_input_bytes=0
                  largest_seam=grid_visible largest_seam_bytes=8110080
session retention panes=12 total_bytes=182451840 grid_visible_bytes=97320960 ...
```

Eight seams meter their own memory, and the figures are **disjoint** — each
counts only what it owns, so no allocation is charged twice. They are also
exhaustive: `total_bytes` is their sum, so the rows below account for every
byte in the total.

| Field | Covers |
| --- | --- |
| `grid_visible_bytes` | cells and row storage in the visible primary grid |
| `grid_history_bytes` | retained scrollback history |
| `grid_alternate_bytes` | saved alternate-screen storage |
| `parser_bytes` | in-flight escape and media capture buffers |
| `hyperlink_bytes` | interned OSC 8 URI and id strings |
| `inline_media_bytes` | decoded inline images retained for display |
| `pty_output_bytes` | local PTY output queued or in flight |
| `pty_input_bytes` | input queued toward the shell, typically a large paste |

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
  waited on, so a sample taken while output is streaming may report fewer panes
  than the window has. `panes=` reports how many were actually measured.
- Not every seam plateaus, and that is intended. Grid reaches a true steady
  state once scrollback fills. Interned hyperlinks grow until their cap,
  because a link stays reachable for as long as the cells referencing it remain
  in retained scrollback — freeing it early would break a link the user can
  still scroll back to.

### If an image disappeared, you do not need `debug`

To stay within its memory budget SonicTerm sometimes discards something you
can see: an image transfer that stopped arriving, or older images in panes you
have left idle. Those two events are **not** diagnostics, so they are written
at the default level — you do not have to have enabled `debug` beforehand:

```
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log
```

Two lines can appear there:

| Line | What happened | What you can do |
| --- | --- | --- |
| `cancelled a media capture that stopped receiving` | an image transfer delivered nothing for a full minute and was abandoned; the image will not appear | re-send it. Common after a laptop sleeps mid-transfer or an SSH link drops |
| `discarded inline images from idle panes` | the total decoded image memory crossed the process ceiling, so older images were dropped from panes you were not using | re-send the images you still need, or keep fewer panes holding images |

Both lines carry the byte figures involved. Everything else about memory is
diagnostics and needs `debug`, described next.

The `memory` target samples what each pane holds at most once every 30
seconds. An idle session arms that deadline itself, and the sampling-only wake
does not request a redraw. At `debug`, the pass writes one `pane retention`
line per pane followed by one `session retention` line
for the whole process. Eight figures are reported separately because the
remedy differs, and together they sum to `total_bytes`:

| Field | What it covers | What you can do |
| --- | --- | --- |
| `grid_visible_bytes` | the cells currently on screen | nothing — it is the screen |
| `grid_history_bytes` | scrollback | lower `scrollback` |
| `grid_alternate_bytes` | the screen saved behind a full-screen program | nothing — it frees itself on exit |
| `parser_bytes` | escape sequences and inline-media capture buffers being parsed right now | nothing — transient |
| `hyperlink_bytes` | OSC 8 link targets | nothing — reclaimed as links scroll away |
| `inline_media_bytes` | decoded inline images | lower image usage, or open fewer panes |
| `pty_output_bytes` | the read buffer held by shell output waiting to be parsed | nothing — it drains as the terminal catches up |
| `pty_input_bytes` | input waiting to reach the shell, usually a large paste | nothing — it drains as the shell reads it |

Read `largest_seam` first. A total tells you a pane is large; that field tells
you which subsystem to look at. A pane holding 60 MB of inline images is
working as designed — a pane holding 60 MB of grid is not.

Compare successive `session retention` lines rather than one sample. The shape
of the curve is what separates a working set that levels off from memory that
keeps climbing.

Not every figure levels off, and that is intended. Cells reach a steady state
once scrollback fills. Interned hyperlinks keep growing until their limit is
reached, because a link stays reachable for as long as the cells referencing it
remain in retained scrollback — freeing it earlier would break a link the user
can still scroll back to.

### Triaging a session that feels heavy

The section above says what each figure means. This one is the order to read
them in when SonicTerm is using more memory than you expect, and it ends at a
pane and a subsystem you can name in a report.

**1. Turn the detail lines on.** The aggregate `memory snapshot` is available
at `info`; the per-pane and allocation/release lines used below require
`debug`. At the default `warn` they are absent, so an empty grep means the
level is wrong, not that the session is clean. Set the level in
`~/.sonicterm/sonicterm.toml` (on Windows, under your user profile), restart
SonicTerm, and use it normally for a few minutes:

```toml
[logging]
level = "debug"
```

**2. Find the heaviest pane.** Logs rotate into `sonicterm.log.<date>`, and a
busy day can add a second file, so pick the newest by modification time rather
than by name. This prints every pane's most recent total, largest first:

```sh
LOG=$(ls -1t ~/.sonicterm/logs/sonicterm.log* | head -1)
grep 'pane retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^pane=/)        { p = substr($i, 6); gsub(/"/, "", p) }
    if ($i ~ /^total_bytes=/)   t = substr($i, 13)
  }
  last[p] = t
} END { for (p in last) printf "%10.2f MB  %s\n", last[p] / 1048576, p }' \
  | sort -rn
```

```powershell
$log = Get-ChildItem ~/.sonicterm/logs/sonicterm.log* |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match 'pane="([^"]+)".*?total_bytes=(\d+)') {
    [pscustomobject]@{ Pane = $Matches[1]; Bytes = [long]$Matches[2] }
  }
} | Group-Object Pane | ForEach-Object {
  $newest = $_.Group[-1]
  [pscustomobject]@{ MB = [math]::Round($newest.Bytes / 1MB, 2); Pane = $_.Name }
} | Sort-Object MB -Descending
```

The label is the window id and the pane id. The pane id stays with the pane
when a tab moves to another window, but the window id in front of it changes —
so if a pane seems to vanish between samples, look for the same pane id behind
a different window id.

**3. Ask which subsystem.** On the heaviest pane, read `largest_seam`. A total
says a pane is big; this field says where to look. Each value names one row of
the field table above:

| `largest_seam` | Field it points at | First thing to try |
| --- | --- | --- |
| `grid_visible` | `grid_visible_bytes` | nothing — a large window is large |
| `grid_history` | `grid_history_bytes` | lower `scrollback` |
| `grid_alternate` | `grid_alternate_bytes` | quit the full-screen program in that pane |
| `parser` | `parser_bytes` | recheck next sample — see the note below |
| `hyperlinks` | `hyperlink_bytes` | nothing — bounded, reclaimed as links scroll away |
| `inline_media` | `inline_media_bytes` | display fewer images, or close image-heavy panes |
| `pty_output` | `pty_output_bytes` | let the pane finish printing |
| `pty_input` | `pty_input_bytes` | let the shell drain a large paste |

The seam value is `hyperlinks` but the field is `hyperlink_bytes`; grep for the
one you actually want.

`parser` is the one seam that is normally transient — it holds a sequence being
parsed right now, so it should fall on the next sample. If it stays large
across several, an image transfer probably stopped mid-flight; SonicTerm
cancels that itself and says so at `warn` level (step 6).

**4. Separate large from growing.** This is the step that decides whether you
have a bug. One sample cannot tell them apart — a pane holding 60 MB of images
steadily is working as designed, and a pane climbing every sample is not. Track
one pane across samples, substituting its label from step 2:

```sh
PANE='pane="WindowId(50820809344)/3"'
grep 'pane retention' "$LOG" | grep -F "$PANE" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^total_bytes=/)    t = substr($i, 13)
    if ($i ~ /^largest_seam=/) { s = substr($i, 14); gsub(/"/, "", s) }
  }
  printf "%s  %8.2f MB  %s\n", substr($1, 12, 8), t / 1048576, s
}'
```

```powershell
$pane = 'WindowId(50820809344)/3'
$re = 'pane="' + [regex]::Escape($pane) + '".*?total_bytes=(\d+).*?largest_seam="([^"]+)"'
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match $re) {
    '{0}  {1,8:N2} MB  {2}' -f $_.Line.Substring(11, 8), ([long]$Matches[1] / 1MB), $Matches[2]
  }
}
```

Flat is fine, even at 60 MB:

```text
05:19:28     60.00 MB  inline_media
05:19:59     60.00 MB  inline_media
05:20:30     60.00 MB  inline_media
```

A steady climb across every sample is what to report:

```text
05:19:28      8.00 MB  grid_history
05:19:59     16.00 MB  grid_history
05:20:30     24.00 MB  grid_history
05:21:02     32.00 MB  grid_history
```

**Do not compare only the endpoints.** Samples are rate-limited to at most one
set every 30 seconds, and they are written from the idle-wake path, so a
session that parks with nothing to do can go longer than that between sets. A
gap in the log does not mean memory was flat across it — it means nothing woke
to measure, or the interval had not yet elapsed when something did. Two lines
ten minutes apart may span a burst you cannot see. Read several consecutive
samples and judge the shape.

**5. Check the process view.** Every pane can sit inside its own ceiling while
the total does not. The `session retention` line is the sum across panes, and
it carries `panes=N` instead of `largest_seam` — the seam breakdown is per pane
only.

```sh
grep 'session retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^panes=/)        n = substr($i, 7)
    if ($i ~ /^total_bytes=/)  t = substr($i, 13)
  }
  printf "%s  panes=%-3s %8.2f MB\n", substr($1, 12, 8), n, t / 1048576
}'
```

```powershell
Select-String 'session retention' $log | ForEach-Object {
  if ($_.Line -match 'panes=(\d+) total_bytes=(\d+)') {
    '{0}  panes={1,-3} {2,8:N2} MB' -f $_.Line.Substring(11, 8), $Matches[1], ([long]$Matches[2] / 1MB)
  }
}
```

If `panes` rises alongside the total, the session is growing because it holds
more panes, which is expected. If the total climbs while `panes` holds steady,
go back to step 2 and find which pane is responsible.

**6. Account for what the renderer holds.** Pane figures cover what panes own,
and the renderer's own buffers belong to no pane — so a session can hold tens
of megabytes that the `session retention` total does not include. Those are
reported separately, one `renderer retention` line per renderer, on the same 30
second cadence:

```
renderer retention window=WindowId(1) role=visible total_bytes=17301504
                   glyph_atlas_bytes=16777216 glyph_atlas_items=412
                   image_atlas_bytes=524288 image_atlas_items=3
                   software_frame_bytes=0
renderer retention window=warm[0] role=warm total_bytes=16777216
                   glyph_atlas_bytes=16777216 glyph_atlas_items=0 ...
```

| Field | What it covers | What you can do |
| --- | --- | --- |
| `glyph_atlas_bytes` | rasterized glyph pixels held on the CPU for this renderer | nothing for a visible window — it is bounded, and shared by every pane in the window |
| `image_atlas_bytes` | inline-image pixels uploaded for display | lower image usage, or open fewer panes |
| `software_frame_bytes` | the full-window software presentation buffer | Windows software rendering only; zero everywhere else |

```sh
grep 'renderer retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^window=/)      { w = substr($i, 8); gsub(/"/, "", w) }
    if ($i ~ /^role=/)        { r = substr($i, 6); gsub(/"/, "", r) }
    if ($i ~ /^total_bytes=/)   t = substr($i, 13)
  }
  printf "%s  %-14s %-8s %8.2f MB\n", substr($1, 12, 8), w, r, t / 1048576
}'
```

```powershell
Select-String 'renderer retention' $log | ForEach-Object {
  if ($_.Line -match 'window="?([^"\s]+)"? role="?([^"\s]+)"?.*?total_bytes=(\d+)') {
    '{0}  {1,-14} {2,-8} {3,8:N2} MB' -f $_.Line.Substring(11, 8), $Matches[1], $Matches[2], ([long]$Matches[3] / 1MB)
  }
}
```

**Read `role` before deciding what to do.** `visible` is a window you have open.
`warm` is a renderer SonicTerm built ahead of time so the next window opens
instantly — it belongs to no window on screen, and **closing windows will not
release it.** A warm renderer holds a full-size glyph atlas, typically ~16 MB
each, so on a default session roughly half the renderer memory reported may be
warm. Lower `warm_window_pool` in the Configuration page to reclaim it, at the
cost of slower window opening.

Add the lines up yourself — one appears per renderer, so a session with two
windows and one warm entry reports three. Which renderer holds the memory is the
part that tells you what to change.

`software_frame_bytes` is the one to check first on Windows if you are using
software rendering. It is the largest buffer a renderer holds, and unlike the
atlases it scales with the window: about 32 MB for a 4K window, roughly 59 MB
at 5K, and it can reach 160 MB before the size is refused. Making the window
smaller is what lowers it. It is zero on macOS and on any Windows session using
GPU rendering. `[appearance].software_render_mode` in the Configuration page
controls which path is in use.

These figures are host memory. Textures on the GPU are not included — the
graphics driver owns those and does not report their size — so these lines
account for the CPU-side copies, not video memory.

**7. Two warnings that are not bugs.** These appear at the default `warn`
level, and both mean SonicTerm corrected something rather than that something
broke. Finding one is not by itself worth reporting:

- `cancelled a media capture that stopped receiving; the transfer was abandoned and its staging is reclaimed` — an image transfer stopped mid-flight and its buffer was released instead of being pinned for the life of the pane.
- `revisited idle panes holding an inline-media budget sized for a smaller session` — panes that filled up early were still holding a share of the image budget from when fewer panes existed, and it was handed back.

### When inline images disappear

SonicTerm bounds decoded inline images per pane and across the whole process.
A pane that keeps displaying new images will eventually drop its oldest ones,
and with many panes open each gets a share of the process budget rather than
the full per-pane amount. Every pane always keeps at least its newest image.

This is deliberate, and it is logged: look for `inline media evicted to hold
the process-wide ceiling` at `warn` level. If images vanish and that line is
absent, it is a bug worth reporting.

Hyperlinks behave differently. When the link table fills, SonicTerm reclaims
entries whose text has scrolled out of history so that new links keep working.
Links that are still on screen are never reclaimed.

### Crash and hang diagnostics

The panic hook writes the panic payload, source location, backtrace, and a
small ring of recent tracing events to the crash directory. Normal event-loop
shutdown also emits a `sonic_exit` marker. A UI deadlock or hang may produce
neither a panic nor a crash file.

On macOS, capture a process sample before force-quitting:

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

For a torn-out-window problem, also record whether multiple panes were
producing continuous output, whether surviving windows remained responsive,
and whether the pane child processes exited after the window closed.

### After a session disappears

A process killed with `SIGKILL`, Force Quit, `TerminateProcess`, or an
out-of-memory kill runs **no** cleanup code. It writes no dump, no final log
line, and flushes nothing. **SonicTerm cannot capture a memory dump for those
terminations, and does not claim to.** No terminal application can: the process
is destroyed before any handler runs.

What SonicTerm does instead is leave evidence *before* the failure. At startup
it writes a session marker under:

```
~/.sonicterm/logs/sessions/session-<id>.marker
```

The marker is removed when the session reaches its shutdown path. One still
present on the next launch means that session never got there. The next launch
reports it at `warn`, so it reaches the log you already have:

```
grep 'did not reach its shutdown path' ~/.sonicterm/logs/sonicterm.log
```

A marker carries only process identity — session id, pid, version, platform,
start time, and state. No shell, no command, no environment, no window or tab
titles, no paths you opened.

Alongside it, a background breadcrumb worker takes one immediate fixed-cost
process sample and then another every 5 seconds. It retains at most 48 samples,
about four minutes. The stable wire record is exactly
`event=resource_history private_committed=... resident=...`: there is no
`virtual` field. On Windows this uses fixed-cost `GetProcessMemoryInfo`; the
expensive `VirtualQuery` walk of virtual address space remains only on the
30-second full `event=resource` / `memory snapshot` path. macOS private commit
remains explicitly `unsupported`.

The breadcrumb file has three partitions:

- The latest version, platform, renderer and adapter, counts, full resource
  sample, retention, and allocator state are pinned. Full process state uses
  `event=resource private_committed=... resident=... virtual=...`; retention
  uses `event=retention session_bytes=... renderer_bytes=...
  live_renderers=...`. Allocator state uses either `allocator=unsupported` or
  the same five `allocator_*` fields listed in the aggregate report.
- Lifecycle transitions remain ordered and bounded.
- `resource_history` is rolling; only its oldest samples are evicted.

History therefore cannot evict identity or retention. Custom file budgets are
accepted only when validation proves that the configured budget can hold the
mandatory pinned records, configured lifecycle (`ring_capacity`) capacity, and
one maximum-width history record. The documented minimum is 4096 bytes, but
that does not imply every `ring_capacity` fits every budget. Within an accepted
budget, mandatory state is preserved first and the newest history that fits is
retained.

Each rewrite is a same-directory atomic replace. A hard OOM or
`TerminateProcess` cannot run a handler, so the surviving artifact is the last
complete **pre-OOM breadcrumb**, not an OOM dump and not proof of cause. If the
process dies during a rewrite, the in-progress temporary write may be lost while
the prior complete file survives. A timestamp gap in which `resource_history`
continues after the pinned `retention` timestamp stops is evidence of an
event-loop stall or starvation.

**A stale marker proves the session did not finish. It does not say why.**
`SIGKILL`, a power cut, an OOM kill, and a hard reset are indistinguishable
from the marker alone, so the report names no cause rather than guessing one.

Three properties are worth knowing when reading a report:

- **A still-running session is never reported.** Running several SonicTerm
  windows at once is ordinary, so a marker whose process is still alive is
  skipped. The check uses the pid, so PID reuse can in rare cases hide a stale
  marker — under-reporting a rare case beats reporting every concurrent
  instance as a crash.
- **A truncated marker is still evidence.** A marker half-written when the
  power failed is reported as an interrupted session rather than ignored.
- **Each session is reported once.** The marker is cleared after reporting, so
  a finding does not repeat on every launch forever.

When a Rust panic still leaves the process in control, SonicTerm writes a
session-tagged artifact under `crashes/` with the panic, backtrace, and recent
tracing events. Fatal-signal evidence follows a smaller, signal-safe path:

| Failure | Evidence SonicTerm produces |
| --- | --- |
| Rust panic on any thread | Session-tagged artifact under `crashes/` |
| Unix SIGSEGV, SIGBUS, SIGILL, SIGABRT, or SIGFPE | Fixed `FATAL: SIG…` marker appended to `sonicterm.log`, then the signal is re-raised for OS diagnostics |
| Windows fatal exception or process termination | Standard WER or LocalDumps records, when Windows is configured to produce them |
| Allocation failure on Unix | The SIGABRT marker above; the marker cannot distinguish allocation failure from another abort |

Compatible artifacts already present under `crashes/` are still classified by
content as panic, fatal signal, or allocator failure. That compatibility does
not mean the current signal handler writes a session-tagged crash file. When a
stale marker has no artifact, the report says so in plain words — that no
process-written memory dump exists, and why one cannot. It then points at what
does survive.

SonicTerm also looks for postmortem records the operating system wrote:

| Platform | Locations checked |
| --- | --- |
| macOS | `~/Library/Logs/DiagnosticReports`, `/Library/Logs/DiagnosticReports` (`.ips`) |
| Windows | `%LOCALAPPDATA%\CrashDumps`, `%LOCALAPPDATA%\Microsoft\Windows\WER\ReportQueue`, `...\ReportArchive` |

These are matched by **filename convention**, not provenance, so the report
says a record "may relate to" the session rather than asserting it is
SonicTerm's. Two limits are stated rather than implied: on Windows the WER
registry configuration under `HKLM\SOFTWARE\Microsoft\Windows\Windows Error
Reporting\LocalDumps` is **not** read, so finding nothing means "no artifacts
in the standard locations", not "WER is disabled"; and on macOS a crash during
very early startup can land in the system directory rather than the user one,
which is why both are checked.

### Bug-report bundle

Include:

1. SonicTerm version and OS version.
2. The last 200 lines of the newest `sonicterm.log`.
3. The relevant crash dump or process sample, if any.
4. A screenshot or short recording for rendering, font, VT, input, or pane-layout issues.
5. Exact reproduction steps and whether the problem occurs on a hardware or software GPU.
6. For a memory report, the `pane retention` lines for the pane identified in
   [Triaging a session that feels heavy](#triaging-a-session-that-feels-heavy)
   and every `session retention` line over the same window — at least five
   consecutive samples, roughly three minutes of a session in use, not two
   lines far apart. Say what the session was doing, how many panes were open,
   and quote the pane's `largest_seam`.

Avoid posting secrets, tokens, environment dumps, or sensitive command output.

## 中文

### 路径

- 当前日志：`~/.sonicterm/logs/sonicterm.log`
- 轮转日志：`~/.sonicterm/logs/sonicterm.log.*`
- 崩溃转储和退出追踪：`~/.sonicterm/logs/crashes/`
- 会话标记：`~/.sonicterm/logs/sessions/`
- 诊断面包屑：`~/.sonicterm/logs/breadcrumbs/`

在 Windows 上，`~` 表示当前用户的配置文件目录。

### 配置

```toml
[logging]
level = "warn"          # error | warn | info | debug
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

默认级别是 `warn`。SonicTerm 会先读取 `sonicterm.toml`，再安装 tracing
subscriber，因此正常启动阶段会直接使用配置的级别。诊断时可以通过 `RUST_LOG`
临时覆盖过滤器；stderr 仍以 warning 为下限，避免宽泛的 debug 过滤器刷满控制台。

清理任务在后台异步执行。默认删除两天以前的轮转日志和崩溃转储，最多保留三个轮转日志和
十个崩溃转储；保留策略不会删除当前活动日志。把对应的 age 设置为 `0` 可以关闭该类文件的
按年龄清理。

崩溃转储与面包屑还额外受**总字节数**限制，因此即便数量与年龄都在限制之内，一批体积很大
的工件也无法占满磁盘。每一类都独立执行数量、年龄与总大小三条限制，且**先触发的限制生效**。
把某个 `*_bytes` 键设为 `0` 可以只关闭这一条限制，而不影响另外两条。

### 渲染与性能诊断

把日志级别改成 `debug` 后复现问题：

```toml
[logging]
level = "debug"
```

`render_timing` target 会记录每帧的网格遍历、overlay 组装、字形上传、surface 获取、
提交和 present 等阶段，并标明主窗口或子窗口，以及帧使用完整还是局部组装。没有单独的
render-timing 配置项。

启动时，渲染器会记录选中的 wgpu adapter、设备类型和是否检测为软件 adapter。在 RDP、
虚拟机或 VDI 中，请先查找 `software-render degrade engaged`，并结合
[配置 / Configuration](Configuration) 中的 `[appearance].software_render_mode` 排查。

### 内存诊断

#### `info` 级别的聚合快照

最有用的内存记录**不需要** `debug`。在 `info` 级别下，`target="memory"` 最多每
30 秒输出一行 `memory snapshot`，在单条记录中承载完整画面——进程、会话与渲染器：

```toml
[logging]
level = "info"
```

```
memory snapshot process_private_committed_bytes=unsupported process_resident_bytes=412876800
                process_virtual_bytes=419923525632 process_private_committed_delta=unavailable
                process_resident_delta=+1841152 process_virtual_delta=+0
                session_total_bytes=182451840 session_delta=+1048576
                grid_visible_bytes=97320960 grid_history_bytes=76841472 grid_alternate_bytes=0
                parser_bytes=0 hyperlink_bytes=3050880 inline_media_bytes=5238528
                pty_output_bytes=0 pty_input_bytes=0 panes_total=12 panes_sampled=12 panes_contended=0
                renderer_total_bytes=35651584 renderer_total_items=1042 renderer_delta=+0
                live_renderers=2
                renderers=visible[WindowId(1)] glyph=16777216/1038 image=2097152/4 software=0/0
                          total=18874368/1042; warm[0] glyph=16777216/0 image=0/0 software=0/0
                          total=16777216/0
```

这一行正是为“没人预料到问题”的场景而存在。会话增长到数 GB 后被终止时，只剩下日志
中已有的内容；而必须**事先**知道要设置 `debug` 的用户，早已失去了想要解释的那个
会话。`info` 接纳这条聚合记录及其中按渲染器的拆分；下一节描述的按窗格与分配/释放
明细仍保持在 `debug`。

**进程数据来自操作系统**，而非 SonicTerm 自身的记账，因此包含下方各接缝无法看到的
一切：分配器碎片、尚未归还的退役页、映射文件、GPU 驱动映射、线程栈。

| 字段 | 含义 |
| --- | --- |
| `process_private_committed_bytes` | 仅计入本进程的内存；内存不足终止通常针对该值 |
| `process_resident_bytes` | 当前位于物理内存中的页（macOS 常驻大小，Windows 工作集） |
| `process_virtual_bytes` | 已保留的地址空间 |

三者要一起读。**虚拟字节数通常极大，且通常无害**——对 GPU 进程而言数百 GB 属于
正常，那是保留的地址空间而非实际占用。把它当作占用，是这一行最常见的误读。

各平台的覆盖范围不同，该行会如实说明而不做猜测：

| 数据 | macOS | Windows |
| --- | --- | --- |
| private/committed | `unsupported` | `PrivateUsage` |
| resident / 工作集 | 常驻大小 | `WorkingSetSize` |
| virtual | 任务虚拟大小 | 非空闲区域求和 |

`unsupported` 绝不等于零。macOS 上有意义的 private 数据是 `phys_footprint`，
SonicTerm 所依赖的 API 无法获取；用替代值上报，等于在唯一以可信为价值的报告中
放入一个虚构的测量值。

**增量**与前一次快照比较。`+0` 是一次测量——数值没有变化；`unavailable` 不是：它
表示这是会话的首次采样，或该平台不提供此数据。两者刻意保持可区分，因为按“没有
变化”行事会排除掉错误的子系统。

`panes_total` 是本轮访问的全部窗格数，`panes_sampled` 是计入
`session_total_bytes` 的子集，`panes_contended` 是因解析器锁被占用而跳过的子集。
只要跳过数非零，会话总量就是不完整的。繁忙窗格比空闲窗格占用更多内存，因此静默
省略某个窗格，恰恰会在会话最大的时刻低估它。

`live_renderers` 读取自渲染器自身的进程级计数器，而非由 `renderers=` 明细推导。
两者一致才是有用的信号：泄漏的渲染器虽然存活，却无法从任何窗口访问，因此它会抬高
计数，却不会在明细中留下条目。

`renderers=` 字段同时覆盖可见渲染器与预热渲染器。待命池中的预热渲染器与可见渲染器
一样持有全尺寸字形图集，因此省略它们的报告会低估进程占用——而且可见条目所暗示的
处置方式（关闭窗口）根本无法触及预热渲染器。对它们而言，可调的是预热池大小。

采样沿用既有的 30 秒保留量节奏，而非新增计时器；空闲会话也会按该节奏唤醒以完成
采样。**该次唤醒不会绘制任何帧。** 它先在 `NewEvents` 中抑制重绘，再在
`AboutToWait` 中记录到期采样，因此空闲会话被测量时不会重绘。

聚合报告还会按每个 device/context 对共享 wgpu 分配器**只报告一次**，而不是对每个
可见或预热渲染器各报告一次。已测量的报告包含
`allocator_allocated_bytes`、`allocator_reserved_bytes`、
`allocator_allocations`、`allocator_blocks` 与
`allocator_largest_block_bytes`。`allocator_state` 明确区分 `measured`、所选 backend
不提供报告时的 `unsupported`，以及没有渲染器时的 `none`。它仍沿用 30 秒聚合周期；既
不是逐帧数据，也不属于 5 秒进程历史采样器。

#### 窗格与会话保留量

在 `debug` 级别下，`target="memory"` 会采样每个窗格保留的内存，最多每 30 秒一次。
每个窗格输出一行 `pane retention`，随后输出一行 `session retention`：

```
pane retention    pane=WindowId(1)/7 total_bytes=15204320 grid_visible_bytes=8110080
                  grid_history_bytes=6405120 grid_alternate_bytes=0 parser_bytes=0
                  hyperlink_bytes=254240 inline_media_bytes=434880 pty_output_bytes=0
                  pty_input_bytes=0
                  largest_seam=grid_visible largest_seam_bytes=8110080
session retention panes=12 total_bytes=182451840 grid_visible_bytes=97320960 ...
```

八个接缝各自统计自身的内存，且这些数字**互不重叠**——每个只统计自己拥有的部分，
因此不会有任何一次分配被重复计入。它们同时也是完备的：`total_bytes` 是它们的总和，
所以下表的各行覆盖了总量中的每一个字节。

| 字段 | 含义 |
| --- | --- |
| `grid_visible_bytes` | 可见主网格中的单元格与行存储 |
| `grid_history_bytes` | 保留的回滚历史 |
| `grid_alternate_bytes` | 保存的备用屏幕存储 |
| `parser_bytes` | 处理中的转义序列与媒体捕获缓冲区 |
| `hyperlink_bytes` | 驻留的 OSC 8 URI 与 id 字符串 |
| `inline_media_bytes` | 为显示而保留的已解码内联图像 |
| `pty_output_bytes` | 已排队或传输中的本地 PTY 输出 |
| `pty_input_bytes` | 排队送往 shell 的输入，通常是大段粘贴 |

`largest_seam` 指出占比最大的子系统。请先读它：仅有总量只能说明窗格很大，
却没有指出该往哪里查，而每个接缝的处理方式并不相同——
一个持有 60 MB 内联图像的窗格属于设计预期，而持有 60 MB 网格的窗格则不是。

`session retention` 这一行才是用来对照内存增长报告的。每个窗格的数字都各自受限，
因此在所有窗格都合规的情况下，会话总量仍可能远超任何单个上限；只有会话总量能反映这一点。
排查增长时，请比较连续多行 `session retention`，而不是任何单次采样——
曲线的形状才能区分「工作集趋于平稳」与「保留量持续攀升」。

阅读这些数据时有两点需要注意：

- 若某个窗格的解析器锁正被其 VT 线程持有，该窗格会被**跳过**而不是等待，
  因此在输出流式刷屏时采样，报告的窗格数可能少于窗口实际拥有的数量。
  `panes=` 给出的是实际测量到的数量。
- 并非每个接缝都会趋于平稳，这是预期行为。网格在回滚缓冲填满后会进入真正的稳态。
  驻留的超链接会一直增长到其上限，因为只要引用它的单元格仍留在保留的回滚历史中，
  该链接就仍可被访问——提前释放会破坏用户回滚后仍能点击的链接。

### 如果图像消失了，无需开启 `debug`

为了控制内存占用，SonicTerm 有时会丢弃你能看到的内容：停止传输的图像，或
闲置面板中较早的图像。这两类事件**不是**诊断信息，因此在默认级别下即会写
入日志——无需事先开启 `debug`：

```
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log
```

其中可能出现两种日志行：

| 日志行 | 发生了什么 | 可采取的措施 |
| --- | --- | --- |
| `cancelled a media capture that stopped receiving` | 图像传输整整一分钟没有新数据，已被放弃；该图像不会显示 | 重新发送。笔记本在传输中途休眠或 SSH 连接中断后常见 |
| `discarded inline images from idle panes` | 已解码图像的总内存超出进程上限，因此丢弃了未使用面板中较早的图像 | 重新发送仍需要的图像，或减少持有图像的面板数量 |

两种日志行都会附带相关的字节数。其余内存信息均属诊断范畴，需要 `debug`
级别，见下文。

`memory` 目标最多每 30 秒采样一次每个面板占用的内存。空闲会话会自行安排这个
截止时间，而仅用于采样的唤醒不会请求重绘。在 `debug` 级别下，该轮会为每个面板
写入一行 `pane retention`，随后为整个进程写入一行 `session retention`。以下八项数值
分别报告，因为对应的处理方式不同；它们相加即为 `total_bytes`：

| 字段 | 含义 | 可采取的措施 |
| --- | --- | --- |
| `grid_visible_bytes` | 当前显示在屏幕上的单元格 | 无 — 这就是屏幕本身 |
| `grid_history_bytes` | 回滚缓冲 | 调低 `scrollback` |
| `grid_alternate_bytes` | 全屏程序背后保存的主屏幕 | 无 — 程序退出时自动释放 |
| `parser_bytes` | 当前正在解析的转义序列与内联媒体暂存缓冲 | 无 — 瞬时占用 |
| `hyperlink_bytes` | OSC 8 链接目标 | 无 — 链接滚出后自动回收 |
| `inline_media_bytes` | 已解码的内联图像 | 减少图像使用，或减少面板数量 |
| `pty_output_bytes` | 等待解析的 shell 输出所占用的读取缓冲区 | 无 — 终端处理完毕后自动释放 |
| `pty_input_bytes` | 等待送往 shell 的输入，通常来自大段粘贴 | 无 — shell 读取后自动释放 |

先看 `largest_seam`。总量只能说明面板占用大，而该字段指出应当检查哪个子系统。
占用 60 MB 内联图像的面板属于正常设计；占用 60 MB 网格的面板则不正常。

请比较连续多行 `session retention`，而不是只看单次采样。曲线的形状才能区分
「工作集趋于平稳」与「内存持续增长」。

并非每个数值都会趋于平稳，这是有意为之。回滚缓冲填满后，单元格占用达到稳态；
而暂存的超链接会持续增长直至上限，因为只要引用它们的单元格仍留在回滚历史中，
这些链接就仍可通过向上滚动访问。

### 排查内存偏高的会话

上一节说明每个数值的含义。本节给出的是当 SonicTerm 内存占用超出预期时，应当按
什么顺序去读这些数值，最终定位到可以写进报告里的具体面板和子系统。

**1. 先打开明细日志行。** 聚合 `memory snapshot` 在 `info` 级别即可获得；下文
使用的按窗格与分配/释放日志行需要 `debug`。默认 `warn` 级别下不会出现这些行，
因此 grep 不到内容说明级别不对，而不是会话没有问题。请在
`~/.sonicterm/sonicterm.toml`（Windows 上位于用户配置文件目录）中设置级别，
重启 SonicTerm，并正常使用几分钟：

```toml
[logging]
level = "debug"
```

**2. 找出占用最高的面板。** 日志会轮转为 `sonicterm.log.<date>`，繁忙的一天还
可能产生第二个文件，因此请按修改时间而不是文件名挑选最新的一个。下面的命令按
从大到小列出每个面板最近一次的总量：

```sh
LOG=$(ls -1t ~/.sonicterm/logs/sonicterm.log* | head -1)
grep 'pane retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^pane=/)        { p = substr($i, 6); gsub(/"/, "", p) }
    if ($i ~ /^total_bytes=/)   t = substr($i, 13)
  }
  last[p] = t
} END { for (p in last) printf "%10.2f MB  %s\n", last[p] / 1048576, p }' \
  | sort -rn
```

```powershell
$log = Get-ChildItem ~/.sonicterm/logs/sonicterm.log* |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match 'pane="([^"]+)".*?total_bytes=(\d+)') {
    [pscustomobject]@{ Pane = $Matches[1]; Bytes = [long]$Matches[2] }
  }
} | Group-Object Pane | ForEach-Object {
  $newest = $_.Group[-1]
  [pscustomobject]@{ MB = [math]::Round($newest.Bytes / 1MB, 2); Pane = $_.Name }
} | Sort-Object MB -Descending
```

标签由窗口 id 和面板 id 组成。标签页移动到另一个窗口时，面板 id 会跟随面板保持
不变，但前面的窗口 id 会改变 —— 因此如果某个面板在两次采样之间「消失」了，
请在不同的窗口 id 下查找相同的面板 id。

**3. 判断是哪个子系统。** 在占用最高的面板上读 `largest_seam`。总量只能说明
面板占用大，该字段指出应当检查哪里。每个取值都对应上一节字段表中的一行：

| `largest_seam` | 对应字段 | 首先可以尝试 |
| --- | --- | --- |
| `grid_visible` | `grid_visible_bytes` | 无 — 窗口大，占用自然大 |
| `grid_history` | `grid_history_bytes` | 调低 `scrollback` |
| `grid_alternate` | `grid_alternate_bytes` | 退出该面板中的全屏程序 |
| `parser` | `parser_bytes` | 在下次采样时复查 — 见下方说明 |
| `hyperlinks` | `hyperlink_bytes` | 无 — 有上限，链接滚出后自动回收 |
| `inline_media` | `inline_media_bytes` | 减少显示图像，或关闭图像较多的面板 |
| `pty_output` | `pty_output_bytes` | 等待该面板输出完毕 |
| `pty_input` | `pty_input_bytes` | 等待 shell 读完大段粘贴 |

注意 `largest_seam` 的取值是 `hyperlinks`，而字段名是 `hyperlink_bytes`；
请按实际需要的那个去 grep。

`parser` 是唯一通常只是瞬时占用的接缝 —— 它保存的是当前正在解析的序列，因此
下次采样时就应当回落。如果它在连续多次采样中都保持很大，多半是某次图像传输
中途停止了；SonicTerm 会自行取消，并在 `warn` 级别记录（见第 6 步）。

**4. 区分「占用大」与「持续增长」。** 这一步决定是否真的存在缺陷。单次采样无法
区分两者 —— 稳定占用 60 MB 图像的面板属于正常设计，而每次采样都在上涨的面板则
不是。把第 2 步得到的标签代入下面的命令，跟踪同一个面板：

```sh
PANE='pane="WindowId(50820809344)/3"'
grep 'pane retention' "$LOG" | grep -F "$PANE" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^total_bytes=/)    t = substr($i, 13)
    if ($i ~ /^largest_seam=/) { s = substr($i, 14); gsub(/"/, "", s) }
  }
  printf "%s  %8.2f MB  %s\n", substr($1, 12, 8), t / 1048576, s
}'
```

```powershell
$pane = 'WindowId(50820809344)/3'
$re = 'pane="' + [regex]::Escape($pane) + '".*?total_bytes=(\d+).*?largest_seam="([^"]+)"'
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match $re) {
    '{0}  {1,8:N2} MB  {2}' -f $_.Line.Substring(11, 8), ([long]$Matches[1] / 1MB), $Matches[2]
  }
}
```

保持平稳就没有问题，即使是 60 MB：

```text
05:19:28     60.00 MB  inline_media
05:19:59     60.00 MB  inline_media
05:20:30     60.00 MB  inline_media
```

每次采样都稳定上涨才是值得上报的情况：

```text
05:19:28      8.00 MB  grid_history
05:19:59     16.00 MB  grid_history
05:20:30     24.00 MB  grid_history
05:21:02     32.00 MB  grid_history
```

**不要只比较首尾两行。** 采样被限制为最多每 30 秒一组，且它们是在空闲唤醒路径上
写出的，因此完全空闲、无事可做的会话，两组之间可能间隔更久。日志中的空档并不表示
这段时间内存是平稳的 —— 只表示期间没有唤醒去测量，或唤醒时采样间隔尚未到期。相隔
十分钟的两行之间，可能夹着你看不到的一次突发增长。请读连续多次采样，据此判断曲线
形状。

**5. 查看进程整体。** 可能每个面板都在各自的上限之内，而总量却不是。
`session retention` 行是各面板的求和，它带有 `panes=N` 而没有 `largest_seam`
—— 按接缝的拆分只存在于单个面板。

```sh
grep 'session retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^panes=/)        n = substr($i, 7)
    if ($i ~ /^total_bytes=/)  t = substr($i, 13)
  }
  printf "%s  panes=%-3s %8.2f MB\n", substr($1, 12, 8), n, t / 1048576
}'
```

```powershell
Select-String 'session retention' $log | ForEach-Object {
  if ($_.Line -match 'panes=(\d+) total_bytes=(\d+)') {
    '{0}  panes={1,-3} {2,8:N2} MB' -f $_.Line.Substring(11, 8), $Matches[1], ([long]$Matches[2] / 1MB)
  }
}
```

如果 `panes` 与总量一起上升，说明会话增长是因为打开了更多面板，属于预期行为。
如果 `panes` 保持不变而总量持续上涨，请回到第 2 步，找出是哪个面板导致的。

**6. 计入渲染器持有的内存。** 面板数值只涵盖面板自身拥有的内存，而渲染器的
缓冲区不属于任何面板 —— 因此会话可能持有数十 MB 内存，却不计入
`session retention` 的总量。这部分内存单独报告，每个渲染器一行
`renderer retention`，采样周期同样是 30 秒：

```
renderer retention window=WindowId(1) role=visible total_bytes=17301504
                   glyph_atlas_bytes=16777216 glyph_atlas_items=412
                   image_atlas_bytes=524288 image_atlas_items=3
                   software_frame_bytes=0
renderer retention window=warm[0] role=warm total_bytes=16777216
                   glyph_atlas_bytes=16777216 glyph_atlas_items=0 ...
```

| 字段 | 含义 | 可采取的措施 |
| --- | --- | --- |
| `glyph_atlas_bytes` | 该渲染器在 CPU 侧保存的字形位图像素 | 对可见窗口无需处理 —— 该值有上限，且由窗口内所有面板共用 |
| `image_atlas_bytes` | 为显示而上传的内联图像像素 | 减少图像使用，或减少面板数量 |
| `software_frame_bytes` | 整窗软件呈现缓冲区 | 仅出现于 Windows 软件渲染；其他情况均为 0 |

```sh
grep 'renderer retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^window=/)      { w = substr($i, 8); gsub(/"/, "", w) }
    if ($i ~ /^role=/)        { r = substr($i, 6); gsub(/"/, "", r) }
    if ($i ~ /^total_bytes=/)   t = substr($i, 13)
  }
  printf "%s  %-14s %-8s %8.2f MB\n", substr($1, 12, 8), w, r, t / 1048576
}'
```

```powershell
Select-String 'renderer retention' $log | ForEach-Object {
  if ($_.Line -match 'window="?([^"\s]+)"? role="?([^"\s]+)"?.*?total_bytes=(\d+)') {
    '{0}  {1,-14} {2,-8} {3,8:N2} MB' -f $_.Line.Substring(11, 8), $Matches[1], $Matches[2], ([long]$Matches[3] / 1MB)
  }
}
```

**在决定如何处理之前，请先看 `role`。** `visible` 表示当前打开的窗口；
`warm` 则是 SonicTerm 预先创建、用于让下一个窗口瞬间打开的渲染器 —— 它不属于
屏幕上任何窗口，**关闭窗口并不会释放它。** 每个预热渲染器都持有一份完整尺寸的
字形图集，通常约 16 MB，因此在默认会话中，报告出的渲染器内存可能有近一半来自
预热渲染器。如需回收，请在配置页面调低 `warm_window_pool`，代价是新窗口打开
变慢。

请自行将各行相加 —— 每个渲染器对应一行，因此「两个窗口 + 一个预热渲染器」的
会话会输出三行。究竟是哪个渲染器占用了内存，才是决定如何调整的依据。

在 Windows 上使用软件渲染时，请优先查看 `software_frame_bytes`。它是单个渲染器
持有的最大缓冲区，且与图集不同，它随窗口尺寸变化：4K 窗口约 32 MB，5K 约 59 MB，
最高可达 160 MB，超过则拒绝分配。缩小窗口才能降低该值。在 macOS 上，以及在使用
GPU 渲染的 Windows 会话中，该值均为 0。具体使用哪条路径由配置页面的
`[appearance].software_render_mode` 控制。

这些数值均为主机内存。GPU 上的纹理不计入其中 —— 这部分由显卡驱动持有，且不报告
大小 —— 因此这几行统计的是 CPU 侧的副本，而非显存。

**7. 两条不代表缺陷的警告。** 这两行在默认的 `warn` 级别下也会出现，且都表示
SonicTerm 已经纠正了某个状况，而不是出了故障。仅仅看到其中一条并不值得上报：

- `cancelled a media capture that stopped receiving; the transfer was abandoned and its staging is reclaimed` —— 图像传输中途停止，其缓冲已被释放，而不是一直占用到该面板关闭为止。
- `revisited idle panes holding an inline-media budget sized for a smaller session` —— 早期填满的面板仍持有面板数较少时分得的图像预算份额，现已归还。

### 内联图像消失时

SonicTerm 对已解码的内联图像按面板和整个进程分别设限。持续显示新图像的面板
最终会丢弃最旧的图像；打开多个面板时，每个面板获得进程预算的一份份额，而非
完整的单面板额度。每个面板始终至少保留最新的一张图像。

这是有意行为，并且会记录日志：查找 `warn` 级别的
`inline media evicted to hold the process-wide ceiling`。若图像消失而该行缺失，
则属于值得上报的缺陷。

超链接的行为不同。链接表填满时，SonicTerm 会回收其文本已滚出历史记录的条目，
使新链接继续可用。仍显示在屏幕上的链接永远不会被回收。

### 崩溃与卡死诊断

panic hook 会把 panic 内容、源码位置、backtrace 和最近一小段 tracing 事件写入崩溃目录。
正常事件循环退出还会记录 `sonic_exit`。UI 死锁或卡死不一定产生 panic 或 crash 文件。

macOS 上请在强制退出前抓取进程 sample：

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

如果问题与拖出的子窗口有关，还应说明：是否有多个窗格持续输出、其它窗口是否仍有响应，
以及关闭窗口后对应窗格的子进程是否退出。

### 会话意外消失之后

被 `SIGKILL`、强制退出、`TerminateProcess` 或内存不足终止的进程**不会**运行任何
清理代码：不写转储、不写最后一行日志、不刷新任何缓冲。**SonicTerm 无法为这些终止
方式捕获内存转储，也不会声称可以。** 任何终端程序都做不到：进程在任何处理器运行
之前就已被销毁。

SonicTerm 的做法是在故障发生*之前*留下证据。启动时它会写入会话标记文件：

```
~/.sonicterm/logs/sessions/session-<id>.marker
```

会话走到关闭路径时该标记会被删除。若下次启动时它仍然存在，说明那个会话从未走到
那一步。下次启动会以 `warn` 级别报告，因此它会出现在你已有的日志中：

```
grep 'did not reach its shutdown path' ~/.sonicterm/logs/sonicterm.log
```

标记只记录进程身份——会话 id、pid、版本、平台、启动时间与状态。不含 shell、命令、
环境变量、窗口或标签标题，也不含你打开过的任何路径。

与此同时，后台面包屑 worker 会立即做一次固定成本的进程采样，之后每 5 秒再采样一次；
最多保留 48 个样本，约四分钟。稳定的 wire 记录严格为
`event=resource_history private_committed=... resident=...`，没有 `virtual` 字段。
Windows 使用固定成本的 `GetProcessMemoryInfo`；代价较高的 `VirtualQuery` 虚拟地址空间
遍历只保留在每 30 秒一次的完整 `event=resource` / `memory snapshot` 路径。macOS 的
private commit 仍明确为 `unsupported`。

面包屑文件分为三个区段：

- 最新 version、platform、renderer 与 adapter、counts、完整 resource 样本、retention
  和 allocator 状态固定保留。完整进程状态使用
  `event=resource private_committed=... resident=... virtual=...`；retention 使用
  `event=retention session_bytes=... renderer_bytes=... live_renderers=...`。allocator
  状态使用 `allocator=unsupported`，或聚合报告中列出的同一组五个 `allocator_*` 字段；
- lifecycle transition 保持有序且有界；
- `resource_history` 滚动保留，只淘汰最旧的历史样本。

因此历史无法挤掉身份或 retention。只有当校验能证明配置预算容得下强制保留的固定状态、
配置的 lifecycle（`ring_capacity`）容量与一条最大宽度历史记录时，才接受自定义文件预算；
文档下限为 4096 bytes，但这不表示每种 `ring_capacity` 都适合每个预算。对已接受的预算，
先保留强制状态，再保留能够放下的最新历史。

每次重写都在同一目录内做原子替换。硬 OOM 或 `TerminateProcess` 无法执行 handler，
所以最终留下的是最后一份完整的 **OOM 前面包屑**，不是 OOM dump，也不是原因证明。
若进程在重写中途终止，正在写的临时文件可能丢失，而此前完整文件仍会保留。如果
`resource_history` 时间戳继续前进，但固定的 `retention` 时间戳停止，二者之间的空档
说明 event loop 发生了 stall 或 starvation。

**残留标记只能证明会话没有正常结束，不能说明原因。** 仅凭标记无法区分 `SIGKILL`、
断电、OOM 终止与硬重启，因此报告不会臆测原因。

阅读报告时有三点值得了解：

- **仍在运行的会话绝不会被报告。** 同时开启多个 SonicTerm 窗口是常态，因此进程仍
  存活的标记会被跳过。该检查基于 pid，所以在极少数 PID 复用的情况下可能漏报一个
  残留标记——漏报罕见情况，好过把每个并发实例都报成崩溃。
- **被截断的标记同样是证据。** 断电时只写了一半的标记会被报告为“中断的会话”，而
  不是被忽略。
- **每个会话只报告一次。** 报告后标记即被清除，因此同一条结论不会在此后每次启动
  时重复出现。

当 Rust panic 发生后进程仍保有控制权时，SonicTerm 会在 `crashes/` 下写入带会话 id
的工件，其中包含 panic、backtrace 与最近的 tracing 事件。致命信号则走更小的、信号
安全的路径：

| 故障 | SonicTerm 产生的证据 |
| --- | --- |
| 任意线程上的 Rust panic | `crashes/` 下带会话 id 的工件 |
| Unix SIGSEGV、SIGBUS、SIGILL、SIGABRT 或 SIGFPE | 向 `sonicterm.log` 追加固定的 `FATAL: SIG…` 标记，然后重新触发信号以交给操作系统诊断 |
| Windows 致命异常或进程终止 | Windows 已配置生成时，使用标准 WER 或 LocalDumps 记录 |
| Unix 内存分配失败 | 上述 SIGABRT 标记；该标记无法区分内存分配失败与其它 abort |

`crashes/` 下已有的兼容工件仍会按内容分类为 panic、致命信号或分配器失败；这项兼容性
并不表示当前信号处理器会写入带会话 id 的崩溃文件。当残留标记没有对应工件时，报告会
明确说明不存在由进程写出的内存转储，以及为什么不可能存在，然后指向确实留存下来的
证据。

SonicTerm 还会查找操作系统写出的事后记录：

| 平台 | 检查的位置 |
| --- | --- |
| macOS | `~/Library/Logs/DiagnosticReports`、`/Library/Logs/DiagnosticReports`（`.ips`） |
| Windows | `%LOCALAPPDATA%\CrashDumps`、`%LOCALAPPDATA%\Microsoft\Windows\WER\ReportQueue`、`...\ReportArchive` |

这些记录按**文件名约定**匹配，而非按来源，因此报告只会说某条记录“可能与”该会话
相关，而不断言它就是 SonicTerm 的。两条限制是明说而非暗示：Windows 上**不会**读取
`HKLM\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps` 下的 WER 注册表
配置，因此没有发现结果只意味着“标准位置下没有工件”，而不是“WER 已禁用”；macOS 上
启动极早期的崩溃可能落在系统目录而非用户目录，这也是两者都要检查的原因。

### Bug 报告材料

请附上：

1. SonicTerm 版本和操作系统版本。
2. 最新 `sonicterm.log` 的最后 200 行。
3. 相关崩溃转储或进程 sample（如果存在）。
4. 渲染、字体、VT、输入或窗格布局问题的截图或短录屏。
5. 精确复现步骤，以及问题发生在硬件 GPU 还是软件 GPU 上。
6. 内存问题请附上：在
   [排查内存偏高的会话](#排查内存偏高的会话) 中定位到的那个面板的
   `pane retention` 行，以及同一时间段内所有 `session retention` 行 ——
   至少五次连续采样，约相当于三分钟的实际使用，而不是相隔很远的两行。
   同时说明当时会话在做什么、打开了多少个面板，并附上该面板的 `largest_seam`。

不要公开密钥、token、完整环境变量或敏感命令输出。
