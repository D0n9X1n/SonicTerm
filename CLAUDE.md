# CLAUDE.md — SonicTerm

SonicTerm is a GPU-accelerated terminal for macOS, Windows, and Linux. Keep
changes small, typed, and cross-platform unless the crate is explicitly platform-only.
The workspace version is the source of truth (`Cargo.toml` `[workspace.package]`).

## Read first

- [`wiki/Architecture.md`](wiki/Architecture.md) — system shape, data flow, seams.
- [`wiki/Architecture-Internals.md`](wiki/Architecture-Internals.md) — accounting verification, rendering invariants, native boundaries, release gate.
- [`wiki/Crate-Reference.md`](wiki/Crate-Reference.md) — crate map and per-crate detail.
- [`wiki/Code-Ownership.md`](wiki/Code-Ownership.md) — the platform area of each path, and how agents claim work.
- [`wiki/Logging.md`](wiki/Logging.md) — logs, diagnostics, retention, hang investigation.
- [`wiki/Memory.md`](wiki/Memory.md) — what each subsystem holds, and the resource governor.
- [`wiki/Rendering-Modes.md`](wiki/Rendering-Modes.md) — software vs GPU rendering and frame pacing.
- [`wiki/Packaging.md`](wiki/Packaging.md) — local macOS, Windows, and Linux packaging.

**Canonical documentation rule:** `wiki/` is the single documentation surface,
for agents and humans alike. It carries the technical detail — architecture,
invariants, verification, governance, packaging, release boundary — alongside
user-facing usage, configuration, keybindings, themes, and the feature
requirements. There is no separate maintainer-only documentation tree; a fact
worth writing down belongs on a wiki page, with separate English and Chinese files.

Each topic has an English `wiki/<Page>.md` and a Chinese
`wiki/<Page>-zh-CN.md`. Keep titles and prose in the file's language; do not combine
full translations or add language-half headings in one file. Preserve matching
heading structure and equivalent facts, updating both files in the same change.
Use reciprocal language-switch links and otherwise stay in the current language.
Pages link by bare page name — `[Logging](Logging)` or
`[日志](Logging-zh-CN)`, not `.md` paths — for the published wiki.

**Load only the English files for routine agent context.** The Read first links
above and crate `CLAUDE.md` files are the agent entry points. Read Chinese files
only when the task requires translation editing or verification; do not load
both translations just to understand the project.

**Verify documentation against current code.** Before describing behavior, check
the implementation, configuration defaults, tests, and workflows it refers to.
Correct stale statements in both language files in the same PR. Use Mermaid
flowcharts where they clarify control flow or data flow, with equivalent
structure and localized labels in each language file.

Do not track standalone implementation specs, plans, review audits, or
version-audit documents. `docs/specs/`, `docs/plans/`, and `docs/reviews/`
remain ignored local working folders.

When touching a crate, also read that crate's local `CLAUDE.md`.

## Searching

**Search is filtered by default, and the filter is silent.** A root `.ignore`
excludes the vendored upstream trees — FreeType, libpng, zlib, HarfBuzz, and the
pinned winit source — from `rg` and from most editors and agents that read it. The imported subsets
and exact versions are pinned in `scripts/native-dependencies.json`; default
searches deliberately omit those source trees.

This matters for reading a result, not just for speed: **an empty result may
mean the match is in a filtered tree, not that it does not exist.** When a
symbol is expected and search finds nothing, re-run with `--no-ignore` before
concluding it is absent.

```bash
rg 'FT_Load_Glyph'                                   # first-party only
rg --no-ignore 'FT_Load_Glyph'                       # including vendored
git grep 'FT_Load_Glyph'                             # git ignores .ignore entirely
```

`git grep` and `git ls-files` are unaffected, which makes them the right tool
when the question is "what does the repository contain" rather than "where is
our code". `.github/` is explicitly un-hidden, since `rg` skips dot-directories
by default and the CI workflows are first-party files worth finding.

## Crates

