# Architecture Internals

[简体中文](Architecture-Internals-zh-CN)

These are the rules that tests must protect: accurate memory reports, complete
frames, safe native lifetimes, and verified release assets. Read
[Architecture](Architecture) first; resource limits are in [Memory](Memory).

### Heap-truth accounting

Integration tests compare retention reports with live heap using a counting
`#[global_allocator]` for allocation, deallocation, and reallocation. They check:

- the reported value does not materially understate live heap;
- the reported value does not materially overstate live heap;
- live heap itself stops within the enforced cap and stated tolerance.

These tests must stay in `tests/`. A `#[global_allocator]` applies to the whole
crate, so a sibling unit-test module cannot isolate it.

The allocator is process-global. Every test in one measurement binary holds a
file-local `Mutex` for its full measurement lifetime. The test builds fixture
strings and buffers before opening the measurement window. Otherwise sibling
work or test-harness allocations become part of the subject's result.

The heap-truth checks cover the grid, hyperlink registry, PTY queues, VT capture
staging, inline media, owner close, and long-lived atlas. Important enforced
limits include:

| Seam | Current limit |
| --- | --- |
| Grid storage | `MAX_GRID_CELLS = 1,048,576`; visible geometry is capped by `MAX_VISIBLE_GRID_CELLS = 524,288` |
| Hyperlink registry | `MAX_HYPERLINKS = 16,384`, `MAX_HYPERLINK_URI_BYTES = 8 KiB`, `MAX_HYPERLINK_CLIENT_ID_BYTES = 1 KiB`, `MAX_HYPERLINK_METADATA_BYTES = 8 MiB` |
| VT media payload | `MAX_MEDIA_PAYLOAD_BYTES = 16 MiB` |
| Process VT capture staging | `MAX_PROCESS_CAPTURE_STAGING_BYTES = 64 MiB`, with a `MIN_CAPTURE_STAGING_BYTES = 4 MiB` floor and `GUARANTEED_CONCURRENT_CAPTURES = 13` |
| PTY input | 4 queued messages; each message is at most 16 MiB |
| PTY output | 64 queued chunks plus one blocked sender chunk; each retained reader ring is 64 KiB; structural worst case is 65 rings, or 4.0625 MiB |
| Retained inline media | 128 images and 64 MiB per pane; 256 MiB process target plus at most one 4 MiB newest-image residual per live pane before reclamation converges; each rendered side is at most 1,024 pixels |

Capture staging and inline media each count against one pool. Production uses
`CaptureStagingPool::process_default()` and `InlineMediaPool::process_default()`,
so those limits hold per process. The capture-staging heap-truth test measures
the default staging pool against the real heap. The inline-media heap-truth
test checks the retained-media figure that pane charges are set from, not the
media pool's totals. Unit tests that need a capture admitted, or that measure
admission or budgets, inject private pools.

A grid report includes cell storage, rare attributes, combining text, row
containers, and reserved capacity. Scrollback is limited by configured rows and
retained bytes. The scroll path checks the byte budget in amortized batches.

The hyperlink registry counts its URI strings and both hash tables. `clear`
shrinks those tables. The registry may reclaim unreferenced entries when full;
it does not sweep the grid for every OSC 8 link.

The PTY output report counts ring allocations pinned by queued `Bytes` views.
It does not multiply the slot count by an assumed chunk size, and it does not
mistake payload length for retained allocation.

### Resource ledger invariants

The GUI creates this live owner tree:

```text
Process
  Window
    AppPane
```

Process and window owners use tracking-only limits. Each `AppPane` owner uses
`PANE_COMMITTED_BUDGET_BYTES`. That value is twice
`PANE_SEAM_CAP_SUM_BYTES`, which is calculated from the grid, inline-media,
hyperlink, parser-capture, PTY-output, and PTY-input seam caps.

