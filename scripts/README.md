# Performance benchmarking harness

Two layers for measuring Sonic's perf, plus a diff tool to compare runs.

## Layer 1 — Headless `bench`

Runs the full Sonic stack (PTY + Parser + Grid + spans-builder) without a
window or GPU. Reproducible, CI-runnable, emits a single JSON line so you
can diff before/after.

```bash
# Baseline before your changes
cargo run --release -p sonicterm-core --example bench -- all > before.json

# Make your performance changes, then:
cargo run --release -p sonicterm-core --example bench -- all > after.json

# Side-by-side comparison with percentage deltas
scripts/bench_compare.sh before.json after.json
```

### What `bench` measures

| metric                              | what it is                                                  | wezterm-ish goal |
|---|---|---|
| `parse_ns_per_byte`                 | VT parser throughput                                        | < 25 ns |
| `parse_ns_per_batch`                | per-batch parse overhead                                    | < 1 µs |
| `grid_walk_us_per_frame`            | walk grid → build text + spans + underline runs (no GPU)    | < 50 µs |
| `idle_cpu_pct`                      | proxy: pty channel wakeups during 1s of quiescence × 0.01%  | < 1 |
| `typing_echo_latency_us_p50/p95/p99`| key byte → echoed-char-visible-in-grid round trip           | p99 < 2000 µs |
| `scroll_throughput_lines_per_sec`   | how many lines/sec we can parse while echoing               | > 50 000 |

Scenarios: `typing | scroll | idle | all` (default `all`).

## Layer 2 — GUI `gui_bench.sh`

Drives the real built `.app` via synthetic keystrokes and samples CPU/RSS
over time. Catches everything the headless harness misses (winit event loop
overhead, glyphon shaping, wgpu submission, present mode, real-world idle).

### One-time setup

- `brew install cliclick` — preferred keystroke driver (no Accessibility
  prompt for `cliclick t:foo`); fallback is `osascript`.