| Crate | Role |
| --- | --- |
| `sonicterm-types` | Shared contract types and trait seams. |
| `sonicterm-resource` | Resource governor: ledger, owner hierarchy, reservations, reaper. |
| `sonicterm-vt` | VT/ANSI parsing, including host-aware OSC 7 state. |
| `sonicterm-grid` | Cells, scrollback, dirty rows. |
| `sonicterm-cfg` | Config, themes, keymaps, URL/path detection, URI safety. |
| `sonicterm-io` | PTY and process IO. |
| `sonicterm-text` | Glyph atlas and row text cache. |
| `sonicterm-font` | Font discovery, shaping, fallback, rasterization. |
| `sonicterm-font-config` | Font configuration model shared by the font stack. |
| `sonicterm-freetype` | FreeType rasterization FFI wrapper. |
| `sonicterm-harfbuzz` | HarfBuzz shaping FFI wrapper. |
| `sonicterm-fontconfig` | Fontconfig discovery FFI wrapper (non-macOS). |
| `sonicterm-engine` | Font-facing engine seam (`FontStack`, cell metrics). |
| `sonicterm-block-glyph` | Box/block/Powerline/Braille geometry. |
| `sonicterm-render-model` | Renderer-agnostic frame data. |
| `sonicterm-ui` | Tabs, palette, search, selection, IME. |
| `sonicterm-gpu` | wgpu renderer. |
| `sonicterm-app-core` | Winit-independent reducer/state. |
| `sonicterm-app` | Cross-platform app orchestration, path probes, and native direct-open. |
| `sonicterm-mac` | macOS binary/glue. |
| `sonicterm-windows` | Windows binary/glue. |
| `sonicterm-linux` | Linux binary/glue and package metadata. |
| `sonicterm-logging` | Logs, panic hook, exit tracing. |

`crates/sonicterm-winit/` is the retained Windows/macOS/Linux source subset of
upstream `winit` 0.30.13, not a first-party workspace package. It keeps its
upstream identity, license, and source pin while Cargo patches it locally.
Examples, historical documentation, and non-desktop backends are not retained.

## Local gate

`scripts/local-gate.py` is the one runnable definition of the local gate. Run it
from the repository root:

```bash
python3 scripts/local-gate.py
```

<!-- local-gate:begin -->