The seam caps remain the primary enforcement points. The pane budget is a
backstop. Retention is measured before it is charged, so a failed charge does
not undo memory already retained. A failed growth keeps the previous charge. A
failed new charge leaves that class absent and writes a `memory` debug record.

A pane owns one `CommittedReservation` per charged `ResourceClass`. Retention
uses `try_resize` in place, including samples where bytes grow while items shrink
or vice versa. Final admission is `current total - old charge + new charge`, not
a component-wise maximum or release/re-reserve cycle. Any growing axis requires
open ancestors; reductions can settle while closing. State locks precede class
locks and ascending owner-usage locks; process-byte growth uses the existing CAS
as the final fallible step. Refusal leaves the token and all balances unchanged.
Snapshots remain observational rather than one linearizable global reading.

One-pane reconciliation uses `CommittedReservation::transfer_batch`; tab
attachment uses `transfer_many` for every moved pane's charges and individual
target owner in one same-ledger transaction. Both preserve classes and validate
source balances, target states, and final owner limits before changing any
shard. Immutable parent ids precede children, so both follow the same ordered
state/class/owner locks. Success changes owner-path balances and token owner ids
without changing process or per-class totals. Refusal preserves all source
tokens; provisional empty owners drop before source custody is restored.

Close order is load-bearing:

1. clear pane charges;
2. close each `AppPane` owner;
3. close the parent `Window` owner.

`PaneState` declares `charges` before `owner`. `WindowState` declares `panes`
before its window `owner`. Rust drops fields in declaration order, so the normal
drop path follows the same leaf-first rule. `finish_close` refuses an owner with
live charges or children. `OwnerGuard::drop` logs a warning and retains a refused
record; it does not retry.

A failed window-owner registration leaves the window usable but omits that
window and its panes from hierarchy accounting for the rest of the window's
life. A failed pane-owner registration leaves the pane usable; periodic
reconciliation can retry it. Renderer-owned surfaces, glyph atlases, row glyph
and quad caches, and software frames are measured outside this ledger. The row
cache classes are explicit `UnchargedRetention`: reports carry exact current
allocation while the coverage table records conservative per-renderer high-water
envelopes. No report invents GPU memory or presents an uncharged class as a
governor reservation.

`ReapShutdownHandle` separates shutdown control from `ReaperSupervisor::run_until`.
Closing admission wakes reservers and can only shorten the shared drain deadline;
it does not set cancellation or run tasks on the control thread. A running loop
uses the earlier shared deadline at cutoffs and clock waits. While deferred work
exists, clock waits are bounded by `HELPER_POLL_INTERVAL` so a deadline published
after a wait begins is observed without waking the deferred task early. An empty
loop returns without polling. A timed-out helper remains counted and its task
stays retained; a shutdown request does not itself prove settlement.

### Rendering correctness invariants

SonicTerm retains rendered pixels between frames. Damage therefore decides
correctness, not only speed.

- A primary-screen pane contributes the union of its dirty-row strips. The strips
  include pane padding and are clipped to the pane and surface.
- A dirty alternate-screen pane contributes its complete surface-clipped pane.
  A clean alternate-screen pane contributes no damage.
- Full-surface replacement clears the retained attachment once; partial damage
  uses a non-blending background reset under its scissor. Reset and content share
  one buffer upload and separate draw ranges, preserving content source-over and
  LCD blending while erasing prior ink without alpha accumulation.
- Projected background-cache validity includes the viewport row slot. Each
  `(pane id, absolute row)` owns one projection, replaced when its hash changes;
  dirty invalidation remains absolute and pane-local with the same capacity cap.
- Inline-image visibility intersects the original destination, pane content, and
  surface for both atlas residency and emission. Visible UVs preserve the source
  transform; separate original tile bounds clamp GPU and CPU bilinear taps. Images
  retain fractional pixel coordinates without changing text glyph alignment.
