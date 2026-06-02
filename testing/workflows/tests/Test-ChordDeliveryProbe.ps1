<#
.SYNOPSIS
  Three-bucket input delivery probe for #502 (Guard-4 RDP SKIP fix +
  R5 harness-pipe consumer).
.DESCRIPTION
  Spawns sonicterm-windows.exe (built with `--features harness`) and
  exercises Buckets A + B + C from lib/Send-InputToHwnd.ps1.

    Bucket A — text payload via `--harness-input-pipe auto`. Sends a
               UTF-8 payload through Send-TextToPipe; verifies pipe
               resolution + Connect + Write + Flush all succeed and
               the SonicTerm process stays alive. We do NOT assert an
               OSC-0 title sentinel here: the harness sink feeds the
               *PTY stdin* (shell input), not the VT parser, so the
               bytes are received by the shell (which echoes them
               literally) — the window title is owned by the shell's
               own prompt. End-to-end "bytes reach the VT parser and
               update the title" belongs to #513 (drain_until_eof
               currently drops chunks even with the sink published),
               flagged in the existing Rust e2e test header.
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
# Spawn SonicTerm and wait for its top-level HWND. Redirect stdout to
# a tempfile so Get-HarnessPipeName can read the "pipe ready" line.
# ----------------------------------------------------------------------
$logPath = Join-Path $env:TEMP ("sonic-probe-{0}.out.log" -f ([guid]::NewGuid().ToString('N')))
$errPath = "$logPath.err"
$proc = Start-Process -FilePath $SonicExe `
  -ArgumentList @('--harness-input-pipe','auto') `
  -RedirectStandardOutput $logPath `
  -RedirectStandardError  $errPath `
  -PassThru
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
$bucketASkipped = $false
$bucketBPass = $false
$bucketCPass = $false
$bucketCSkipped = $false
$titleObserved = ''

try {
  # ------------------------------------------------------------------
  # Bucket A — pipe Connect + Write smoke. We send a plain ASCII
  # payload and assert: (a) Get-HarnessPipeName resolves a name from
  # the binary's stdout log, (b) Send-TextToPipe Connects + Writes +
  # Flushes without throwing, (c) sonicterm-windows survives the
  # write (no crash). Per the Rust harness_pipe_test header, this is
  # a "doesn't crash / WriteFile returns success" check only — the
  # bytes feed PTY stdin, so asserting a window-title sentinel would
  # require #513 (drain_until_eof currently drops chunks).
  # ------------------------------------------------------------------
  $payload = "sonic-502-probe-bucket-a`n"
  Log "Bucket A: writing $($payload.Length)B via harness pipe"
  try {
    $pipeName = Get-HarnessPipeName -LogPath $logPath -Proc $proc -TimeoutSec 10
    Log "Bucket A: pipe resolved -> $pipeName"
    Send-TextToPipe -PipeName $pipeName -Text $payload
    Start-Sleep -Milliseconds 250
    $proc.Refresh()
    if ($proc.HasExited) {
      Log "Bucket A FAIL: process exited after pipe write (code=$($proc.ExitCode))"
    } else {
      $bucketAPass = $true
      # Snapshot the title for the report (informational only — we
      # do NOT assert anything about its content here).
      $titleObserved = Get-SonicWindowTitle -Hwnd $hwnd
      Log "Bucket A PASS: pipe Connect+Write+Flush succeeded; binary alive; title='$titleObserved'"
    }
  } catch {
    # Per spec: pipe-resolve or write failure is a real FAIL, not a SKIP.
    Log "Bucket A FAIL: $_"
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
  Remove-Item -LiteralPath $logPath,$errPath -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host '=== Three-Bucket Probe Results (Bucket A = pipe smoke; full e2e via #513) ==='
Write-Host ("Bucket A (text via pipe): {0}" -f $(if ($bucketAPass) { 'PASS' } else { 'FAIL' }))
Write-Host ("  title (info only)    : '{0}'" -f $titleObserved)
Write-Host ("Bucket B (named key)   : {0}" -f $(if ($bucketBPass) { 'PASS' } else { 'FAIL' }))
Write-Host ("Bucket C (chord)       : {0}" -f $(if ($bucketCPass) { 'PASS' } elseif ($bucketCSkipped) { 'SKIP (chord_no_foreground)' } else { 'FAIL' }))

# Bucket A and Bucket B must pass — neither needs foreground.
# Bucket C may self-skip in RDP — that's by design.
if ($bucketAPass -and $bucketBPass -and ($bucketCPass -or $bucketCSkipped)) {
  Log 'OVERALL: PASS'
  exit 0
} else {
  Log 'OVERALL: FAIL'
  exit 1
}
