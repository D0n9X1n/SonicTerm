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