- Changes to terminal cells mark affected rows in the same frame. This includes
  scrolling, reverse index, line insertion/deletion, erase, resize, and
  wide-cell repair.
- Nonempty primary-history erasure advances the revision and exact eviction
  counter and marks all visible rows presentation-dirty, without changing their
  content stamps. Empty history and alternate-screen ED3 change neither screen.
  Every history-prefix removal drops prompts whose starts were removed and
  rebases surviving coordinates; saved-primary prompts stay with their rows.
- Cursor-position replies clamp the insertion sentinel to a physical column
  without consuming delayed wrap. Hard LF/VT/FF/IND/NEL advancement scrolls only
  at the effective bottom margin and otherwise clamps to physical bounds. Fill
  and carriage-return policies remain explicit per control.
- Each `Line` packs an incoming automatic-wrap bit into its existing content
  sequence word. Only an actual margin wrap sets it. Hard line advances,
  full-row erases, recycled rows, non-reflow resize, and uncertain region
  surgery clear affected boundaries. The bit travels into scrollback and enters
  row equality/hash identity, so an evicted predecessor remains detectable
  without increasing the row header.
- Recorded-wrap URI and local-target reconstruction joins at most eight visible
  rows and 4 KiB across those boundaries. URI resolution precedes
  local-target configuration gating and uses the complete candidate for preview,
  every highlight fragment, and a fresh activation-time lookup. No incomplete
  chain falls back to a row-local URI prefix. OSC 8 remains authoritative;
  file URIs retain filesystem authorization. The local-target authorization key binds every row
  fingerprint and wrap bit, ordered absolute spans, pointed cell, viewport,
  screen epoch, scrollback-eviction generation, and exact-pane OSC 7 state.
  Hard newlines, incomplete chains, unsafe cells, and any identity change fail
  closed before activation-time native revalidation.
- Balanced quoted paths retain delimiter cells in safety checks while excluding
  them from the active span. Grouped source locations yield exactly one candidate
  for the pointed member, with the entire anchor/group in the validated span;
  comma/space separators do not initiate activation. No shell expansion occurs.
  Scanner results carry separate display and source byte ranges. A scalar-to-cell
  map assigns one wide character to its lead/continuation pair without duplicate
  byte offsets. Valid wide filename and boundary scalars keep both cells under
  combining/hyperlink/pair-integrity checks. List-member and leading explicit-path
  prose alternatives carry missing-literal dependencies through candidate caps, path
  resolution, authorization, and native activation; a dropped or unresolvable literal
  cannot authorize a shorter fragment.
- HTTP(S) extraction also recognizes explicit `()`/`[]` wrappers across at most
  eight visible hard rows and 4 KiB. The first fragment must contain the complete
  authority and a path slash; non-final fragments reach the margin and subsequent
  rows share at most eight ASCII spaces of indentation. A matching closer and
  exact whole-URI scanner match are required. This tri-state scan precedes logical
  URI fallback: incomplete owned fragments are inert, unrelated text is untouched,
  and complete destinations use the same preview, span, and fresh activation lookup.
  Unsafe cells, internal whitespace/wrappers, multiple schemes, and mixed wrap kinds
  cannot authorize a join. OSC 8 stays first; filesystem targets never use this path.
- Changes to overlays or window chrome promote damage to the full surface.
  One hovered target carries up to eight ordered viewport fragments in the
  frame key. Hover-only changes on the accelerated path damage the old and new
  pane rows, including glyph ink padding, rather than the whole window. Preview
  and other chrome changes retain full-surface damage. Active recoloring salts
  only each intersecting row cache key; underline geometry emits one clipped
  quad per fragment. A busy event-time lookup preserves the existing hint but
  drops modifier-only feedback when the modifier is released, and requests a
  source-window redraw. Main and child frames resolve hover from their held parser
  snapshots before presentation.
  Each window retains at most one current-epoch filesystem-probe completion and
  requests a frame to validate it against a fresh target, rather than discarding
  it during parser contention. Clicks always require a fresh lookup, never
  authorization from retained visuals or an unvalidated completion.
  Effective per-pane scrollbar opacity is window chrome: its quantized
  pane identity participates in the frame key, and a bucket change damages the full
  surface.