| Step | Command | Local hosts | Class | Needs | CI jobs |
| --- | --- | --- | --- | --- | --- |
| `pty-close-baseline` | `cargo test -p sonicterm-app --lib pty_close_baseline -- --ignored --nocapture` | macOS, Windows, Linux | `local` | `rust`, `native` | `macos-core`, `windows-tests`, `linux-core` |
| `fmt` | `cargo fmt --all --check` | macOS, Windows, Linux | `local` | `rust` | `macos-core`, `windows-checks`, `linux-core` |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` | macOS, Windows, Linux | `local` | `rust`, `native` | `macos-core`, `windows-checks`, `linux-core` |
| `doc` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | macOS, Windows, Linux | `local` | `rust`, `native` | `macos-core`, `windows-checks`, `linux-core` |
| `doc-resource-features` | `RUSTDOCFLAGS="-D warnings" cargo doc -p sonicterm-resource --all-features --no-deps` | macOS, Windows, Linux | `local` | `rust` | `linux-core` |
| `authored-comments` | `bash scripts/check-authored-rust-comments.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-checks`, `linux-core` |
| `script-identifiers` | `bash scripts/check-script-identifiers.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-checks`, `linux-core` |
| `no-raw-exit` | `bash scripts/check-no-raw-process-exit.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-checks`, `linux-core` |
| `rust-version` | `bash scripts/check-rust-version.sh` | macOS, Windows, Linux | `local` | `rust`, `bash` | `macos-core`, `windows-checks`, `linux-core` |
| `window-owner` | `bash scripts/check-window-owner-registration.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-checks`, `linux-core` |
| `workflow-supply-chain` | `bash scripts/check-workflow-supply-chain.sh` | macOS, Windows, Linux | `local` | `rust`, `bash` | `macos-core`, `windows-checks`, `linux-core` |
| `workspace-crates` | `bash scripts/check-workspace-crates.sh` | macOS, Windows, Linux | `local` | `rust`, `native`, `bash` | `macos-core`, `windows-tests`, `linux-core` |
| `doctests` | `cargo test --workspace --doc --no-fail-fast` | macOS, Windows, Linux | `local` | `rust`, `native` | `macos-core`, `windows-tests`, `linux-core` |
| `pty-feasibility` | `bash scripts/pty-backend-feasibility.sh --check` | macOS, Windows, Linux | `local` | `rust`, `bash` | `macos-core`, `windows-tests` |
| `resource-inventory` | `bash scripts/test-resource-inventory.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-tests` |
| `resource-baseline-tests` | `bash scripts/test-resource-baseline-evidence.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-tests` |
| `soak-harness` | `bash scripts/test-soak-harness.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-tests` |
| `linux-packages-tests` | `bash scripts/test-linux-packages.sh` | macOS, Windows, Linux | `local` | `bash` | `linux-core` |
| `release-assets-tests` | `bash scripts/test-release-assets.sh` | macOS, Windows, Linux | `local` | `rust`, `bash` | `linux-core` |
| `release-notes-tests` | `bash scripts/test-release-notes.sh` | macOS, Windows, Linux | `local` | `bash` | `macos-core`, `windows-tests`, `linux-core` |
| `wiki-publish-tests` | `bash scripts/test-wiki-publish.sh` | macOS, Windows, Linux | `local` | `rust`, `bash` | `macos-core`, `windows-tests`, `linux-core` |
| `logic-coverage` | `scripts/rust-logic-coverage.sh` | macOS, Linux | `local` | `rust`, `native`, `llvm-cov` | `macos-coverage` |
| `windows-warp-allocator` | `cargo test -p sonicterm-gpu --test windows_warp_allocator_baseline -- --nocapture` | Windows | `local` | `rust`, `native`, `warp` | `windows-tests` |
| `msi-validator-tests` | `.\scripts\validate-windows-msi_tests.ps1` | Windows | `local` | `pwsh` | `windows-tests` |
| `windows-perf-build` | `cargo build --locked -p sonicterm-app --example perf_scenarios` | Windows | `local` | `rust`, `native` | `windows-tests` |
| `windows-perf-smoke` | `python scripts/perf-compare.py --smoke` | Windows | `local` | `rust`, `native` | `windows-tests` |
| `macos-selection-build` | `cargo build --locked -p sonicterm-app --example native_split_selection` | macOS | `local` | `rust`, `native` | `macos-smoke` |
| `macos-selection-smoke` | `python3 scripts/native-selection-smoke.py` | macOS | `local` | `rust`, `native` | `macos-smoke` |
| `macos-perf-smoke` | `python3 scripts/perf-compare.py --smoke` | macOS | `local` | `rust`, `native` | `macos-smoke` |
| `release-macos` | `cargo build --release -p sonicterm-mac` | macOS | `release` | `rust`, `native` | `macos-smoke` |
| `release-windows` | `cargo build --release -p sonicterm-windows` | Windows | `release` | `rust`, `native` | `windows-smoke` |
| `release-linux` | `cargo build --release -p sonicterm-linux` | Linux | `release` | `rust`, `native` | `linux-packages` |
| `windows-target` | `bash scripts/check-windows-target.sh` | macOS | `optional` | `rust`, `win-target`, `bash` | — |

Classes: `local` steps run by default; `release` steps run with `--with-release`; `optional` steps run with `--with-optional` and never run in CI.

Local hosts are the hosts where the runner selects a step. CI jobs are where CI runs it, which can cover fewer hosts, or none.

Needs:

- `rust`: the Rust toolchain from `rust-toolchain.toml`, with rustfmt and clippy.
- `native`: the platform's native build libraries: Cairo and pkg-config on macOS (`brew install cairo pkg-config`), Cairo from `scripts/setup-windows-cairo.ps1` on Windows, and the packages the `linux-core` job installs on Linux.
- `bash`: `bash` on `PATH`; on Windows, run the gate from Git Bash so Git's `bash` is found first.
- `pwsh`: PowerShell 7 (`pwsh`) on `PATH`.
- `llvm-cov`: `cargo-llvm-cov` at the `CARGO_LLVM_COV_VERSION` that `ci.yml` pins.
- `win-target`: the `x86_64-pc-windows-msvc` standard library (`rustup target add x86_64-pc-windows-msvc`).
- `warp`: a DX12 WARP adapter with allocator reporting.
- Every step also needs Git and Python 3 on `PATH`.

<!-- local-gate:end -->

How the runner executes each step (process groups and leftover detection,
Windows job objects and preparation records, output-path and Git-state rules,
timeouts, the `ci.yml` parity reader, and per-step notes) is on
[`wiki/Local-Gate.md`](wiki/Local-Gate.md). The CI shards, what a green gate does
not prove, and the workflow supply chain are on
[`wiki/CI-and-Coverage.md`](wiki/CI-and-Coverage.md).

**Run the gate to the end, and read its final summary before concluding
anything.** Later steps still run after a failure, a timeout, or a launch error.
The runner selects the host's `local` steps in table order; `--with-release`
adds the host's `release` steps, `--with-optional` adds its `optional` steps,
`--step ID` runs only the named steps, and `--list` prints the selection with
each step's timeout, prerequisites, and CI jobs. A slow machine or a cold build
can report `TIMEOUT` for a step that would pass; rerun it with `--step ID` once
the build is warm. To change a gate step, change the table in
`scripts/local-gate.py` first, then paste the rendered blocks: `--render en` into
this file and `wiki/Development-and-Release.md`, and `--render zh-CN` into
`wiki/Development-and-Release-zh-CN.md`. `scripts/local-gate_tests.py` fails when
the table, `ci.yml`, and those blocks disagree.

**Never merge or enable auto-merge while any required pull-request CI job is
queued, in progress, missing, cancelled, unexpectedly skipped, or failed.** The
macOS, Windows, and Ubuntu jobs must each finish with `SUCCESS` on the exact
reviewed head commit before merge. In particular, Windows must compile and run
its Windows-only tests successfully; green macOS/Ubuntu results, local gates, or
review approval cannot substitute for that result. After merge, verify Wiki
publication before starting the next serialized PR. Successful exact-head PR CI
is the CI gate for PR work; `main` CI is a release-provenance gate only and does
not block the next PR.

**Keep every wait off the main agent.** For each lifecycle that must wait or
monitor — a long local gate, pull-request CI, post-merge Wiki publication,
release-provenance `main` CI, or a release workflow — start one dedicated watcher subagent, not
one subagent per job. Give it an immutable handoff: repository/worktree path,
expected commit SHA, PR number or run ID, exact required jobs or commands,
timeout, and success criteria. The watcher owns that lifecycle until terminal
`SUCCESS`, `FAILURE`, `BLOCKED`, or `STALE`, and reports the expected and observed
SHA, run IDs, every required result, and actionable failure evidence. It must
return immediately when the head changes or a required job fails, is cancelled,
or is unexpectedly skipped; it never follows a replacement run or accepts a
green result by branch name alone.

While the watcher runs, the main agent advances only a non-overlapping item in a
separate worktree based on the current default branch; it never edits the tree
being tested. Watchers do not push, merge, tag, publish, or clean shared state.
Run at most one full Cargo gate or build on the host at once, never share a
`CARGO_TARGET_DIR` between concurrent worktrees, and use heavy-gate time for
research, editing, or lightweight checks. A watcher failure, blocker, or stale
SHA immediately returns the main agent to the current lifecycle.

Concurrency does not relax publication order: do not merge before the current
PR's exact-head checks pass, and do not open the next PR before the current PR is
merged and its exact merge-SHA Wiki publication is verified. Do not wait for
`main` CI to advance PR work; require it when validating a release commit. Then
update the next worktree onto the new default-branch tip and rerun affected
validation before publication. Once those gates pass, fetch and prune the
default remote, then clean local state against its symbolic default branch:
remove only clean, unlocked worktrees whose HEAD is merged there, and delete
only merged local branches that are not attached to a preserved worktree. Never
force removal or discard dirty, unmerged, or locked worktrees or any stash.

Before trusting a green run:

- `rust-logic-coverage.sh` gates a deterministic-logic subset at 80% and skips
  9 of the 23 crates outright, including `sonicterm-app` and `sonicterm-gpu`, so
  a passing subset figure says nothing about code in those crates. The per-crate
  floor in `scripts/coverage-baseline.json` catches regressions, not low coverage.
- Tests behind `#![cfg(target_os = "windows")]` compile to nothing on macOS, so a
  Windows-gated test file that would fail to *compile* still reports `ok`
  locally. The optional `windows-target` step narrows that gap by linting and
  compiling for the Windows target from macOS; Windows CI is the only place
  Windows code runs.

