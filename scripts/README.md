# Performance benchmarking harness

Two layers for measuring Sonic's perf, plus a diff tool to compare runs.

## Layer 1 — Headless `bench`

Runs the full Sonic stack (PTY + Parser + Grid + spans-builder) without a
window or GPU. Reproducible, CI-runnable, emits a single JSON line so you
can diff before/after.

```bash
# Baseline before your changes
cargo run --release -p sonic-core --example bench -- all > before.json

# Make your performance changes, then:
cargo run --release -p sonic-core --example bench -- all > after.json

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
cargo build --release -p sonic-mac          # produce target/release/sonic-mac
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
cargo build --release -p sonic-mac
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
