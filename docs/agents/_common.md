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

Per CLAUDE.md §3, every PR follows this cycle. Models alternate
deliberately so no single model's blind spot drives a change.

PM is the dispatcher between steps, **never the author at any step.**

| Step | Who | What | Output |
|---|---|---|---|
| 0. Clarify | PM | When the user files a request, **first** restate it in PM's own words + name unstated constraints + ask any blocking questions BEFORE filing the issue. No issue gets filed until PM has confirmed scope with the user (single confirmation message OK; no need for AskUserQuestion if scope is unambiguous). | PM message to user |
| 1. Raise | Haiku | Draft + intake-review the issue from the Step-0 clarified scope (title front-loads symptom; concrete repro; evidence bundle; dedupe; labels). | `gh issue create` |
| 2. Investigate | Opus | Diagnose-only. **NO production code.** Mandatory adjacency check (see "widen-the-net" below). | `gh issue comment` with diagnosis report |
| 3. Review diagnosis | Haiku | Audit Step 2: root cause specific (file:line)? Fix sound? Test plan right shape? Size realistic? Adjacency check actually applied? | `gh issue comment` `APPROVED-DIAG` / `REVISE-DIAG` |
| 4. Implement | Opus | Open PR. Prompt MUST quote the diagnosis verbatim — no scope drift. | `gh pr create` |
| 5. Review code | Haiku | Audit the PR: gate green, scope matches diagnosis, no §4 land-mine regression. | `gh pr comment` `APPROVED` / `REVISE` |

If a step says REVISE, the same model re-runs (Haiku re-audits Haiku's draft; Opus re-implements; etc.) — never let PM "just fix it quickly" past a REVISE.

**Skip Step 0 + Step 1 allowed only for**:
- Trivial: typo, single-line config flip the user explicitly requested with no design ambiguity.
- fmt/clippy followup commits on an already-reviewed PR.
- PM-authored docs the user directly instructed AND that don't touch §4 land-mines.

For anything else, **even one-line testing-harness fixes go through the full rotation.** Skipping is what produces the "fix one place, miss the next" failure mode.

### Step 0 clarify rule

PM MUST do this for every user ask before dispatching anyone:
1. **Restate the ask** in 1–2 sentences so the user can confirm or correct.
2. **Name unstated constraints** (e.g. "this implies a breaking change for users with pinned dependencies — confirm OK?"; "this conflicts with existing doc X — should we update X?"; "this touches hot file Y per §15 — needs 2-PM coord").
3. **Ask blocking questions** only when scope is ambiguous — prefer narrow concrete clarifications over open-ended "what do you want?".
4. **Then** dispatch Step 1 with the clarified scope quoted verbatim into the Haiku prompt.

If the user's ask is unambiguous and already names constraints, the Step-0 restate can collapse into a single line in the Step-1 dispatch prompt ("user said: X, confirmed").

### Long-term-greater-good rule (applies to every step where there's a design choice)

When PM or any agent faces uncertainty between options, the first priority is **what's best for the project long-term**, not what's the smallest diff right now. Heuristics:

- Prefer fixing the underlying anti-pattern over patching the symptom — even if it touches more files.
- Prefer the design that won't need re-architecture in 6 months over the one that ships today.
- Prefer reducing maintenance surface (delete code, collapse duplication) over preserving every existing pattern.
- "Minimum changes" is a soft preference, NOT a hard constraint. Override it whenever the small change paints the project into a worse corner.
- When the tradeoff is unclear, dispatch a cross-model second opinion (see "Cross-model agreement" below). Don't let "the path of least change" win by default.

This is why we file structural sub-issues during Step-2 widen-the-net rather than rolling them in piecemeal — short-term effort, long-term clarity.

### Cross-model agreement rule (resolving uncertainty)

When PM or any agent is uncertain between two design choices:

1. Dispatch the SAME question to two agents using the **two cross-check model slots from environment**: `$SONIC_REVIEWER_MODEL_A` and `$SONIC_REVIEWER_MODEL_B` (configured via shell env, e.g. `export SONIC_REVIEWER_MODEL_A=haiku SONIC_REVIEWER_MODEL_B=sonnet`). Defaults if unset: A=haiku, B=sonnet.
2. **NEVER hardcode model names** (`haiku`, `sonnet`, `opus`) in dispatch prompts when doing cross-model checks. Read the env var into the prompt at dispatch time.
3. If both agents independently agree on the same direction → follow it.
4. If they disagree → escalate to the implementer model (`$SONIC_IMPLEMENTER_MODEL`, default `opus`) for a tiebreaker reasoning pass, then PM picks.
5. Both agents must reach their verdict WITHOUT seeing each other's response (otherwise it's not independent).

This is distinct from the 5-step rotation (where Haiku and Opus play fixed roles per step). The cross-model check is an ad-hoc tiebreaker for design uncertainty inside any step.

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
bash tools/check-harness-bash-strict.sh
bash tools/check-contract-docs.sh
bash tools/check-ownership.sh
bash scripts/check-visual-snapshots.sh
bash testing/workflows/test-ocr-skip.sh
```

Code-touching PRs MUST also run the per-crate `## Test gate (local)`
block from each touched crate's `CLAUDE.md`.

---

## Visual harness — OCR language packs (issue #593)

CJK ocr expects (e.g. `中文`, `中X`) need the tesseract CJK pack —
without it OCR returns Latin garbage and cases FAIL instead of SKIP.
Install: `brew install tesseract-lang` (macOS). `mac.sh` preflights
`tesseract --list-langs` and exports
`SONICTERM_HARNESS_CJK_AVAILABLE={0,1}`. `run_case.sh` SKIPs CJK
ocr_contains expects when 0 (mirrors PR #530 OCR_AVAILABLE pattern),
and passes `-l chi_sim+chi_tra+jpn+kor+eng` to tesseract when 1.
