# Local Gate

[简体中文](Local-Gate-zh-CN)

The step table and the `python3 scripts/local-gate.py` invocation are on
[Development and Release](Development-and-Release#local-verification-gate). This
page describes how the runner executes those steps.

## PTY close baseline

`pty-close-baseline` explicitly selects the ignored real-PTY measurement on every
desktop host. Its local budget remains 1200 seconds. CI runs it immediately after
Cargo dependency restore and includes building the test binary, without a job or
step timeout override. Only the baseline uses a 640-second isolated-child
observation envelope and 1 MiB complete-output cap. Output overflow fails explicitly while both pipes
continue draining, never producing a successful truncated report. Ordinary
`isolated()` callers retain their 60-second deadline, 64 KiB diagnostic tail,
and quiet successful output. On Linux and macOS an isolated child closes the
descriptors it inherits above stderr when it starts, so it never holds another
test's capture pipe open.

The envelope reserves 20 ordinary and 20 stalled samples. Ordinary setup has one
4-second wait; the Windows stalled setup also has a 4-second flood wait. The
configured native receive/retry waits total 6.5 seconds: one shared 500 ms cancel,
500 ms each for reader, writer, termination retry and reap, plus 2 seconds each
for ConPTY close and drain. The observer's 4-second limit and 2-second completion
allowance overlap close, so their maximum is used rather than adding both paths.
Each sample also allows 2 seconds for fixture cleanup; 60 seconds covers launch,
scheduling and reporting. The resulting 640 seconds is a measurement budget,
not a production-close upper bound: native calls, locks and joins can still hang.
A genuine close hang remains a harness failure with child-tree cleanup at the
enclosing deadline, while bounded unsettled samples still reach the summaries.

For ordinary and stalled scenarios it uses the same shell, one pane, and 20
samples per scenario. Setup, settlement and cleanup remain bounded within the
isolated child's deadline; summaries use nearest-rank percentiles.
Every `PTY_CLOSE_BASELINE` sample and p50/p95/max summary distinguishes the
`close_pty_pane` caller's blocking time from an independent process observer's
settlement time, both starting at close. The caller is the headless App-owning
test thread, not a running native event loop. Windows fills the ConPTY output
queue; Unix keeps a slave descriptor open in the test process, outside the shell
session, through the observation. The observer uses retained Windows handles for
the shell, its descendant and externally identified conhost/OpenConsole. Unix
matches PID plus start time: the shell must be absent (`observation=reaped`),
while descendants may be absent or zombie (`observation=exited_or_zombie`); a
zombie may remain when its new parent does not reap it. The test never reaps the
PTY shell. A four-second settlement limit prints a censored `>4000.000` sample;
each summary includes its `censored` count and preserves lower-bound percentile
markers. Cleanup afterward cannot turn it into a successful measurement. Durations never
fail the test; setup/observation failures do. If close itself never returns, the
isolated child deadline ends the run and retains the last phase, rather than
claiming a completed sample. Native scenario setup is not proof that the
historical stalled teardown reproduced.

## Step selection and process groups

The runner selects the host's `local` steps and runs them in table order.
`--with-release` adds the host's `release` steps, `--with-optional` adds its
`optional` steps, `--step ID` runs only the named steps, and `--list` prints the
selection with each step's timeout, prerequisites, and CI jobs. POSIX steps run in
their own process groups under deadlines, reusing the native smoke runner's
launch and tree-kill logic. Windows uses owned jobs as described below. Later
steps still run after failure, timeout, or launch error. On macOS and Linux a step also fails when
members of its process group are still running two seconds after its leader
exits: the runner kills them and records the count in the step log and both
summaries. The runner observes the leader's exit without reaping it, with
`os.waitid` and `WNOWAIT` where Python provides it and otherwise, on macOS
Python builds without `os.waitid`, with a kqueue exit event. The leader stays an
unreaped zombie, so its PID, which is the process-group id, cannot be reused by
an unrelated group while the runner polls and kills the group; only then is the
leader reaped. On macOS a group kill fails with EPERM when the unreaped leader
is the only member left, where Linux reports success. When a deadline or Ctrl-C
comes after the leader has exited, the runner records that refusal in the step's
detail and still kills or reaps the leader, so the step records its result and
the summaries are written. On macOS and Linux the runner refuses to start when
SIGCHLD is ignored (exit 2), because the kernel can then reap each leader
before the runner can read its exit status or hold its group id. When the
runner detects that another reaper collected a leader during a step, the step
fails with its exit status recorded as unavailable, never as 0, and the runner
sends no signal to that leader's pid or group. A concurrent reaper that
collects the leader after the runner has seen it exit, and before the runner
reaps it, is unsupported: a group scan or kill in that window can aim at a
group id no process reserves. A POSIX host whose Python has neither mechanism
reaps the leader first, as before, so there the group id can be reused before
the group is killed, and such a host cannot detect a leader that another reaper
collected, because Popen then reports exit 0. Group emptiness comes from the
member list, read from `/proc` on Linux and from `ps` elsewhere, which leaves
zombies out; when the list still cannot be read at the end of the grace period,
the group is killed and the step fails with an unknown leftover count.
On macOS and Linux the leftover check sees only the step's process
group: a child that calls `setsid`, or otherwise leaves the group, is neither
seen nor killed, and if it also redirects its output away from the step's pipe,
the runner does not bound it at all, because the deadline kills only the group.

## Windows job objects and preparation

On Windows, only the local gate uses an unnamed kill-on-close Job Object without
breakaway. A trusted bootstrap is assigned through its retained process handle
before it can launch the target; assignment or startup-protocol failure refuses
execution. Job `ActiveProcesses` is checked before waiting for output EOF, with
a two-second grace capped by the step deadline and a two-second cleanup budget.
Cleanup terminates the owned job, never processes selected by name or reopened
PID. Accounting, protocol, output, and cleanup errors fail the step; closing the
job handle alone is not evidence of verified emptiness. A Python startup hang
before assignment remains outside the parent-crash containment guarantee.

The Windows policy defaults to strict: surviving descendants fail mixed tests,
doctests, workspace scripts, and native steps, including compiler helpers in
those steps. Among standalone commands, only `clippy`, `doc`,
`doc-resource-features`, and `release-windows` permit forced compilation cleanup after target exit 0, complete capture and
protocol, and verified job emptiness. Their result is `CLEANED_NOT_NATURAL`, not
`PASS`. Logs and JSON preserve the original unsigned target exit, policy, job
accounting, and cleanup outcome; the text summary counts cleaned steps separately.
A run containing only `PASS` and permitted `CLEANED_NOT_NATURAL` steps exits 0,
but its overall verdict remains `CLEANED_NOT_NATURAL` if any step required cleanup.
Mixed cold steps can still fail; no process-name exemption changes that boundary.

On Windows, `pty-close-baseline` and `windows-warp-allocator` first compile the
same selected tests with `--no-run` inserted before `--`. `pty-feasibility` first
builds its evidence example without running it. `workspace-crates` first prepares
the pinned winit tests, its documentation, and the workspace tests, in that order.
Each preparation uses a separate owned job with compile-only cleanup; the original
step command then runs unchanged in a new strict job. Cargo still selects and runs
the tests with its own runtime environment. Preparation is not proof that Cargo
will reuse the cache. Any surviving descendant during strict execution still
fails. Doctests are not split or exempted.

The explicit preparation records must match every Cargo invocation in the two
scripts, in source order. The narrow verifier joins backslash continuations and
normalizes line endings; it rejects unsupported shell layouts, missing or changed
records, and invocation or environment-scope drift before launching any phase.
A parity failure means the original script is `NOT_RUN`. Winit uses the caller's
nonempty `CARGO_TARGET_DIR`, otherwise the repository's `target` directory;
`RUSTDOCFLAGS=-D warnings -A rustdoc::invalid_html_tags` is overridden only for its documentation preparation.
Preparation output enters the step log, never feasibility's evidence/hash pipeline.
Only canonical table objects authorize preparation or standalone compile cleanup;
a synthetic step with the same ID cannot borrow that permission.

All phases share the original step deadline and optional child-output byte budget;
neither restarts per phase. Existing bounded cleanup remains available after the
deadline. An ordinary nonzero preparation exit keeps the overall result `FAIL`
but permits remaining preparations and the original command while time remains,
provided job emptiness, bootstrap reaping, protocol and capture are all verified
without errors. Unsafe custody, launch, protocol or capture failure stops the step;
interruption or timeout also stops it, and unstarted phases remain `NOT_RUN`.
Logs and text/JSON summaries show each phase's actual argv, environment overrides,
exit and custody separately; the step exit remains the original execution's exit
or unavailable if it never ran. Preparation cleanup can produce an accepted
aggregate `CLEANED_NOT_NATURAL` only when every preparation succeeds and the
original strict execution naturally passes. Any ordinary preparation failure
remains an overall failure even when the original execution later passes.

## Output, logs, and Git state

The local gate preserves DEVNULL input, argv, working directory, environment,
and existing color settings. One merged output pipe streams raw bytes to disk
without retaining the complete output in memory. Without an explicit cap the
full stream is logged; an explicit cap keeps its prefix, drains excess output,
and fails on overflow. Console output remains step progress and log tails.
`native-smoke-runner.py` and direct CI/Release invocations retain their existing
behavior; this local custody policy does not apply to those callers. Windows
custody regressions run through `local-gate_tests.py` with a cleanup-inclusive
60-second group budget; the complete local supply-chain step retains its 120-second budget.

Per-step logs, `summary.txt`, and `summary.json` go to a new temporary
directory, or to `--log-dir`, which cannot be the repository root or an
ancestor of it (exit 2), and the exit status is nonzero when any step fails.
Step logs keep each step's exact output bytes, and console text that the
console cannot encode, such as CJK text on a cp1252 Windows console, is printed
escaped instead of stopping the run.
Because the summaries are written after the final snapshot, the runner also
refuses, before any step runs, a runner-owned output path (a step log,
`summary.txt`, or `summary.json`) that is tracked, compared case-insensitively,
or that is a symlink or a hard link (exit 2). The runner creates each step log
and summary as a new file, created exclusively in the log directory and renamed
onto the output path, so a symlink or hard link found there is replaced, never
written through, and a log tail is read without following a symlink. A symlink
or hard link that appears at an output path during the run, or a log directory
replaced during the run, fails the run with a message naming the path. When the
runner finds the log directory replaced, it stops starting steps: the remaining
steps do not run and no summaries are written. The runner records tracked and
untracked Git state, including each path's type and permission bits, before and
after the run: changes already present are reported as pre-existing, a change
made during the run fails the gate, and the runner never cleans the tree. Only
the runner's own untracked logs and summaries are left out of that comparison.

## Timeouts and CI parity

Each local step has an explicit timeout independent of CI timeout policy; a step
that no CI job runs gets a bound well above its measured runtime. A slow machine
or a cold build can therefore report `TIMEOUT` for a step that would pass; rerun it with
`--step ID` once the build is warm.

`ci.yml` keeps explicit steps for per-step progress without job or step timeout
overrides; GitHub Actions platform limits still apply. The table checks command
and job parity, not timeout parity, and is not generated into the workflow.
`scripts/local-gate_tests.py` runs through `check-workflow-supply-chain.sh` in `macos-core`, `windows-checks`, and
`linux-core`. It fails when a table command is missing from a CI job it names,
when a `ci.yml` step runs a `scripts/` gate or a `cargo fmt|clippy|doc|test`
command that is neither a table step nor on the reasoned CI-only list, and when
the block on [Development and Release](Development-and-Release#local-verification-gate),
its Chinese page's block, or the `CLAUDE.md` block differs from
`python3 scripts/local-gate.py --render en` or `--render zh-CN`. The `ci.yml`
reader models this repository's workflow layout and raises on any `run:` form it
does not model, so parity fails loudly instead of skipping a step. It also raises
on a `defaults:` key at the workflow's top level or in a job, in block or flow
form, because inherited `run` defaults (`working-directory`, `shell`) apply to
every run step and the reader does not model them, and on any top-level line
that is not a plain `key:` line. A step's `shell:` must be a plain `bash` or
`pwsh`: another shell, a custom template, or a quoted, flow, block, or
continued spelling raises, like a `working-directory:`. Before classifying a
command it normalizes
`cargo +toolchain`, quoted script paths, and `scripts\` separators; it checks
every command joined by `&&` or `;` and every line of a `run:` block, and it
reports a gate it cannot prove, such as one behind a pipe, `||`, a wrapper
command, or a command substitution. It also reports a `${{ }}` workflow
expression in any position that decides what runs: the command word, the cargo
subcommand (also after `+toolchain` or a leading cargo option), an
interpreter's script argument (also after interpreter options), and the command
after a first-party script's `--` separator. Expressions in ordinary data
arguments stay supported. The classifier does not analyze shell exit status:
checking every command on a compound line or block does not prove that a
failure propagates. Whether a failure before `;` or on an earlier line of a
block fails the step depends on the shell's own error handling, such as
`bash -e` or PowerShell's last exit code, which the parity check does not
model. Parity compares command text only, so it does not model job or workflow
`env:`, such as a `BASH_ENV` or `RUSTFLAGS` that changes what an unchanged gate
does, or a shell expansion that supplies a gate word or script path at run time,
such as `cargo $SUB`, `bash "$SCRIPT"`, or `cargo $(echo test)`; a workflow edit
is reviewed like any other change. Each CI-only entry
carries a reason: dependency setup, an evidence rerun of an integration test
that the same job's workspace step already runs, or runtime and package evidence
that needs hosted runners, release binaries, or built packages. A first-party
test, or a `cargo fmt|clippy|doc` run, that only CI runs cannot be CI-only, so a
missing local test or gate fails parity.

## Step notes

The separate `doc-resource-features` step is required because
`test-util` is the workspace's only optional feature and `cargo doc` builds no
dev-dependencies. Workspace Clippy and tests already compile `test-util`
through `sonicterm-logging`'s dev-dependency on it. The font stack has no
optional vendor features: St.Helens is a normal tracked asset and other fallback
faces come from native discovery.
`check-workspace-crates.sh` first runs the native-source verifier unit tests,
its offline integrity check, and portable macOS bundle tests, then runs one fail-complete
`cargo test --workspace --lib --bins --tests --no-fail-fast` command for default
features. Each phase runs even if an earlier phase fails. It covers every workspace library, binary, and integration-test target
without repeating the unit and binary targets in a serial per-package loop. Its
pinned winit phases honor a caller's `CARGO_TARGET_DIR`. That command compiles no
doctests. The `doctests` step compiles and runs ordinary doctests, compiles
`no_run` examples without running them, and skips `ignore` examples.
Before its Cargo phases it also runs `rustfmt --check` on the pinned winit's
authored Windows `keyboard_tests.rs`: the preserved dependency is excluded from
workspace formatting, but its authored tests are not.
Its pinned winit documentation phase denies every rustdoc warning except
`rustdoc::invalid_html_tags`: the crate's doc comments are upstream text that the
offline integrity check pins byte for byte, and rustdoc from Rust 1.99 reads the
`<kbd>*</kbd>` list on `KeyCode::NumpadMultiply` as improperly nested Markdown emphasis.

The authored-comment checker enforces purpose Rustdoc on effectively public
functions and public trait functions, `# Safety` on public unsafe functions, and
anchored `// When:`, `// SAFETY:`, `// Lock order:`, `// Ordering:`, and
`// Lifecycle:` contracts. `check-no-raw-process-exit.sh` requires shipping code
to exit through `sonicterm_logging::exit_with`.
`check-workflow-supply-chain.sh` enforces the workflow contract described in
[Workflow supply chain](CI-and-Coverage#workflow-supply-chain); it runs its own parser tests
first, so a scan that silently stops matching cannot report a green gate. It
also runs the local-gate runner and parity tests, and the tests of the native
selection smoke and performance comparison scripts.

The `windows-warp-allocator` step is the release-blocking deterministic
allocator test on Windows. It requires a DX12 WARP adapter and allocator report.
Production reserved bytes must be below 64 MiB, the largest block below 128 MiB,
and production reserved bytes below the old-default control. The Windows CI test
shard runs it explicitly, and Release accepts only an exact successful `main` CI
run that includes that shard. Windows CI is the
only reliable compiler and runner for `#![cfg(target_os = "windows")]` tests; on
macOS such files can compile to no tests.

The optional `windows-target` step is a pre-push aid on macOS, never a CI gate.
`scripts/check-windows-target.sh` fails when a workspace member is in neither of
its two lists, then runs
`cargo clippy --locked --target x86_64-pc-windows-msvc --all-targets -- -D warnings`
on the 13 members that need no Windows C toolchain: `sonicterm-types`, `-grid`,
`-vt`, `-cfg`, `-logging`, `-resource`, `-text`, `-ui`, `-app-core`, `-io`
(including its ConPTY code and Windows-gated tests), `-render-model`,
`-block-glyph`, and `-font-config`. That scope is default features, all targets,
and the target-specific dev and build closure, in a separate target directory. It
also runs `cargo check --locked` on the pinned winit with `serde`, so its Windows
keyboard tests compile, and it fails with the
`rustup target add x86_64-pc-windows-msvc` hint when the target is missing. It
does not check the other ten members: `sonicterm-freetype` and
`sonicterm-harfbuzz` run native C/C++ builds, `sonicterm-fontconfig` discovers a
system library through pkg-config, and `sonicterm-font`, `-engine`, `-gpu`,
`-app`, `-mac`, `-windows`, and `-linux` need the unverified native font and
Cairo closure. Cairo is a system dependency, not vendored: its pkg-config probe
rejects the cross-compile target, and with that probe bypassed the first native
blocker is `sonicterm-freetype`'s vendored zlib, which needs Windows CRT
headers. The check compiles and lints only; Windows CI is the only place
Windows code runs. CI runs a static classification-completeness check instead of
the step: `scripts/local-gate_tests.py` compares the script's two crate lists
with the workspace members in `macos-core`, `windows-checks`, and `linux-core`
without running the script, so every new crate must be classified.

The Windows `windows_font_weight_present` test yields to native message dispatch
between setup, render, capture, individual weight actions, and cache checks.
Every phase except a scale's first render checks that its window remains
responsive. That render builds the scale's fonts, glyph atlas, and GPU pipelines
without pumping messages, so on a slow runner it can pass the 5-second rule of
`IsHungAppWindow` while it is still working. A watchdog thread aborts the test
process when one phase runs longer than 60 seconds or the whole run longer than
240 seconds, because a phase that never returns also stops the checks on the
event-loop thread. Errors and completion release the renderer and verify the
live-renderer baseline. A missing redraw fails at the 180-second test deadline.
Native GDI pixel comparisons remain required, including when
`SONICTERM_FONT_PROBE_DIR` enables dense readback and image evidence.

Release preparation also builds the shipping platform binary:
`python3 scripts/local-gate.py --with-release` adds the host's `release` step.

## Native split selection

`windows_native_split_selection` runs with the Windows workspace integration
tests. It creates native windows and renderers, then sends synthetic in-process
pointer events through the production App handlers. Main and child windows cover
side-by-side, stacked, and nested splits; the assertions check the selected pane,
exact copied text, terminal mouse reports, Shift selection, and frame-count
advancement. Copy uses an in-memory clipboard; no PTY or system clipboard is used.
This is not physical drag-gesture or pixel-readback evidence.

The fixture returns to the native event loop between presentations. Each native
redraw maps the actual window id to the fixture's App entry and makes one
production redraw attempt. Local press and release stages each require a completed
frame; physical pointer and keyboard input are ignored. Stage and deadline records
include both window ids, native/App redraw counts, completed frames, and the last
observed native occlusion event. A missing event does not establish visibility;
`is_visible` is not the native occlusion state. Missing redraw routing is a failure.
Attempts that never complete the first frame report `BLOCKED` without inferring a
cause; later deadlines fail. Neither result satisfies native acceptance.

macOS runs the same fixture through an example on the process main thread.
Ordinary workspace tests and coverage do not execute the example, so the macOS
local gate explicitly builds and runs it. Both required `macos-smoke` CI matrix
legs run the same commands before the release build and packaging, without CI
job or step timeout
overrides. The local example-build budget remains 25 minutes; selection runtime
limits are independent and unchanged. On macOS the fixture forwards `new_events`
and `about_to_wait` to App, preserving its earlier deadline or polling request
alongside the case deadline; App owns deferred retries rather than a fixture
redraw loop. The unrelated warm-window pool is disabled in this fixture. Windows
keeps its existing callback and retry path.

Each macOS case first presents successfully, then injects one backend-occluded
acquisition through the existing renderer test seam. It must observe a
nonpresenting attempt and a later completed frame. Any resize, scale-factor or
occlusion event between injection and that frame fails the case without
reinjection: such an event could otherwise bypass deadline-driven recovery. This
post-first-frame control does not reproduce a startup failure with zero frames;
a recurring startup failure remains blocking.

```sh
cargo build --locked -p sonicterm-app --example native_split_selection
python3 scripts/native-selection-smoke.py
```

The verifier selects Metal, enables the renderer's adapter records on stderr,
removes inherited `NO_COLOR`, and preserves `HOME`. It passes Python's selected
OS temporary root as `TMPDIR` so Rust uses the same root. The verifier launches
`cargo run --locked -p sonicterm-app --example native_split_selection -- --run <fixture>`
from the repository root rather than guessing an executable path. Cargo resolves
`CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `build.target-dir` and the configured
target, and checks build freshness before execution. A missing Cargo executable,
build/configuration error or timeout fails the gate; it never falls back to an
older default-path binary. The fixture gets a new child under the OS temporary
directory, not an assumed `RUNNER_TEMP` location, with isolated config and logs.
Its watchdog remains 180 seconds and each window/topology case has a 20-second
deadline. The verifier directly reuses the local gate's 190-second process-group
launcher for Cargo and the example, including any needed rebuild, unreaped-leader
ownership and the post-exit leftover check. Run the separate build step first to
keep cold compilation within its own budget. The local gate's documented `setsid`
escape limitation also applies here.

Success requires exit 0, exactly one PASS for every main/child topology, one final
PASS, and a selected-adapter record per case with Metal, a non-CPU device type, and
`software_rendering=false`. Each case must also have one preceding
`PASS native surface retry` record with positive `baseline_frames`, larger
`resumed_frames`, and `recovery_events=0`; missing, duplicate, malformed or
out-of-order recovery evidence fails. Missing or duplicate cases, `NOT_EXERCISED`, `BLOCKED`,
panics, cleanup warnings, surviving fixture directories or process-group members
fail the gate. The launcher retains at most 8 MiB of child output, continues
draining after overflow, and fails instead of accepting truncation. Evidence stays
in the printed OS-temporary directory; CI uploads it on failure. Retain only the
needed evidence, then remove that directory. A Windows pass cannot substitute for
macOS execution, and a direct example invocation without `--run` is not acceptance.

## Performance scenario smoke

`macos-perf-smoke` checks the comparison tooling, not performance. It runs
`python3 scripts/perf-compare.py --smoke`, which builds the current tree's
`perf_scenarios` example in debug, with no base ref, worktree, or release build,
and runs three short cases with the harness's `--short`, each in a fresh process
with its own scratch directory and the repository root as its working directory,
where the App finds the tracked fonts:

1. S1;
2. S3;
3. S1, killed like a run at its deadline as soon as its session has started.

With `--short`, every hold lasts 5 s, each scenario's closing idle phase lasts
until at least 5 s after its workload starts instead of 60 s, and S3 floods
`head -n 200000` and a 5 MB file. The first two cases pass when their results
match the result schema, the harness never took the front application from
another application, and cleanup leaves no process. When the App reports that the
configured primary font failed to load, the smoke fails at once. The killed case
passes when its cleanup settles and no process survives; at the deadline, the
script signals the harness only while that process still has the pid and start
time recorded when the harness was accepted. Every case also snapshots
`~/.sonicterm` before and after, with a sentinel file marking its start; a new,
changed, or removed file there fails the smoke. The exceptions belong to another
SonicTerm instance: breadcrumb files named for another process, and daily-log
growth or log removals while another instance is running. `.DS_Store` is
ignored, and the harness logs its scratch path at startup, so a misdirected log
is recognized ([Isolation checks](Development-and-Release#isolation-checks)).
The check also covers a symlink there through its target, so a write through the
link is a change; when the check cannot read a target or reaches its walk bound,
it cannot finish, and the smoke fails. The smoke asserts no timing value; only
a comparison on an idle host measures speed or memory.

| Exit | Result | When |
| --- | --- | --- |
| 0 | pass | every case passes as above |
| 1 | fail | a case that is not valid, not an occlusion, and not `BLOCKED`, or a tree whose assets do not resolve; it fails at once, without a retry |
| 3 | `BLOCKED` | no valid exercised run results |

A case that is not valid, not an occlusion (retried within the bound), and not
`BLOCKED` fails the smoke at once (`smoke_verdict` in `scripts/perf-compare.py`).
The causes of exit 1 include:

- a schema, focus-safety, or isolation failure;
- a session-record problem;
- an unresolved cleanup: survivors, process-group members that outlived the
  harness or could not be counted, an unsettled `finish_session`, or session
  members without a valid anchor;
- an occlusion whose `finish_session` did not settle, the deadline case
  included: that is a cleanup failure, decided before the occlusion check, so
  it is not retried;
- a harness invalidation other than an occlusion, such as a checkpoint whose
  `.done` never arrived;
- a harness or `run_step` timeout;
- a refusal (exit 2);
- a primary-font load failure;
- a home check that cannot finish;
- a `run_step` status other than PASS while the harness exits 0;
- an unexpected harness exit;
- a tree whose assets do not resolve, caught before any case runs.

Only an occlusion is retried, at most 3 times per case; when a case has no valid
exercised run, the smoke reports `BLOCKED`. The local gate accepts only exit 0,
so `BLOCKED` fails the step. The harness becoming active is not focus theft on a
host with no front application, or on a GitHub Actions runner
(`GITHUB_ACTIONS=true`), which reports a front application but has no user whose
focus could be taken; the log notes it. The scenarios run only on macOS;
elsewhere the harness prints `NOT_EXERCISED`, so the step is macOS-only.

The main display, where the harness opens its window, must show a desktop
Space, not a full-screen app: with a full-screen app there, the harness window
opens on the hidden desktop Space and presents no frame. When the main window
presents no frame within 10 s of opening, the harness ends the run as invalid
(exit 3), with a reason saying the window was occluded during startup, likely by
a full-screen app on its display. The smoke retries it as an occlusion and
reports `BLOCKED` when no valid run results.

The local budget is 45 minutes: the selection build's 25-minute cold-build
allowance plus 100 s for each of up to 12 harness runs, since each of the three
cases is retried up to 3 times. Both required `macos-smoke` CI matrix legs run
the same command after the native split selection and before the release build,
without CI job or step timeout overrides. The CI parity check fails when that
step gains an `if:` or `continue-on-error:`, or moves out of that position.

When the smoke fails in CI, the job uploads its evidence.
`perf-compare.py --smoke` appends `SONICTERM_PERF_EVIDENCE_DIR=<dir>` to
`$GITHUB_ENV`, and that directory holds each case's `result.json` and logs, the
session records, `front-samples.log`, and the cleanup and home-check findings in
`cleanup.json` and `home-check.json`. It also holds each case's `progress.json`.
After each phase ends, outside the measured windows, the harness writes that
file in its scratch directory: the schema version, the harness hash, status
`running`, and the phases completed so far, in `result.json`'s shape. A run
killed by `run_step`'s timeout or by the harness's watchdog therefore still shows
its earlier phases. `progress.json` is evidence only; `result.json` stays the
only result. The smoke prints each distinct `lsappinfo` sample form once.

`scripts/perf-compare_tests.py` tests the script, including these failure rules,
and `check-workflow-supply-chain.sh` runs it on macOS, Windows, and Linux.
[Development and Release](Development-and-Release#comparing-performance)
describes how to run and read a comparison.

## Reviewed block-glyph rasters

`sonicterm-block-glyph` keeps a reviewed raster digest table in
`crates/sonicterm-block-glyph/raster-digests.golden.tsv`. Each row records a
codepoint, the cell width, height, and underline thickness, the alpha sum, the
ink bounding box, and an FNV-1a 64 digest over the tile size and row-major
alpha, the only channel the renderer keeps. The table holds 37 codepoints,
including at least one from each block-key family that `from_char` maps. The
test requires a reviewed row for all 222 combinations of these codepoints and
the six case sizes: 5×9/1, 8×16/1, 15×31/2, 16×32/2, 30×40/2, and 45×60/3 (cell
width × height / underline, in texels). `raster_digests_match_reviewed_table`
requires an exact match on every host, with no tolerance, and prints old and
new values and the regeneration command for each differing row.

Spinner segments remain rasterizable in thin and small cells. When their
inner clear circle collapses, it contributes no path and clears no pixels;
the outer fill and the remaining sector-clearing paths still run. This does
not alter nonempty paths or the rasters at sizes where the hole is positive.
The spinner regression tests include 1×1 cells and both orientations around
the `min(width, height) = 6 × underline` boundary. A subpixel sector may be
fully transparent in a tiny cell; its tile must still have the requested
size and storage.

Regenerate the table only for a named geometry change:

```sh
SONICTERM_BLESS_BLOCK_GLYPH=1 cargo test -p sonicterm-block-glyph raster_digests
cargo test -p sonicterm-block-glyph
```

The bless run rewrites the table, prints each changed raster in hex, and always
fails, so only the plain rerun can pass. Reviewers compare the old and new alpha
sums and bounding boxes of each changed row. A digest change needs a named
geometry change and the attached rasters; otherwise it is a regression.

Digests are not the only oracle. Without stored data, `customglyph_tests.rs`
rasterizes every mapped codepoint at two sizes, 8×16/1 and 16×32/2, and requires
the requested size and visible ink, with U+2800 as the only blank. It also pins
full-block opacity, shade levels, line centering and thickness, box joins,
Braille dot positions, and Powerline coverage. If hosts disagree on
anti-aliased texels, report the per-texel deltas; the maintainer chooses a
documented tolerance or per-platform tables.
