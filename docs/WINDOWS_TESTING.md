
---

## §17 Windows visual-test workflow (PM repro recipe)

Sub-agents in this repo can't drive a GUI directly, but the foreground PM session on Windows CAN — using PowerShell + Win32 + SendKeys + screencap. Use this recipe when an issue needs visual confirmation (especially #461-class glyph rendering bugs).

### Setup
```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cd Q:\FunCode\sonic
cargo build --release -p sonicterm-windows
```

### Prereqs

#### OCR (optional but recommended)

Mirrors `brew install tesseract` from `testing/workflows/mac.sh`.

```powershell
winget install UB-Mannheim.TesseractOCR
tesseract --version  # verify install
```

Without tesseract, OCR-only cases (~7 of 23) gracefully SKIP per #492. With tesseract, they run.

### Harness driver

Primary entry point for the Windows visual gate:

```powershell
pwsh -File testing/workflows/windows.ps1 -All
pwsh -File testing/workflows/windows.ps1 -Case <case-id>
pwsh -File testing/workflows/windows.ps1 -Build -All
```

Results land under `testing/results/win-<git-short-sha>/` with per-case
`screen.png`, `case.json`, `expect.log`, and a top-level `report.md`.

#### Prereqs

- **PowerShell 7+** (`pwsh`). Windows PowerShell 5.1 is not supported.
- **`--features harness` Cargo flag (hard prereq).** Without it the
  named-pipe input bridge (`crates/sonicterm-windows/src/harness_pipe.rs`)
  is absent and Guard 3 fails fast:
  ```powershell
  cargo build --release -p sonicterm-windows --features harness
  ```
- **Git Bash** at `C:\Program Files\Git\bin\bash.exe` for cases that
  set `shell = "bash"`. Other cases don't need it.
- **tesseract — OPTIONAL** (see § OCR above). Missing tesseract skips
  OCR cases per #492.
- **No elevation.** Guard 5 rejects elevated shells (SendKeys cross-IL
  is dropped silently).
- **Defender note.** First run from a clean checkout may stall ~15 s
  on real-time scanning of the freshly-built binary. Pre-exclude the
  workspace or expect the warmup.

#### Guards (Guard 1–6)