For release prep also run the host's `release` step
(`python3 scripts/local-gate.py --with-release`) and, on Windows, the
release-blocking `windows-warp-allocator` step. Release packaging starts only
after provenance validation finds an exact successful `main` CI run for the tag
commit, so a failed WARP baseline cannot reach the Windows build or publication.

Before opening a release PR, verify that README and `wiki/` match any changed
config, logging, window, palette, or input behavior.
After pushing a release tag, verify the GitHub release workflow finishes and
publishes two macOS DMGs, the Windows MSI, Linux `.deb` and `.tar.gz`,
`release-assets.json`, and `SHA256SUMS.txt` from the exact validated upload list.

## Debugging

Work a defect in this order. **Baseline → Theory → Probe → Log → Confirm →
Fix → Pin.** Skipping a step does not save time; it produces a confident wrong
answer that costs more to retract than it did to reach.

- **Baseline** — measure the working case first, on the same subject you will
  measure after. A delta against a different subject is an invalid inference,
  not a weak one.
- **Theory** — name the mechanism and the reading that would refute it, before
  instrumenting. A theory no measurement can refute will survive every one.
- **Probe** — instrument the exact transform, logging its input and output on
  one line. A probe placed upstream records what arrived, not what the block
  did, and every conclusion drawn from it describes the wrong code.
