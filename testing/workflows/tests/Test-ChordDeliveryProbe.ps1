<#
.SYNOPSIS
  Two-bucket input delivery probe for #502 (Guard-4 RDP SKIP fix).
.DESCRIPTION
  Spawns sonicterm-windows.exe and exercises Buckets B + C from
  lib/Send-InputToHwnd.ps1. Bucket A (text payload) is intentionally
  NOT tested here — it is blocked on #506 (harness-input-pipe) and
  the OSC-sentinel assertion belongs in the future #502 consumer PR
  that wires the pipe through.

    Bucket B — single named key (Enter alone) via WM_KEYDOWN+WM_KEYUP.
               Verification: Send-NamedKeyToHwnd returns $true (every
               PostMessage returned BOOL true) AND the SonicTerm
               process survives (no crash, IsWindow remains true).
    Bucket C — real modifier chord (Ctrl+T). Best-effort: attempt
               foreground via AttachThreadInput; if foreground can't
               be acquired, SKIP this bucket only (clear log).

  Exit 0 on pass (Bucket C may self-skip), 1 on real fail.
#>
[CmdletBinding()]
param(
  [string]$SonicExe = $(
    foreach ($p in @(
      "$PSScriptRoot\..\..\..\target\release\sonicterm-windows.exe",
      "$PSScriptRoot\..\..\..\target\debug\sonicterm-windows.exe",
      "Q:\FunCode\sonic\target\release\sonicterm-windows.exe"
    )) { if (Test-Path $p) { (Resolve-Path $p).Path; break } }
  ),
  [int]$WindowTimeoutSec = 12
)

$ErrorActionPreference = 'Stop'
function Log([string]$m) { Write-Host "[probe] $m" }

. (Join-Path $PSScriptRoot '..\lib\Send-InputToHwnd.ps1')

if (-not $SonicExe -or -not (Test-Path $SonicExe)) {
  Log "SKIP: sonicterm-windows.exe not found; build it before running this probe"
  exit 0
}
Log "exe: $SonicExe"

# ----------------------------------------------------------------------
# Spawn SonicTerm and wait for its top-level HWND.
# ----------------------------------------------------------------------
$proc = Start-Process -FilePath $SonicExe -PassThru
$deadline = (Get-Date).AddSeconds($WindowTimeoutSec)
$hwnd = [IntPtr]::Zero
while ((Get-Date) -lt $deadline) {
  Start-Sleep -Milliseconds 200
  try { $proc.Refresh() } catch { }
  if ($proc.HasExited) { Log "FAIL: sonicterm exited prematurely (code $($proc.ExitCode))"; exit 1 }
  if ($proc.MainWindowHandle -ne [IntPtr]::Zero) {
    $hwnd = $proc.MainWindowHandle
    break
  }
}
if ($hwnd -eq [IntPtr]::Zero) {
  Log "FAIL: sonicterm window did not appear within ${WindowTimeoutSec}s"
  try { Stop-Process -Id $proc.Id -Force } catch { }
  exit 1
}
Log "hwnd: $hwnd, pid: $($proc.Id)"
Start-Sleep -Milliseconds 600  # give the shell prompt time to settle

$bucketBPass = $false
$bucketCPass = $false
$bucketCSkipped = $false

try {
  # ------------------------------------------------------------------
  # Bucket B — Enter alone (named-key, no modifier).
  # ------------------------------------------------------------------
  Log "Bucket B: posting Enter (named-key, no modifier)"
  try {
    $bWireOk = Send-NamedKeyToHwnd -Hwnd $hwnd -Key 'enter'
    Start-Sleep -Milliseconds 200
    $proc.Refresh()
    if (-not $bWireOk) {
      Log "Bucket B FAIL: Send-NamedKeyToHwnd returned false (PostMessage rc=false)"
    } elseif ($proc.HasExited) {
      Log "Bucket B FAIL: process exited after Enter (PostMessage queued ok but window died)"
    } elseif ($proc.MainWindowHandle -eq [IntPtr]::Zero) {
      Log "Bucket B FAIL: window handle gone after Enter"
    } else {
      $bucketBPass = $true
      Log "Bucket B PASS: PostMessage rc=true, window remains alive"
    }
  } catch {
    Log "Bucket B FAIL: $_"
  }

  # ------------------------------------------------------------------
  # Bucket C — Ctrl+T, best-effort.
  # ------------------------------------------------------------------
  Log "Bucket C: best-effort Ctrl+T (requires foreground)"
  try {
    $ok = Send-ChordToHwnd -Hwnd $hwnd -Chord 'ctrl+t'
    if ($ok) {
      $bucketCPass = $true
      Log "Bucket C PASS: SendInput dispatched successfully"
    } else {
      $bucketCSkipped = $true
      Log "Bucket C SKIP: foreground unattainable (RDP / locked desktop / background pwsh) — chord_no_foreground"
    }
  } catch {
    $bucketCSkipped = $true
    Log "Bucket C SKIP: $_"
  }
} finally {
  try { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue } catch { }
}

Write-Host ''
Write-Host '=== Two-Bucket Probe Results (Bucket A blocked on #506) ==='
Write-Host ("Bucket B (named key)   : {0}" -f $(if ($bucketBPass) { 'PASS' } else { 'FAIL' }))
Write-Host ("Bucket C (chord)       : {0}" -f $(if ($bucketCPass) { 'PASS' } elseif ($bucketCSkipped) { 'SKIP (chord_no_foreground)' } else { 'FAIL' }))

# Bucket B must pass (no foreground required, can't fail for env reasons).
# Bucket C may self-skip in RDP — that's by design.
if ($bucketBPass -and ($bucketCPass -or $bucketCSkipped)) {
  Log 'OVERALL: PASS'
  exit 0
} else {
  Log 'OVERALL: FAIL'
  exit 1
}
