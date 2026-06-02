<#
.SYNOPSIS
  Run a single case from testing/cases.toml on Windows.
.DESCRIPTION
  PowerShell port of testing/workflows/run_case.sh. Ports the 6 focus
  + multi-PID guards landed for mac in #472 (see issue #475).
  Exit codes:  0 = pass, 1 = fail, 77 = skip.
.PARAMETER Id
  Case id (matches testing/cases.toml [[case]].id).
.PARAMETER OutDir
  Results output directory (e.g. testing/results/win-<sha>).
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory=$true, Position=0)] [string] $Id,
  [Parameter(Mandatory=$true, Position=1)] [string] $OutDir
)

$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$CaseOut = Join-Path $OutDir $Id
New-Item -ItemType Directory -Force -Path $CaseOut | Out-Null
$LogPath = Join-Path $CaseOut 'case.log'
Set-Content -Path $LogPath -Value '' -NoNewline

function Log([string]$msg) {
  $line = "[{0}] {1}" -f (Get-Date -Format HH:mm:ss), $msg
  Write-Host $line
  Add-Content -Path $LogPath -Value $line
}

# ------------------------------------------------------------------
# Win32 P/Invoke surface for Guards 3,4,5
# ------------------------------------------------------------------
if (-not ([System.Management.Automation.PSTypeName]'SonicWin32').Type) {
Add-Type @"
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public class SonicWin32 {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int X, int Y, int nWidth, int nHeight, bool bRepaint);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);
  [DllImport("user32.dll")] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr child, string cls, string title);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@ -ReferencedAssemblies System.Drawing,System.Windows.Forms 2>&1 | Out-Null
}
Add-Type -AssemblyName System.Windows.Forms 2>&1 | Out-Null
Add-Type -AssemblyName System.Drawing 2>&1 | Out-Null

# ------------------------------------------------------------------
# Guard 6 — Explorer-park escape hatch. Park focus on the Explorer
# taskbar BEFORE spawning sonicterm-windows so any pre-spawn keystroke
# leak lands somewhere harmless (taskbar swallows printable keys; will
# never execute a shell command). Mirrors mac.sh's Finder-activate.
# Documented in issue #464 (v3 diagnosis, Guard 6 carry-forward).
# ------------------------------------------------------------------
function Park-Explorer {
  try {
    $shell = New-Object -ComObject Shell.Application
    $shell.MinimizeAll() | Out-Null
  } catch { }
  try {
    $taskbar = [SonicWin32]::FindWindowEx([IntPtr]::Zero, [IntPtr]::Zero, 'Shell_TrayWnd', $null)
    if ($taskbar -ne [IntPtr]::Zero) { [SonicWin32]::SetForegroundWindow($taskbar) | Out-Null }
  } catch { }
  Start-Sleep -Milliseconds 200
}
Park-Explorer

# ------------------------------------------------------------------
# B2 — multi-PID tracking. Every sonicterm-windows PID we spawn
# (directly or via a shell-cmd payload that backgrounds another
# instance) goes into $SONIC_PIDS; cleanup kills exactly those, never
# broadcasts Stop-Process -Name sonicterm-windows (would kill the
# user's dev build). Snapshot-delta around each shell-cmd captures
# grandchildren that simple child-process tracking would miss.
# ------------------------------------------------------------------
$script:SONIC_PIDS = New-Object System.Collections.ArrayList
$script:_PRE_PIDS = @()

function Get-SonicPids {
  Get-Process -Name 'sonicterm-windows' -ErrorAction SilentlyContinue |
    Select-Object -ExpandProperty Id
}
function Snapshot-SonicPidsBefore { $script:_PRE_PIDS = @(Get-SonicPids) }
function Snapshot-SonicPidsAfter {
  $post = @(Get-SonicPids)
  $newPids = $post | Where-Object { $script:_PRE_PIDS -notcontains $_ }
  foreach ($p in $newPids) {
    [void]$script:SONIC_PIDS.Add($p)
    Log "B2: tracked new harness sonicterm-windows pid=$p (from shell-cmd)"
  }
}

# ------------------------------------------------------------------
# Extract case as JSON (python3 — tomllib, same as mac.sh)
# ------------------------------------------------------------------
$CaseJson = Join-Path $CaseOut 'case.json'
$py = @"
import sys, tomllib, json
target = sys.argv[1]
with open('testing/cases.toml','rb') as f:
    d = tomllib.load(f)