- **Log** — record identity, not only magnitude. Sizes say something changed;
  names, ids, and indices say what.
- **Confirm** — vary exactly one thing, and check that the numbers fit the
  mechanism's shape. Arithmetic refutes a wrong mechanism before any code is
  read.
- **Fix** — at the seam the evidence implicates. One symptom can have two
  independent mechanisms; do not collapse them into the first plausible cause.
- **Pin** — a regression test that fails before the change and passes after.
  Without it there is a claim, not a fix.

Four rules carry the same weight as the steps:

- **Use the normal color profile for visual tests.** Never launch SonicTerm with
  `NO_COLOR` or another no-color profile; it invalidates terminal color and visual
  behavior. Remove inherited no-color overrides before starting the app.
- **Isolate the diagnostic run.** Point config and logs at scratch directories.
  Override those specific directories, not `HOME` — the shell inside the
  terminal inherits it, so a scratch `HOME` changes the repro itself.
- **Say what did not reproduce.** A run that reproduces the baseline but not
  the defect has bounded the problem. Silence about the gap is how a wrong root
  cause survives.
- **Retract explicitly when superseded.** Name the dead claim and the
  measurement that killed it. A thread that only accumulates is unreadable to
  whoever arrives next.

Instrumentation is not free of effect: a probe can hide a race by perturbing
its timing, so a non-reproduction under instrumentation is weaker evidence than
a reproduction.

## Conventions

- **A change that alters documented behavior updates the wiki in the same PR.**
  Config keys and defaults, log fields and levels, keybindings, CI gates,
  rendering and pacing behavior, packaging steps, and crate roles are all
  described on wiki pages, and a page that describes the old behavior is worse
  than no page — a reader trusts it and is wrong. Update `CLAUDE.md` too when
  the change touches something it states: the crate table, the local gate, or a
  convention. Both language files, in the same commit as the behavior.

  This is not hypothetical drift. `Development-and-Release` asserted that CI ran
  neither `cargo fmt` nor `cargo clippy` long after both had become jobs, so a
  contributor reading it would have believed formatting was unchecked.

- **The wiki describes what the project is and does, not what it might become.**
  No roadmaps, phase plans, proposal lists, or sequencing documents. Those decay
  into fiction the moment priorities move, and a reader cannot tell a stale plan
  from a current one. Design intent belongs next to the mechanism it explains —
  why the governor accounts rather than enforces, why the software path is
  paced differently — not in a document about future work. Track intended work
  in issues, where it has a state.

