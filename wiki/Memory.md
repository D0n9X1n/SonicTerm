# Memory

[简体中文](Memory-zh-CN)

Use the table below to find a memory limit and what happens when it is reached.
For a growing process, compare consecutive samples using [Logging](Logging).
This page explains what those figures count; protocol and atlas details are in
[Terminal IO and VT](Terminal-IO-and-VT) and [Rendering and Fonts](Rendering-and-Fonts).

### Resource limits

| Owner | Exact bound | Behavior at the bound |
| --- | --- | --- |
| Grid geometry | axis ≤ 4,096; one visible screen ≤ 524,288 cells; visible + history + saved primary ≤ 1,048,576 cells | dimensions and requested history are clamped |
| Grid retained storage | `MAX_GRID_CELLS × size_of::<Cell>()`, about 24 MiB on the current build, shared by visible/history/saved primary; a history row is charged its stored cells, not its width | compact row capacity, then drop oldest history in 64-row blocks; scroll-path checks are amortized every 512 rows |
| Cell combining extras | 64 UTF-8 bytes per cell | additional zero-width data is not retained |
| OSC 8 registry | 16,384 links, 8 KiB per URI, 1 KiB per client id, 8 MiB of shared strings and tables | reclaim entries no retained cell references, then admit; otherwise refuse the new link |
| Escape sequence | 1 MiB | discard through its terminator |
| OSC 0/2/7/8 raw collector | 16 KiB whole payload | reject oversized input; report retained capacity, without incrementing media-capture count |
| Media payload | 16 MiB per transfer | refuse rather than truncate or partially render |
| Media capture staging | 64 MiB process-wide; 4 MiB floor; 13 concurrent floor reservations guaranteed | refuse an unstaged capture; cancel after two unchanged 30 s progress samples |
| Decoded inline images | 64 MiB and 128 images per pane; 256 MiB process target divided across live panes; 4 MiB minimum and newest image retained | discard oldest images; a process under pressure may retain at most one 4 MiB newest-image residual per live pane beyond the target until the idle-pane pass converges |
| Encoded image dimensions | declared width/height ≤ 2,048 and pixels ≤ 2,048² | reject before decode |
| Rendered image dimensions | width/height ≤ 1,024; BGRA8 ≤ 4 MiB | resize iTerm2/kitty images; Sixel decodes into the bounded buffer |
| PTY input | one fixed 64-byte pending pointer-motion slot per pane; four queued UI messages, 16 MiB each; reply FIFO uses 64 KiB RAM including framing, ≤32 KiB writer output, ≤32 KiB + 4 B read scratch, ≤32 KiB app reply-batch payload, and <32 KiB parser-dispatch payload (growable vectors may retain spare capacity) | UI refuses with bytes intact; replies spill to private temporary storage without waiting for native input capacity |
| Reply spill disk | no fixed disk quota; consumed prefixes remain until the FIFO file drains | delete on drain, writer exit, or pane teardown; storage errors explicitly fail reply delivery while output/exit observation continues |
| PTY output | 64 queued chunks plus one blocked sender chunk, each backed by a 64 KiB reader ring; structural worst case 4.0625 MiB | block the reader and apply OS backpressure |
| Glyph atlas | one BGRA8 CPU atlas per renderer that grows by doubling up to 2048×2048, 16 MiB and 16,384 entries | grow first; at 2048 or the entry cap, evict the coldest quarter and retry |
| Image atlas | 1×1 placeholder; 2048×2048 BGRA8 only while media is active | skip older images when full; release to placeholder after 240 media-free frames, or without a frame 30 s after renderable media was last visible |
| Windows software frame | axis ≤ 16,384; total ≤ 160 MiB | reject construction or resize and preserve the old valid allocation |
| Pane command events | 1,024 events | drop the oldest and shrink retained vector capacity |
| Crash event history | 50 records; 4 KiB owned variable payload per record including a target up to 256 bytes; 64 KiB aggregate variable retention | format within the bound and evict oldest records for both count and bytes |
| Panic text | 4 KiB each for the dump payload and rendered summary | truncate at UTF-8 boundaries, including the marker within the bound |