- A degraded wgpu frame with work repaints the full surface. Windows degraded
  presentation also composes a full CPU surface. Degraded scrollbars snap and
  arm one idle-hide deadline; accelerated scrollbars request bounded fade frames.
- `FramePlan` reports `RenderMode::Noop` for an unchanged key on either presenter,
  or a changed degraded key with no visible work. It does not rebuild or clear
  dirty rows; an unchanged Windows CPU frame may still be reblitted.
- Windows software glyph presentation stabilizes NDC roundoff at integer and
  half-pixel origins before one-to-one raster placement. The row glyph cache also
  keys the viewport row slot because cached instances carry screen coordinates.
- Every authored quad color is finite premultiplied linear RGBA. Opacity and
  coverage changes scale RGB and alpha together; debug builds validate both quad
  layers immediately before the GPU/software presenter split. Windows software
  quads decode retained sRGB destination channels, blend the same source in
  linear light, encode RGB once, and source-over alpha as linear UNORM.
- Windows LCD text is eligible only for an opaque configured backdrop, effective
  opacity `1`, and either the CPU/GDI presenter or a wgpu device with
  `DUAL_SOURCE_BLENDING`. The effective `off`/`rgb`/`bgr` mode enters the frame
  key. GPU and CPU presenters apply the foreground transform before coverage,
  attenuate destination RGB independently in linear light, and use maximum
  channel weight for alpha. Images and color glyphs have higher branch priority.
- A display-scale transition commits one physical inner size through winit's
  event-scoped writer. On macOS, the observed native size already uses AppKit's
  current backing scale; convert from that scale, not the stored previous event
  scale. Other platforms retain the stored-scale input contract. The target
  preserves logical geometry and the 30×10 terminal minimum; Windows additionally
  caps it to the destination monitor work area. Renderer surface, pane grids/PTYs,
  IME geometry, and redraw follow the same target before the native size commit.

The event-loop thread collects a complete frame without waiting on the VT
worker. It uses `try_lock` for every active-tab parser and for required
inline-image stores. If any lock is unavailable, it drops all collected guards,
records a pending redraw, and does not call `GpuRenderer::render`.

One `FramePlan` composes the key, mode, damage, clips, and viewport slots from
captured metadata. Copy-mode identity covers every field and quick-select hint
without cloning its owned text. The plan retains visible-pane and dirty-row
metadata, never hidden history or cell rows. Existing parser guards remain held
through stateful assembly and presentation; no PTY write can interleave on those
grids during that borrow.

Dirty rows clear only in `finish_successful_frame`, and only when the pane id and
current grid revision exactly match that plan's captured expectation:

- on Windows CPU presentation, after `SetDIBitsToDevice` returns success;
- on wgpu presentation, after command submission and `queue.present(frame)` are
  invoked.

`SetDIBitsToDevice` can report failure. wgpu's present call has no result that
reports a later presentation failure. Surface timeout, occlusion, outdated,
suboptimal, and lost results invalidate the frame key and request another
redraw. Outdated and suboptimal surfaces are reconfigured. A lost surface is
recreated. Validation errors propagate. None of these acquisition failures
clears dirty rows.

Grid geometry accounts for retained row allocations, not only visible
`cols × rows`. A material column shrink compacts rows. Adjacent resize changes
keep reusable capacity to avoid repeated allocation. Reducing the scrollback
limit releases excess `VecDeque` capacity. Column shrink checks only each new
right edge for a clipped `WIDE` lead and replaces it with the resize fill;
complete pairs and compact storage survive. The same `Line` operation covers
visible, history, and saved-primary rows without reflow or later resurrection.