- **Every issue and pull request carries labels and a milestone.** Set them
  when you open the item, not afterwards: an unlabelled issue with no
  milestone does not appear in the filtered views the work is tracked
  through, so it is invisible to everyone who is not reading the raw list.
  Pick labels that describe the change — `bug`, `enhancement`,
  `documentation`, `chore`, `refactor`, `perf`, `regression` — plus a
  `platform:` label when the change is not cross-platform. Use the milestone
  the work ships in. `gh issue create` and `gh pr create` take `--label` and
  `--milestone` directly; `gh issue edit` and `gh pr edit` fix an item that
  was opened without them.
- **Workflows use GitHub Actions platform limits without job or step
  `timeout-minutes` overrides.** This applies to CI, Performance comparison, Release and Wiki publication.
  Local and native process deadlines are independent and remain required. Scripts that capture child-process output must bound and reap the
  child process tree so timeout evidence and checksums survive. Keep the timeout
  policy tests green when adding or renaming workflow jobs and steps.
- **Performance is measured in CI: a 30-minute PR pipeline and an unbounded
  release pipeline. Local runs only make sure it works.** Every before/after
  perf number comes from the `Performance comparison` workflow
  (`.github/workflows/perf.yml`) on GitHub-hosted macOS runners, never from a
  local comparison: a developer's Mac is in use, and its input, focus changes
  and load invalidate runs or widen the noise.
  - **PR pipeline:** runs for every PR labelled `perf` and must finish within
    30 minutes. It compares the merge base with the head using `--short --runs 5`
    and a release profile without LTO (the same for both refs), split across
    five parallel jobs. Its table is the PR's before/after evidence. Keep it
    within 30 minutes when you add scenarios or change the workflow: rebalance
    the shards or shorten the runs, never drop the budget.
  - **Release pipeline:** runs for each pushed release tag and may take hours.
    It compares the previous release tag with the new one using full-length
    runs and the shipping release profile. Put any long or exhaustive perf
    measurement here, not in the PR pipeline.
  - **Locally:** build and run the functional checks only. The local gate,
    including `macos-perf-smoke` and `windows-perf-smoke`, proves the tooling
    works and asserts no timing.
  - **Windows:** `perf.yml` runs on macOS only, and a GitHub-hosted Windows
    runner renders on a software adapter, so its timings say nothing about a
    Windows host. Windows numbers come from an A/A or before/after comparison
    on an idle Windows host with no user input during the runs, and the PR
    names that host. The Windows CI smoke checks the tooling, the wgpu
    presenter and role-exit handling, never timing.
- **Flowcharts and data-flow diagrams in markdown are `mermaid` fenced blocks.**

  Hand-drawn ASCII loses alignment across fonts and cannot be edited without
  redrawing the whole picture. Directory trees and layout wireframes stay as
  plain text — their meaning lives in the character positions, which Mermaid
  discards. Keep each diagram in both the English `<Page>.md` and Chinese
  `<Page>-zh-CN.md` files, with localized labels and identical structure.
- **Keep first-party shell automation flat in `scripts/`.** Every SonicTerm-owned
  `.sh` and `.ps1` file must be a direct child of `scripts/`; do not create
  nested script folders or top-level `tools/` or `packaging/` directories.
  Embedded upstream FreeType, libpng, zlib, and HarfBuzz scripts retain their
  vendored layouts and are exempt. Packaging executables belong in `scripts/`,
  while maintained packaging instructions belong on the `Packaging` wiki page.
- **Every code change gets an authored-comment pass, including unit tests.**
  Before declaring work complete, inspect every changed Rust implementation and
  unit-test file for missing comments. Public APIs require concise purpose
  Rustdoc; non-obvious control flow, invariants, ownership, units, platform
  behavior, ignored failures, safety, and performance constraints require
  rationale at the relevant code. Every added or materially changed unit test
  requires a concise comment stating the behavior or contract it protects and
  explaining any non-obvious setup or assertion. Passing the comment checker is
  only the minimum; its test exclusions are not permission to leave tests
  undocumented. Do not add comments that merely restate syntax.
