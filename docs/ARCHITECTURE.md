# SonicTerm Architecture

Developer documentation: **Architecture** · [Modules](MODULES.md) · [Logging](LOGGING.md) · [Packaging](packaging/README.md)

SonicTerm is a native macOS + Windows terminal built around small Rust crates
with a strict data-flow boundary:

```text
platform shell -> sonicterm-app -> sonicterm-render-model -> sonicterm-gpu
                        |                    ^                    ^
                        v                    |                    |
                  sonicterm-io -> sonicterm-vt -> sonicterm-grid |
                                             \-> sonicterm-ui ----/

font-config/fontconfig/freetype/harfbuzz -> sonicterm-font
                                          -> sonicterm-engine/text
                                          -> sonicterm-gpu

sonicterm-resource ..... cross-cutting: owner tree and retained-memory ledger
```

This diagram shows runtime data flow and the primary dependency seams. In the
Cargo graph, `sonicterm-gpu` depends on `sonicterm-render-model`, while
`render-model` depends on and re-exports grid/config/UI types through its
`boundary` module; arrows do not imply the reverse Cargo dependency.
`sonicterm-resource` is drawn apart because it carries no frame data: it is the
accounting seam that `sonicterm-app` charges retained memory to, described
under [Resource governance](#resource-governance). `sonicterm-gpu` does not
depend on it and does not charge — see
[Renderer retention is measured, not charged](#renderer-retention-is-measured-not-charged).

## Core flow

1. `sonicterm-mac` / `sonicterm-windows` load config, logging, assets, and the
   platform event loop.
2. `sonicterm-app` owns the authoritative live windows, tabs, pane trees,
   PTYs/parsers, command palette state, selection, search, drag/drop, and redraw
   scheduling. `sonicterm-app-core` supplies the backend-free intent/effect
   reducer mirror; complete live topology has not migrated into it.
3. `sonicterm-io` transports child bytes; `sonicterm-vt` parses them into
   `sonicterm-grid` mutations and events.
4. `sonicterm-render-model` carries renderer-agnostic pane/frame inputs and is
   the declared boundary through which the GPU sees grid/config/UI types.
5. `sonicterm-gpu` builds quads and glyph instances for wgpu presentation. When
   no usable GPU is present it falls back to a CPU rasterizer; on Windows the
   software path (`crates/sonicterm-gpu/src/software_windows.rs` +
   `crates/sonicterm-windows/src/software_presenter.rs`) repaints the whole
   surface deterministically each frame. Glyphs rasterize via DirectWrite by
   default on Windows
   (`crates/sonicterm-font/src/rasterizer/directwrite.rs`), FreeType elsewhere
   and as the Windows fallback.
6. `sonicterm-resource` holds the process-local owner tree and retained-memory
   ledger. `sonicterm-app` creates a `Window` owner per live window and an
   `AppPane` owner per pane. Charging what each pane retains runs on the
   idle-wake path at every log level; memory-target debug diagnostics govern
   only the retention log lines.

## Design rules

- The renderer never blocks on PTY locks during the event loop hot path.
- `sonicterm-gpu` reaches terminal-grid, config/theme, and UI-state types only
  through `sonicterm_render_model::boundary::{grid, cfg, ui}` — it does not depend
  on `sonicterm-grid`/`sonicterm-cfg`/`sonicterm-ui` directly. `render-model` is
  the single declared seam for the `vt/grid -> gpu` and `ui -> gpu` boundaries.
- Platform crates stay thin; cross-platform behavior belongs in `sonicterm-app`
  or lower crates.
- Public contracts live in `sonicterm-types`; changes there affect every crate.
- User-facing settings live in `sonicterm-cfg` and are applied on explicit
  reload; there is no config file watcher.
- Memory bounds are enforced at the subsystem seam that owns the memory, not by
  the resource governor. Reservations stay coarse — never per cell, per parser
  byte, or per rendered glyph — and the governor adds no process-global
  hot-path mutex.
- WezTerm-proven terminal/font behavior is absorbed into Sonic-owned crates; do
  not add new dependencies on a `vendor/` tree.

## Resource governance

`sonicterm-resource` owns a process-local **resource governor**: the accounting
and attribution layer for retained memory. It answers "how much is held, by
which owner, in which class" — it is not the layer that bounds allocation.

### Enforcement belongs to the seams

Per-seam caps enforce. The governor accounts, attributes, and backstops. The GUI
process deliberately constructs its governor with `process_bytes: usize::MAX`
and unlimited per-class byte ceilings, and window owners are created with
tracking-only limits.

That is a design decision, not an omission. Two limits that must agree and are
maintained separately will drift, and the one that stops agreeing keeps
reporting itself as enforced. Each seam — grid cells, retained inline media,
interned hyperlink metadata, parser capture staging, escape sequences in flight,
command events — already bounds itself and is tested at its own boundary.

The one governor limit that does bind is the `AppPane` owner's committed budget,
`PANE_COMMITTED_BUDGET_BYTES`. It is **a tripwire, not a second enforcement
point**: it is computed as the sum of the per-seam caps times a headroom
multiplier, so it cannot disagree with the seam caps — it is derived from them —
and sits far enough above correct operation that it never fires there. What it
catches is the failure the per-seam caps structurally cannot: a seam that has
stopped bounding while still reporting itself as bounded.

### Sharded ledger

One `Ledger` behind an `Arc`, shared by every clone of the governor handle.
Contention is split rather than serialized on a process-global hot-path mutex:

- The owner registry is sharded 16 ways, each shard an `RwLock<HashMap>` keyed
  by owner ID.
- Per-class usage is an `EnumMap<ResourceClass, Mutex<ClassUsage>>` — one lock
  per class, so unrelated classes never contend.
- The aggregate process byte total is a single atomic updated by a validating
  compare-exchange loop. An items-only reservation skips that atomic entirely.

Ordering is fixed to keep the lock graph acyclic: the class lock is taken before
owner usage locks on the reserve path, and owner records are sorted by ID before
locking on the close and transfer paths.

Usage accounting deliberately skips the process root record. Root figures are
derived from the class shards instead, so an owner's charge is not counted twice.
A snapshot sums both axes from the class shards it just observed rather than
mixing in the process atomic, so the snapshot agrees with itself; a reader
cannot mistake sampling skew for a real imbalance.

### Owner hierarchy

Owners form a tree with one immutable process root, and IDs are allocated
monotonically and never reused. Which parent may hold which child is fixed per
process kind and rejected at creation time rather than left to convention.

The contract permits, for a GUI process:

```text
Process -> Window -> AppPane -> LocalPty
```

A GUI process root also admits `SharedFont`, `SharedRaster`, `SharedAtlas`, and
the `MuxConnection` it opened as a client; a mux process root instead admits
`MuxSession -> MuxPane -> PtyTransport`.

What production actually instantiates today is the first two levels:

```text
Process -> Window -> AppPane
```

`LocalPty`, the `Shared*` owners, and every `Mux*` owner are permitted by the
ledger but never registered — no code path creates them. They are reserved
capacity in the contract, not live topology.

Each owner is `Open`, then `Closing`, then `Closed`. `Closing` stops admitting
new reservations and new children while still letting live tokens finalize
during teardown, and closing is refused while an owner still has live children
or nonzero charges.

Registration failure is not retried uniformly, and the difference matters:

- A **pane** that fails to register keeps working and is picked up by the next
  reconcile pass, which registers any unowned pane under an already-registered
  window. Reconcile runs whenever a pane is created — new tab or split — and
  again on every retention pass, at every log level.
- A **window** that fails to register is never retried. Reconcile skips a window
  that has no owner, so neither it nor any of its panes appears in hierarchy
  accounting for the rest of that window's life.

In both cases the window or pane keeps working; a diagnostic gap is preferred
over a lost window.

### Reservation tokens

Two RAII tokens, both of which release their charge on drop:

- `Reservation` — taken *before* retaining or allocating. `commit(actual)`
  settles it at an amount no greater than the reservation and yields a
  `CommittedReservation`.
- `CommittedReservation` — the retained charge for a live allocation.

Both support `split` to carve off an independent charge and `transfer` to
atomically move attribution to another owner and class; `CommittedReservation`
additionally supports `shrink` and `try_grow` to resize a charge in place.
Resizing in place is what the pane retention pass uses: releasing and
re-reserving on every sample would pass the charge through zero, leaving the
ledger briefly disagreeing with reality and letting a concurrent reservation
take the budget in that window.

#### Charging runs at every log level; the gate governs only the log lines

Owner registration, charging, and the retention log lines are three separate
things, and the distinction matters when reading a snapshot:

- **Owners always exist.** A `Window` owner is registered when the window is
  created, and an `AppPane` owner when the pane is created — new tab, split, or
  window registration all reconcile unconditionally. This happens at every log
  level. Window registration is structural rather than conventional: inserting a
  window and registering its owner are one operation, and the insertion is not
  reachable without it. A window that registered no owner would not merely be
  uncharged — reconciliation and re-attribution both skip a window whose owner
  is absent, so its panes would never be adopted and the whole subtree would
  stay missing from the hierarchy for as long as the window lived.
- **Charging always runs.** The pass that samples what a pane retains and moves
  its charges to match runs on the idle-wake path at every log level. At the
  default level the owner hierarchy is fully populated and its figures report
  what each pane actually holds.
- **Only the log lines are gated.** The `enabled!(target: "memory", DEBUG)`
  check in `sample_pane_retention` guards the `pane retention` and `session
  retention` lines, which are additionally rate-limited to one sample every 30
  seconds. Charging is not rate-limited by that interval.

Charging sits above the gate because it is not a diagnostic: it is what puts a
pane's retention into the ledger that `PANE_COMMITTED_BUDGET_BYTES` is enforced
against. Below the gate, a shipped session — which installs no `memory`
subscriber — charged nothing, so the per-pane backstop had no figure to apply
itself to and the tripwire could not fire in any shipped build. Two reclamation
passes sit above the gate for the same reason — freeing memory a user is owed
does not depend on whether anyone is watching: stalled captures are cancelled,
and panes still holding an inline-media budget sized for an earlier, smaller
session are revisited. Both recover memory the owning seam cannot recover for
itself, because a seam has no clock and no view above the pane — an idle pane
re-evaluates its own budget only while decoding, so nothing on its behalf would
otherwise look again.

An integration test installs **no** subscriber — the shipped default — enters
through the real gated path, and asserts both that a pane's retention reaches
the ledger and that the charged figure agrees with what the pane measures, in
`crates/sonicterm-app/tests/governor_charges_without_subscriber.rs`.

The walk skips rather than blocks: a pane whose parser lock is contended is
passed over and keeps its previous charge until the next pass, so charging never
stalls the event loop behind a parsing VT thread.

The seam caps bound memory at every log level, as they always did; what changed
is that the governor's own figures are now populated in shipped sessions rather
than reading zero.

Every failure preserves the original ownership and accounting — `CommitError`,
`TransferError`, and `CommittedTransferError` each hand the untouched original
token back to the caller. A release that cannot be applied is counted rather
than asserted, because panicking inside `Drop` would turn a recoverable
accounting fault into an abort during unwind; the count is surfaced through
snapshots so the violation stays observable in builds with debug assertions
compiled out.

### Class coverage

`ResourceClass` is deliberately complete rather than minimal — a class exists for
each owner kind's documented payload even where no subsystem charges it yet. A
class with no charge site is otherwise indistinguishable from a class someone
forgot, so `ResourceClass::coverage()` records which it is, as an exhaustive
match that fails to compile until a new variant is given a decision:

| `ClassCoverage` | Meaning |
| --- | --- |
| `Charged` | A production site reserves and charges this class. |
| `MeasuredNegligible { per_pane_bytes }` | Real retention, measured, and small enough that charging would cost more than it reports. The measurement is recorded so "small" is a finding rather than an assumption. |
| `TransientWithinCall` | Allocated and released within one call, so a charge would be taken and returned before any sampler could observe it. |
| `FeatureGated` | Compiled out of shipped builds by a feature gate. |
| `SubsystemAbsent` | The subsystem that would own it does not exist yet; charging it would be charging nothing. |
| `UnchargedRetention { per_owner_bytes }` | Real retention that nothing charges, recorded with what it holds. Distinct from `SubsystemAbsent` (charging would charge nothing) and `TransientWithinCall` (gone before a sampler could see it): the memory is present, significant, and outside the ledger. |

The classes with live charge sites are the grid trio, parser capture, protocol
metadata, retained inline media, and the local PTY output and input queues, all
charged from the pane retention pass. `RegistryMetadata` is `SubsystemAbsent` by
construction rather than by schedule: owner records are the ledger's own storage,
and charging them to a ledger class would make the ledger account for itself, a
recursion with no fixed point.

#### Renderer retention is measured, not charged

`GlyphAtlas` and `SoftwareFrame` record `UnchargedRetention`, carrying what they
hold rather than a charge site they do not have. The gap is structural rather
than pending: `sonicterm-gpu` declares no dependency on `sonicterm-resource`, so
`GpuRenderer::retained_amounts()` reports its figures but has no governor to
reserve against — and nothing calls it. Closing the gap requires a new
dependency edge, not merely a call site.

`UploadStaging` is recorded the same way, for a different reason. Its staging
buffer is a renderer field that is cleared between copies and never shrunk, so
it holds the largest dirty rect it has ever staged. That reuse is deliberate —
reallocating per frame on the upload path would cost more than the memory does,
and a test asserts the capacity survives the call — so the row records what the
reuse costs rather than proposing to remove it.

The consequence for anyone reading a snapshot: atlas, software-frame, and
staging memory is real, is measured, and is absent from the ledger. A process
total taken from the governor is a total of what the app charges, not of what
the process holds.

### Memory invariants this release guarantees

These hold for the classes with live charge sites, listed above:

- Every seam that retains memory is bounded by its own tested cap, and the
  reported figure for that seam tracks real heap. This is the invariant that
  bounds memory, and it holds at every log level.
- Retained memory that is charged is attributable to an owner and a class, and
  the owner tree reflects live window/pane topology — an owner closes when its
  window or pane drops.
- Charges follow their resource across pane migration: a pane torn out into a
  new window transfers attribution rather than leaking it to the process root.
- Accounting cannot silently go negative or double-count; a release that would
  underflow is refused and counted rather than applied.
- The per-pane backstop cannot disagree with the seam caps, because it is
  derived from them.
- Classes that are not charged are not merely uncharged — each carries a
  recorded reason.

One scope limit is deliberate and is *not* a defect: renderer retention is
outside the ledger entirely, as described above. Charging itself is not scoped —
it runs at every log level, so these figures report live retention rather than
zero in a shipped session.

## Accounting verification

Accounting claims are verified against **real heap**, not against the number the
figure was derived from. Every accounting defect found in this milestone shared
one shape: a reported figure measured against its own derivation, each with a
test that passed. A grid figure under-reported by 1.67x, a queued-output figure
restated a constant, and the hyperlink registry under-reported by 4.8x.

The ground truth is a counting `#[global_allocator]` that tracks live bytes
across allocate, deallocate, and reallocate, in
`crates/sonicterm-grid/tests/grid_heap_truth.rs` and
`crates/sonicterm-grid/tests/hyperlink_heap_truth.rs`. These assert both
directions — that a reported figure does not understate real heap (an undercount
admits past the cap) and does not wildly overstate it (an overcount refuses work
while memory is available) — and that real heap, not merely the reported figure,
stops below the cap. With tables uncounted the hyperlink registry stopped at
8,388,244 bytes against a cap of 8,388,608 — compliant on its own number — while
actually holding roughly 12.1 MB.

Two constraints are structural rather than stylistic:

- **`#[global_allocator]` is crate-wide**, so these must live in `tests/` as
  integration tests; they cannot be flat sibling unit tests.
- **The counting allocator is process-global**, so every test in such a file must
  serialize on a file-local `Mutex`. Two tests measuring concurrently attribute
  each other's allocations to whichever one is reading. Measured: all pass
  serially and all fail in parallel, reporting a 5.80x "undercount" that was
  entirely sibling noise. The lock is used rather than `--test-threads=1`
  because the gate cannot be told to serialize one file, and a suite that only
  works under a flag is a suite that will eventually run without it.

Allocation-measuring tests must also build their fixture data *before* the
measurement window; a `format!` per iteration inside the window attributes the
harness's own garbage to the subject under test.

## Rendering and redraw invariants

SonicTerm retains rendered pixels between frames, so damage calculation is part
of terminal correctness rather than a paint optimization:

- A dirty alternate-screen pane repaints its complete surface-clipped pane.
  Primary-screen panes retain narrow dirty-row damage.
- VT/grid mutations mark affected rows in the same frame, including scrolling,
  insert/delete line, reverse index, erase, resize, and wide-cell repair.
- Grid geometry budgets include retained row allocation, not only visible
  `cols × rows`; material column shrink compacts surviving rows while adjacent
  resize oscillation retains reusable capacity, and history-limit reductions
  release excess `VecDeque` capacity. Grid-level aggregate checks include
  visible, history, and saved-primary row capacity and force-compaction when
  retained storage would exceed the corresponding cell budget.
- Clipboard serialization preserves isolated or incomplete right-edge
  box-drawing text and removes only a coherent multi-row side ending in a
  lower-right frame corner. Frame detection reads physical row ends without
  widening the selected output span.
- CAN/SUB cancellation resets VT escape accounting before cancelled DCS media
  can reach `unhook` and emit an incomplete image.
- Windows software rendering keeps the established full-surface presenter path;
  it is not coupled to retained GPU damage decisions.
- Pane VT workers never call native window APIs. After output coalescing they
  copy the pane's `WindowId` under a short mutex guard and send
  `UserEvent::RequestRedraw`; the winit event-loop thread resolves the live
  window and calls `request_redraw()` after the guard has been released.
- Tear-out and tab transfer update the pane's redraw target by `WindowId`, so a
  worker survives migration without retaining an `Arc<Window>` or calling
  AppKit/Win32 from the worker thread.

## Font and native boundaries

Font discovery, shaping, and rasterization remain split from renderer policy.
Generated FreeType/HarfBuzz/Fontconfig bindings stay in their wrapper crates;
`sonicterm-font` owns safe allocation and fallback behavior. Variable-font
metadata is optional: malformed, missing, or out-of-range variation metadata
falls back to base OS/2/default weight and width rather than aborting the app.
Embedded bitmap strikes are loaded metrics-only and checked against the glyph
allocation budget before FreeType may decode their pixels.
Glyph/image atlas textures initialize lazily through dirty-tile uploads.
Same-dimension atlas resets clear metadata and packing state in place without
zeroing or replacing the retained CPU pixel allocation; cached UV generations
are invalidated before newly inserted tiles overwrite any sampled rectangles.
The inline-image atlas starts as a 1×1 CPU/GPU placeholder and promotes to its
bounded full size only when a renderable image first appears. Atlas uploads
coalesce compatible dirty rectangles and reuse staging storage across frames.
On Windows, deterministic software presentation keeps the full CPU glyph atlas
but replaces GPU atlas textures with 1×1 placeholders. Returning to GPU
presentation recreates matching textures, resets atlas metadata and UV-bearing
caches, and forces a full redraw before the new textures can be sampled.

The hidden warm-renderer pool defaults to one on every adapter. A configured
value of zero disables it; hardware honors values up to five, while software
rendering caps every nonzero target at one.

PTY handles own their native reader and writer threads. Unix natural exit is
observed with `waitid(..., WNOWAIT)`; teardown repeatedly terminates every
process in the unreaped leader's session before reaping, so session identity
cannot be reused first. Windows teardown caches process exit and keeps a
dedicated cloned output reader draining concurrently with ConPTY master close,
including the pre-Windows 11 24H2 blocking contract. Both platforms use
bounded thread, close, and child-exit deadlines.
Terminal-input enqueue remains non-blocking and bounded; saturation,
disconnection, and oversized messages return typed errors that retain the
rejected bytes instead of reporting false success. App callers forward those
bytes to the event loop for a visible retry notification. The mux probes child
exit on an independent timer, applies an absolute post-exit output-drain
deadline, removes exited panes, prunes empty sessions, rechunks subscriber
output to 8 KiB frames, bounds all control replies, actively interrupts blocked
transport writes before joining, and queues `Spawned` before enabling output,
`Exit`, or reap.

Native GPU presentation, real PTYs/SSH, AppKit/Win32 handles, generated C ABI
behavior, and installer signing are verified by build, integration, platform CI,
and release smoke checks rather than hollow unit tests.

## Release and verification boundary

The workspace version in root `Cargo.toml` is authoritative for all first-party
crates and internal requirements. Releases are created only by pushing an
owner-approved `v*` tag. The tag workflow builds the expected macOS DMG(s),
Windows MSI, generated release notes, and checksum manifest. Maintained local
packaging instructions live in `docs/packaging/`.

The local release gate is:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p sonicterm-io --features ssh --all-targets -- -D warnings
cargo metadata --no-deps --format-version 1
cargo test --workspace --lib --bins
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-rust-version.sh
bash scripts/check-window-owner-registration.sh
bash scripts/check-workspace-crates.sh
scripts/rust-logic-coverage.sh
bash scripts/test-release-notes.sh
bash scripts/pty-backend-feasibility.sh --check
bash scripts/test-resource-inventory.sh
bash scripts/test-soak-harness.sh
bash scripts/test-resource-baseline-evidence.sh
cargo build --release -p sonicterm-mac
```

`ssh` is an optional feature and the default feature set is empty, so
`--workspace --all-targets` never compiles it — `--all-targets` is not
`--all-features`. Its own clippy line is what keeps the backend building
against its dependencies.

`check-rust-version.sh` recomputes the effective minimum from every locked
dependency and fails when the declared `rust-version` falls below it. CI
installs floating `stable` and so cannot notice that drift on its own.

The deterministic Rust coverage threshold is 80%. Note that
`cargo test --workspace --lib --bins` deliberately excludes integration tests;
the cross-crate suites under each crate's `tests/`, including the counting-
allocator heap-truth suites, run under `scripts/rust-logic-coverage.sh`, which
invokes `cargo llvm-cov --workspace --lib --bins --tests`. Native exceptions use
the substitute checks above; exclusions must not hide difficult deterministic
code.
Before merge, macOS and Windows PR checks must pass. Release sign-off also
includes a macOS launch with Vim/nvim alternate-screen exercise and a busy
multi-pane torn-out-window close check that confirms responsive surviving
windows and reaped pane processes.

## Assets

Runtime assets live under `assets/` and are packaged beside the binaries:

- `assets/themes/*.toml`
- `assets/keymaps/*.toml`
- `assets/fonts/*`
- `assets/icons/*`
- `assets/i18n/*`

macOS also exposes bundled fonts through `Contents/Resources/Fonts` and
`ATSApplicationFontsPath` so AppKit/CoreText can resolve `Rec Mono St.Helens`.
Windows MSI installs the same `assets/fonts/RecMonoSt.Helens-*.ttf` files next
to the executable.

The default theme is `wezterm`, a modified Gruvbox dark hard palette with
SonicTerm's near-black background. The default keymap is platform-specific and
WezTerm-compatible.
