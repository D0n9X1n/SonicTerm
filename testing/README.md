# SonicTerm visual test harness

Local-session visual gate for render/UX-affecting changes. PM (mac side) runs
`just visual mac` before merging any PR that touches the GUI smoke surface
(see CLAUDE.md §13). The Windows PM will add `testing/workflows/windows.ps1`
in a follow-up; both drivers consume `testing/cases.toml` — that file is the
**single source of truth** for what we promise to verify.

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

## Windows extension point

`testing/workflows/windows.ps1` is intentionally absent in this PR. The
Windows PM owns it. Contract:

- Consume `testing/cases.toml` with the same schema.
- Emit results into `testing/results/win-<git-short-sha>/`.
- Skip cases whose `applies_to` doesn't include `"windows"`.
- Honor the same env / arg shape: `--build`, `--case <id>`, `--all`.

See `testing/workflows/mac.sh` + `run_case.sh` for the reference
implementation. The Python expectation evaluator inside `run_case.sh`
should port verbatim — replacing only `osascript`/`screencapture` with
the Windows equivalents (PowerShell `SendKeys` + `Add-Type
System.Windows.Forms` for screenshots).

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