Clipboard serialization keeps isolated or incomplete right-edge box drawing.
It removes only a coherent multi-row side that ends in a lower-right frame
corner. On Windows, a successful OSC 52 write gets one delayed reassertion only
when the clipboard has reverted to the exact text observed before that write;
a newer or unreadable clipboard owner is never overwritten.

Nested applications under rmux/tmux emit OSC 52 through `DCS tmux` passthrough.
Trusted sessions require both options; `set-clipboard` alone does not relay the
wrapper:

```tmux
set -s set-clipboard on
set -g allow-passthrough on
```

Both options trust pane output. Arbitrary DCS passthrough remains outside
SonicTerm's OSC 52 parser; the multiplexer must validate and unwrap it.

CAN and SUB cancel an active escape sequence. The parser resets escape
accounting before a cancelled DCS or APC media sequence can emit a partial
image. A host-cancelled stalled capture discards the remaining payload until its
terminating boundary instead of printing it into the grid.

### Atlas and font invariants

The CPU glyph atlas is fixed at 2,048 × 2,048 BGRA8 pixels, about 16 MiB. Its
metadata holds at most `MAX_ATLAS_ENTRIES = 16,384` entries, including blank and
missing sentinels.

On a miss, the atlas uses reclaimed rectangles before its shelf packer. Under
metadata or packing pressure, it deterministically evicts the coldest quarter.
An eviction changes the atlas epoch. If that happens during frame assembly, the
renderer discards the frame, resets the atlas in place, invalidates UV-bearing
row caches, and requests a new frame. The retry disables eviction until one
frame presents successfully. The fixed pixel allocation does not grow.

`RowGlyphCache` and `LineQuadCache` use keys based on pane id, absolute row, and
row hash. Their capacities are about four times the sum of visible rows across
all panes. A capacity or geometry-size change clears the affected cache. Dirty
rows invalidate their absolute-row entries. Font, theme, scale, surface resize,
and atlas replacement invalidate the corresponding caches. Retention counts the
hash table's allocated key/entry buckets and every nested vector's capacity.
Ordinary clearing leaves table capacity reusable, so bounded churn forms a
high-water envelope rather than a flat byte line. When a pane leaves a renderer,
one event-loop-owned operation removes its glyph-cache entries first and its
quad-cache entries second, preserves peer hits, then asks each table to shrink.
No concurrent retention snapshot can observe only half that ordered eviction.

The inline-image atlas starts as a 1 × 1 CPU/GPU placeholder. It promotes to a
2,048 × 2,048 atlas when renderable media appears. After 240 rendered frames
without inline media, it returns to the placeholder. Text and image atlases are
separate so image pressure cannot evict text glyphs.

On Windows degraded presentation, the full CPU atlases remain available while
GPU atlas textures become 1 × 1 placeholders. Returning to wgpu presentation
recreates matching textures, resets atlas state, invalidates UV-bearing caches,
and forces a full redraw before sampling the new textures.

Weight adjustment follows face selection for every monochrome style and fallback.
At a fixed size/DPI it changes coverage only: cell pitch, baseline, bitmap bounds,
bearings, and advances stay fixed. Color artwork is excluded. Windows disables
DirectWrite grid fitting and routes color-capable faces through FreeType.

DirectWrite subpixel tiles remain native linear BGRA coverage in both the CPU
atlas and the GPU unorm coverage view; no hidden contrast curve precedes the
explicit `weight_scale` control. They must never pass through the color-rectangle
conversion or the sRGB color view. Alpha remains the maximum RGB coverage so
ineligible and `off` presentation has a deterministic grayscale value. Changing
LCD mode is presentation-only and must not rebuild or reinterpret either atlas.