- **Unit tests use the exact flat `file_tests.rs` sibling pattern, never inline.**
  For every source file `foo.rs`, put its unit tests beside it in
  `foo_tests.rs` and declare them from `foo.rs` with
  `#[cfg(test)] #[path = "foo_tests.rs"] mod foo_tests;`. Crate-root tests use
  `lib_tests.rs` (or `main_tests.rs` for a binary) with the same declaration
  pattern. Do not use `#[cfg(test)] mod tests { … }`, a generic `tests.rs`, or a
  `<module>/tests.rs` subdirectory. Tests stay in-crate for private-item access
  via `use super::*;`; Rust does not discover sibling files automatically, so
  every `file_tests.rs` requires its source-module declaration.
- **`tests/` is for cross-crate integration only.** Reserve each crate's
  `tests/` directory for genuine integration tests that exercise the crate
  through its public API or across crate boundaries. Do not put trivial
  "does this symbol export" checks there — fold those into `lib_tests.rs`.
- **Some test state is process-global; inject the pool.** Production composes
  VT capture staging and inline media against one process-default pool each
  (`CaptureStagingPool`, `InlineMediaPool`), so a test that relied on a default
  pool would see captures and panes its siblings create. A test that needs a
  capture admitted, or that measures admission, budgets, or totals, injects a
  private pool instead — `Parser::new_with_staging_pool`,
  `App::with_capture_staging_pool`, `App::with_inline_media_pool`, or
  `PaneState::new_with_media_pool` — and no unit test measures a default pool.
  Staging a capture on the process-default pool panics under the VT crate's
  unit tests. What stays process-global is the default pools themselves and
  `NEXT_IMAGE_ID`. The capture-staging heap-truth test measures the default
  staging pool under its own lock; the inline-media heap-truth test checks the
  retained-media figure against the real heap, not the media pool's totals.
  Tests that assert on captured tracing output use
  `sonicterm_logging::test_capture`. Its `with_default` wrapper preserves each
  test's subscriber, filter, and sink while a silent process-global dispatcher
  prevents uncaptured first reaches from disabling call sites. Do not combine
  it with production logging initialization in the same test process.
- **Authored Rust comments are enforced contracts.** Effectively public
  functions and public trait functions require concise purpose Rustdoc; public
  unsafe functions also require a `# Safety` section. Objective control-flow
  boundaries require substantive `// When:` rationale, while mechanical value
  selectors remain checker advisories. Every unsafe boundary requires
  `// SAFETY:`, functions that order distinct locks require `// Lock order:`,
  non-`SeqCst` atomic protocols require `// Ordering:`, and `Drop`
  implementations require `// Lifecycle:`. Keep each marker at the exact anchor
  accepted by `scripts/check-authored-rust-comments.sh`, bind its prose to the
  relevant identifiers, and keep it within two comment lines and 160 characters.
  The checker excludes vendored, generated, preserved-upstream, build, and
  ordinary test contexts from non-safety rules; unsafe constructs still require
  `// SAFETY:` in test code.
- **Comments describe behavior, not history.** Explain what the code does and
  the problem it solves; do not cite issue/PR/Epic numbers or reviewer names
  in comments, log strings, or panic messages.
- **Names say what they hold.** Variables, parameters, closures, loop bindings,
  fields, functions and constants are never one character, a letter followed
  by digits, or two letters outside `allowed-idents-below-min-chars` in
  `clippy.toml`, in production code and in tests. Name the quantity and its
  unit: `row_count`, `timeout_s`, `width_px`. Exempt: generic type parameters,
  lifetimes, const generics, `_`, vendored code, generated FFI bindings,
  `extern` declarations and `#[repr(C)]` fields that copy a C header, and names
  fixed by an external contract (serde keys, log fields, config keys, CLI
  flags). Clippy's `min_ident_chars` enforces the rule in each crate that
  enables it. `scripts/regenerate-freetype.sh` and
  `scripts/regenerate-harfbuzz.sh` rewrite `crates/sonicterm-freetype/src/lib.rs`,
  `crates/sonicterm-freetype/src/types.rs` and
  `crates/sonicterm-harfbuzz/src/lib.rs`, so those generated bindings carry no
  naming-lint attribute; the hand-written modules in those two crates enable the
  lint with an inner `#![warn(clippy::min_ident_chars)]`.
  `scripts/check-script-identifiers.py` (the `script-identifiers` gate step)
  enforces the rule for the tracked `scripts/*.py` files.