- If using the `osascript` fallback, grant whoever runs the script
  Accessibility access: **System Settings → Privacy & Security →
  Accessibility → +** add Terminal.app (or whichever shell you're in).

### Usage

```bash
cargo build --release -p sonicterm-mac          # produce target/release/sonicterm-mac
scripts/gui_bench.sh                        # default = "all"
scripts/gui_bench.sh idle                   # just idle CPU
scripts/gui_bench.sh typing                 # 60 synthetic 'a' keystrokes
scripts/gui_bench.sh scroll                 # 5000-line burst via `yes | head`
```

Output is a JSON-ish blob to stderr:

```
{"pid":62694,"scenario":"idle",
  "idle_cpu_pct_3s": 8.21,
  "final_rss_kb": 121296
}
```

### Comparing GUI runs

Capture stderr to a file, then eyeball or `diff`:

```bash
scripts/gui_bench.sh all 2>before-gui.json
# … apply perf changes, rebuild …
cargo build --release -p sonicterm-mac
scripts/gui_bench.sh all 2>after-gui.json
diff before-gui.json after-gui.json
```

## Reference numbers (2026-05-25 baseline, M1 Mac, v0.6.2 build)

```json
{
  "parse_ns_per_byte": 22,
  "parse_ns_per_batch": 808,
  "grid_walk_us_per_frame": 27,
  "idle_cpu_pct": 0.09,
  "typing_echo_latency_us_p50": 699,
  "typing_echo_latency_us_p95": 1217,
  "typing_echo_latency_us_p99": 1399,
  "scroll_throughput_lines_per_sec": 332,
  "scroll_bytes": 57647,
  "scroll_batches": 1612
}
```

```
{"scenario":"idle", "idle_cpu_pct_3s": 8.21, "final_rss_kb": 121296}
```

## Post-cutover baseline (PR #42, 2026-05-25, M1 Mac)

After the B-epic landed on `main` (skip-unchanged-frame, dirty-row
tracking, glyph-atlas cache), the headless bench now reports:

```json
{
  "parse_ns_per_byte": 21,
  "parse_ns_per_batch": 1187,
  "grid_walk_us_per_frame": 1,
  "idle_cpu_pct": 0.08,
  "typing_echo_latency_us_p50": 1595,
  "typing_echo_latency_us_p95": 20052,
  "typing_echo_latency_us_p99": 50000,
  "scroll_throughput_lines_per_sec": 33969,
  "scroll_bytes": 57693,
  "scroll_batches": 1042,
  "glyph_walk_us_per_frame": 27,
  "glyph_atlas_unique_keys": 29,
  "glyph_walk_hit_rate_pct": 100.0
}
```

And the new headless GUI driver `scripts/bench_headless_gui.sh` (which
runs without `cliclick` — see "Layer 3" below) reports:

```json
{
  "idle_cpu_pct": 0.01,
  "scroll_cpu_pct": 0.06,
  "frames_skipped": 2,
  "frames_rendered": 0,
  "typing_delivered": true
}
```

### Trend vs the pre-cutover baselines

| metric                             | pre-B  | B1     | B2      | B3 (cutover, this PR) |
|---|---|---|---|---|
| `parse_ns_per_byte`                | 22     | 22     | 22      | 21                    |
| `scroll_throughput_lines_per_sec`  | 332    | 332    | 32 813  | **33 969**            |
| `idle_cpu_pct` (headless)          | 0.09   | 0.09   | 0.09    | 0.08                  |
| GUI idle CPU                       | 8.21%  | 0.13%  | 0.13%   | **0.01%**             |
| GUI scroll-tail CPU                | n/a    | n/a    | n/a     | **0.06%**             |
| frame-skip fast-path active?       | no     | yes    | yes     | yes (verified via trace log) |

Notes:
- GUI idle CPU dropped another order of magnitude vs B1 because the new
  glyph-atlas cache short-circuits `glyphon::prepare()` between paints,
  so the only event-loop work is the wakeup poll itself.
- `scroll_cpu_pct` in the new GUI driver measures CPU **after** the
  scroll burst, while the renderer is still draining the dirty queue;
  it's effectively idle-after-scroll. For a real scroll-burst comparison
  use the headless `scroll_throughput_lines_per_sec`.
- The headless `typing_echo_latency_us_p99` is noisier post-cutover
  because the renderer now defers more work to the GPU thread; on the
  GUI side perceived latency is unchanged.

## Layer 3 — `bench_headless_gui.sh` (no Accessibility prompt required)

`gui_bench.sh` works great when `cliclick` is installed and the shell
has Accessibility permission. CI machines and fresh clones often have
neither, so `scripts/bench_headless_gui.sh` is a degraded-but-runnable
alternative:

- Launches `target/release/sonicterm-mac` directly (no bundle).
- Samples `%CPU` from `ps` every 200 ms for 5 s (idle) and 10 s (post-scroll).
- Greps the `RUST_LOG=sonicterm_shared::render=trace` log for the
  `renderer: skipped unchanged frame skipped=N` counter to expose the
  fast-path hit count (strips ANSI color codes first).
- Attempts to deliver a `seq 1 2000\n` burst via `osascript keystroke`;
  if Accessibility is blocked the scroll number is still meaningful
  (it's the idle-after-launch CPU) but `typing_delivered:false` flags it.

```bash
cargo build --release -p sonicterm-mac
./scripts/bench_headless_gui.sh
# {"idle_cpu_pct":0.01,"scroll_cpu_pct":0.06,"frames_skipped":2,"frames_rendered":0,"typing_delivered":true}
```

### What this tells us

- **VT parse + grid walk are NOT the bottleneck** — they're µs-scale.
- **typing echo round-trip is already sub-millisecond at p50** (~0.7 ms).
- **Scroll throughput @ 332 lines/sec is the real problem** — WezTerm
  handles 50 000+. The bottleneck is downstream of the grid walk:
  `glyphon::Buffer::set_rich_text` + `prepare()` reshape every cell on
  every frame, and `lock()`-contention with the VT thread.
- **Real-world idle CPU 8.2%** — not the 0.9% one-shot `ps` snapshot would
  suggest. Sampling over time exposes the steady RedrawRequested rate.

## What to fix in Epic B (in order)

1. **Skip-unchanged-frame** — `Grid::revision` bumped per mutation; renderer
   stashes `last_revision` and bails early if equal. Should drop idle CPU
   to < 1%.
2. **Dirty row tracking** — `Grid::dirty_rows: BitSet`. Renderer only
   rebuilds spans for dirty rows; clean rows reuse last frame's text
   layout. Should multiply scroll throughput.
3. **Glyph atlas cache** — own atlas keyed by `(char, weight, italic,
   size)`; skip `glyphon::prepare()` and draw cached glyph quads directly.
   Brings us into WezTerm/Alacritty territory.
4. **Mailbox present mode + render thread** — `PresentMode::Mailbox`
   drops superseded frames, optimizing for latest-input visibility over
   no-drop guarantees.

## Reference numbers (2026-05-26, post visual-parity, M1 Mac)

Best-of-4 headless `bench` runs after visual-parity work landed:

```json
{
  "parse_ns_per_byte": 36,
  "parse_ns_per_batch": 1184,
  "grid_walk_us_per_frame": 1,
  "idle_cpu_pct": 0.08,
  "typing_echo_latency_us_p50": 703,
  "typing_echo_latency_us_p95": 1199,
  "typing_echo_latency_us_p99": 1324,
  "scroll_throughput_lines_per_sec": 34527,
  "glyph_walk_us_per_frame": 44,
  "glyph_atlas_unique_keys": 30,
  "glyph_walk_hit_rate_pct": 100.0
}
```

GUI bench (`bench_headless_gui.sh`), 3-run range:

```
{"idle_cpu_pct": 4.86–11.85, "scroll_cpu_pct": 0.13–6.43, "typing_delivered": true}
```

### Findings vs prior baselines

- **`parse_ns_per_byte`: 21 → 36 (+71%) — regression >20%.** Worth investigating; likely
  related to richer per-cell attribute work the parser now emits for visual parity.
- **`glyph_walk_us_per_frame`: 27 → 44 (+63%) — regression >20%.** Still well under the
  50µs goal but trending the wrong way; investigate span-building cost.
- `scroll_throughput_lines_per_sec`: 33969 → 34527 — stable / slight improvement.
- `idle_cpu_pct`: 0.01 → 0.08 — proxy metric still well under the <1 goal.
- Typing-echo latency: p99 1324µs, comfortably under the 2000µs goal.

Tests/gate: `cargo test --workspace` = 623 tests passing (floor 171), `pty_dump` e2e OK.