Font discovery, shaping, and rasterization stay separate from renderer policy.
Generated FFI bindings remain in their wrapper crates. Malformed, missing, or
out-of-range variable-font metadata falls back to base OS/2 weight and width.
FreeType embedded bitmap strikes are checked against the 2,048-pixel and 16 MiB
glyph allocation limits before pixel decoding. BGRA crop bounds are half-open:
all nontransparent ink survives, and removing `(crop_x, crop_y)` changes bearings
to `bitmap_left + crop_x` and `bitmap_top - crop_y`. Fully transparent nonempty
rasters retain their dimensions, metrics, and valid blank atlas representation.

Crash history intersects the selected filter with a DEBUG ceiling and an
explicit payload-exclusion predicate, including original log-facade targets.
Owned variable retention is bounded by 50 records, 4 KiB per record including a
256-byte target limit, and 64 KiB in aggregate. Formatting stops within those
bounds; panic payload and summary each have a separate 4 KiB limit. Fixed record
metadata is count-bounded. Backtraces and allocations inside arbitrary producer
formatters are outside these guarantees; this is not generic secret sanitization.

The hidden warm-window pool defaults to one. Zero disables it. Normal hardware
accepts at most five. An actual software adapter or resolved degradation caps
any nonzero target at one. A live config reload clears the pool; later
`about_to_wait` passes rebuild it one entry at a time.

### PTY and native-thread invariants

Terminal input enqueue is non-blocking. `PtyHandle::send_input_nonblocking`
uses `try_send` on a four-message channel. A message over 16 MiB, a full queue,
or a disconnected writer returns `PtyInputError` with the original bytes. The
app drops the payload and posts metadata-only `UserEvent::PtyInputRejected`,
logs the pane, current window, typed source, and concurrent queue/writer
observations, and notifies that pane's window if it still exists. It does not
replay refused discrete bytes automatically. Before admission, pointer motion
coalesces in a fixed 64-byte pane slot; queue saturation retains its latest
position for a non-rendering retry. Following discrete input combines the pending
position into one admission when it fits; otherwise it supersedes that position.
Changed mouse profiles invalidate the pending slot; unavailable profiles defer
motion-only retries, while discrete input supersedes unvalidated motion. Queue occupancy excludes the active native
write/flush, whose phase, size, elapsed time, and progress are observed separately.

PTY resize is fallible and its cache is success-only. The callback holds the
native call and the last applied `(cols, rows)` behind one lock, so native
resizes are serialized and the cache records the last successful native call. A
zero axis is refused as an `InvalidInput` error before the native call and
before the cache changes; a request equal to the last *applied* size is skipped;
only a successful native call caches a size. A failed request is therefore not
deduplicated away — the next identical request reaches the native call again —
and the last successful size stays cached. The first request always reaches the
native call, because the cache starts empty rather than seeded from the spawn
dimensions.

The host first calls `Parser::resize`: grid bounds apply, and an effective row or
column change clears parser-owned scrolling margins without homing the cursor.
Duplicate-size relayouts preserve partial margins. The grid is never rolled back
when the native call fails: the pane keeps the requested geometry and only the
child's view of it lags.
`PaneState::resize_pty` reports the failure once per failing run — first failure
logged with pane id, requested columns and rows, and error, then silence until a
success clears the latch. The latch gates the log line only. Warning suppression
never suppresses a resize attempt; an invalid size and a successful duplicate
are decided at the IO boundary, not by the latch.

The PTY reader uses a reusable 64 KiB `BytesMut` allocation. It sends
`PtyOutputChunk` views through a 64-slot channel. A full channel blocks the
reader and lets the operating system apply back-pressure; output is not dropped.

A pane VT worker holds only that pane's parser lock while advancing VT state and
snapshotting parser-owned modes. It moves the owned VT events out of that guard,
then handles clipboard and command events, decodes or resizes inline media, and
updates retained stores after unlocking. Main-born and child-born panes use the
same host-event processor. Tear-out changes the shared redraw `WindowId`, so the
worker follows the pane without retaining `Arc<Window>`.