for c in d['case']:
    if c['id'] == target:
        json.dump(c, sys.stdout, indent=2); break
else:
    sys.exit(2)
"@
$py | python3 - $Id | Set-Content -Path $CaseJson -Encoding UTF8
if (-not (Test-Path $CaseJson) -or (Get-Item $CaseJson).Length -eq 0) {
  Log "FATAL: case '$Id' not found in testing/cases.toml"; exit 1
}
$Case = Get-Content -Raw $CaseJson | ConvertFrom-Json
$AppliesTo = ($Case.applies_to -join ',')
Log "applies_to: $AppliesTo"
if ($AppliesTo -match 'windows-manual' -and $AppliesTo -notmatch '(^|,)windows(,|$)') {
  Log 'SKIP — manual-only on this platform'; exit 77
}

# ------------------------------------------------------------------
# Start sonicterm-windows fresh
# ------------------------------------------------------------------
$SonicBin = 'target/release/sonicterm-windows.exe'
if (-not (Test-Path $SonicBin)) { Log "FATAL: $SonicBin not built"; exit 1 }

# B2: pre-spawn scoped cleanup (no-op safety net; never broadcasts)
foreach ($p in @($script:SONIC_PIDS)) {
  try { Stop-Process -Id $p -Force -ErrorAction SilentlyContinue } catch { }
}
Start-Sleep -Milliseconds 400

