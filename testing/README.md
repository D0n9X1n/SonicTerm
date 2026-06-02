# SonicTerm visual test harness

Local-session visual gate for render/UX-affecting changes. PM (mac side) runs
`just visual mac` and PM (windows side) runs
`pwsh -File testing/workflows/windows.ps1 -All` before merging any PR that
touches the GUI smoke surface (see CLAUDE.md §13). Both drivers consume
`testing/cases.toml` — that file is the **single source of truth** for what
we promise to verify.

## ⚠️ Quit other terminals first

The harness drives sonicterm-mac via macOS `osascript` UI keystrokes, which
always land in whatever app is **frontmost at that instant**. If WezTerm,
iTerm, kitty, Claude Code, Terminal.app, or any other terminal is running,
the harness will refuse to start (`exit 2`) — its keystrokes can otherwise
leak into those windows if sonicterm-mac loses focus mid-case (issue #464).

Quit every other terminal before `just visual mac`. If you must run the
harness FROM another terminal during dev, set
`SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1` — but be aware that any case where
sonicterm-mac fails to stay frontmost may then execute keystrokes against
the source terminal. The Guard 4 frontmost-verify catches the common case
and exits 77 (skip) instead of leaking.

## Quick start

```bash
brew install tesseract yq just
pip3 install Pillow

just build                                # cargo build --release -p sonicterm-mac
just visual mac                           # run every mac case
just visual-case tab-open-cmd-t mac       # run one
```

Results land in `testing/results/mac-<git-short-sha>/` with per-case
screenshot, expectation log, and a top-level `report.md`.

## Dependencies

- **yq** — TOML / YAML query (`brew install yq`). Driver also uses
  Python's `tomllib` for TOML, so the hard dependency is just Python ≥ 3.11.
- **tesseract** — OCR for `text-in-region` and `ocr-text` expectations
  (`brew install tesseract`).
- **Pillow** — pixel sampling for `pixel-near` expectations
  (`pip3 install Pillow`).
- **just** — task runner (`brew install just`). Optional — you can call
  `bash testing/workflows/mac.sh` directly.
- **osascript** / **screencapture** — built into macOS.

## Schema (`testing/cases.toml`)

```toml
[meta]
schema_version = 1
description = "..."

[[case]]
id              = "kebab-case-unique"
section         = 1                       # matches docs/RELEASE_TESTING.md
title           = "..."
applies_to      = ["mac", "windows"]      # or ["mac-manual"], ["windows-manual"]
covers_bugs     = ["#196", "#200"]        # issues/PRs this guards against
covers_landmines = ["try-lock-no-deadlock"]  # ids from CLAUDE.md §4
setup           = ["open-3-tabs", "clear", "wait:0.3"]
keystrokes      = [
  { kind = "key",  value = "cmd+t" },
  { kind = "text", value = "echo hi" },
  { kind = "wait", value = 0.3 },
]
expect = [
  { kind = "tab-count", value = 3 },
  { kind = "pixel-near", x = 100, y = 200, rgba = [220,50,47,255], tolerance = 30 },
  { kind = "text-in-region", region = "body", value = "hi" },
  { kind = "screenshot", region = "window" },
]
fail_on = "what a visible failure looks like"
```

### Keystroke kinds

| kind | fields | notes |
|---|---|---|
| `key` | `value` (chord like `cmd+t`, `escape`, `ctrl+c`) | special keys: `enter`, `escape`, `up`, `down`, `left`, `right`, `page-up`, `plus`, `minus` |
| `text` | `value` (literal string) | sent via `osascript keystroke` |
| `wait` | `value` (seconds) | |
| `key-repeat` | `value`, `count`, `delay` | useful for stress paths |
| `shell-cmd` | `value` (bash) | runs in driver shell, not in the terminal |
| `click-region` / `cmd-click-region` / `drag` / `hover-region` / `resize-window` / `focus-window` | region/coords | region click stubs print a TODO; resize is fully implemented |

### Expectation kinds

| kind | fields | implementation |
|---|---|---|
| `screenshot` | `region` | archival only — presence check |
| `pixel-near` | `x`, `y`, `rgba`, `tolerance` | Pillow; coords are in 1000×700 logical pixels, scaled to actual |
| `text-in-region` / `ocr-text` | `region`, `value` | tesseract OCR |
| `not-text-in-region` | `region`, `value` | negated OCR |
| `tab-count` / `pane-count` / `window-count` / `padding-min` / `scrollback-min-lines` | various | **heuristic-pass** today — SonicTerm doesn't yet expose an introspection IPC. When `sonicterm-mac --json-state` lands these flip to real checks. |
| `process-count` | `program`, one of `value`/`min`/`max` | `pgrep -f` count |
| `process-cpu-max` | `program`, `max_pct` | `ps pcpu` sample |
| `process-spawned` / `process-not-spawned` | `program`, `since` | heuristic |
| `no-orphan-shells` | `parent` | best-effort orphan scan |
| `file-absent` | `path` | direct stat |
| `exit-code` | `cmd`, `value` | shells out, checks rc |
| `responsive-within` | `seconds` | heuristic |

Heuristic-pass expectations document intent and ride along with the
screenshot for human review until the introspection IPC lands. They DO
NOT silently green-light a regression visible in the screenshot — the
reviewer still looks at the image.

## How to add a case

1. Pick the smallest section that fits in `cases.toml`. Sections track
   `docs/RELEASE_TESTING.md`.
2. Add a `[[case]]` block. Keep `id` kebab-case and globally unique.
3. List `covers_bugs` (any GitHub `#NNN` this guards) and
   `covers_landmines` (ids from CLAUDE.md §4).
4. Validate parse: `python3 -c "import tomllib; tomllib.load(open('testing/cases.toml','rb'))"`.
5. Run it once locally: `just visual-case <your-id> mac`. Inspect
   `testing/results/mac-<sha>/<your-id>/screen.png` to confirm the case
   is doing what you think.
6. Commit `cases.toml` + the matching reference screenshot if useful.

## Windows runner

```powershell
pwsh -File testing/workflows/windows.ps1 -All
pwsh -File testing/workflows/windows.ps1 -Case tab-open-ctrl-shift-t
pwsh -File testing/workflows/windows.ps1 -Build -All
```

Results land in `testing/results/win-<git-short-sha>/`. Same per-case
layout as mac (`screen.png`, `case.json`, `expect.log`, `status`).

### Prereqs

- **PowerShell 7+** (`pwsh`). Windows PowerShell 5.1 will not work — the
  driver relies on `pwsh`-only features.
- **`--features harness` Cargo flag (hard prereq).** The Windows binary
  under test must be built with the `harness` feature so the named-pipe
  input bridge (`crates/sonicterm-windows/src/harness_pipe.rs`) is
  compiled in. The driver will refuse to start if it can't connect to
  the pipe.
  ```powershell
  cargo build --release -p sonicterm-windows --features harness
  ```
- **Git Bash** (`C:\Program Files\Git\bin\bash.exe`) for any case that
  sets `shell = "bash"` (see Bucket C below). Cases that don't request
  bash skip this dependency.
- **No elevation.** The driver runs as the invoking user; an elevated
  shell will be rejected (Guard 5).
- **Defender note.** A first run from a fresh checkout may stall on
  real-time AV scanning of `target\release\sonicterm-windows.exe`.
  Either pre-exclude the workspace or expect a one-time ~15 s warmup.
- **tesseract — OPTIONAL.** Required only for `ocr-text` /
  `text-in-region` expectations. See `docs/WINDOWS_TESTING.md` for the
  install recipe; OCR-only cases gracefully SKIP without it (#492).

### Guards (Guard 1–6)

The driver runs six pre-flight guards before any case executes. Any
guard failure exits 2 (skip) before keystrokes are dispatched, so a
misconfigured host never corrupts results.

| # | Guard | One-liner |
|---|---|---|
| 1 | Competing terminals | Refuse to start if a GUI terminal app is already foreground-capable (#464; see env overrides below). |
| 2 | Multi-PID tracking | Wait for **every** `sonicterm-windows.exe` PID spawned in this run to be ready; one stale PID from a prior run is detected and skipped. |
| 3 | Pipe handshake | Connect to `\\.\pipe\sonicterm-harness-<pid>` via `NamedPipeClientStream`; fail fast if `--features harness` was omitted. |
| 4 | Foreground verify | Confirm the SonicTerm window is foreground at keystroke time; SKIP the case (exit 77) rather than leak keys into another window. |
| 5 | No-elevation | Reject `IsElevated == true` — SendKeys cross-integrity-level is silently dropped on Windows. |
| 6 | Workspace clean | Confirm `target\release\sonicterm-windows.exe` mtime ≥ source mtime; warn-skip the build step if a stale binary would mask a regression. |

### Guard 1 env overrides

| Variable | Effect |
|---|---|
| `SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1` | Global bypass — skip Guard 1 entirely. Use during dev when launching the driver from a competitor terminal. |
| `SONICTERM_HARNESS_EXTRA_TERMS=name1,name2` | Comma-separated process names appended to the built-in 16-name list (case-insensitive, whitespace trimmed, empties ignored). No `.exe` suffix. |

### Bucket A / B / C input model

Cases choose one of three input dispatch modes; the driver picks per
case based on the `keystrokes` shape:

- **Bucket A — SendKeys (legacy).** PowerShell `SendKeys` against the
  foreground window. Cheap, but fragile under focus loss; gated by
  Guard 4.
- **Bucket B — multi-PID SendKeys.** Same as A, but the driver tracks
  every spawned PID so a freshly-opened tab/window can receive its own
  burst without losing the prior PID.
- **Bucket C — named-pipe input.** When `--harness-input-pipe auto` is
  active (the default), the consumer chain is:

  ```
  run_case.ps1 → Send-InputToHwnd.ps1 → NamedPipeClientStream → harness_pipe.rs
  ```

  This bypasses SendKeys entirely — keystrokes are injected into the VT
  layer in-process, so focus loss is irrelevant. Bucket C is required
  for any case whose first keystroke is sent before the window is
  guaranteed to be foreground.

### `shell = "bash"` per-case field

Cases may override the default shell with a top-level `shell` field
(per #493/#500):

```toml
[[case]]
id    = "bash-pipe-grep"
shell = "bash"      # forces Git Bash; default is the user shell
```

The driver resolves `bash` via `C:\Program Files\Git\bin\bash.exe`;
absence of Git Bash makes the case SKIP, not FAIL.

### Exit codes (CI contract)

| Code | Meaning |
|---|---|
| `0` | All cases PASS. |
| `1` | At least one case FAIL. |
| `77` | At least one case SKIP and zero FAIL (treated as "soft green" in CI). |

CI gates the merge on exit `0` or `77`; any `1` blocks.

## Results directory layout

```
testing/results/mac-<sha>/
├── report.md             # top-level summary table
├── <case-id>/
│   ├── case.json         # parsed case definition
│   ├── case.log          # driver log
│   ├── expect.log        # per-expectation pass/fail
│   ├── screen.png        # window-only when possible
│   ├── sonicterm.log         # stdout/stderr of sonicterm-mac under test
│   ├── status            # PASS | FAIL
│   └── steps.sh          # generated keystroke script (for repro)
└── ...
```

## Cross-PM channel

Per CLAUDE.md §15, this directory IS the cross-PM smoke channel —
attach the relevant `screen.png` (or its commit-hash path) on a hot-file
PR rather than describing it free-form.

---
*Maintained alongside `testing/workflows/*.ps1` and `crates/sonicterm-windows/src/harness_pipe.rs`. PRs touching either should update this doc.*