`PtyHandle::drop` always starts with cancellation, synchronous-I/O cancellation
where supported, and child termination. The remaining order differs by platform.

On Unix, `waitid(P_PID, ..., WEXITED | WNOHANG | WNOWAIT)` observes natural
exit without releasing the session id. Teardown kills the original process group
and repeatedly kills active members of the same session. It closes the master
before waiting for I/O threads. Reader and writer each get 500 ms. Termination
retry and child reap each use a separate 500 ms deadline. If session cleanup
cannot be proved, the leader remains unreaped so its id cannot be reused
unsafely.

On Windows, each synchronous-I/O cancel runs on its own short-lived
`sonic-pty-cancel` thread that owns a duplicate of the I/O thread's handle.
Teardown waits at most 500 ms for those cancels, then continues without the ones
still running. It then waits up to 500 ms for the reader and another 500 ms for
the writer before master close. `sonic-conpty-drain` drains a cloned reader while
`sonic-conpty-close` closes the master. Close gets 2 seconds. If close succeeds,
drain gets another 2 seconds. Timeouts detach the helpers. Helper-start or close
failure returns an incomplete-close result and warns. Child exit/reap has a
separate 500 ms bound.

These deadlines keep `Drop` from blocking the UI indefinitely. They do not turn
an incomplete native close into success.

Teardown adds no synthetic input: destroying the writer contributes nothing to
the child's input stream. Ordinary terminal input and parser-generated replies
are unchanged, since replies are legitimate input the terminal produces. That is
an invariant of writer *construction*, not of teardown ordering: a writer's
destructor is part of the child's input stream, so `pty_writer` decides it once
per platform rather than at the spawn site. Unix writes through an
`F_DUPFD_CLOEXEC` duplicate of the master descriptor, whose close is silent. The
duplicate shares the master's open file description, so file-status flags such
as `O_NONBLOCK` remain shared, while `FD_CLOEXEC` is a per-descriptor flag set
on the duplicate alone; its lifetime is independent, so it keeps delivering
after the master is dropped. A Unix master exposing no descriptor is an
`Unsupported` error; there is no fallback to `portable-pty`'s Unix writer, which
writes a newline and `VEOF` when dropped whenever the line discipline reports
one. Windows keeps that crate's writer, whose ConPTY destructor writes nothing.

The verification boundary is the child side of a real PTY. `pty_tests.rs` opens
a pair, puts the line discipline in raw mode with an explicit nonzero `VEOF`,
and asserts that precondition through the *master* — the descriptor the upstream
destructor reads — because a zero `VEOF` would suppress the synthetic write and
make every later assertion vacuous. Each of the four ways the writer thread can
end — cancellation, input-channel disconnect, a failed native write, a failed
native flush — consumes its typed bytes first, requires the thread to have
exited on its own before joining, and then requires the child-side stream to be
empty. Proving exit matters because the shutdown path detaches on timeout, which
would leave the writer undropped and an empty result meaningless. Write and
flush failures are injected by a test-only wrapper that owns the real production
writer, so the production destructor still runs; the child side is never closed,
so an empty result means nothing was sent rather than nothing could be read.
Separate tests assert the duplicate is close-on-exec and outlives its master,
that an explicit Ctrl+D still ends a canonical shell whose readiness was proven
by a marker it had to execute to emit, and that dropping a handle reaps both the
shell leader and a background descendant of its session within a bounded drop.
The no-fallback contract has its own test: a master double exposing no
descriptor must make `pty_writer` return an `Unsupported` error, with the
double's `take_writer` never called. A live PTY always exposes a descriptor, so
nothing else reaches that branch, and a silent fallback would restore the defect
with every other test still green. These are Unix-gated: they compile to nothing
on Windows.

### Release verification boundary

