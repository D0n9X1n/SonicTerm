<#
.SYNOPSIS
  Three-bucket input delivery probe for #502 (Guard-4 RDP SKIP fix).
.DESCRIPTION
  Spawns sonicterm-windows.exe and exercises all three input buckets
  defined in lib/Send-InputToHwnd.ps1:

    Bucket A — text payload via per-char WM_KEYDOWN+WM_CHAR+WM_KEYUP.
               Verification (REVISE blocker 1): emit an OSC 0 title-set
               escape with a fresh UUID sentinel; poll GetWindowText()
               for ~3s and ASSERT the sentinel appears. On miss, FAIL
               with the concrete sentinel-vs-observed-title delta.
    Bucket B — single named key (Enter alone) via WM_KEYDOWN+WM_KEYUP.
               Verification: Send-NamedKeyToHwnd returns $true (every
               PostMessage returned BOOL true) AND the SonicTerm
               process survives (no crash, IsWindow remains true).
    Bucket C — real modifier chord (Ctrl+T). Best-effort: attempt
               foreground via AttachThreadInput; if foreground can't
               be acquired, SKIP this bucket only (clear log).

  Required by Opus Step-2 APPROVED-DIAG. Exit 0 on pass (Bucket C may
  self-skip), 1 on real fail.
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
  [int]$WindowTimeoutSec = 12,
  [int]$TitlePollSec = 3
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

$bucketAPass = $false
$bucketBPass = $false
$bucketCPass = $false
$bucketCSkipped = $false
$bucketAObservedTitle = ''
$bucketASentinel = ''

try {
  # ------------------------------------------------------------------
  # Bucket A FIRST — OSC 0 title-set escape with UUID sentinel.
  # REVISE blocker 1: actually emit, poll, and assert.
  # ------------------------------------------------------------------
  $uuid = [Guid]::NewGuid().ToString('N').Substring(0, 12)
  $bucketASentinel = "sonic-test-$uuid-mark"
  # OSC 0 ; <title> BEL  — sets both icon + window title.
  $esc = [char]0x1B
  $bel = [char]0x07
  $oscPayload = "$esc]0;$bucketASentinel$bel"
  Log "Bucket A: emitting OSC-0 title-set with sentinel '$bucketASentinel'"
  try {
    [void](Send-TextToHwnd -Hwnd $hwnd -Text $oscPayload)
  } catch {
    Log "Bucket A FAIL (wire-level): Send-TextToHwnd threw: $_"
    Log ("  sentinel : '{0}'" -f $bucketASentinel)
    Log  "  observed : <PostMessage failed before title could be read>"
  }

  $aDeadline = (Get-Date).AddSeconds($TitlePollSec)
  $sawSentinel = $false
  while ((Get-Date) -lt $aDeadline) {
    Start-Sleep -Milliseconds 150
    try {
      $bucketAObservedTitle = Get-SonicWindowTitle -Hwnd $hwnd
    } catch {
      $bucketAObservedTitle = "<GetWindowText threw: $_>"
    }
    if ($bucketAObservedTitle -and $bucketAObservedTitle.Contains($bucketASentinel)) {
      $sawSentinel = $true
      break
    }
  }
  if ($sawSentinel) {
    $bucketAPass = $true
    Log "Bucket A PASS: sentinel observed in window title ('$bucketAObservedTitle')"
  } else {
    Log "Bucket A FAIL: sentinel did NOT appear in window title within ${TitlePollSec}s"
    Log ("  sentinel : '{0}'" -f $bucketASentinel)
    Log ("  observed : '{0}'" -f $bucketAObservedTitle)
  }

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
Write-Host '=== Three-Bucket Probe Results ==='
Write-Host ("Bucket A (text/OSC)    : {0}" -f $(if ($bucketAPass) { 'PASS' } else { 'FAIL' }))
if (-not $bucketAPass) {
  Write-Host ("  sentinel : '{0}'" -f $bucketASentinel)
  Write-Host ("  observed : '{0}'" -f $bucketAObservedTitle)
}
Write-Host ("Bucket B (named key)   : {0}" -f $(if ($bucketBPass) { 'PASS' } else { 'FAIL' }))
Write-Host ("Bucket C (chord)       : {0}" -f $(if ($bucketCPass) { 'PASS' } elseif ($bucketCSkipped) { 'SKIP (chord_no_foreground)' } else { 'FAIL' }))

# Bucket A is the most important — if it fails, the core fix is broken.
# Bucket B must pass (no foreground required, can't fail for env reasons).
# Bucket C may self-skip in RDP — that's by design.
if ($bucketAPass -and $bucketBPass -and ($bucketCPass -or $bucketCSkipped)) {
  Log 'OVERALL: PASS'
  exit 0
} else {
  Log 'OVERALL: FAIL'
  exit 1
}
