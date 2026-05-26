# TESTING.md — Sonic testing methodology

This document is the single source of truth for **how Sonic is tested before
code lands on `main`**. It consolidates the rules scattered across
`CLAUDE.md` §2, §11, §12, §13 and `scripts/README.md` into one operational
checklist. If you are an agent or a human contributor opening a PR, the
gates here are mandatory unless explicitly waived by the human PM.

The intent is the same as `CLAUDE.md`: **any contributor dropped into this
repo can be productive in 5 minutes.** Read this once before opening your
first PR.

---

## 1. The local gate — run before every commit

The headless gate is the floor. It catches roughly 70 % of regressions and
takes < 2 min on an M1. Run it from the repo root:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --example pty_dump          -p sonic-core   --release  # must print [e2e] OK
cargo run --example pty_dump_unicode  -p sonic-core   --release  # CJK + emoji e2e
cargo build --release -p sonic-mac                               # fat-LTO release
```

Notes:

- `cargo-deny check` runs in CI on Ubuntu. Run it locally if you touched
  any dep: `cargo install cargo-deny --locked && cargo deny check`.
- CI matrix is `macos-14` + `windows-latest`. Linux is not a supported
  target before v1.0.
- **Test floor:** the workspace test count must never regress. The current
  floor is **445**. Confirm with:

  ```bash
  cargo test --workspace 2>&1 | grep "test result" \
    | awk -F'[ .,]' '{s+=$5} END {print "TOTAL:", s}'
  ```

  If the number drops, you have either deleted a test or skipped a binary
  — both require an explanation in the PR body.

- `pty_dump` is the canonical end-to-end gate for VT / grid / PTY: it
  spawns the user shell, runs `ls --color=always /` plus a bold / italic /
  underline `printf`, and exits non-zero if the resulting grid lacks the
  expected colored or styled cells.
- `pty_dump_unicode` is the analogous gate for the **renderer** path
  (added with the capability matrix in §2). It writes CJK + emoji into the
  grid so a "primary-family-only, no-fallback" rasterizer cannot ship
  silently.

---

## 2. Capability matrix

The capability matrix is the **anti-tofu rule** that was added after
PR #42 cut over to a swash-rasterized atlas, passed every existing gate,
and still shipped a renderer that drew every non-ASCII glyph as a tofu
box. The cause: every prior test, example, and benchmark used pure ASCII,
so the rasterizer's no-fallback code path was never exercised on a CJK
glyph in CI.

Run both halves:

```bash
cargo test -p sonic-core   --test vt_capability_matrix
cargo test -p sonic-shared --test render_capability_matrix
```

What each class verifies:

| Test class            | Crate          | Asserts                                                                                                |
|---|---|---|
| `vt_capability_matrix`| `sonic-core`   | Parser/grid round-trips for ASCII echo, SGR colors + bold/italic/underline, ED/EL erase modes, alt-screen 1049h re-entry, wide chars + combining marks, OSC 8 hyperlinks. |
| `render_capability_matrix` | `sonic-shared` | Renderer surface produces non-empty glyph quads for ASCII, CJK (`中文`), emoji (`🎉`), box-drawing, Nerd-Font private-use glyphs, and that the atlas falls back to a secondary font family rather than emitting tofu. |

**Mandatory whenever you touch any of:**

- `sonic-shared/src/render.rs`
- `sonic-shared/src/swash_rasterizer.rs`
- `sonic-shared/src/glyph_atlas.rs`
- `sonic-shared/src/text_pipeline.rs`
- anything matching `sonic-shared/src/*atlas*` or `*pipeline*`

**Rules about the matrix itself:**

- Do not delete or weaken a class.
- If a class is intentionally dropped from scope, mark the test
  `#[ignore]` with a comment naming the deciding PR. **Never** `#[cfg(skip)]`
  or silent deletion.
- An `#[ignore]` documents an open capability gap (e.g. waiting on
  `fix/atlas-font-fallback`). Removing the `#[ignore]` attribute in the
  fix's PR is the canonical green light that the gap is closed.

---

## 3. GUI smoke test (mandatory for render / input / VT / window changes)

The headless gate is necessary but not sufficient. Several real bugs
shipped past it: blank window (PR #36), CJK tofu (PR #42), 100 % idle CPU
(PR #31), sRGB gamma washing out theme colors, low-DPI blur on Retina.
None of these surface in `cargo test` — they require a real wgpu surface,
a real macOS window, and real glyph uploads.

**Mandatory whenever you touch any of:**

- `sonic-shared/src/render*.rs`, `swash_rasterizer.rs`, `glyph_atlas.rs`,
  `text_pipeline.rs`, `app.rs`, `quad.rs`, `tabbar_view.rs`
- `sonic-core/src/vt.rs`, `sonic-core/src/grid.rs`
- any theme or keymap asset under `assets/`

### Recipe

```bash
pkill -9 -f sonic-mac 2>/dev/null; sleep 0.3
./target/release/sonic-mac > /tmp/gui-smoke.log 2>&1 &
sleep 2.5
PID=$(pgrep -f sonic-mac | head -1)

# 1. Bring to front and position deterministically so the screencap
#    actually captures Sonic, not whatever was previously frontmost.
osascript <<EOF
tell application "System Events"
  tell process "sonic-mac"
    set frontmost to true
    set position of window 1 to {500, 200}
    set size of window 1 to {1000, 700}
  end tell
end tell
EOF
sleep 0.5

# 2. Inject a payload that exercises ASCII echo, CJK rasterization,
#    emoji color rendering, and Enter/RET handling.
osascript -e 'tell application "System Events" to keystroke "echo 中文 🎉 sonic && date"'
sleep 0.3
osascript -e 'tell application "System Events" to key code 36'   # Return
sleep 1

# 3. Screencap full main display.
screencapture -x -D 1 /tmp/gui-smoke.png

# 4. Inspect /tmp/gui-smoke.png against the checklist below.

kill -9 $PID 2>/dev/null
```

### Inspection checklist (open the screenshot — do not trust silence)

- [ ] Window background pixel value matches `theme.colors.background`
      (no sRGB / linear double-gamma).
- [ ] `中` and `文` render as glyphs — not `?`, not tofu boxes.
- [ ] `🎉` renders **in color**, not as a monochrome silhouette.
- [ ] Cursor is visible at the prompt.
- [ ] Text is sharp on Retina — no HiDPI upscale blur.
- [ ] CPU sits **< 5 %** during the 5 s window. Sample with
      `ps -p $PID -o %cpu` mid-run (a single snapshot is not enough —
      see the idle-CPU history in `scripts/README.md`).

If **any** check fails, the PR is not ready. The PR body must include
the screenshot path and a one-line observation per check item.

**Background agents** running without a display MUST flag that fact
explicitly in their reply. The PM then runs the smoke locally before
merging.

---

## 4. Disk hygiene

Every multi-agent PR cycle creates a `/tmp/<scratch>` clone (~1.8 GB
each: full repo + `target/`). With ~10 PRs in flight in parallel, this
trivially fills a 460 GB SSD to 99 % — at which point `df`, `echo`, and
the harness itself begin failing with ENOSPC and silently corrupt
in-flight agents.

### Per-agent rule

The **final** step of every dispatch prompt MUST be:

```bash
cd / && rm -rf /tmp/<scratch>
```

No exceptions. List it as an explicit numbered step in the prompt and
require the agent to confirm it ran in the reply.

### PM sweep

After every merge or "task complete" notification, sweep stragglers:

```bash
du -sh /tmp/* 2>/dev/null | sort -h | tail
# anything >100 MB that isn't currently in-flight: rm -rf
```

The acceptable in-flight footprint is one scratch directory per active
agent. Once all agents return, `/tmp` should be back to ~0 B of sonic
clones.

### Local `target/`

The repo's `target/` runs ~5 GB (debug + release + deps + incremental).
Reclaim it periodically:

```bash
cargo clean
```

For comparison, the shipped `.dmg` is ~22 MB. The 5 GB is purely
build-time intermediates.

---

## 5. Benchmarking

There are three layers, all documented in `scripts/README.md`. Use them
in order; each adds signal the previous layer misses.

### Layer 1 — headless `bench` (CI-runnable, reproducible)

Runs the full Sonic stack (PTY + Parser + Grid + spans builder) without
a window or GPU. Emits a single JSON line for diffing.

```bash
# Baseline before your changes
cargo run --release -p sonic-core --example bench -- all > before.json

# Apply your changes, then:
cargo run --release -p sonic-core --example bench -- all > after.json

# Side-by-side comparison with percentage deltas
scripts/bench_compare.sh before.json after.json
```

Measured metrics (and current targets vs WezTerm):

| metric                              | what it is                                          | goal       |
|---|---|---|
| `parse_ns_per_byte`                 | VT parser throughput                                | < 25 ns    |
| `parse_ns_per_batch`                | per-batch parse overhead                            | < 1 µs     |
| `grid_walk_us_per_frame`            | walk grid → build text + spans + underline runs     | < 50 µs    |
| `idle_cpu_pct`                      | pty wakeups during 1 s of quiescence × 0.01 %       | < 1        |
| `typing_echo_latency_us_{p50,p95,p99}` | keystroke → echoed char visible in grid          | p99 < 2 ms |
| `scroll_throughput_lines_per_sec`   | lines/sec parseable while echoing                   | > 50 000   |

Scenarios: `typing | scroll | idle | all` (default `all`).

### Layer 2 — `scripts/gui_bench.sh` (real app, real CPU)

Drives the built `.app` via synthetic keystrokes (`cliclick` preferred,
`osascript` fallback). Catches everything the headless harness misses:
winit event loop overhead, glyphon shaping, wgpu submission, present
mode, real-world idle.

```bash
cargo build --release -p sonic-mac
scripts/gui_bench.sh            # default = "all"
scripts/gui_bench.sh idle       # idle CPU only
scripts/gui_bench.sh typing     # 60 synthetic 'a' keystrokes
scripts/gui_bench.sh scroll     # 5000-line burst via `yes | head`
```

Requires either `brew install cliclick` (no Accessibility prompt) or
Accessibility permission granted to your shell.

### Layer 3 — `scripts/bench_headless_gui.sh` (no Accessibility prompt)

Degraded-but-runnable alternative for CI machines and fresh clones.
Launches `target/release/sonic-mac` directly, samples `%CPU` via `ps`
every 200 ms, and greps the trace log for the `skipped unchanged frame`
counter.

```bash
cargo build --release -p sonic-mac
./scripts/bench_headless_gui.sh
# {"idle_cpu_pct":0.01,"scroll_cpu_pct":0.06,"frames_skipped":2,"frames_rendered":0,"typing_delivered":true}
```

### Comparing against baselines

`scripts/README.md` carries two baselines:

- **Pre-cutover** (2026-05-25, M1 Mac, v0.6.2): the numbers you must
  not regress.
- **Post-cutover** (PR #42, 2026-05-25, M1 Mac): the current state of
  `main` after the B-epic (skip-unchanged-frame + dirty-row tracking
  + glyph-atlas cache).

A new perf PR must include before/after JSON for **both** Layer 1 and
Layer 2 (or Layer 3 if Accessibility is unavailable). Significant
regressions on any metric must be called out and justified in the PR
body.

---

## 6. Multi-agent PR workflow

The full agent-driven PR pipeline is described in `CLAUDE.md` §6. The
testing-relevant summary:

1. **PM** picks one well-scoped task (one milestone item) and dispatches
   an **Opus implementer** in a fresh `/tmp/<scratch>` clone with a
   prompt that includes the **complete local gate** (§1) plus, when
   applicable, the **capability matrix** (§2) and **GUI smoke** (§3).
2. The implementer opens a PR and replies with the URL.
3. **PM dispatches a Haiku reviewer.** Haiku pulls a fresh clone, runs
   the gate, reads the diff, and posts the verdict as a PR comment:
   `APPROVED` or `CHANGES REQUESTED`.
4. PM acts on the verdict:
   - `APPROVED` → `gh pr merge <N> -R D0n9X1n/sonic --squash --admin --delete-branch`.
   - `CHANGES REQUESTED` → re-dispatch Opus with the feedback (or fix
     directly for small bugs), then re-dispatch Haiku.
5. Up to **3 fix cycles** per PR before the PM escalates.

CI status is **not a merge blocker** for admin-merged PRs — the local
gate is. CI failures still get a follow-up PR so the badge stays green.

Parallelism rules (test-relevant):

- Two PRs that both touch `render.rs` or `app.rs` WILL conflict —
  serialize them.
- Docs PRs (this one included) are independent of code and always
  parallelizable.

Disk hygiene per §4 is mandatory in every dispatch prompt.

---

## 7. Visual regression checklist

When inspecting a `gui-smoke.png` (or any post-change screenshot), walk
this list explicitly. Reviewers will compare your shot against the
previous baseline.

- [ ] **Background pixel matches theme.** Sample with Digital Color
      Meter or `screencapture` + a pixel picker; the value must equal
      the configured `theme.colors.background` hex. A mismatch usually
      means double-gamma (sRGB applied twice) or a wgpu surface format
      regression.
- [ ] **CJK renders.** `中文` shows as glyphs, not `?` and not tofu
      boxes. Failure means the atlas fallback chain is broken.
- [ ] **Emoji renders in color.** `🎉` is full-color, not a monochrome
      silhouette. Failure means the COLR/sbix table is being stripped or
      the rasterizer is ignoring color glyph data.
- [ ] **No tofu anywhere on screen.** Including box-drawing characters
      and Nerd Font private-use glyphs (powerline arrows etc.).
- [ ] **No blur on Retina.** Text edges are crisp at the pixel level.
      A blur usually means scale factor or DPI metadata is wrong and the
      framebuffer is being upscaled.
- [ ] **Cursor visible.** At the prompt, in the correct cell, in the
      configured cursor color/shape.
- [ ] **Idle CPU < 5 %.** Sampled with `ps -p $PID -o %cpu` over the 5 s
      smoke window — **not** a single snapshot. A single `ps` call can
      under-report idle CPU by 10×; see the 8.21 % vs 0.9 % discrepancy
      noted in `scripts/README.md`.

If any item fails, the PR is blocked.

---

## 8. Comparison testing vs WezTerm

When you want a side-by-side diff (most common during perf or rendering
work), the deterministic recipe is:

```bash
# 0. Build both at known versions.
cargo build --release -p sonic-mac
# Assume /Applications/WezTerm.app exists.

# 1. Launch each at known coordinates so screencaps are directly diffable.
pkill -9 -f sonic-mac 2>/dev/null
open -a /Applications/WezTerm.app
sleep 1.5
./target/release/sonic-mac &
sleep 2.5

osascript <<EOF
tell application "System Events"
  tell process "sonic-mac"
    set frontmost to true
    set position of window 1 to {100, 200}
    set size of window 1 to {900, 600}
  end tell
end tell

tell application "System Events"
  tell process "WezTerm"
    set position of window 1 to {1050, 200}
    set size of window 1 to {900, 600}
  end tell
end tell
EOF
sleep 0.5

# 2. Send the same payload to whichever is frontmost; repeat after
#    activating the other.
for app in "sonic-mac" "WezTerm"; do
  osascript -e "tell application \"System Events\" to tell process \"$app\" to set frontmost to true"
  sleep 0.3
  osascript -e 'tell application "System Events" to keystroke "echo 中文 🎉 && ls --color /"'
  osascript -e 'tell application "System Events" to key code 36'
  sleep 1
done

# 3. Screencap and diff.
screencapture -x -D 1 /tmp/sonic-vs-wezterm.png

# Optional: crop and diff with ImageMagick.
# magick /tmp/sonic-vs-wezterm.png -crop 900x600+100+200 /tmp/sonic.png
# magick /tmp/sonic-vs-wezterm.png -crop 900x600+1050+200 /tmp/wez.png
# magick compare -metric AE /tmp/sonic.png /tmp/wez.png /tmp/diff.png
```

What to compare:

- Glyph shapes — should be identical for shared fonts (JetBrainsMono
  Nerd Font is bundled in both).
- Color reproduction — `ls --color` ANSI 8/16 should match exactly.
- CJK + emoji parity (no tofu in either).
- Idle CPU after the payload settles (`ps` snapshot for both PIDs).

For perf comparisons, prefer the Layer 1/2/3 benches in §5 over visual
diffs — they are reproducible and quantitative.

---

## Appendix — quick reference

```bash
# Full local gate
cargo fmt --all -- --check && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo test --workspace && \
cargo run --example pty_dump         -p sonic-core --release && \
cargo run --example pty_dump_unicode -p sonic-core --release && \
cargo build --release -p sonic-mac

# Capability matrix
cargo test -p sonic-core   --test vt_capability_matrix
cargo test -p sonic-shared --test render_capability_matrix

# Workspace test count (must be ≥ 445)
cargo test --workspace 2>&1 | grep "test result" \
  | awk -F'[ .,]' '{s+=$5} END {print "TOTAL:", s}'

# Disk sweep
du -sh /tmp/* 2>/dev/null | sort -h | tail

# Reclaim ~5 GB
cargo clean
```