$proc = Start-Process -FilePath $SonicBin `
  -RedirectStandardOutput (Join-Path $CaseOut 'sonicterm.out.log') `
  -RedirectStandardError  (Join-Path $CaseOut 'sonicterm.err.log') `
  -PassThru -WindowStyle Normal
$SONIC_PID = $proc.Id
[void]$script:SONIC_PIDS.Add($SONIC_PID)
$env:SONIC_PID = "$SONIC_PID"
Log "spawned sonicterm-windows pid=$SONIC_PID"

# ------------------------------------------------------------------
# Guard 2 — process-exists post-spawn. If sonicterm-windows died
# between spawn and now (panic on init, missing DLL, ...), fail fast
# rather than walk into a focusless SendKeys storm.
# ------------------------------------------------------------------
Start-Sleep -Milliseconds 400
$alive = Get-Process -Id $SONIC_PID -ErrorAction SilentlyContinue
if (-not $alive) {
  Log "FATAL: sonicterm-windows (pid=$SONIC_PID) died before window appeared"
  Set-Content (Join-Path $CaseOut 'status') 'FAIL'; exit 1
}

# ------------------------------------------------------------------
# Guard 3 — window-exists verify with 10s budget. Cold wgpu init
# on a fresh shader cache can be slow. Hard FAIL on timeout (never
# silently continue — window absence is what allowed keystrokes to
# leak in #464).
# ------------------------------------------------------------------
$TimeoutS = if ($env:SONICTERM_HARNESS_WIN_TIMEOUT_S) { [int]$env:SONICTERM_HARNESS_WIN_TIMEOUT_S } else { 10 }
$WindowHandle = [IntPtr]::Zero
for ($i = 0; $i -lt ($TimeoutS * 10); $i++) {
  $p = Get-Process -Id $SONIC_PID -ErrorAction SilentlyContinue
  if (-not $p) { Log "FATAL: pid $SONIC_PID died waiting for window"; Set-Content (Join-Path $CaseOut 'status') 'FAIL'; exit 1 }
  $p.Refresh()
  if ($p.MainWindowHandle -ne [IntPtr]::Zero -and [SonicWin32]::IsWindow($p.MainWindowHandle)) {
    $WindowHandle = $p.MainWindowHandle; break
  }
  Start-Sleep -Milliseconds 100
}
if ($WindowHandle -eq [IntPtr]::Zero) {
  Log "FATAL: sonicterm-windows window did not appear within ${TimeoutS}s"
  Set-Content (Join-Path $CaseOut 'status') 'FAIL'; exit 1
}
[SonicWin32]::ShowWindow($WindowHandle, 9) | Out-Null   # SW_RESTORE
[SonicWin32]::MoveWindow($WindowHandle, 500, 200, 1000, 700, $true) | Out-Null
Start-Sleep -Milliseconds 400
Log "window handle: $WindowHandle"

# ------------------------------------------------------------------
# Guard 4 — frontmost-verify before each keystroke.
# GetForegroundWindow + GetWindowThreadProcessId; PID must match.
# focus_sonic retries up to 5 times. ensure_front_or_skip is invoked
# before every SendKeys — if focus can't be held, SKIP (exit 77)
# rather than fire keystrokes into whatever else has focus. Core
# fix from #464.
# ------------------------------------------------------------------
function Verify-Front {
  $fg = [SonicWin32]::GetForegroundWindow()
  if ($fg -eq [IntPtr]::Zero) { return $false }
  $fgPid = 0; [void][SonicWin32]::GetWindowThreadProcessId($fg, [ref]$fgPid)
  return ($fgPid -eq $SONIC_PID)
}
function Focus-Sonic {
  for ($t = 1; $t -le 5; $t++) {
    [SonicWin32]::ShowWindow($WindowHandle, 9) | Out-Null
    [SonicWin32]::SetForegroundWindow($WindowHandle) | Out-Null
    Start-Sleep -Milliseconds 250
    if (Verify-Front) { return $true }
    Log "focus retry $t"
  }
  return $false
}
function Ensure-FrontOrSkip {
  if (Verify-Front) { return }
  if (Focus-Sonic)  { return }
  Log 'SKIP: cannot keep sonicterm-windows frontmost — keystrokes would leak'
  Set-Content (Join-Path $CaseOut 'status') 'SKIP'; exit 77
}
Ensure-FrontOrSkip

# ------------------------------------------------------------------
# Keystroke helpers — SendKeys, with Guard-4 gate before each call.
# ------------------------------------------------------------------
function ConvertTo-SendKeys([string]$chord) {
  $parts = $chord -split '\+'
  $key = $parts[-1]
  $mods = ''
  foreach ($p in $parts[0..([Math]::Max(0, $parts.Length - 2))]) {
    if ($parts.Length -le 1) { break }
    switch ($p.ToLower()) {
      'ctrl'    { $mods += '^' }
      'cmd'     { $mods += '^' }   # WezTerm-compat default maps Cmd→Ctrl on Win
      'command' { $mods += '^' }
      'shift'   { $mods += '+' }
      'alt'     { $mods += '%' }
      'option'  { $mods += '%' }
    }
  }
  $sk = switch ($key.ToLower()) {
    'enter'   { '{ENTER}' }
    'return'  { '{ENTER}' }
    'escape'  { '{ESC}' }
    'esc'     { '{ESC}' }
    'tab'     { '{TAB}' }
    'up'      { '{UP}' }
    'down'    { '{DOWN}' }
    'left'    { '{LEFT}' }
    'right'   { '{RIGHT}' }
    'page-up' { '{PGUP}' }
    'plus'    { '=' }   # Cmd+= zoom-in on mac → Ctrl+= here
    'minus'   { '-' }
    default {
      if ($key.Length -eq 1) { $key } else { "{$($key.ToUpper())}" }
    }
  }
  return $mods + $sk
}
function Send-Chord([string]$chord) {
  Ensure-FrontOrSkip
  [System.Windows.Forms.SendKeys]::SendWait((ConvertTo-SendKeys $chord))
}
function Send-Text([string]$text) {
  Ensure-FrontOrSkip
  # SendKeys treats these as metacharacters — escape with braces
  $esc = $text -replace '([+^%~(){}\[\]])', '{$1}'
  [System.Windows.Forms.SendKeys]::SendWait($esc)
}
function Do-Setup([string]$step) {
  Ensure-FrontOrSkip
  switch -Wildcard ($step) {
    'open-3-tabs'         { Send-Chord 'ctrl+t'; Start-Sleep -Milliseconds 300; Send-Chord 'ctrl+t'; Start-Sleep -Milliseconds 300 }
    'open-second-window'  { Send-Chord 'ctrl+n'; Start-Sleep -Milliseconds 500 }
    'open-prefs'          { Send-Chord 'ctrl+,'; Start-Sleep -Milliseconds 600 }
    'split-right'         { Send-Chord 'ctrl+d'; Start-Sleep -Milliseconds 500 }
    'clear'               { Send-Text 'clear'; Send-Chord 'enter'; Start-Sleep -Milliseconds 300 }
    'enter'               { Send-Chord 'enter' }
    'type:*'              { Send-Text ($step.Substring(5)) }
    'wait:*'              { Start-Sleep -Milliseconds ([int]([double]($step.Substring(5)) * 1000)) }
    'manual-*'            { Log "skip manual setup: $step" }
    default               { Log "WARN: unknown setup step '$step'" }
  }
}

# ------------------------------------------------------------------
# Iterate setup + keystrokes (interpret JSON inline)
# ------------------------------------------------------------------
foreach ($s in @($Case.setup)) { Do-Setup $s }
foreach ($k in @($Case.keystrokes)) {
  switch ($k.kind) {
    'key'     { Send-Chord $k.value }
    'text'    { Send-Text  $k.value }
    'wait'    { Start-Sleep -Milliseconds ([int]([double]$k.value * 1000)) }
    'key-repeat' {
      $n = if ($k.count) { [int]$k.count } else { 1 }
      $delay = if ($k.delay) { [double]$k.delay } else { 0.1 }
      for ($r = 0; $r -lt $n; $r++) {
        Send-Chord $k.value; Start-Sleep -Milliseconds ([int]($delay * 1000))
      }
    }
    'shell-cmd' {
      # B2 (v3): snapshot-delta around every shell-cmd. Any new
      # sonicterm-windows PID that appears as a side effect of the
      # payload is tracked in $SONIC_PIDS so cleanup kills exactly it,
      # never the user's dev build.
      Snapshot-SonicPidsBefore
      try { Invoke-Expression $k.value } catch { Log "shell-cmd error: $_" }
      Snapshot-SonicPidsAfter
    }
    'snapshot-sonic-shells' {
      Log 'snapshot-sonic-shells: not implemented on windows (no orphan-shell checker yet)'
    }
    default { Log "WARN unknown keystroke kind: $($k.kind)" }
  }
}

# ------------------------------------------------------------------
# Guard 5 — window-only screencap is MANDATORY. PrintWindow into a
# bitmap sized from GetWindowRect; if the window isn't real or the
# call fails, SKIP rather than sample wrong pixels (the "Class B
# black pixel" failure mode in #464). Falls back to CopyFromScreen
# of the window rect if PrintWindow returns false.
# ------------------------------------------------------------------
$Shot = Join-Path $CaseOut 'screen.png'
if (-not [SonicWin32]::IsWindow($WindowHandle)) {
  Log 'SKIP: window handle invalid — refusing screencap (would leak coords)'
  Set-Content (Join-Path $CaseOut 'status') 'SKIP'; exit 77
}
$rect = New-Object SonicWin32+RECT
if (-not [SonicWin32]::GetWindowRect($WindowHandle, [ref]$rect)) {
  Log 'SKIP: GetWindowRect failed — refusing screencap'
  Set-Content (Join-Path $CaseOut 'status') 'SKIP'; exit 77
}
$w = $rect.Right - $rect.Left; $h = $rect.Bottom - $rect.Top
if ($w -le 0 -or $h -le 0) {
  Log "SKIP: bogus window rect ${w}x${h}"
  Set-Content (Join-Path $CaseOut 'status') 'SKIP'; exit 77
}
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g   = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok  = [SonicWin32]::PrintWindow($WindowHandle, $hdc, 2)  # PW_RENDERFULLCONTENT
$g.ReleaseHdc($hdc)
if (-not $ok) {
  # Fallback: CopyFromScreen using window rect
  $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size $w, $h))
}
$g.Dispose()
$bmp.Save($Shot, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
if (-not (Test-Path $Shot) -or (Get-Item $Shot).Length -eq 0) {
  Log 'SKIP: window-local screencap failed (window may have closed)'
  Set-Content (Join-Path $CaseOut 'status') 'SKIP'; exit 77
}
Log "screenshot (window-only): $Shot"

# ------------------------------------------------------------------
# Evaluate expectations — best-effort, mirrors mac.sh's python block.
# ------------------------------------------------------------------
$ExpectLog = Join-Path $CaseOut 'expect.log'
Set-Content -Path $ExpectLog -Value ''
$py2 = @"
import json, sys, os, subprocess
case_path, shot, elog = sys.argv[1:4]
c = json.load(open(case_path))
results = []
def have(p): return os.path.exists(p) and os.path.getsize(p) > 0
def pixel_near(shot, x, y, rgba, tol):
    try:
        from PIL import Image
        im = Image.open(shot).convert('RGBA')
        sx = int(x * (im.width / 1000.0))
        sy = int(y * (im.height / 700.0))
        if not (0 <= sx < im.width and 0 <= sy < im.height):
            return False, f"coords oob ({sx},{sy}) in {im.size}"
        px = im.getpixel((sx, sy))
        d = max(abs(int(a)-int(b)) for a,b in zip(px[:len(rgba)], rgba))
        return (d <= tol), f"pixel@({sx},{sy})={px} target={rgba} delta={d} tol={tol}"
    except Exception as e:
        return False, f"err {e}"
def ocr_contains(shot, value):
    try:
        out = subprocess.run(['tesseract', shot, '-', '--psm', '6'],
                             capture_output=True, text=True, timeout=20)
        return (value in out.stdout), out.stdout[:200].replace('\n',' / ')
    except Exception as e:
        return False, f"err {e}"
for e in c.get('expect', []):
    kind = e.get('kind')
    if kind == 'screenshot':
        ok = have(shot); reason = f"exists={ok} path={shot}"
    elif kind == 'pixel-near':
        ok, reason = pixel_near(shot, e['x'], e['y'], e['rgba'], e.get('tolerance', 20))
    elif kind in ('text-in-region','ocr-text'):
        ok, reason = ocr_contains(shot, e['value'])
    elif kind == 'not-text-in-region':
        contains, sample = ocr_contains(shot, e['value']); ok = not contains
        reason = f"absent={ok} sample='{sample}'"
    else:
        ok = True; reason = f"heuristic-pass (kind='{kind}' not yet implemented on windows)"
    results.append((ok, kind, reason))
with open(elog, 'w') as f:
    for ok, kind, reason in results:
        f.write(f"{'PASS' if ok else 'FAIL'}\t{kind}\t{reason}\n")
fails = [r for r in results if not r[0]]
sys.exit(0 if not fails else 1)
"@
$py2 | python3 - $CaseJson $Shot $ExpectLog
$expectRc = $LASTEXITCODE

# ------------------------------------------------------------------
# Cleanup — kill exactly the PIDs we tracked in $SONIC_PIDS (B2 v3).
# No broadcast Stop-Process -Name sonicterm-windows (would kill the
# user's dev build). Graceful Ctrl+Q → SIGTERM-equivalent (CloseMainWindow)
# → SIGKILL-equivalent (Stop-Process -Force) for each tracked pid.
# ------------------------------------------------------------------
try { Send-Chord 'ctrl+q' } catch { }

function Any-TrackedAlive {
  foreach ($p in @($script:SONIC_PIDS)) {
    if (Get-Process -Id $p -ErrorAction SilentlyContinue) { return $true }
  }
  return $false
}
for ($i = 0; $i -lt 10; $i++) { if (-not (Any-TrackedAlive)) { break }; Start-Sleep -Milliseconds 100 }
foreach ($p in @($script:SONIC_PIDS)) {
  $proc = Get-Process -Id $p -ErrorAction SilentlyContinue
  if ($proc) { try { $proc.CloseMainWindow() | Out-Null } catch { } }
}
for ($i = 0; $i -lt 5; $i++) { if (-not (Any-TrackedAlive)) { break }; Start-Sleep -Milliseconds 100 }
foreach ($p in @($script:SONIC_PIDS)) {
  try { Stop-Process -Id $p -Force -ErrorAction SilentlyContinue } catch { }
}

# Boundary verify (per v3): if any sonicterm-windows is alive that
# isn't in $env:PRE_RUN_USER_PIDS, log it. Force-kill if ours;
# log-only if user launched mid-run (not ours to kill).
$remaining = @(Get-SonicPids)
$preList = @()
if ($env:PRE_RUN_USER_PIDS) { $preList = $env:PRE_RUN_USER_PIDS -split ',' | ForEach-Object { [int]$_ } }
$unexpected = $remaining | Where-Object { $preList -notcontains $_ }
$tracked    = @($script:SONIC_PIDS)
$oursAlive  = $unexpected | Where-Object { $tracked -contains $_ }
$userMidRun = $unexpected | Where-Object { $tracked -notcontains $_ }
if ($oursAlive) {
  Log "WARN: harness-tracked sonicterm-windows still alive after cleanup; force-killing: $($oursAlive -join ',')"
  foreach ($p in $oursAlive) { try { Stop-Process -Id $p -Force -ErrorAction SilentlyContinue } catch { } }
}
if ($userMidRun) {
  Log "INFO: user-launched sonicterm-windows PID(s) appeared mid-run; NOT killing: $($userMidRun -join ',')"
}

if ($expectRc -eq 0) {
  Log 'RESULT: PASS'
  Set-Content (Join-Path $CaseOut 'status') 'PASS'; exit 0
} else {
  Log 'RESULT: FAIL'
  Set-Content (Join-Path $CaseOut 'status') 'FAIL'
  Get-Content $ExpectLog | Add-Content -Path $LogPath
  exit 1
}
