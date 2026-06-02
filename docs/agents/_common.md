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

## 5-step rotation (mandatory for every non-trivial change)

Per CLAUDE.md §3, every PR follows this 5-step cycle. Models alternate
deliberately so no single model's blind spot drives a change.

| Step | Who | What | Output |
|---|---|---|---|
| 1. Raise | Haiku | Draft + intake-review the issue (title front-loads symptom; concrete repro; evidence bundle; dedupe; labels). | `gh issue create` |
| 2. Investigate | Opus | Diagnose-only. **NO production code.** | `gh issue comment` with diagnosis report |
| 3. Review diagnosis | Haiku | Audit Step 2: is the root cause specific (file:line)? Fix sound? Test plan right shape? Size realistic? | `gh issue comment` `APPROVED-DIAG` / `REVISE-DIAG` |
| 4. Implement | Opus | Open PR. Prompt MUST quote the diagnosis verbatim — no scope drift. | `gh pr create` |
| 5. Review code | Haiku | Audit the PR: gate green, scope matches diagnosis, no §4 land-mine regression. | `gh pr comment` `APPROVED` / `REVISE` |

PM is the dispatcher between steps, **never the author at any step**. If a step says REVISE, the same model re-runs (Haiku re-audits Haiku's draft; Opus re-implements; etc.).

**Skip allowed only for**:
- Trivial: typo, single-line config flip the user explicitly requested.
- fmt/clippy followup commits on an already-reviewed PR.
- PM-authored docs the user directly instructed.

### Step 2 widen-the-net rule

When investigating, the Opus diagnose agent MUST ask: *is this symptom one instance of a class of bugs?* Look for:

- **Sibling files with the same anti-pattern.** E.g. if `app/window_event.rs` had a `.lock()` that should have been `.try_lock()`, grep every sibling `app/*.rs` for the same call shape. PR-#21's reviewer caught the orphan-shell bug this way.
- **Same root cause in adjacent stack frames.** E.g. a parser ED handler bug — also check EL, ICH, DCH, every CSI arm that does range-erase.
- **Shared helper called from multiple callers.** A bug in the helper is a bug everywhere; the diagnosis must enumerate callers.
- **Recently-merged PRs that touched the same surface.** Walk back ~14 days of `git log` on the affected files. The "fix" PR may have introduced the new bug.
- **The symptom under different config** — different theme, different terminal size, different shell. Some bugs only fire in narrow configurations.

Diagnosis report MUST contain a `## Adjacency check` section listing:
- Each sibling file/helper/caller inspected
- Whether the same anti-pattern was found there
- If found: filed as separate sub-issue OR rolled into the current fix scope (PM decides)

This is the **canonical defense** against the "fix one place, miss the next" failure mode that gave us #349 / #352 and the post-merge red CI on main. Don't skip it.

---



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