Media capture staging and decoded inline images each account against one
shared pool: production parsers stage in `CaptureStagingPool::process_default()`,
and production panes charge `InlineMediaPool::process_default()`, which is what
makes those limits process-wide. Tests that need a capture admitted, or that
measure admission or budgets, inject private pools instead of sharing a lock.

Crash-history payloads retain exact-sized owned strings rather than spare
string capacity. Fixed record metadata is separately bounded by record count;
backtraces, the chained panic hook, and allocations inside arbitrary producer
formatters are outside the variable-retention bound. Admission and payload
exclusions are described on [Logging](Logging).

The inline-media figure is deliberately stated as a process **target**, not an
absolute 256 MiB ceiling. Every pane must keep its newest image, and one decoded
image is bounded at 4 MiB. The stateable aggregate bound under pressure is
therefore:

```text
256 MiB + live_panes × 4 MiB
```

While `live_panes × 4 MiB` fits the target, the periodic idle-pane walk returns
the total to 256 MiB or below. The larger formula is the pre-convergence bound,
and remains the bound when the required one-image-per-pane floor itself exceeds
the target.

The grid’s approximate 24 MiB figure is also one shared bound, not “24 MiB of
scrollback plus the visible screen.” `[terminal].scrollback` sets a row limit;
cell count and retained bytes can bind first when rows carry hyperlinks,
combining marks, or non-default underline metadata.

A row that scrolls into history is stored compactly. A uniform row becomes one
run of identical cells. Any other row whose trailing run of identical cells
(its fill) is long enough is stored as its cells up to the last one that
differs from the fill, then the fill once, plus its width: the row must keep
at least two fill columns and save at least a quarter of its storage and
256 B. A row of 40 characters at 200 columns drops from 4,800 B to 984 B.
Reading such a row is unchanged; editing it restores the full row. A resize
that pads a history row with a cell other than its fill stores it in full at
the new width. Scrolling reuses the buffer the old row released, so it takes at
most one row allocation per row.

`CSI 3 J` releases active primary history and excess history-container capacity
without lowering either configured history limit. It resets the budget-check
cadence only for that explicit erasure; ordinary FIFO row reuse retains the
512-scroll cadence. History-prefix removal updates exact eviction identity and
prompt coordinates, including prompts stored with the saved primary. Shrinking
columns repairs only a clipped wide lead at each new row edge, preserving
compact runs and existing capacity hysteresis rather than flattening history.

### Ownership model

`sonicterm-resource` tracks each owner's charges by `ResourceClass`, not the
payload memory. Its process-local governor holds the owner tree and ledger;
RAII reservation tokens release their charges when dropped.

Production GUI topology is:

```mermaid
flowchart TD
    process["Process"] --> window["Window"]
    window --> pane["AppPane"]
    process --> retired["Retired PtyTransport"]
```

Each retired PTY gets a unique process-root `PtyTransport` owner and one
`ReaperWork` item. The transport retains its native payload, permits, owner and
charge until whole native completion and worker joins, or transfers them together
to `UnresolvedSink`. Retired transport custody does not keep the former window or
pane owner open. `ReaperWork` is charged in production but contributes no pane
seam-cap term because its charge belongs to the retired transport.

One App-owned supervisor admits at most 256 task units, including reservations,
retained tasks and sink entries. Windows uses 20 helper slots and 2,048 native
handle permits; Unix uses 12 helper slots and 768 descriptor permits. One unit
reserves eight handles on Windows or three descriptors on Unix. A started unit
claims a whole helper grant of five slots on Windows or one on Unix; retries and
live workers retain that grant. Counts describe custody, not only running threads.

Admission refusal keeps ownership. A slotless close retries once, then takes an
explicit synchronous fallback; it does not inherit the reserved route's fast
caller return. Sink entries retain capacity until process exit, so `QueueFull`
can persist for the rest of the process. Terminal disposal preserves incomplete
native payloads and their accounting instead of running unsafe destructors.

The type system also defines `SharedFont`, `SharedRaster`, `SharedAtlas`,
`LocalPty`, and mux owner kinds, but the GUI does not register those nodes.

Owner ids are monotonic and never reused. Legal parent/child combinations depend
on `ProcessKind` and are checked at creation. An owner moves from `Open` to
`Closing` to `Closed`; closing stops new children and reservations, and final
close requires zero live children and charges.