Root `Cargo.toml` `[workspace.package]` is the version source. The release
workflow starts for tags matching `v[0-9]+.[0-9]+.[0-9]+*`. It first peels the
tag ref to its commit, then requires that commit to be in `origin/main` history
and an exact completed successful `CI` push run to exist for it. It continues
only when
`prepare-release-assets.py check-version` parses the tag as a semantic version,
finds that version on every workspace package, and the normal source-consistency
gates pass.

The workflow builds five required package tuples:

| Platform | Architecture | Package |
| --- | --- | --- |
| macOS | `aarch64` | `.dmg` |
| macOS | `x86_64` | `.dmg` |
| Windows | `x86_64` | `.msi` |
| Linux | `x86_64` | `.deb` |
| Linux | `x86_64` | `.tar.gz` |

Each package has a typed JSON fragment. Consolidation requires all five tuples,
rejects duplicate names or tuples, recalculates hashes, and rejects unregistered
`.dmg`, `.msi`, `.deb`, and `.tar.gz` files in `dist`. It emits
`release-assets.json`, deterministic `SHA256SUMS.txt`, and
`release-upload-paths.txt`. The release action uploads only the paths in that
list.

The Windows CI test shard runs:

```bash
cargo test -p sonicterm-gpu --test windows_warp_allocator_baseline -- --nocapture
```

The gate requires WARP and allocator reporting. Production reserved bytes must
be below 64 MiB. The largest block must be below 128 MiB. The
`MemoryHints::MemoryUsage` candidate must reserve fewer bytes than the
`MemoryHints::Performance` control under the same allocations. Release requires
an exact successful `main` CI run for the tag commit before `build-windows`
starts, so a failed gate blocks the MSI and publication without being rerun at
tag time.

Linux package verification builds both `.deb` and `.tar.gz` layouts. The runtime
smoke runs them on X11/Xvfb and Wayland/Weston with Vulkan/lavapipe. Like the
macOS and Windows binary smokes, it requires native window and renderer/device
creation, a platform-shell PTY marker observed in the live grid, a later native
presentation, and default warm-renderer create/report/adopt/release with the
process renderer count restored.

macOS packaging verifies binary architecture and the app's ad-hoc signature.
Each just-built macOS architecture and the just-built Windows executable must
pass the shared native runtime smoke before packaging can advance. For each
macOS architecture, package verification mounts the DMG, copies its app into an
installed layout, and runs that executable's runtime smoke with Homebrew access
denied. The workflow does not perform Developer ID signing, notarization, MSI
signing, or MSI install-run; those remain outside the verified release boundary.

Release validation requires an exact completed successful `main` CI run for the
tag commit, then validates every workspace package version and the release-asset
tooling. It does not repeat the source, unit, integration, documentation,
platform-runtime, allocator, coverage, or normal-CI package gates. The platform
jobs start directly from that provenance boundary and perform only native
release builds, native binary smokes, package validation, MSI metadata validation,
and installed-DMG and Linux package smokes. Release Rust target builds do not read or write Rust caches; the Windows
job may restore the vcpkg binary cache published immediately by normal CI.

### Source and check map

| Contract | Primary source or check |
| --- | --- |
| Heap-truth tests | `crates/sonicterm-{grid,io,vt,app,resource,text}/tests/` |
| Resource inventory and baseline | `scripts/test-resource-inventory.sh`, `scripts/test-resource-baseline-evidence.sh` |
| Damage and present completion | `crates/sonicterm-gpu/src/core.rs` |
| Glyph atlas and row caches | `crates/sonicterm-text/src/{glyph_atlas,row_glyph_cache}.rs`, `crates/sonicterm-gpu/src/row_quad_cache.rs` |
| PTY teardown | `crates/sonicterm-io/src/pty.rs` |
| Owner and charge ordering | `crates/sonicterm-app/src/app/{mod,retention}.rs` |
| Release asset contract | `scripts/prepare-release-assets.py`, `scripts/test-release-assets.sh` |
| Release job graph | `.github/workflows/release.yml` |