## Release

SonicTerm releases are created by pushing a tag matching
`v[0-9]+.[0-9]+.[0-9]+*`; validation peels that ref to its commit, then requires
that exact commit in `main`, a completed successful `CI` push run for it, all
source-consistency gates, and a supported semantic version matching every
workspace package. The tag workflow builds:

- macOS Apple Silicon and Intel `.dmg` files
- Windows x64 `.msi`
- Linux x86_64 `.deb` and `.tar.gz`
- a validated asset manifest, checksums, and release notes from commits since the previous reachable tag

Release notes also list deduplicated resolved issues proven by GitHub closure
metadata and exact base-exclusive/head-inclusive ancestry, including merge
commits. Milestones, current closed state, and mentions alone are not proof.
Null-closer manual events are disclosed separately as unverified release linkage,
only for current closures inside the base/head commit-date window; valid dates
and unique matching closure metadata are mandatory. They never enter the verified list.
Canonical Git revert markers cancel reverted contributions; unknown metadata,
ambiguous provenance, and collector caps fail generation rather than imply an
empty result. The publish job alone grants issue/PR read access alongside its
existing contents write access and passes its short-lived token as `GH_TOKEN`.
Before tagging, preview the exact reviewed merge commit without creating a tag:
`python3 scripts/release-issues.py --repo D0n9X1n/SonicTerm --head <merge-sha> --base <previous-tag>`.
The shell generator rejects shallow history and fails when predecessor lookup
fails; only explicit `RELEASE_FIRST=1` permits no-base notes, and it conflicts
with any set `PREVIOUS_TAG`. The full rule, bounds, and limitations are on
`wiki/Release-Process.md`; `bash scripts/test-release-notes.sh` includes
offline real-history/fake-API tests.

## Wiki

The repository-tracked `wiki/` directory is the **only source of truth** for
SonicTerm's documentation. Edit and review wiki pages in the same branch and
pull request as the behavior they describe.

The GitHub wiki is a **published mirror** of that directory. The
`publish-wiki.yml` workflow runs after every push to `main` — including every
merged pull request — and on manual dispatch. It uses `scripts/publish-wiki.sh`
to replace the flat Markdown page set, so renames and deletions are mirrored and
an unchanged run is a successful no-op. The wiki's rendered branch is **`master`**,
not `main`.

Publication uses the workflow's short-lived, repository-scoped
`GITHUB_TOKEN` with `contents: write`; no PAT, GitHub App key, or long-lived
secret is stored. Do not replace it with an account-wide classic PAT.

Never edit the GitHub wiki directly. Edits made in its web UI are not tracked,
are not reviewed, and are overwritten on the next publish. Do not maintain any
other wiki repository or pull wiki content back into `wiki/`.

A merged pull request is not complete until its wiki publication run is verified:

```bash
gh run list --workflow=publish-wiki.yml --limit 3
gh run view <run-id>
tmp="$(mktemp -d)"
git clone "https://github.com/D0n9X1n/SonicTerm.wiki.git" "$tmp/wiki"
git -C "$tmp/wiki" log -1 --oneline
ls "$tmp/wiki"
```

The newest successful run must correspond to the merge SHA. When the source
wiki changed, the newest wiki commit must correspond to that SHA; an unrelated
merge is expected to complete as a successful no-op without a wiki commit.
Inspect the live Wiki and click representative English and Chinese navigation
links; a successful workflow exit alone does not prove that the published pages
render or link correctly.

## WezTerm

SonicTerm thanks WezTerm and uses it as the reference for terminal behavior,
font behavior, keymap conventions, and rendering edge cases. Absorb proven
behavior into Sonic-owned crates; do not reintroduce a `vendor/` dependency.
