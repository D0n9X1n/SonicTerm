# docs/agents/_common.md

**The ONLY agent-shared doc.** Every dispatched agent loads this file plus
the root `CLAUDE.md` and the per-crate `CLAUDE.md` for the crates being
touched. Keep it ≤ 100 lines.

---

## §4 land-mines (machine-checked mirror lives in `landmines.toml`)

Each rule below has a matching entry in `landmines.toml`. Editing one
without editing the other will fail `tools/check-landmines.sh`.

### Threading / event loop
- **LM-001** `render path uses try_lock not lock`. AB-BA deadlock on the
  macOS main thread under shell-startup bursts. Files:
  `crates/sonicterm-app/src/app/window_event.rs`,
  `crates/sonicterm-app/src/app/{child_window,misc}.rs`.
- **LM-002** **PTY-thread redraw coalescer = 3 ms min + 128 KB byte flush.**
  Never per-byte redraw. Lives in `crates/sonicterm-app/src/app/spawn_pane.rs`.
- **LM-003** **PTY burst flag is a generation counter, not a bool.**
  Bool version raced when renderer cleared between bursts. See PR #162.
- **LM-004** **No unconditional heartbeat redraw at end of `window_event`** —
  it forms a feedback loop. Real triggers cover every case.

### Parser correctness
- **LM-005** **CSI `J` (ED) and `K` (EL) MUST honor the mode parameter.**
  `J0` = below, `J1` = above, `J2` = all. Lives in `crates/sonicterm-vt/src/vt.rs`.
  Regression: `vt::shell_prompt_redraw_preserves_above_cursor`.
- **LM-006** **CSI `?1049h` MUST be a no-op when already in alt screen.**
  Otherwise vim/fzf re-entry clobbers `saved_cursor`.
  Regression: `dec_1049h_repeated_does_not_clobber_saved_cursor`.
- **LM-007** **`PtyHandle::Drop` MUST kill the child explicitly.**
  Dropping the trait object alone does not terminate the shell.
  Lives in `crates/sonicterm-io/src/pty.rs`.

### Security / safety
- **LM-008** **`sonicterm_cfg::url_open::validate()` is mandatory before
  spawning anything.** OSC 8 URIs come from untrusted PTY output.
  Allow-list: `http`, `https`, `mailto`, `file`. Deny control chars +
  shell metacharacters. Length capped at 4096. Lives in
  `crates/sonicterm-cfg/src/url_open.rs`.

---

## Commits

- **Conventional Commits** with scope: `feat(v1.0): ...`, `fix(vt): ...`,
  `chore(deps): ...`, `docs: ...`, `refactor(crates): ...`,
  `chore(modularization): M<N> — <title>`.
- **Mandatory trailer** on every Claude-authored commit:
  ```
  Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
  ```
- **Touches line** — every PR body MUST start with:
  ```
  touches: crates/sonicterm-app/src/app/window_event.rs, ...
  ```
  So another PM can detect hot-file collisions.

---

## Scratch hygiene (the SSD-full rule)

Per-agent clone footprint is ~1.8 GB (repo + `target/`). With ~10 PRs
in flight, this trivially fills a 460 GB SSD until even `df` fails.

- **Final step of every agent prompt MUST be**
  `cd / && rm -rf /tmp/<scratch>`.
- PM sweep after every merge / task notification:
  ```bash
  du -sh /tmp/* 2>/dev/null | sort -h | tail
  # anything >100 MB not currently in-flight: rm -rf
  ```

---

## Local gate (full sweep before any commit)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-deny.sh
bash tools/check-landmines.sh
bash tools/check-contract-docs.sh
bash tools/check-ownership.sh
bash scripts/check-visual-snapshots.sh
```

Code-touching PRs MUST also run the per-crate `## Test gate (local)`
block from each touched crate's `CLAUDE.md`.
