<#
.SYNOPSIS
  Regression test for issue #496 — run_case.ps1 must hard-preflight
  python3 + Pillow with exit 2 when invoked standalone.
.DESCRIPTION
  Two assertions, per Opus Step-2 APPROVED-DIAG test plan:

  1. python3 missing from PATH → run_case.ps1 must exit 2 with
     stderr mentioning "python3".
  2. python3 present but `from PIL import Image` fails → run_case.ps1
     must exit 2 with stderr mentioning "Pillow".

  Both checks must fire BEFORE OCR re-detect and case extraction
  (which themselves depend on python3 + Pillow), so we don't need a
  built sonicterm-windows.exe or a real cases.toml — failing fast at
  the very top is precisely the contract under test.

  Strategy:
   - Assertion 1: scrub every PATH dir that contains python3 / python3.exe,
     then invoke run_case.ps1. Empty Get-Command lookup → exit 2.
   - Assertion 2: prepend a sandbox dir to PATH containing a fake
     python3.cmd shim that exits non-zero on any `-c "from PIL ..."` call,
     so Get-Command succeeds but the import check fails.

  Exit 0 on pass, 1 on fail.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
$RunCase  = Join-Path $RepoRoot 'testing\workflows\run_case.ps1'
if (-not (Test-Path $RunCase)) { Write-Error "run_case.ps1 not found at $RunCase"; exit 1 }

$failures = @()
function Assert([bool]$cond, [string]$msg) {
  if ($cond) { Write-Host "  PASS: $msg" -ForegroundColor Green }
  else { Write-Host "  FAIL: $msg" -ForegroundColor Red; $script:failures += $msg }
}

$Sandbox = Join-Path $env:TEMP ("sonic-496-test-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0,8)))
New-Item -ItemType Directory -Force -Path $Sandbox | Out-Null

# Invoke run_case.ps1 in a child pwsh whose $env:PATH has been
# overridden in-process (Start-Process -Environment merges with the
# parent env on Windows and App Execution Aliases under WindowsApps
# resolve outside PATH entirely, so we need to do this from inside
# the child after launch).
function Invoke-RunCase([string]$childPath, [string]$tag) {
  $out = Join-Path $Sandbox "$tag.stdout"
  $err = Join-Path $Sandbox "$tag.stderr"
  $launcher = @"
`$env:PATH = '$($childPath.Replace("'","''"))'
& '$($RunCase.Replace("'","''"))' fake-case fake-out
exit `$LASTEXITCODE
"@
  $launcherFile = Join-Path $Sandbox "$tag.ps1"
  Set-Content -Path $launcherFile -Value $launcher -Encoding UTF8
  $child = Start-Process -FilePath 'pwsh' `
    -ArgumentList '-NoProfile','-File',$launcherFile `
    -WorkingDirectory $Sandbox `
    -PassThru -Wait -NoNewWindow `
    -RedirectStandardOutput $out `
    -RedirectStandardError  $err
  return @{
    rc     = $child.ExitCode
    stderr = if (Test-Path $err) { Get-Content -Raw $err } else { '' }
    stdout = if (Test-Path $out) { Get-Content -Raw $out } else { '' }
  }
}

# ------------------------------------------------------------------
# Assertion 1: python3 missing → exit 2, stderr mentions python3.
# Build a minimal PATH containing only System32 + the pwsh dir, with
# no python3 anywhere, and explicitly excluding any WindowsApps dir
# (which resolves python3 via App Execution Alias even when PATH is
# pared down).
# ------------------------------------------------------------------
Write-Host "[1/2] python3 unavailable → expect exit 2" -ForegroundColor Cyan
$PwshDir = Split-Path -Parent (Get-Command pwsh).Source
$MinPath = "$PwshDir;C:\Windows\System32;C:\Windows"
$r1 = Invoke-RunCase $MinPath 'no-python'
Assert ($r1.rc -eq 2) "missing python3 → exit 2 (got $($r1.rc))"
Assert ($r1.stderr -match 'python3') "stderr mentions python3 (got: $($r1.stderr.Trim()))"

# ------------------------------------------------------------------
# Assertion 2: python3 present but Pillow import fails → exit 2,
# stderr mentions Pillow. Drop a fake python3.cmd shim ahead of the
# minimal PATH so Get-Command finds it but the PIL import always
# errors.
# ------------------------------------------------------------------
Write-Host "[2/2] python3 shim that fails PIL import → expect exit 2" -ForegroundColor Cyan
$ShimDir = Join-Path $Sandbox 'shim'
New-Item -ItemType Directory -Force -Path $ShimDir | Out-Null
Set-Content -Path (Join-Path $ShimDir 'python3.cmd') `
  -Value "@echo off`r`nexit /b 1" -Encoding ASCII

$ShimmedPath = "$ShimDir;$MinPath"
$r2 = Invoke-RunCase $ShimmedPath 'no-pillow'
Assert ($r2.rc -eq 2) "Pillow import fails → exit 2 (got $($r2.rc))"
Assert ($r2.stderr -match 'Pillow') "stderr mentions Pillow (got: $($r2.stderr.Trim()))"

# ------------------------------------------------------------------
# Cleanup
# ------------------------------------------------------------------
Remove-Item -Recurse -Force $Sandbox -ErrorAction SilentlyContinue

if ($failures.Count -gt 0) {
  Write-Host "`nFAILED: $($failures.Count) assertion(s)" -ForegroundColor Red
  $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
  exit 1
}
Write-Host "`nALL PASS" -ForegroundColor Green
exit 0
