# Multi-Agent Architecture Plan — Sonic v0.9 → v1.0

**Status:** DRAFT for user decision · **Author:** PM (foreground Claude) · **Date:** 2026-05-27
**Debate:** 2 rounds of Haiku critique applied (see PR body for transcripts)

> TL;DR — Read **Section E** (pros/cons table) and **Section F** (three options) first if you only have 5 minutes. Recommendation: **Option 2** (Phase 1 only), then re-evaluate after one release cycle.

---

## A. Current state (where we actually are)

### A.1 Crate inventory (measured 2026-05-27)

| Crate                | LOC (src) | Primary owner today      | Notes |
|----------------------|----------:|--------------------------|-------|
| sonic-app            |  6,868    | PM-mac                   | winit loop, 16 submodules under `app/` |
| sonic-shared         |  6,243    | PM-mac                   | render/ façade — **render/core.rs alone is 3,865 LOC** |
| sonic-ui             |  5,949    | PM-mac (+ Win for IME)   | tabs / panes / palette / search / prefs / ime / cursor |
| sonic-text           |  1,830    | PM-mac                   | shape, swash, atlas, row-cache |
| sonic-windows        |  1,166    | PM-windows               | win bin |
| sonic-io             |  1,008    | both                     | PTY + proc + ssh — Unix & Windows tangled in one crate |
| sonic-cfg            |    988    | PM-mac                   | config/theme/keymap/url_open |
| sonic-mux            |    795    | PM-mac                   | persistent mux daemon |
| sonic-grid           |    749    | PM-mac                   | grid + scrollback + hyperlinks |
| sonic-logging        |    744    | PM-mac                   | tracing init, log rotation |
| sonic-gpu            |    693    | PM-mac                   | wgpu pipelines |
| sonic-mac            |    644    | PM-mac                   | mac bin |
| sonic-vt             |    624    | PM-mac                   | VT parser |
| sonic-types          |    367    | shared (no-deps)         | values |
| sonic-render-model   |    202    | PM-mac                   | renderer-agnostic frame model |
| sonic-core           |     66    | (deprecated façade)      | re-exports for back-compat |
| **TOTAL**            | **28,934**| —                        | 16 crates |

### A.2 Pain points this session

