
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
