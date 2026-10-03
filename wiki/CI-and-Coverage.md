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
not a required job. It measures a quick before/after table, within 30 minutes,
for pull requests labelled `perf`, and the full comparison for each release tag;
[Development and Release](Development-and-Release#what-ci-measures) describes
both modes.

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
tests` requires `macos-core`, `macos-coverage`, and `macos-smoke`, while
`windows-latest / unit tests` requires `windows-native`, `windows-checks`,
`windows-tests`, and `windows-smoke`.
Each aggregate runs with `if: always()` and accepts only explicit `success`
results, so a failed, cancelled, or skipped shard cannot turn into a successful
required check.

The macOS core shard first measures the real PTY close baseline after Cargo
restore, then runs source-policy checks, strict Rustdoc, the one-pass workspace
test gate, workspace doctests, host probes, tooling tests, and real resource-baseline
capture. Its independent coverage shard installs the pinned
`cargo-llvm-cov`, runs the deterministic logic coverage gate, and uploads its
evidence artifact after success and after failure once the coverage step has started. The
`macos-smoke` matrix builds shipping release binaries on macOS 14 Apple Silicon
and macOS 15 Intel with distinct dependency-cache keys. Its Intel lane may save
dependencies only on a push to `main`; the Apple Silicon lane restores only.
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
builds a cold miss, and saves that result immediately before the three dependent
shards start. A restored fallback archive may contain no compatible packages
after a hosted-image or vcpkg revision change, so consumers still run Cairo
installation and may perform a cold build. CI does not override job or step
timeouts. The early app-only baseline build can be rebuilt under the workspace's
unified dev-dependency features.
The checks shard runs format, Clippy, source-policy, comment,
script-identifier, and Rustdoc gates. The test shard measures the real PTY close baseline after Cargo
restore, then runs the one-pass workspace tests, doctests, host probes,
fail-closed GDI presentation verification, WARP allocator,
software-selection presentation, the perf scenario harness build and its
smoke, which checks the comparison tooling on a software adapter without timing
([Windows](Local-Gate#windows)), tooling tests, and real resource-baseline
capture; when the perf smoke fails, the job uploads its evidence. The GDI wrapper accepts only one `capability=EXERCISED` verdict;
`HOST_INCAPABLE` remains informational and cannot satisfy the gate. The
restore-only `windows-smoke` shard builds the shipping release binary and
requires its bounded native smoke plus `frame-validation` and `device-recovery`
scenario smokes in separate steps with native process deadlines.

Rust-consuming shards share a dependency cache key within each platform and
architecture, excluding workspace-crate artifacts. The Apple Silicon core,
Windows checks, and Linux core shards are their keys' only writers. The Intel
macOS smoke lane is its architecture's only writer because it has no core shard.
Every writer saves only on a push to `main`; other shards and every pull-request
lane restore only. Release builds neither restore nor save Rust caches. This
bounds entries and avoids duplicate writers within one workflow run; overlapping
`main` runs can still compete to save the same immutable key.

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
`linux-core` and `linux-packages`, using the same fail-closed result check as
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
  other crates. CI runs it only on macOS, although the local runner also selects
  it on Linux. A green percentage does not cover native windows, real PTYs, GPU
  surfaces, generated FFI, installers, or Windows-only logic.
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
  low still passes. The `sonicterm-windows` and `sonicterm-linux` rows measure
  code compiled on macOS, not execution on those platforms. The baseline is
  keyed to the macOS arm64 CI runner; CI on another host fails as not
  comparable, and local runs print informational deltas without a verdict. Floor
  numbers change only in a reviewed diff from a retained run's verified
  evidence, as [Coverage evidence and rebaselining](Development-and-Release#coverage-evidence-and-rebaselining)
  describes.
- `deny.toml` records advisory, license, source, and wildcard-dependency policy,
  but no CI job runs `cargo deny check`.
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