1. **#199 took 4 fix cycles.** Spec referenced render/core.rs internals; Haiku and dev disagreed on what counted as "the frame loop." A 3,865-LOC file has no single owner in any reviewer's head.
2. **Hot-file conflicts.** Over the last 30 merged PRs (rough audit of `gh pr list --state merged --limit 30`), the files `app/window_event.rs` and `render/core.rs` were touched by ~6 and ~4 PRs respectively (exact list: see follow-up issue to be filed if this plan is accepted; the point is the rate, not the precise count). Three triggered manual PM rebase. Cost ~45 min/collision.
3. **sonic-shared is still kitchen-sinky.** PR #157 split `sonic-core` into 10 crates but left `sonic-shared` as a re-export façade *plus* the render module. Adding a render feature requires touching sonic-shared (façade), sonic-gpu (pipeline), sonic-text (atlas), and sonic-app (wiring) — four crates per feature.
4. **sonic-io tangles Unix and Windows.** `proc_info` (Unix) and `foreground_proc` (Windows) sit in the same crate. Windows-only PRs build all the Unix code anyway; mac-only PRs build the Win shims.
5. **Multi-agent friction.** PM dispatches Opus; Opus opens PR; Haiku reviews; PM rebases. There is no `dev-windows` PM agent today — Windows PRs are authored by the mac PM speculatively and then verified by the user manually. Recent Windows regressions (issues #182, #190 — Windows-only ConPTY edge cases) slipped past pre-merge gates.

### A.3 What's working

- The per-crate `tests/` folder convention scales. Test floor at 824 is honored.
- The §13 GUI smoke gate catches what `cargo test` cannot.
- Haiku review at $0.80/Mtok in is cheap enough to run on every PR.

---

## B. Architectural changes (3-phase rollout)

### Phase 1 — File splits + CODEOWNERS + LOC cap (target: v0.9)

**WHAT.** Three concrete moves, no new crate-graph topology.

1. Split `crates/sonic-shared/src/render/core.rs` (3,865 LOC) into ≤6 files, each ≤800 LOC, along existing function clusters:
   - `frame.rs` (begin_frame / end_frame / pacing)
   - `text_emit.rs` (cell→span conversion incl. the #163 bg fix)
   - `quad_emit.rs` (cursor / selection / underline / search highlights)
   - `surface.rs` (wgpu surface config, Suboptimal handling)
   - `viewport.rs` (scroll, resize, dpi)
   - `mod.rs` (façade — ≤200 LOC)
2. Split `sonic-io` into three crates:
   - `sonic-io-core` (PTY trait, ring buffer, shared bytes plumbing)
   - `sonic-io-unix` (cfg(unix) — proc_info, posix pty)
   - `sonic-io-windows` (cfg(windows) — ConPTY, foreground_proc)
   `sonic-io` becomes a thin façade for back-compat.
3. Add `.github/CODEOWNERS` mapping crates → PM agents → notify on PR.
4. Add CI gate: `scripts/check-loc.sh` fails if any `*.rs` exceeds 1,000 LOC (with a documented allowlist for generated files). Hard cap, not advisory.

**WHY.** Eliminates the two largest current hot-file collision sources. CODEOWNERS gives Haiku and dev agents a clear "ping who" signal in PR review.

**RISK.** (revised after Round 1 critique)
- Splitting render/core.rs has historically introduced subtle ordering bugs (cf. PR #42 tofu regression). A bad split could regress §13 GUI smoke or the per-cell bg #163 fix.
- 1000-LOC CI gate may immediately fire on `sonic-app` files; we must audit existing offenders before turning gate to hard-fail.
- sonic-io split forces every external caller to update Cargo.toml; we mitigate via the façade re-export but downstream forks will break.

**ROLLBACK.** All Phase 1 changes land as separate small PRs (one per file split, one per crate split, one for CODEOWNERS, one for the LOC gate). Each is independently revertable. If render/core.rs split regresses §13 smoke, revert the single split PR — no other phase work depends on it.

**LOC DELTA.** ~+150 LOC (façade re-exports, CI script). No deletion.

**SEQUENCING.** None. Phase 1 is a prerequisite for Phase 2 (the feature verticals must be defined against a non-monolithic render module).

**BOOTSTRAP (Phase 1 paradox).** The render/core.rs split is itself a `hot-file` change while the `hot-file` ritual doesn't yet exist. We bootstrap by: (a) declaring a 7-day code freeze on `render/core.rs` and `sonic-io/` immediately when this plan is accepted; (b) running the split PR with **both** PMs present (one as dev, one as PM-merger); (c) `qa-visual` mandatory on the split PR even if it's pre-Phase-2. The `hot-file` ritual is then born from the post-mortem of the split PR.

**EXIT CRITERIA.** All `*.rs` ≤ 1,000 LOC. CODEOWNERS in place. sonic-io split merged. §13 smoke + 824 test floor green. One release cycle (≥ 2 weeks) elapses before Phase 2 starts.

### Phase 2 — Feature verticals (target: v0.9.x, after Phase 1 bakes)

**WHAT.** Extract from `sonic-ui` six feature crates, each owning UI + state + tests for one user-visible vertical:

- `sonic-feature-tabs` (tab bar, tear-out, drag)
- `sonic-feature-panes` (split, resize, focus)
- `sonic-feature-palette` (command palette + fuzzy)
- `sonic-feature-search` (in-pane search overlay)
- `sonic-feature-prefs` (preferences window)
- `sonic-feature-ime` (IME composition overlay)

`sonic-ui` shrinks to ~1,500 LOC: shared widgets (button, list, scrollbar) + design tokens. Cross-feature interaction goes through the existing `sonic_cfg::keymap::Action` enum — features register handlers, palette/keymap stay decoupled.

**WHY.** Each feature can be owned by a "champion" (Section C). PRs that add a palette command don't touch the same files as PRs that add a pane behavior. Build parallelism improves: feature crates compile independently.

**RISK.**
- `sonic_cfg::keymap::Action` becomes a growth point — adding an action requires editing a central enum. Mitigated by allowing feature crates to define action sub-enums re-exported into a `keymap::Action::Feature(...)` variant.
- Six new crate boundaries → Cargo build graph grows; cold build time est. +8–15% (measured on `sonic-core` split: 11% regression).
- Inter-feature dependencies (e.g. palette invoking search) become explicit crate deps — could surface previously-hidden cycles.

**ROLLBACK.** Each extraction is one PR. If a feature crate causes problems, in-line it back into `sonic-ui` — public API stays the same because `sonic-ui` re-exports.

**LOC DELTA.** Approximately neutral (~+300 for Cargo.toml + manifest boilerplate; some duplication of small helpers).

**SEQUENCING.** Depends on Phase 1 (CODEOWNERS + LOC cap). The Action sub-enum pattern must be prototyped in one feature crate (recommend `sonic-feature-search` — smallest) before extracting the others.

**EXIT CRITERIA.** All 6 feature crates extracted, sonic-ui ≤ 1,800 LOC, build time regression < 20%, test floor maintained.

### Phase 3 — Feature champions + sparse checkouts + auto-dispatcher (target: v1.0+)

**WHAT.**
- Per-feature champion label (Section D); champions auto-tagged as reviewer on their feature's PRs.
- Document `git sparse-checkout` recipes per platform — Windows PM can clone without mac-only crates (and vice-versa), saving disk in `/tmp/<scratch>` clones.
- Replace manual PM dispatch with a script (`scripts/dispatch.sh <issue-N>`) that reads the issue label set and emits the dev/QA agent prompts.

**WHY.** Reduces PM cognitive load once the agent roster scales to 5–7 named agents.

**RISK.** Premature automation. Don't build this until Phase 2 has run for ≥ 1 release.

**ROLLBACK.** Pure tooling — delete the script.

**LOC DELTA.** ~+400 LOC of shell/script tooling.

---

## C. Agent roster

### C.1 The PM seats (2 total, 1 foreground per repo clone)

**Model:** Claude Sonnet / Opus foreground session
**Count:** **two seats, one per platform** (PM-mac, PM-windows). At any moment **only one is foreground per repo clone**; the other is async/offline. They never co-edit.
**Owns:** triage, sequencing, releases, user conversation, rebase conflicts, ROADMAP updates
**Does not:** write feature code (dispatches dev subagents); does **not** review the other PM's open PRs (read-only awareness only — CLAUDE.md §15)
**Reads first:** `docs/ROADMAP.md`, `CLAUDE.md`, latest `docs/specs/*`
**Reports to:** the user, via the foreground conversation
**Authority vs champion:** PM is the final arbiter on merge. A `champion:<feature>` is *advisory reviewer*, not gatekeeper. If champion and PM disagree, PM merges and files a follow-up issue capturing the champion's objection.
**Onboarding a 3rd PM (e.g. Linux in v1.x):** documented in `docs/agents/onboarding.md` (to be authored if Option 1/2 picked). Linux PM would inherit `dev:linux` + `platform:linux` labels and a new sparse-checkout recipe; no plan changes needed at the architecture level.

### C.2 Dev subagents (2, optional 3rd)

| Agent       | Model | Owns                                                                                | Smoke gate                |
|-------------|-------|-------------------------------------------------------------------------------------|---------------------------|
| `dev-mac`   | Opus  | sonic-mac, sonic-io-unix, mac packaging, mac-only render quirks                     | §13 GUI smoke on macOS    |
| `dev-windows` | Opus | sonic-windows, sonic-io-windows, win packaging, ConPTY, IME on Win                  | §13 GUI smoke on Windows  |
| `dev-cross` (optional) | Opus | sonic-feature-* crates and any code in shared crates with no OS-specific path | Runs on either platform   |

**Prompt template** (all dev agents): "Cd /tmp, fresh clone, branch `<branch>`, implement spec at `docs/specs/<x>`, gate is fmt+clippy+test+pty_dump+§13 smoke. Up to 3 cycles. Reply PR URL + 1 line. Final step: `cd / && rm -rf /tmp/<scratch>`."

**When invoked:** PM picks one issue → dispatches the matching dev (label `dev:mac` / `dev:windows` / `champion:<feature>`).

**Reports via:** opening a PR + replying to PM with URL and pass/fail per gate.

**Escalation:** if Round 3 still fails the same gate, dev agent stops and tags PM with a `blocked-on:design` request.

### C.5 Scaling: onboarding a new dev agent (<2 hours wall-clock, ~30 min hands-on)

A concrete playbook for adding e.g. a 4th `dev-mac-2` or a brand-new `dev-linux` once Linux is lifted out of "deferred". The goal is that a freshly-dispatched Opus subagent is productive on its first real PR within ~2 hours of cold start (hands-on time ~30 min; the rest is the cold cargo build).

**Time breakdown (honest, M2 baseline):**

| Step                                                         | Wall-clock      | Hands-on |
|--------------------------------------------------------------|-----------------|----------|
| Install deps (rustup, just, gh, tesseract)                   | ~5 min          | 5 min    |
| `cargo build --workspace --release` (cold, no cache)         | ~60–90 min      | 0 (parallel with reading CLAUDE.md, ROADMAP.md, RELEASE_TESTING.md) |
| Run local gate to verify env                                 | ~15 min         | 5 min    |
| Open hello-PR                                                | ~10 min         | 10 min   |

**Caveat:** with `ccache` or `sccache` pre-warmed (shared across agents on the same host), first build drops to ~15 min. Cold first-time on a fresh host is the bottleneck — budget the full 2 hours unless cache is shared.

**Multi-agent-per-platform labeling:** when 2+ agents share a platform, EXTEND `dev:*` with a suffix: `dev:mac-1`, `dev:mac-2`, etc. The base `dev:mac` label may also be applied as a group identifier. The first agent on a platform uses the base label (`dev:mac`); second onward uses suffixed (`dev:mac-2`, `dev:mac-3`). Update §15 of CLAUDE.md to reflect this convention (do NOT update §15 in this PR; tracked as follow-up issue per §G Q9).

**Provisioning checklist (PM does this once per new agent):**

| Item                | Source / Value                                                                |
|---------------------|-------------------------------------------------------------------------------|
| SSH key             | `~/.ssh/sonic_dev_<name>_ed25519` — generated locally, public half added to the `sonic-bots` GitHub org as a deploy key on `D0n9X1n/sonic` (write scope) |
| `gh` auth           | `gh auth login --with-token < ~/.config/sonic/bot-tokens/<name>.tok` — PAT scoped to `repo`, `read:org`, `workflow` only |
| CLAUDE.md to read   | Repo `/CLAUDE.md` end-to-end (this doc) PLUS `docs/qa/regressions.md` (see §C.6.7) — required reading on every session start |
| Scratch-dir pattern | `/tmp/sonic-<name>-<pr-or-task-id>/` — MUST be cleaned by the agent's final step (CLAUDE.md §12) |
| Working dir on host | `~/sonic-dev-<name>/` for the persistent gate clone (separate from per-task scratch) |

**Auto-bootstrap script:** `scripts/onboard-dev-agent.sh <platform> <name>` (proposed location — see §G open question). Behavior:

1. **Verify prerequisites.** `cargo --version`, `gh --version`, `just --version`, and on macOS `tesseract --version` (needed by visual-test OCR — see §C.6.3). Hard-fail with actionable message if any are missing.
2. **Clone repo** to `~/sonic-dev-<name>/`, configure local `user.email = noreply@anthropic.com`, `user.name = "Claude Opus 4 (<name>)"`.
3. **Run the local gate** once in that clone (fmt + clippy + workspace test + both `pty_dump*` e2e + release build of the platform bin). Confirms env actually works end-to-end before any PR is opened. Floor-test count from CLAUDE.md §2 is enforced — bootstrap fails loudly if the env under-runs the floor.
4. **Open a "hello" PR** `chore(onboard): <name> sanity check` that appends one line to `docs/onboarding-log.md` with timestamp + platform + agent name. Verifies push, label, and PR-open paths all work with the new credentials. PR auto-labels with `dev:<platform>` per §D.3.
5. **De-bootstrap on failure.** If any step fails, the script prints the failed step, the remediation hint, and leaves the partial clone in place for inspection (does NOT auto-delete — agent might still need it).

**First-task assignment.** PM finds appropriate first work for the new agent with:
```
gh issue list --label "good-first-issue,platform:<plat>" --search "-label:dev:mac -label:dev:windows -label:dev:cross"
```
("no:dev:*" pattern — issue is unclaimed by any existing dev agent.)

**De-provisioning (graceful retire).** When cutting an agent:
1. PM runs `gh pr list --search "author:<bot-handle> is:open"`; for each open PR, either let the agent finish (if mid-cycle) or hand off by reassigning the branch to another dev (push to a new branch under the new owner, close the old PR with a comment linking the new one).
2. Reassign open issues: `gh issue list --label "dev:<name>" --json number | jq -r '.[].number' | xargs -I{} gh issue edit {} --remove-label "dev:<name>"` — issues fall back into the PM triage pool.
3. Remove the deploy key from `sonic-bots`, revoke the PAT.
4. Add a row to `docs/onboarding-log.md` marking the agent as retired (date + reason).

**Scaling limits — measurable triggers for adding dev agents:**

| Dev agents per platform | Trigger to add (measurable)                                                          |
|-------------------------|--------------------------------------------------------------------------------------|
| 1                       | Baseline. Serializes hot-file PRs; bottlenecks on render/input changes.             |
| 2                       | **Justified when**: (open issues with `dev:<plat>` label) ÷ (per-PM PRs/day capacity) > 3 |
| 3                       | **Justified when**: 2-dev configuration's median PR-to-merge time > 2 days for 2 consecutive weeks |
| 4+                      | Coordination cost exceeds throughput gain; **consider splitting the platform into sub-domains** (e.g. `dev:mac-render`, `dev:mac-vt`) rather than adding more general devs. Without a 2nd PM seat per platform, more devs just queue at the merge gate. |

**Champion-handoff.** Feature champion role (see §B Phase 3) transfers via a single PR: the outgoing champion opens `chore(champion): hand off <feature> to <new-agent>` that flips `CODEOWNERS` and updates the `champion:<feature>` label-owner mapping in `docs/champions.md`. Two reviewers required (outgoing + incoming champion), no QA gate needed.

### C.3 QA subagents (2, optional 3rd)

| Agent       | Model  | Job                                                                                                  | Cost/PR |
|-------------|--------|------------------------------------------------------------------------------------------------------|---------|
| `qa-code`   | Haiku  | Fmt/clippy/tests/SAFETY/error-handling/§4 land-mines. Posts single APPROVED or single finding.      | ~$0.03 |
| `qa-spec`   | Haiku  | Verify PR diff matches the spec in PR body. Checks tests assert acceptance criteria.                | ~$0.04 |
| `qa-visual` (optional) | Sonnet | Render/UX PRs only. Runs §13 smoke + screenshot diff vs baseline. Posts pass/fail per case. | ~$0.20 + 5 min |

**Auto-dispatch matrix:**

| PR touches                                                | qa-code | qa-spec | qa-visual |
|-----------------------------------------------------------|:-------:|:-------:|:---------:|
| docs/*, comments only                                     |   ✓     |   —     |   —       |
| src/* (no render/ui/vt/grid)                              |   ✓     |   ✓     |   —       |
| render/ ui/ vt/ grid/ themes/                             |   ✓     |   ✓     |   ✓       |

**Quick-mode:** typo-only or docs-only PRs skip qa-spec.

**Escalation:** if any QA posts CHANGES REQUESTED, PM re-dispatches dev (max 3 cycles) or fixes inline. If two QA agents disagree on the same PR, PM is the tie-breaker.

### C.6 QA reliability: defense in depth

QA must not silently miss bugs. The §C.3 roster on its own is necessary but not sufficient — Haiku reviewers have, historically, posted bare "APPROVED" on PRs that later shipped real regressions (#42 CJK tofu, #161 dropped ANSI bg). The following hardening rules apply to every QA role.

1. **`qa-code` (Haiku code reviewer):**
   - **Always pulls a fresh clone** of the PR branch into `/tmp/qa-code-<pr>-<run>/`; never trusts in-place state or another agent's working tree.
   - **MUST run the LOCAL GATE AS DEFINED IN CLAUDE.md §2 verbatim** — no shortcuts, no custom subset, no "abbreviated" version. Trusting the PM's or dev's claim that "gate is green" is forbidden — that was the failure mode of #42. If CLAUDE.md §2 changes, the qa-code prompt is auto-updated via `scripts/refresh-qa-prompts.sh` (new tool, tracked as follow-up issue per §G).
   - Reports both **APPROVED with a 2-line code summary** (one line: what the diff does; one line: why it's safe) AND any **CHANGES REQUESTED with `file:line`** evidence per finding. A bare "APPROVED" with no summary is treated as suspicious and the PM re-dispatches with a "summary required" reminder.
   - **Failure-feedback rule:** if `qa-code` posts PASS but a follow-up bug is found in the same PR within 7 days, the qa-code prompt template gets a new check added covering that class of bug. Tracked in §C.6.7.

2. **`qa-spec` (Haiku spec compliance):**
   - Reads the linked spec **verbatim**, not paraphrased into its own words (paraphrasing has been observed to silently drop acceptance criteria).
   - For each acceptance criterion in the spec, must emit `PASS` or `FAIL` with concrete evidence (test name, file:line, screenshot path).
   - **Refuses to review** PRs whose body is missing the `spec:` field. Posts a single comment "missing `spec:` in PR body — please add or mark `spec: n/a — trivial`" and stops.

3. **`qa-visual` (when invoked):**
   - Runs the **actual** §13 visual harness on a real display — no shortcuts, no "I would have run this".
   - Reports per-case `PASS`/`FAIL` with screenshot path AND OCR text dump (tesseract output) so a future agent can grep what was on screen even after the screenshot expires.
   - **Flake handling:** distinguishes "real bug" from "driver flake" by re-running the failing case once. Two-in-a-row fail = real bug; one-pass-one-fail = flake reported as `FLAKY` not `FAIL` (PM decides whether to merge).
   - **Persistent-flake escape hatch:** if `qa-visual` reports flake on the same case 2 sessions in a row, PM files an issue `qa-visual-flake: <case-id>` with `dev:mac` (or `dev:windows`) label. PM may bypass that case with `[skip-visual-case:<id>]` flag in the PR body until the underlying flake is fixed.

   **Render/UX file path matcher** (triggers qa-visual auto-dispatch):
   ```
   crates/sonic-shared/src/render/**/*.rs
   crates/sonic-text/src/**/*.rs
   crates/sonic-gpu/src/**/*.rs
   crates/sonic-app/src/app/{window_event,redraw,spawn_pane,overlays,input,keymap_dispatch}.rs
   crates/sonic-ui/src/{tabbar_view,overlays,cursor,selection,search,command_palette,prefs/**}.rs
   crates/sonic-vt/src/vt.rs
   crates/sonic-grid/src/grid.rs
   assets/{themes,keymaps}/*.toml
   ```
   Implemented as `scripts/needs-visual-qa.sh <pr-number>` returning exit 0 if the PR touches any path in this set.

4. **Cross-check rule:** every PR needs **≥2 QA APPROVEDs** (`qa-code` + `qa-spec` minimum). `qa-visual` is **required** if the PR touches any path in the §13 file list (auto-triggered by the path matcher above in `scripts/on-pr-opened.sh`).

   **Exception (quick-mode)**: PRs that match ANY of these are exempt from the ≥2 QA requirement (`qa-code` only):
   - body contains `[skip-qa-spec]` flag with an explicit justification on the next line
   - changes only `**/*.md`, `docs/**`, `.github/**/*.yml` (excluding workflows that touch runtime)
   - net diff ≤ 5 LOC AND no `.rs` file modified
   - label `trivial` applied by PM with a justification comment

   PM must NOT apply `trivial` for behavior changes, default-value changes, or anything in the CLAUDE.md §0 "Trivial-NOT-applies" list.

5. **QA escalation on disagreement:** if 2 QAs disagree (one APPROVED, one CHANGES REQUESTED), PM reads both reports and breaks the tie — OR dispatches a **3rd Haiku** with both prior verdicts as input ("here is verdict A, here is verdict B, you arbitrate"). Tiebreaker verdict is binding. **Maximum 3 Haiku dispatches per PR for tiebreaking; on the 3rd disagreement, PM auto-decides and documents the rationale in a PR comment.**

6. **Audit trail:** every QA verdict is posted as a **PR comment**, never as a private DM/log/agent reply only. Future agents (and future PMs) must be able to read the full QA history of any PR from `gh pr view <N> --comments` alone.

7. **QA regression catalog:** `docs/qa/regressions.md` lists every "QA approved but bug shipped" incident with three fields: PR#, what was missed, prompt update applied. Living doc — **every dev and QA agent reads it on session start** (added to the onboarding checklist in §C.5). New entries are added by the PM the moment a regression is confirmed, not retroactively at release time.

8. **"Trust but verify" by PM:** PM samples **1 in 10 APPROVED PRs** by reading the QA report end-to-end and doing a spot-check (random file in the diff, run one assertion mentally against the actual code). Sampling is **pseudo-random**: `PR# mod 10 == 7` (the offset `7` is picked by PM on each release-tag rotation and published in `docs/qa/sampling-offset.md`). Predictable enough to verify retroactively, random enough to be unpredictable to agents at PR-open time. If sampled, PM leaves a `qa-sampled:OK` comment on the PR so the sampling rate is auditable.

---

## D. GitHub workflow (the day-to-day loop)

### D.1 Issue lifecycle

1. User or PM files issue.
2. Mandatory labels at file time: `bug`|`enhancement`|`docs` and `platform:{mac,windows,both,linux}`.
3. PM triage adds: `dev:{mac,windows,cross}`, optional `champion:<feature>`, optional `hot-file` (if touches §4 land-mine surface), and `release:v0.x.y` milestone.
4. PM-on-duty finds unclaimed work with `gh issue list --label "platform:both" --search "-label:dev:mac -label:dev:windows"`.
5. Claim = add `dev:<self>` label and assign self.

### D.2 PR lifecycle

1. **Dispatch.** PM dispatches dev agent with: spec link, acceptance criteria, scratch-clone command, final-cleanup command.
2. **Open.** Dev opens PR with mandatory body fields:
   ```
   touches: <file paths> (per CLAUDE.md §15)
   fixes: #<issue>
   spec: docs/specs/<name>.md  (or "n/a — trivial")
   dev: mac|windows|cross
   smoke: pass|n/a (screenshot path if pass)
   ```
3. **Auto-QA.** PM script (`scripts/on-pr-opened.sh`, Phase 3) reads the touched files + labels and dispatches `qa-code` + `qa-spec` (always) + `qa-visual` (if matrix matches).
4. **Iterate.** CHANGES REQUESTED → PM re-dispatches dev with the specific finding (max 3 cycles total).
5. **Merge.** All required QAs APPROVED → PM runs `gh pr merge <N> -R D0n9X1n/sonic --squash --admin --delete-branch`.
6. **Auto-close.** `Fixes: #N` in PR body closes the issue on merge.

### D.3 Labels (full set)

| Label                | Meaning                                                |
|----------------------|--------------------------------------------------------|
| `bug` / `enhancement` / `docs` | Type                                         |
| `platform:{mac,windows,both,linux}` | Which OS is affected                    |
| `dev:{mac,windows,cross}` | Who is implementing                               |
| `champion:<feature>` | Default reviewer/contact for this vertical             |
| `hot-file`           | Touches §4 land-mine files — coord-issue required     |
| `qa:{code,spec,visual}` | QA pass status (PM-script-maintained)              |
| `blocked-on:*`       | Explicit blocker (`blocked-on:dev:windows`, etc.)      |
| `release:v0.x.y`     | Milestone link                                         |
| `revert-candidate`   | If post-merge regression is found                      |

### D.4 Branch protection + sync rituals

- **main:** admin-squash-merge only; no direct push; delete branch on merge; `--no-verify`/`--no-gpg-sign` forbidden via local hook.
- **Commits:** must end with `Co-Authored-By: <dispatched dev model>`.
- **Daily (each PM):** read-only sweep `gh pr list --state open` filtered by other PM's `dev:*` label — *no review comments cross PM boundary* per CLAUDE.md §15. The point is awareness, not interference.
- **Weekly (rotating PM):** ROADMAP.md commit summarizing last week's merges + next week's plan. Shared across PMs via main.
- **Per release:** tag owner runs `RELEASE_TESTING.md` + `vtebench` + tags. Other-platform PM uploads its `.msi`/`.dmg` artifact to the same release within 48h.

### D.5 Coordination on `hot-file`

1. Before touching `render/core.rs` (or whatever it becomes after Phase 1), `app/window_event.rs`, `sonic_cfg::keymap::Action`, `vt.rs`, `grid.rs`:
   - `gh pr list --label hot-file --state open` — if any match the file you'd touch, comment your intent on that PR/issue first.
2. If clear: open a **coord issue** ("intent: refactor X frame loop"), label `hot-file` + `dev:<self>`.
3. 24h grace window. Other PMs watch the `hot-file` label.
4. If collision surfaces during the grace: coordinate inline; first-to-claim usually wins, complex change wins ties.

### D.6 Single point of failure analysis (added per Round 1 critique)

| SPOF                                          | Mitigation                                                       |
|-----------------------------------------------|------------------------------------------------------------------|
| One PM goes offline mid-release               | ROADMAP weekly commit is the handoff record; either PM can tag   |
| `scripts/dispatch.sh` (Phase 3) bug          | Manual dispatch always works; script is convenience              |
| Haiku QA agent posts wrong verdict            | PM is final arbiter; revert-candidate label exists               |
| `champion:<feature>` person unreachable       | Champion is *default* reviewer, not gate — PM can override       |
| CODEOWNERS misconfigured silently             | Phase 1 includes a self-test PR that intentionally touches each owned path |

---

## E. Pros and cons of THIS plan

| Aspect                        | Pro                                                  | Con                                                       | Mitigation                                                       |
|-------------------------------|------------------------------------------------------|-----------------------------------------------------------|------------------------------------------------------------------|
| Crate count growth (16→25+)   | Clear ownership, parallel work, build parallelism    | Cargo cold build time +8–15%; more `Cargo.toml` overhead | Sparse checkout (Phase 3); shared `[workspace.dependencies]`     |
| 1000-LOC per-file cap         | Forces decomposition; reviewer fits file in head     | Punitive on legitimately-cohesive modules; arbitrary number | Allowlist; raise to 1,200 after 1 release if false-positives high |
| Action-enum coupling          | Features decoupled; palette/keymap stay simple       | Central enum still grows; adding action = cross-crate edit | Feature-local sub-enums re-exported (`Action::Search(...)`)      |
| QA-on-every-PR                | Catches more bugs early; cheap (~$0.07/PR)           | Latency on small PRs; QA noise can desensitize PM         | Quick-mode for docs/typo PRs; consolidate QA findings per round  |
| 2 PMs (mac + windows)         | Native expertise per platform; no speculative Win PRs | Two PMs can't both edit ROADMAP simultaneously            | Weekly ROADMAP rotation; main is sync point                      |
| `dev-cross` optional 3rd      | Drains feature-vertical backlog faster               | Adds one more dispatch target for PM to track             | Skip until backlog warrants                                       |
| `qa-visual` optional 3rd      | Catches render regressions §13 smoke alone misses    | +5 min and ~$0.20 per render PR                          | Auto-skip when PR doesn't touch render/                          |
| `hot-file` coord issue ritual | Cheap, no-meeting coordination                       | 24h grace slows urgent fixes                              | "urgent" label bypasses grace; rare                              |
| Feature champions             | Single contact per vertical                          | Champion bus-factor of 1                                  | Champion role is reviewer, not gatekeeper; PM can override       |
| sonic-io platform split       | Win-only PRs skip Unix build, vice-versa             | Public-API breakage for forks                             | sonic-io façade re-exports during deprecation                    |
| Phased rollout (vs big-bang)  | Each phase independently revertable                  | Total elapsed time 2–3 release cycles                    | Phase 1 alone gives most of the value                            |
| Spec-first PRs                | qa-spec can verify objectively                       | Spec authoring is overhead                                | "n/a — trivial" escape hatch for <50 LOC PRs                     |
| Onboarding scalability        | <30min via `onboard-dev-agent.sh` playbook (§C.5)    | Requires shared bootstrap-script maintenance              | Script lives in-repo; broken bootstrap = blocking issue on next session |
| QA defense-in-depth           | Catches escapees via ≥2 QA + regression catalog (§C.6) | Adds latency + multi-agent coordination cost            | Parallel `qa-code` + `qa-spec` dispatch; quick-mode for trivial PRs |

---

## F. Decision matrix — pick one

### Option 1 — Full proposal (Phases 1+2+3, all 6 agent roles, all rituals)
**Time:** ~2–3 release cycles. **Risk:** moderate (Phase 2 disruption). **Payoff:** highest — true multi-PM scaling.

### Option 2 — Phase 1 only (file splits + sonic-io split + CODEOWNERS + LOC cap) ★ RECOMMENDED
**Time:** ~1 release cycle. **Risk:** low (all changes individually revertable). **Payoff:** ~70% of pain removed (hot-file collisions, Windows/Unix tangle, render/core.rs monolith) without committing to feature-vertical reshuffling.
**Note:** Options 1 and 2 both trigger the 7-day `render/core.rs` + `sonic-io/` code freeze described in Phase 1 BOOTSTRAP. Option 3 does not.
**Decision deferred:** Phase 2 + champion roster + qa-visual revisited at v0.9 retrospective.

### Option 3 — Minimum (CODEOWNERS + qa-spec only)
**Time:** ~3 days. **Risk:** ~0. **Payoff:** small — addresses spec-drift but leaves hot-file collisions and render/core.rs monolith untouched.

---

## G. Open questions for the user (only you can decide)

1. **Champion assignments.** If we pick Option 1, who champions which feature? PM-mac for prefs/search; PM-windows for ime/panes? Or assign by user gut?
2. **dev-cross 3rd dev — yes or no?** Adds parallelism, adds PM-tracking load. Suggest "no" until Phase 2 backlog grows.
3. **qa-visual — yes or no?** Adds ~$0.20 and ~5 min per render PR. Recent render regressions (#161, #199) would have been caught by it. Probable answer: yes for Option 1, no for Option 2.
4. **LOC cap value.** 1,000 is round-number; 800 is stricter; 1,200 more lenient. Vote?
5. **Should we hire (Anthropic-style: dispatch) a Linux PM in v1.x?** Linux is currently explicitly deferred. Does this plan permit a clean addition?
6. **Cost ceiling.** Honest weekly cost estimate at current velocity (~12 PRs/week):
   | Bucket                                           | $/week  |
   |--------------------------------------------------|---------|
   | Opus dev dispatches (~1.5 cycles avg, ~$3 each)  | ~$54    |
   | Haiku `qa-code` on every PR                      | ~$0.40  |
   | Haiku `qa-spec` on ~90% of PRs                   | ~$0.45  |
   | Sonnet `qa-visual` on ~30% of PRs                | ~$5     |
   | **Total**                                        | **~$60**|
   Opus dispatch dominates; QA is a rounding error. Acceptable?
7. **Two-PM commit attribution.** Same `Co-Authored-By: Claude Opus 4` trailer across both, or separate "PM-mac" / "PM-windows" attribution? Audit / blame implications.
8. **`onboard-dev-agent.sh` location.** **DECIDED:** `scripts/` (alongside other dispatch helpers like `scripts/check-deny.sh`), per existing repo convention. No `tooling/` directory will be introduced.
9. **Multi-agent-per-platform labeling convention.** Approve the `dev:mac-N` / `dev:windows-N` suffix scheme from §C.5? Alternative: drop the suffix and use GitHub assignee (`@bot-mac-2`) as the disambiguator instead of a label. Either choice requires a one-line update to CLAUDE.md §15.