| # | Guard | One-liner |
|---|---|---|
| 1 | Competing terminals | Refuse to start if a GUI terminal app is foreground-capable (#464; see § Guard 1 below). |
| 2 | Multi-PID tracking | Wait for every `sonicterm-windows.exe` PID spawned in this run to be ready; stale PIDs from prior runs are detected. |
| 3 | Pipe handshake | Connect to `\\.\pipe\sonicterm-harness-<pid>`; fail fast if `--features harness` was omitted. |
| 4 | Foreground verify | Confirm the SonicTerm window is foreground at keystroke time; SKIP (exit 77) rather than leak keys into another window. |
| 5 | No-elevation | Reject `IsElevated == true`. |
| 6 | Workspace clean | Confirm release binary mtime ≥ source mtime; warn-skip stale binaries. |

#### Bucket A / B / C input model

The driver dispatches keystrokes via one of three buckets:

- **Bucket A — SendKeys (legacy).** PowerShell `SendKeys` against the
  foreground window. Gated by Guard 4.
- **Bucket B — multi-PID SendKeys.** Tracks every spawned PID so a
  freshly-opened tab/window can receive its own burst without losing
  the prior PID.
- **Bucket C — named-pipe input.** When `--harness-input-pipe auto` is
  active (default), the consumer chain is:

  ```
  run_case.ps1 → Send-InputToHwnd.ps1 → NamedPipeClientStream → harness_pipe.rs
  ```

  Bypasses SendKeys entirely; keystrokes go straight into the VT layer
  in-process, so focus loss does not corrupt input.

#### `shell = "bash"` per-case field

User-visible knob (per #493/#500) on each `[[case]]`:

```toml
[[case]]
id    = "bash-pipe-grep"
shell = "bash"      # forces Git Bash; default is the user shell
```

Resolved via `C:\Program Files\Git\bin\bash.exe`. Missing Git Bash
makes the case SKIP, not FAIL.

#### Exit codes (CI contract)

| Code | Meaning |
|---|---|
| `0` | All cases PASS. |
| `1` | ≥ 1 case FAIL. |
| `77` | ≥ 1 case SKIP and 0 FAIL (soft green in CI). |

CI gates the merge on exit `0` or `77`.

### Launch with full glyph-render instrumentation
```powershell
Get-Process sonicterm-windows -ErrorAction SilentlyContinue | Stop-Process -Force
$env:RUST_LOG = "sonic::render::glyph=debug"   # PR-B1 instrumentation target
$p = Start-Process Q:\FunCode\sonic\target\release\sonicterm-windows.exe -PassThru
Start-Sleep 4
```

### Capture a window screenshot programmatically
```powershell
$st = Get-Process sonicterm-windows | Select-Object -First 1
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class W { [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n); [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h); [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r); [StructLayout(LayoutKind.Sequential)] public struct R { public int Left, Top, Right, Bottom; } }
"@
[W]::ShowWindow($st.MainWindowHandle, 3)   # SW_MAXIMIZE
[W]::SetForegroundWindow($st.MainWindowHandle)
$r = New-Object W+R; [W]::GetWindowRect($st.MainWindowHandle, [ref]$r)
Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save("Q:\tmp\sonicterm-shot.png", "Png")
```

### Drive Claude Code via SendKeys
```powershell
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait("claude{ENTER}")
Start-Sleep 12   # wait for Claude Code startup + first frame
# then re-capture as above
```

### Scrape glyph-render log for tofu emissions
```powershell
$log = "$env:LOCALAPPDATA\SonicTerm\Logs\sonicterm.log.$(Get-Date -Format yyyy-MM-dd)"
Select-String -Path $log -Pattern "tofu emitted" | Select-Object -Last 30
# Filter for specific codepoint
Select-String -Path $log -Pattern "U\+23F5"
# Find all non-ASCII chars emitted
Get-Content $log | Select-String "render::glyph" | Where-Object { $_.Line -match 'code_u32=(\d+)' -and [int]$matches[1] -gt 127 }
```

### Inspect a font for codepoint coverage (no `otfinfo` on Windows)
```powershell
$f = "Q:\FunCode\sonic\assets\fonts\RecMonoSt.Helens-Regular.ttf"
$bytes = [System.IO.File]::ReadAllBytes($f)
# Search for big-endian U+XXXX byte-pair (works for cmap format 4 + 12 short ranges)
# Replace 23, F5 with the target codepoint's bytes
$hits = 0; for ($i=0; $i -lt $bytes.Length-1; $i++) { if ($bytes[$i] -eq 0x23 -and $bytes[$i+1] -eq 0xF5) { $hits++ } }
"U+23F5 byte-pair count: $hits"
```

### Verify a fix without breaking your active session
1. `git worktree add Q:\tmp\sonicterm-<issue> -b fix/<name> main`
2. `cargo build --release -p sonicterm-windows --manifest-path Q:\tmp\sonicterm-<issue>\Cargo.toml`
3. Launch the new binary at `Q:\tmp\sonicterm-<issue>\target\release\sonicterm-windows.exe`
4. After PR opens: `git -C Q:\FunCode\sonic worktree remove Q:\tmp\sonicterm-<issue> --force`

### Common pitfalls
- **`Access is denied (os error 5)`** on `cargo build`: a running `sonicterm-windows.exe` holds the lock. Kill it first: `Get-Process sonicterm-windows | Stop-Process -Force`
- **No log output**: `RUST_LOG` filter must match the tracing target exactly. PR-B1's glyph instrumentation uses target `sonic::render::glyph` (NOT `sonicterm_gpu::render`). Use `sonic::render::glyph=debug` to see those lines.
- **Stale .ttf font marker check**: byte-pair search isn't 100% reliable for cmap format 12 (4-byte ranges encode the start codepoint differently). When in doubt: launch with the instrumentation and check `resolve_slot=Some(N)` vs `None` for the suspect codepoint.
- **Stuck worktree on disk**: even after `git worktree remove --force`, the `target/` directory may keep file locks via running processes. Kill processes first, then `Remove-Item -Recurse -Force Q:\tmp\sonicterm-*`.

### Guard 1: competing terminal detection

`testing/workflows/windows.ps1` refuses to start if a competing **GUI terminal application** is already running, because SendKeys lands in whatever window has foreground — a stray Windows Terminal stealing focus mid-case would silently corrupt results (#464).

The classifier is intentionally restricted to GUI terminal apps. Host shells / console hosts (`conhost`, `cmd`, `pwsh`, `powershell`) are NOT in the list — they don't compete for foreground in the way SendKeys cares about, and including them broke launching the harness from any `pwsh` session (#490). Bug #494 narrowed the list accordingly.

**Built-in list (16 names, matched case-insensitively against `.ProcessName`, no `.exe` suffix):**

`WindowsTerminal`, `wt`, `alacritty`, `mintty`, `wezterm-gui`, `wezterm`, `kitty`, `tabby`, `Hyper`, `ghostty`, `Warp`, `rio`, `ConEmu64`, `ConEmuC64`, `FluentTerminal`, `MobaXterm`.

**Environment overrides:**

| Variable | Effect |
|---|---|
| `SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1` | Global bypass — skip Guard 1 entirely. Use when running the driver from a competitor terminal during dev. |
| `SONICTERM_HARNESS_EXTRA_TERMS=name1,name2` | Comma-separated process names appended to the built-in list (case-insensitive, whitespace trimmed, empties ignored). No `.exe` suffix. |

**Self-test:** `pwsh -NoProfile -File testing\workflows\tests\Test-Guard1Classifier.ps1` (exits 0 on pass).

### Harness hardening mitigations (#488)

Three harness-hardening guards layered on top of Guards 1–6:

1. **First-launch Defender budget extension.** On the very first case of a `windows.ps1` invocation, Defender's real-time scan of the freshly built `sonicterm-windows.exe` can add 10–20s of cold-start latency. `run_case.ps1` checks for `$env:TEMP\sonic-harness-first-launch-flag`; while absent, the Guard-3 window-appear budget is bumped from **10s → 30s** and the marker is created. Subsequent cases revert to 10s. `windows.ps1` clears the marker via `try/finally` so a case-1 crash doesn't poison the next driver run. `$env:SONICTERM_HARNESS_WIN_TIMEOUT_S` still wins.

2. **UAC fast-fail (`requires_elevation`).** Cases may set `requires_elevation = true` in `testing/cases.toml`. When the harness is not running as Administrator, `run_case.ps1` logs `[FAIL unelevated_only]` and exits 1 **before** spawning `sonicterm-windows.exe` — avoiding a 30s+ stall on a UAC consent dialog the harness cannot dismiss. Fixture: `windows-elevation-fixture-uac-fast-fail` (§38).

3. **PrintWindow black-frame detection.** Some DWM/GPU configurations cause `PrintWindow(PW_RENDERFULLCONTENT)` to return a fully-black or fully-transparent bitmap even though the call succeeded. After PrintWindow returns true, the harness samples a deterministic 16×16 grid (256 points) across the captured bitmap; if every sample is RGB(0,0,0) or every sample has alpha=0, the case is demoted to SKIP with exit 77 and `skip_reason=printwindow_black_frame`. Full pixel scans are deliberately avoided (Opus #488 Step-2 caveat).

---
*Maintained alongside `testing/workflows/*.ps1` and `crates/sonicterm-windows/src/harness_pipe.rs`. PRs touching either should update this doc.*