A window and its owner are inserted as one operation. Shared topology completion
registers ownerless panes in main and child windows; every 30 s retention pass
also reconciles them. Tab transfer prepares destination pane owners, moves all
existing charges for every moved pane in one atomic `transfer_many` operation,
then swaps guards and closes the empty source owners. Process and per-class
totals stay unchanged, even with contended parsers. A refusal removes provisional
owners and restores the entire source tab, including its original charges and
live PTYs, before any source window can be reaped.

If window registration fails, that window remains usable outside hierarchy
accounting. Only panes without nonzero charges can enter that unregistered
state; an already charged tab cannot silently lose its accounting on transfer.
Failed pane registration can be retried while the window has an owner.

### Enforcement and the pane tripwire

Each seam—an ownership boundary such as the grid, parser, or PTY queue—enforces
its own limit. The GUI governor uses unlimited process and
per-class limits, and window owners are tracking-only. This avoids maintaining a
second set of process limits that can drift from the code doing the allocation.

Each `AppPane` owner does have one committed-byte tripwire. The typed
`pane_seam_cap_terms()` inventory contains every charged pane class exactly
once. Because visible, history, and saved-primary cells share one grid bound,
`GridVisible` carries that cap while `GridHistory` and `GridAlternate` carry
zero. `ParserCapture` carries both parser caps, and PTY input carries its queue
cap once:

```text
PANE_SEAM_CAP_SUM_BYTES = sum(pane_seam_cap_terms().bytes)
PANE_COMMITTED_BUDGET_BYTES = 2 × PANE_SEAM_CAP_SUM_BYTES
```

The factor of 2 leaves room for allocator capacity, amortized overshoot, and the
newest-image residual. It is a backstop for a seam that stopped bounding or
under-reported its retention, not a second normal allocation policy. Each
retention pass settles existing charges through failure-atomic `try_resize`,
including mixed bytes/items changes. Admission checks the final replacement
amount, not an intermediate peak. Any growing axis requires open ancestors;
reductions can settle during close. A refused sample keeps the old charge, so
accounting may lag retained memory until another successful sample. Snapshots
are observational, not a globally linearizable total.

### What is counted

One pane report contains eight disjoint seams:

| Field | Owned memory |
| --- | --- |
| `grid_visible_bytes` | visible rows, prompt storage, and rare cell attributes |
| `grid_history_bytes` | retained scrollback rows, each charged its stored cells (a trimmed row's prefix and one fill cell) |
| `grid_alternate_bytes` | saved primary screen while the alternate screen is active |
| `parser_bytes` | in-flight escape and media-capture buffers |
| `hyperlink_bytes` | interned OSC 8 strings — one allocation per distinct URI and one per client id under it — and the lookup tables |
| `inline_media_bytes` | decoded image pixels retained by the pane |
| `pty_output_bytes` | ring memory pinned by queued PTY output |
| `pty_input_bytes` | queued input vectors |

`total_bytes` is their sum. `largest_seam` names the largest part. A
`session retention` line sums the same fields across sampled panes.

A `memory snapshot` line taken for a perf checkpoint carries the same fields plus
four tags: `checkpoint_index`, `checkpoint_label`, `checkpoint_attempt`, and
`checkpoint_complete` (no pane contended and every pane sampled). Taking it
changes nothing the periodic sample does: no retention pass, reclamation, trim,
or reset of the sampling cadence. In a build with the trim hook it also carries
`trimmed`, `trim_source` and `trim_seq`.

Renderer memory is separate because it is window-owned rather than pane-owned:

- `glyph_atlas_bytes`: CPU glyph atlas pixel capacity plus its dirty-rect list's capacity, so it rises as the atlas grows;
- per renderer, after `total=`: `glyph_atlas_dim`, `glyph_atlas_packed_pixels`, `glyph_atlas_growths`, `glyph_atlas_evictions`, `glyph_atlas_fit` and `glyph_atlas_max_tile`. The fit is the smallest of 256, 512, 1024 and 2048 that holds the resident tiles with a quarter of its height free. Otherwise it is `no_headroom` (every tile packs at 2048 but no size leaves that quarter free), `does_not_fit` (some tile cannot be placed even at 2048) or `evicted` (the atlas has evicted, so its resident set is no longer its working set). `glyph_atlas_growths` counts the renderer's doublings since it was built; a reset does not clear it;
- `image_atlas_bytes`: CPU inline-image atlas capacity;
- `row_glyph_cache_bytes` / `row_glyph_cache_items`: the payload of cached glyph
  records, underline runs, tofu boxes and missing characters, plus tracking
  storage (tables, slot vectors and pin lists), and the cached row count. Payload
  stays within 448 MiB and tracking within 64 MiB per renderer;
- `row_quad_cache_bytes` / `row_quad_cache_items`: hash-table backing, cached
  background/decoration quad vectors, and row count;
- `software_frame_bytes`: Windows CPU/GDI frame, zero elsewhere;
- `row_ink_bytes` / `row_ink_items`: the renderer's `RowInk` part, counted in
  `renderer_total_bytes`: one ink record per visible row (where it last drew) plus
  one frame's staging. Records of closed panes and vanished rows are pruned when a
  frame presents, and the table is shrunk once under a quarter full. The class is
  reported, never charged, with a 144 MiB envelope (twice the maximum visible
  cells in bucket-rounded table entries, plus one frame's staging);
- `vertex_scratch_bytes` / `vertex_scratch_items`: the renderer's `UploadStaging`
  part, counted in `renderer_total_bytes`. It is the presentation pipeline's
  reused CPU vertex-assembly buffer plus, for each of the glyph and image atlas
  uploads, its dirty and coalesced rect lists and its staging buffer. The vertex
  buffer is cleared and refilled every frame. After a frame, a capacity over four
  times that frame's vertices and over 1 MiB is shrunk to twice its use; a frame
  that emits no vertices releases a buffer over 1 MiB entirely. Each upload
  clears both rect lists after a sync and shrinks a list over 1,024 rects to 64.
  During a sync the staging buffer holds at most one whole atlas; after each
  wgpu sync it follows the vertex buffer's rule, keyed on that sync's largest
  write, and a covered-window trim releases it entirely. The items count only the vertex buffer: 1 while it holds an
  allocation. Dropping the renderer frees all of them. The class's recorded
  coverage figure, 34.5 MiB, is the upload envelope: two 16 MiB staging
  buffers, plus 2 uploads × 2 lists × 2 × 16,384 rects × 20 bytes for the rect
  lists during one sync. The vertex buffer is reported live beside it and has
  no fixed ceiling, because it follows the frame's vertex count;
- `frame_scratch_bytes` / `frame_scratch_items`: the renderer's `FrameScratch`
  part, counted in `renderer_total_bytes`. It holds the per-frame draw vectors
  one assembly pass fills (glyphs, quads, overlay glyphs and quads, images, row
  spans, underlines, underline owners, staged ranges, tofu, pane rects, column
  edges and row keys), kept between assembled frames. Every restoration clears
  each vector and holds it within its cap: glyphs and quads 4 MiB each; overlay
  glyphs, overlay quads, images, row spans, underlines and tofu 1 MiB each; pane
  rects and staged ranges 64 KiB each; underline owners and row keys 256 KiB
  each; column edges 1 MiB in total, dropping the largest slots first. Only a
  completed assembly also shrinks a vector to twice its use once its capacity is
  over four times that use and over 1 MiB, and drops the column-edge slots above
  its peak. A failed or retried frame's use understates the frame, so it keeps
  its warm capacity. The class's coverage figure is the sum, 16,384,000 bytes. An unchanged or no-op frame takes no scratch and
  releases nothing. Items are the vectors that hold an allocation; the figure is
  zero while a frame holds the scratch;
- `chrome_cache_bytes` / `chrome_cache_items`: the renderer's `ChromeCache`
  part, counted in `renderer_total_bytes`: the 64-slot tab-title table and the
  32-slot chrome-run table (allocated on first use), each kept title's key text,
  drawn text and glyphs, each kept chrome run's one text and its glyphs, and the
  kept UI palette's color strings. A title or run is kept only within 256 text
  bytes and 512 glyphs. A palette is kept only while its color strings total at
  most 4 KiB; a theme whose strings total more is derived again on every
  request and never kept, so the kept strings never pass that allowance. The class's coverage
  figure, 1,626,560 bytes, is both tables full of maximal entries plus that 4 KiB.
  Items are the kept titles and runs.

These are host-memory copies. GPU textures and buffers are not included because
the driver owns them and wgpu does not expose their sizes. Row-cache reports use
allocated table and nested-vector capacity, not live length. Table capacity is
sticky across ordinary clear/retain operations; when a pane leaves a renderer,
SonicTerm removes that pane's glyph rows and then quad rows in one event-loop
operation, preserves peer entries, and requests table compaction. Nested payload
and item counts fall immediately, while the table allocator may retain its
current bucket class. Every visible and warm renderer is listed.
`live_renderers` comes from an independent process-wide counter; a count larger
than the listed renderer set indicates a live renderer that is no longer
reachable from window topology.

These fields are not a whole-renderer heap census. The per-frame draw vectors
the renderer keeps in its frame scratch are retained and reported under
`frame_scratch`. Frame-key metadata, the transient frame plan, the per-frame
pane views, the per-row and per-run shaping buffers, and other unlisted host
allocations remain outside `renderer_total_bytes`; the OS process reading
includes memory beyond the charged classes.

While search is open, `SearchState` keeps the prepared matcher, including its
compiled regex in regex mode; the matcher is released when search closes or when
the query, mode, or case setting changes, and the resource governor does not
charge that memory.

Outside the pane seams, the App's foreground-probe map holds at most one entry and
one stored result per live pane, released with the pane, and at most one worker
thread, reported as `live_fg_probe_workers`.

Renderer retention is charged to no ledger owner. When the idle image atlas is
released, the renderer's `retained_amounts().image_atlas` drops from 16 MiB to
4 B at once; the aggregate `renderer_total_bytes` shows the drop at the next
memory sample, every 30 s, and each pane's `InlineMediaRetained` charge is
unchanged.

### Aggregate snapshot

Set the log level to `info` for one `memory snapshot` at most every 30 s:

```toml
[logging]
level = "info"
```

This is a sequential diagnostic sample, not a linearizable point-in-time
snapshot. Each pane's parser, inline media, and PTY queues are read separately;
panes, renderers, the shared allocator, and OS memory are then sampled in turn.
Even `panes_contended=0` does not make those readings simultaneous or turn
sampled charges into allocation-time admission limits.

The line combines:

- `process_private_committed_bytes`, `process_resident_bytes`, and
  `process_virtual_bytes` from the OS, with deltas;
- the session total and all eight pane seams;
- `panes_total`, `panes_sampled`, and `panes_contended`;
- renderer totals, roles, and `live_renderers`;
- `live_fg_probe_workers`, the App's foreground-probe worker threads;
- one shared-device allocator reading.

`process_virtual_bytes` is reserved address space, not consumption. GPU
processes can reserve hundreds of gigabytes without holding that much resident
memory. Compare resident/private figures with `session_total_bytes` and
`renderer_total_bytes`.

Absence states are explicit:

| Value | Meaning |
| --- | --- |
| `unsupported` | the platform or backend does not expose this figure |
| `unavailable` | no previous comparable sample exists |
| `panes_contended=N` | N panes were skipped because a parser or inline-image lock was busy; the session total is partial |
| `allocator_state=none` | no renderer exists, so no allocator was queried |

On macOS, private/committed is `unsupported`; SonicTerm reports resident and
virtual memory but does not substitute an invented value for `phys_footprint`.
On Windows, private/committed is `PrivateUsage` and resident is
`WorkingSetSize`. Linux and other platforms without a process-memory sampler
report all three OS figures as `unsupported`; pane, renderer, and allocator
accounting still runs.

The allocator is sampled once per shared device/context, from the main renderer
or a deterministic visible/warm fallback. Every window, including one opened with
New Window, renders through that device, so the one reading covers them all. A
measured report includes:

```text
allocator_allocated_bytes
allocator_reserved_bytes
allocator_allocations
allocator_blocks
allocator_largest_block_bytes
```

Software adapters use wgpu 30 `MemoryHints::MemoryUsage`; hardware adapters use
`MemoryHints::Performance`. On D3D12, the software policy changes initial
allocator blocks from 128 MiB device / 64 MiB host to 8 MiB device / 4 MiB host.
Those are placement and block-sizing hints, not allocation caps; larger
resources still allocate.

### Covered-window trim

A window natively occluded for 30 s gives back what its renderer can rebuild.
The retention pass checks every window once per 30 s interval, so the trim lands
30–60 s after the cover, with no timer or wake of its own. It runs before
charging and the snapshot and outside every logging gate, so it happens at the
default log level. It skips a window already trimmed in this covered stretch, a
parked one, one whose device is stopped or refuses work, and a warm spare.
Backend-only occlusion never starts the 30 s count.

The trim releases, inside the device gate: the retained frame texture, down to
1×1 on the GPU presenter (the software presenter's is already 1×1); both present
buffers, back to their initial 4,096 quads, and the vertex scratch; the row
glyph, row quad and row ink caches; both atlas uploads' staging buffers and rect
lists; the held frame scratch (a lent one is dropped when its lease returns);
the chrome title and run tables, keeping the palette; and a promoted image atlas
when no media is visible. It keeps the glyph atlas and its GPU texture, a
pending atlas retry and its eviction state, the Windows software frame and the
preedit cache. A device that refuses the work changes nothing. The next frame
is a full first frame, and everything regrows on its normal path; returning to
view clears the trim mark.

Renderer parts are reports, not ledger entries, so the trim lowers
`renderer_total_bytes` in the same pass's snapshot and never a pane charge.
Each renderer entry also reports `trimmed` and `gpu_released_requested_bytes`:
the frame texture and present buffers the trim gave back, as request sizes, not
residency, and outside the total.

While a window is visible, the present buffers shrink too: at the end of each
600-draw window, if their capacity exceeds four times that window's peak use
and the initial 4,096 quads, both are recreated at twice the peak rounded up to
a power of two, never below 4,096 quads.

### Detailed retention and reclamation

Set `debug` for per-pane and per-renderer lines:

```toml
[logging]
level = "debug"
```

The 30 s pass uses `try_lock`; it never waits for a pane parser or inline-image
lock. Registration, reconciliation, charging, stalled-capture cancellation, and idle-media
reclamation run at every log level. Only emission is gated. A sampling-only
wake does not request a redraw.

Two reclamations remove user-visible content and therefore log on the
`memory::reclaimed` target even at the default `warn` level:

```sh
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log*
```

| Message | Meaning |
| --- | --- |
| `cancelled a media capture that stopped receiving` | no bytes arrived for two 30 s intervals; staging was released and the image will not appear |
| `discarded inline images from idle panes` | older images were removed from panes that held a share sized for fewer live panes |

A large single snapshot is not evidence of growth. Compare several consecutive
samples. Rising `grid_history_bytes` points to scrollback; rising
`inline_media_bytes` points to images; `parser_bytes` that remains high across
samples points to an in-flight transfer. A nonzero `panes_contended` means the
aggregate understates the session.

### Code locations

| Topic | Primary paths |
| --- | --- |
| Governor, ledger, reservations | `crates/sonicterm-resource/src/{ledger,owner,reservation}.rs` |
| Resource contracts and owner kinds | `crates/sonicterm-types/src/resource.rs` |
| Pane limits and owner registration | `crates/sonicterm-app/src/app/{mod,owners}.rs` |
| Pane measurement, charging, reclamation | `crates/sonicterm-app/src/app/retention.rs` |
| Aggregate snapshot | `crates/sonicterm-app/src/app/memory_snapshot.rs` |
| Inline-media limits | `crates/sonicterm-app/src/app/media.rs` |
| Grid and hyperlink limits | `crates/sonicterm-grid/src/{grid,hyperlink}.rs` |
| Parser capture limits | `crates/sonicterm-vt/src/vt.rs`, `crates/sonicterm-vt/src/vt/staging.rs` |
| PTY queue limits | `crates/sonicterm-io/src/pty.rs` |
| Renderer retention and allocator report | `crates/sonicterm-gpu/src/core.rs` |
