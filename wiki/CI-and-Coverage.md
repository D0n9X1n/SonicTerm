# CI and Coverage

[简体中文](CI-and-Coverage-zh-CN)

This page covers pull-request and `main` CI, the coverage gate and what a green
gate does not prove, and the workflow supply chain. Coverage evidence and
rebaselining are on
[Development and Release](Development-and-Release#coverage-evidence-and-rebaselining).

## Pull-request and main CI

`.github/workflows/ci.yml` runs on pull requests and pushes to `main`. Pull-request
runs share a ref-specific concurrency group and cancel an obsolete run when that
ref advances. Each `main` push instead has a SHA-specific group and never
cancels in progress, so a later merge cannot erase the exact-SHA verification
record for an earlier one.

`.github/workflows/perf.yml` (`Performance comparison`) runs separately and is
not a required job. It measures a quick before/after table, within 35 minutes
of the run's creation, queue included, for pull requests labelled `perf`, and the
full comparison for each release tag, on GitHub-hosted macOS and Windows
runners, with the frame-counter set and its tables on both. One macOS job builds
both refs once and publishes the binaries as a one-day artifact bound to the
run, its attempt and a manifest digest; the macOS comparison shards measure
those binaries, and each Windows shard builds its own. Every comparison passes
`--require-base`, so a base that cannot build or measure fails its shard. The
last job checks out nothing and passes only when the build job and both
comparison jobs succeeded. Only an eligible run names it `Performance
comparison result`; an ineligible run's skipped result shows its unevaluated
name expression, never the real name. Merge evidence is that job's SUCCESS in the exact
eligible run on the exact head, read by run id, never by check name alone; a
superseded run is never counted as success. [Development and
Release](Development-and-Release#what-ci-measures) describes both modes, the job
layout and the concurrency.

Never merge or enable auto-merge while a required pull-request job is queued,
in progress, missing, cancelled, unexpectedly skipped, or failed. The macOS,
Windows, and Ubuntu jobs must each finish successfully on the exact reviewed
head commit before merge. Windows success is mandatory because that job is the
only reliable compiler and runner for Windows-only tests; local, macOS, Ubuntu,
or review results cannot substitute for it. After every merge, verify Wiki
publication before starting the next serialized pull request. Successful
exact-head PR CI is the CI gate for PR work; `main` CI is a release-provenance
gate only and does not block the next PR.

Keep every wait off the main agent. For each lifecycle that must wait or monitor
— a long local gate, pull-request CI, post-merge Wiki publication,
release-provenance `main` CI, or a release workflow — start one dedicated watcher subagent, not one subagent
per job. Give it an immutable handoff: repository/worktree path, expected commit
SHA, PR number or run ID, exact required jobs or commands, timeout, and success
criteria. The watcher owns that lifecycle until terminal `SUCCESS`, `FAILURE`,
`BLOCKED`, or `STALE`, and reports the expected and observed SHA, run IDs, every
required result, and actionable failure evidence. It returns immediately when
the head changes or a required job fails, is cancelled, or is unexpectedly
skipped; it never follows a replacement run or accepts a green result by branch
name alone.

While the watcher runs, the main agent advances only a non-overlapping item in a
separate worktree based on the current default branch; it never edits the tree
being tested. Watchers do not push, merge, tag, publish, or clean shared state.
Run at most one full Cargo gate or build on the host at once, never share a
`CARGO_TARGET_DIR` between concurrent worktrees, and use heavy-gate time for
research, editing, or lightweight checks. A watcher failure, blocker, or stale
SHA immediately returns the main agent to the current lifecycle.

Concurrency does not relax publication order: do not merge before the current
pull request's exact-head checks pass, and do not open the next pull request
before the current one is merged and its exact merge-SHA Wiki publication is
verified. Do not wait for `main` CI to advance PR work; require it when validating
a release commit. Then update the next worktree onto the new
default-branch tip and rerun affected validation before publication. Once those
gates pass, fetch and prune the default remote, then clean local state against
its symbolic default branch. Remove only clean, unlocked worktrees whose HEAD is
merged there, and delete only merged local branches not attached to a preserved
worktree. Never force removal or discard dirty, unmerged, or locked worktrees or
any stash.

### macOS 14 and Windows latest

The stable required checks are fail-closed aggregate jobs: `macos-14 / unit
tests` requires `macos-core`, `macos-smoke`, and `tooling`, while
`windows-latest / unit tests` requires `windows-native`, `windows-glyph-oracle`, `windows-checks`, the
five Windows test shards (`windows-tests-workspace`, `windows-tests-harness`,
`windows-tests-harness-features`, `windows-tests-harness-api` and
`windows-tests-runtime`), and `windows-smoke`; its result loop tests every one.
Each aggregate runs with `if: always()` and accepts only explicit `success`
results, so a failed, cancelled, or skipped shard cannot turn into a successful
required check.

The macOS core shard first measures the real PTY close baseline after Cargo
restore, then runs Rust checks, strict Rustdoc, the one-pass workspace test gate,
workspace doctests, host probes, the macOS-only process-group custody test, and real
resource-baseline capture. The required `tooling` job runs on Ubuntu: Rust-version,
raw-exit, window-owner, workflow supply-chain, authored-comment and script-identifier
checks, plus release-note, wiki-publish, resource-inventory, soak and resource-baseline
script tests. These commands remain local gates on every supported host; the existing
macOS aggregate requires the tooling job, so none became optional. The working-set
measurement stays in macos-core to reuse its compiled harness without adding a cold
macOS build or competing with perf for another runner slot. Logic coverage runs in its own
`linux-coverage` job, in the `ubuntu:22.04` container `linux-core` uses, outside the macOS
runner pool the perf comparison waits for. The container gives its tests the workspace
gate's Cairo, FreeType and Fontconfig; the `ubuntu-latest` host's newer Cairo draws COLR
sweep gradients differently. The job names that container as its image identity, since
the runner does not export one into a container. It installs the pinned `cargo-llvm-cov`
release's prebuilt static x86_64 Linux binary with
`scripts/install-pinned-tool.sh`, which checks that host's SHA-256 before unpacking it
(expected to save the minute a source build takes), then runs the deterministic logic coverage gate, and uploads its
evidence artifact after success and after failure once the coverage step has started.

Because the floors are measured on Linux, the per-crate floors in
`scripts/coverage-baseline.json` no longer measure macOS-only code: CoreText font
discovery, the macOS PTY and IO paths, macOS logging, and the `sonicterm-mac` binary
compile out there, and the Linux-only code (Fontconfig discovery, `sonicterm-linux`)
is measured instead. The 80% gate and its crate list are unchanged. Platform-independent
logic that used to sit behind `cfg(target_os = "macos")`, the UTF-16 to UTF-8 boundary
conversion and the `open` and `xdg-open` URL command builders, now compiles and is
tested on every Unix host, so the Linux run measures it. The macOS-only logic left in
the 80% subset, the AppKit word boundary in `sonicterm-ui`'s `text_edit.rs` and the
macOS arms of `command_label.rs`, keeps its own floor: `macos-core` installs the macOS
pin of the same tool, runs `scripts/macos-logic-coverage.sh`, which instruments
`sonicterm-ui` alone, holds each file to the baseline's `macos` section with the same
1 pp tolerance, and uploads its report and a provenance record after success and after
failure; only that record, verified, can change the section. A file floor
catches a loss larger than the tolerance: losing the AppKit boundary's coverage, 7 of
`text_edit.rs`'s 160 lines, moves it by about 4 pp, while one macOS arm of `command_label.rs` is below 1 pp
of that file and is not caught.

The
native runtime smoke builds shipping release binaries on macOS 14 Apple Silicon,
as steps at the end of `macos-core`, and on macOS 15 Intel, in the one-leg
`macos-smoke` matrix, with distinct dependency-cache keys. The Intel lane may save
dependencies only on a push to `main`; the Apple Silicon lane shares `macos-core`'s
cache entry. It has no job of its own because the repository runs at most five macOS
jobs at once, and a separate leg would hold a slot the perf comparison's shards wait
for. Those steps follow the core gates, so an earlier core-gate failure skips them;
the Intel lane still reports on its own.
Before the release build, both lanes build and run the native split-selection
fixture ([Native split selection](Local-Gate#native-split-selection)), then the
performance scenario smoke, which checks the comparison tooling without timing
([Performance scenario smoke](Local-Gate#performance-scenario-smoke)); when
either smoke fails, the job uploads its evidence. After the release build, both
lanes require the bounded raw-binary smoke, then build and mount a DMG on that
same architecture.
Separate steps with native process deadlines also require the raw binary's
`frame-validation` and `device-recovery` scenario smokes.
The installed bundle passes relative-library closure, signature, deployment-floor,
Homebrew-denied runtime/Cairo drawing, and exact bundled-font registration checks;
a controlled same-binary image pair records compressed font savings. The macOS
aggregate requires both matrix lanes. Release jobs also package on their matching
architecture; the final macOS artifact job collects already-validated DMGs.

Windows first prepares static Cairo through vcpkg. It restores the binary cache,
builds a cold miss, and saves that result immediately before the seven dependent
jobs start. A restored fallback archive may contain no compatible packages
after a hosted-image or vcpkg revision change, so consumers still run Cairo
installation and may perform a cold build. CI does not override job or step
timeouts. The runtime and workspace shards have separate writable target
directories, so the baseline's build products do not warm the workspace shard.
The checks shard runs format, Clippy, source-policy, comment,
script-identifier, and Rustdoc gates. The Windows tests run as five parallel shards, each repeating
checkout, Rust, Cairo and Cargo-cache setup, so CI's wall time follows the longest shard instead of their sum:

- `windows-tests-workspace` runs the one-pass
  workspace tests, doctests, the MSI validator and the tooling tests, and captures and uploads real
  resource-baseline evidence.
- `windows-tests-harness` runs the scenario harness's unit tests, then the same tests with frame counters.
- `windows-tests-harness-features` runs the harness tests with the frame-texture reading and with the echo trace.
- `windows-tests-harness-api` runs the harness tests with every harness API cfg; only the example receives those flags.
- `windows-tests-runtime` first measures the real PTY close baseline after Cargo restore, so its test-binary
  build stays cold, then runs the host probes, fail-closed GDI presentation verification, the WARP allocator,
  software-selection presentation, and the perf scenario harness build and its smoke, which checks the
  comparison tooling on a software adapter without timing ([Windows](Local-Gate#windows)); when the GDI probe or
  the perf smoke fails, the shard uploads its evidence. Its probe reruns print reports for integration tests that
  `windows-tests-workspace` already runs. The GDI wrapper accepts only one `capability=EXERCISED` verdict;
`HOST_INCAPABLE` remains informational and cannot satisfy the gate. The
`windows-smoke` shard builds the shipping release binary and
requires its bounded native smoke plus `frame-validation` and `device-recovery`
scenario smokes in separate steps with native process deadlines.

`workflow-syntax` runs pinned actionlint against CI and perf workflows, checking GitHub
expression-context availability as well as syntax. The tooling job installs its digest-pinned
Linux binary; local hosts install the same version on PATH. The supply-chain script also runs
it whenever available; its absence cannot bypass the separate required syntax gate.

Windows cfg-on tests run the exact Cargo-reported libtest executable under process-tree custody.
The launcher restores Cargo build-script native directories and profile/deps paths on PATH,
and `setup-windows-cairo.ps1` publishes the selected triplet's bin directory as
`SONICTERM_CAIRO_BIN_DIR` and on PATH. Static Cairo remains the default; dynamic triplets
therefore retain their DLL lookup path too.

`windows-glyph-oracle` runs the unchanged ignored working-set gate once. Only after every
completeness, exact drift and start-size check passes does it publish the four S9/S12,
72/144-DPI helper tile sets. The manifest binds all tracked source bytes (including bundled
fonts), system/user font bytes and GDI registry mappings, runner image, Rust/Cargo versions, checkout SHA, run ID and
producer attempt. Consumers verify its output digest and all bindings before testing.
Failed-job reruns name the successful producer's attempt, not their own. Missing or stale
artifacts fail; local runs without an artifact still measure the helper directly.
Workspace, ordinary/counters, frame-texture/echo and API-cfg suites each draw their own real
renderer, retain all completeness/representative-glyph checks, and compare every resident
identity and actual size against the helper. Font content hashes replace machine-local paths;
built-in font identity is bound by the source digest, and GDI memory-face names by the full font inventory.

Windows cache families are `check-windows-latest` (written by `windows-checks`),
`test-windows-latest` (written by `windows-tests-workspace`, restored by the oracle and all
test shards), and `release-windows-latest` (written by `windows-smoke`). The Linux coverage
job uses its separate `instrumented-ubuntu-22.04` family. Its coverage script still discards instrumented
outputs before measurement, so that family provides no warm instrumented-build guarantee. No workspace artifacts are cached;
every writer saves only after a successful push to `main`, never on a pull request. The
other macOS and Linux cache policies are unchanged. Compiler/environment hashing remains
enabled; a static-check archive can no longer occupy the test-codegen cache key.
Overlapping main runs can still race to save an immutable key.

The three macOS perf shards keep their artifact names and run every scenario
exactly once, in sets of about 19 minutes from run 40344066083's per-variant times:
`S7-S4` runs S7, S4, atlas-retry, S12/status-line, S3, S8 and S6/selection-drag;
`S9-S10sync` runs S10/sync, S9, S11, S6, S1, S5, S6/rest, S1/status-line and S12;
`S10-S11release` runs S10, S11/release, S2, S2/flood and S6/flood. All three start
when the binaries land: this repository runs at most five macOS jobs at once, and CI
holds two (core gates, which also runs the aarch64 smoke, and the Intel smoke). The
35-minute PR budget includes queues; exact-head CI timing, not local estimates,
verifies the remaining margin. Runs, counters, S9 laps and release lengths are unchanged.

A compatible successful `main` job must populate a key before a later run can
hit it; compiler or dependency changes can still cause a miss. Cache reuse can
reduce dependency compilation, not hosted-runner queue time. Cold-cache builds
and every existing test, native and package gate remain required.

CI, Release and Wiki publication have no job or step `timeout-minutes`
overrides; GitHub Actions platform limits still apply. Local and native process
deadlines, output limits, and cleanup policies remain independent. The real resource-baseline collector bounds each focused PTY
command at 30 seconds and its live soak at 90 seconds. A timeout kills the
command's process tree, records exit 124 plus partial stdout/stderr in the
evidence bundle, and continues writing checksums.

Python `*_tests.py` entry points default to verbose `unittest` output: each test's
name is flushed before its body runs, followed by its result. Native dependency
checks emit flushed `[native-dependencies] start NAME` and `finish NAME exit=N`
lines to stderr. The native-smoke CLI emits `[native-smoke] start timeout=Ns`
before launch and `finish exit=N` after capability validation; captured child
output is flushed before the final status. Package checks emit
`[package-check] start LABEL timeout=Ns` and `finish LABEL exit=N`; the font/Cairo
probe instead finishes with `result=PASS` or `result=FAIL` after report validation.
These progress lines are flushed to stderr without changing stdout payloads,
exit codes, or captured log files. The resource-baseline collector also emits
flushed per-command start/finish progress. A start line identifies work in
progress, not a passing check.

### Ubuntu 22.04

The stable `ubuntu 22.04 / workspace, packages, X11, Wayland` aggregate requires
`linux-core`, `linux-coverage`, and `linux-packages`, using the same fail-closed result check as
the macOS and Windows aggregates. The core shard installs the compile-time Linux
dependencies plus Vulkan/lavapipe for GPU tests and adapter probes, measures the
real PTY close baseline after Cargo restore, then runs format, Clippy, Rustdoc (including `sonicterm-resource` with its `test-util`
feature), the one-pass workspace test gate, doctests, authored-comment,
script-identifier, exit, Rust-version, window-owner, workflow supply-chain, Linux-package, release-asset,
release-note, and wiki-publisher checks.

The CI and Release Ubuntu dependency-install steps have no workflow timeout
overrides. Their commands and fail-closed shard results remain unchanged, as does
release provenance.
The independent package/runtime shard installs Mesa Vulkan/lavapipe, Xvfb,
Weston, and Debian packaging tools, then:

1. builds `sonicterm-linux` in release mode;
2. derives one workspace version from Cargo metadata;
3. creates and validates the x86_64 `.tar.gz` and `.deb`;
4. validates desktop/AppStream metadata and runs advisory `lintian`;
5. runs both package layouts on X11/Xvfb and Wayland/Weston with Vulkan/lavapipe,
   in separate default, frame-validation and device-recovery steps;
6. uploads the packages, or scenario-qualified smoke logs on failure.

A default platform smoke cannot pass without a native window, renderer/device, a
platform-shell PTY marker observed in the live grid, a later native frame
presentation, the default warm renderer's create/report/adopt/child-present/
release lifecycle with the process renderer count restored, and the GPU fault
phases: an isolated fault still lets a later frame present, a retained-resource
fault stops every presentation while a re-executed PTY marker still arrives, and
a device destroy is recorded as lost while another marker arrives. Every
invocation uses separate scratch config/log roots and the process-tree-reaping
wrapper; a warm-lifecycle failure exits `16`, a fault-containment failure `17`,
and a device-loss failure `18`. Each fresh frame-validation process instead
requires an initial native presentation, a persistent fault that stops later
presentations, and a newly executed PTY marker after the stop. A separate
device-recovery process proves one shared-device rebuild across two live windows
and a warm renderer, subsequent fresh-marker presentations by the original PTYs,
ignored old-generation events, and renderer release; failure exits `19`. The
containment scenarios keep recovery disabled. All three Linux scenario matrices
run in separate steps with distinct state/log paths; each native process retains
its own deadline. Otherwise successful smoke with unsettled native teardown exits `20`;
earlier failures retain their original code. The core shard is the sole
main-only Linux dependency-cache writer; the package shard is restore-only and
workspace-crate artifacts remain excluded.

macOS and Windows smoke also read back native numbered titles and exercise
Unicode rename/reset on startup and warm-adopted windows. Mismatches fail at the
display boundary (exit `11`). Linux still requires external X11 property or
Wayland compositor-visible evidence: winit's X11 getter is unimplemented and its
Wayland getter is only cached state. These checks do not verify OS switcher labels.

The default Windows smoke additionally installs the production OLE backend. Main, warm-adopted,
and fresh child windows must each register and revoke their custom drop target;
the post-run report requires three successful pairs, zero live registrations and
zero failures before OLE is uninitialized. Windows-only COM tests use hidden HWNDs
and real data objects to verify duplicate-owner refusal, Unicode file delivery,
exact target identity and cleanup. They do not synthesize or verify physical drag
gestures. See [Platform Integration](Platform-Integration).

## Gate blind spots

- The one-pass workspace gate includes integration tests for all 23 packages,
  but it still exercises only targets that can compile and run on its host.
- `rust-logic-coverage.sh` requires 80% line coverage only for its selected
  deterministic subset. Its ignore regex excludes 9 whole crates, including
  `sonicterm-app` and `sonicterm-gpu`, plus named native/controller files in
  other crates. CI runs it on Linux, in the `linux-coverage` job's `ubuntu:22.04`
  container; the local runner selects it on macOS and Linux. A green percentage
  does not cover native windows, real PTYs, GPU surfaces, generated FFI,
  installers, or macOS-only and Windows-only logic.
- The same run reports its profiles again without the ignore regex and prints
  line coverage for every workspace member, and `scripts/coverage-floor.py`
  holds each measured crate to its entry in `scripts/coverage-baseline.json`.
  CI fails when a crate drops more than 1.0 percentage point below its entry
  or has no entry, and when the report disagrees with the workspace members and
  declared not-measured crates: a crate with an entry stops being measured, a
  member is neither measured nor declared, or a declared crate starts reporting
  lines. Test, vendored, generated, and build-script code is excluded with
  printed counts; a crate with no eligible line prints `not measured` with its
  reason. The floor catches regressions, not low coverage: a crate that stays
  low still passes. Every row measures code compiled on Linux: macOS-only font
  discovery, PTY and IO paths, logging, and the `sonicterm-mac` binary compile
  out and are not measured, and the `sonicterm-mac` and `sonicterm-windows` rows
  measure only what compiles there, not execution on those platforms. The
  baseline is keyed to the `linux-coverage` job's x86_64 Linux container; CI on
  another host fails as not comparable, and local runs print informational
  deltas without a verdict. Floor numbers change only in a reviewed diff from a
  retained run's verified evidence, as [Coverage evidence and rebaselining](Development-and-Release#coverage-evidence-and-rebaselining)
  describes.
- `scripts/macos-logic-coverage.sh` in `macos-core` holds the macOS-only logic in
  the 80% subset, the AppKit word boundary in `sonicterm-ui`'s `text_edit.rs` and
  the macOS arms of `command_label.rs`, to per-file floors in the baseline's
  `macos` section. CI enforces them only on the macOS 14 arm64 runner, and they
  change only from a verified `macos-core` record. The run instruments
  `sonicterm-ui` alone, so macOS-only code in other crates has no floor, and a
  file floor misses a loss smaller than the 1.0 percentage point tolerance, such
  as one macOS arm of `command_label.rs`.
- `cargo deny --locked check` enforces the advisory, license, ban, and source
  policy in `deny.toml`, in the `linux-core` job and in the local gate, for every
  target. A green audit does not mean the ignored advisories are gone: each
  `ignore` entry names its dependency path, its upstream issue, and the release
  that removes it.
- Native AppKit, Win32, X11/Wayland, font-discovery, PTY, GPU, and installer
  behavior still depends on platform tests, package smokes, release builds, and
  manual use; a symbol-only test cannot prove those boundaries.

## Workflow supply chain

`scripts/check-workflow-supply-chain.sh` enforces action pins and token scope
locally and in macOS, Windows, and Ubuntu core/checks shards. Release requires
an exact successful `main` CI run containing these checks before platform jobs.

**Pin every remote action to a full lowercase 40-character commit SHA**, followed
by its release comment `# vX.Y.Z`. Unlike tags or branches, that identity cannot
be retargeted without a reviewed change here. The checker rejects tags, branches,
abbreviated or uppercase SHAs, tag-pinned `docker://` references, and two different
pins for the same action. Local `./` actions need no pin: their code is reviewed
in the same PR.

`dtolnay/rust-toolchain` is pinned to a full commit SHA with a `# v1` version
comment, like the other remote actions. Every call passes `toolchain: stable`
explicitly. The action implementation is immutable; the requested Rust channel
still follows stable. A version comment is not a mutable tag reference.

**`contents: write` exists only on the job that publishes.** Every workflow
defaults to `contents: read`, and only `release.yml`'s and `publish-wiki.yml`'s
`publish` jobs re-grant write, at job scope. The token a third-party action
inherits is the job's, so a workflow-level write grant hands repository write
access to every action in every job — including the ones that only compile and
package. The release consequence is concrete: the uploader runs after checksum
consolidation, so a write-capable token in a build job could publish bytes
other than the validated set. The permitted jobs and their exact writable scopes are enumerated in
`WRITE_BOUNDARY` in `scripts/check-workflow-supply-chain.py`; adding one is a
reviewable edit to that list, not an unnoticed line in a workflow.

**A workflow downloads no executable itself.** Any `curl` or `wget` in a `run`
block fails the checker, except the exact read-only GitHub API call listed in
`API_READS`. So do `gh release download`, `cargo binstall` of any tool, Python
started with inline source (`-c` or a heredoc) that names `urllib`,
`urlretrieve`, `requests` or `http.client`, and an `actions/download-artifact`
step with a `repository` or `run-id` input, which pulls from another repository
or run; artifacts built earlier in the same run are allowed. Text cannot prove
what a shell will run, so the rule does not try to judge an inline installer.
The rule is a guardrail against common mistakes, not a security boundary: a
determined workflow author can still fetch a file another way, and reviewing
workflow changes is the control. Prebuilt tools (`cargo-deny`, `cargo-llvm-cov`, `actionlint`) are
installed only by `bash scripts/install-pinned-tool.sh <tool>`. Its only input is
the tool name: it reads the version, release URL and SHA-256 from
`scripts/pinned-tools.json`, downloads into a fresh directory, checks that file's
SHA-256, and only then extracts and installs it. The installer trusts the
runner's `PATH` utilities (`curl`, `sha256sum` or `shasum`, `tar`, `install`,
`python3`); it verifies the downloaded file, not the tools that verify it. The
checker requires every manifest field: an exact `X.Y.Z` version, a 64-hex
digest, an https GitHub release URL, an `os` of `linux` or `macos`, an `arch`
of `aarch64` or `x86_64`, a relative archive `member`, non-empty
`version_args` and a `version_output`. It also requires each tool to be
installed by the script with its name alone. Bumping a tool is a reviewed edit
to the manifest.

The checker accepts directly written block mappings for `jobs`, `steps`,
`uses`, and `permissions`. It rejects flow mappings, explicit mapping keys,
anchors, aliases, and merge keys rather than trying to partially interpret
YAML forms that could hide a mutable action or write grant. Explicit YAML type
tags are rejected for the same reason. Quoted scalar keys and permission values
remain supported and are normalized before policy checks.

Dependabot's `github-actions` ecosystem is deliberately unfiltered, unlike the
`cargo` ecosystem's patch-only policy. A pinned SHA has no floating tag
absorbing upstream fixes, so a pin Dependabot may not advance is a pin that
rots and never receives the security patch it is holding back. Dependabot
rewrites both the SHA and its trailing version comment.
