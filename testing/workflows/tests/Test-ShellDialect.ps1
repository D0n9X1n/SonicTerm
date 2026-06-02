<#
.SYNOPSIS
  Regression test for the #493 shell-dialect fix in run_case.ps1.
.DESCRIPTION
  Validates three behaviours of the per-case `shell:` field handling:
    1. shell="bash" + bash NOT on PATH  -> exit 77, skip_reason=bash_unavailable
    2. shell="bash" + stub bash on PATH -> picks up the stub (override written)
    3. shell="invalid"                  -> exit 1, FATAL validation error

  These tests stub the spawn portion of run_case.ps1 — they isolate the
  shell-selection logic by re-implementing its pre-spawn block against
  synthetic case JSON. We do NOT actually build sonicterm-windows or
  spawn it; #493 is a harness-layer fix.

  Exit 0 on pass, 1 on real fail.
#>
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'

$ScriptRoot = $PSScriptRoot
Push-Location $ScriptRoot

# PR #500 revise — dot-source the SAME Resolve-BashExe used by
# production run_case.ps1, eliminating the mirror-drift bug that
# previously hid \x08 corruption in the production candidate strings.
$LibRoot = Resolve-Path (Join-Path $ScriptRoot '..\lib')
. (Join-Path $LibRoot 'Resolve-BashExe.ps1')

# Gate-the-gate: byte-scan production run_case.ps1 for any 0x08
# backspace bytes. If '\b' escape interpretation ever re-corrupts the
# file, this assert fires before the behavioural tests run.
$ProdScript = Resolve-Path (Join-Path $ScriptRoot '..\run_case.ps1')
$prodBytes  = [System.IO.File]::ReadAllBytes($ProdScript)
$bsCount    = @($prodBytes | Where-Object { $_ -eq 0x08 }).Count
if ($bsCount -gt 0) {
  Write-Host "  FAIL run_case.ps1 contains $bsCount literal 0x08 (backspace) byte(s)" -ForegroundColor Red
  Write-Host "       likely '\b' escape corruption — quote hard-coded paths with single quotes" -ForegroundColor Red
  exit 1
} else {
  Write-Host "  PASS run_case.ps1 contains no 0x08 corruption bytes" -ForegroundColor Green
}

$TmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("sonic493-test-" + [Guid]::NewGuid().ToString('N').Substring(0,8))
New-Item -ItemType Directory -Force -Path $TmpRoot | Out-Null
$origAppData = $env:APPDATA
$origPath    = $env:PATH

$fail = 0
function Assert([bool]$cond, [string]$msg) {
  if ($cond) { Write-Host "  PASS $msg" -ForegroundColor Green }
  else       { Write-Host "  FAIL $msg" -ForegroundColor Red; $script:fail++ }
}

function Reset-Env {
  $env:APPDATA = Join-Path $TmpRoot ("appdata-" + [Guid]::NewGuid().ToString('N').Substring(0,6))
  New-Item -ItemType Directory -Force -Path $env:APPDATA | Out-Null
  $env:PATH = $origPath
}

$AllowedShells = @('bash','cmd','pwsh','cross')

# Mirror of run_case.ps1's pre-spawn block. The bash-resolver itself is
# now dot-sourced from the shared lib (see top of file), so this stub
# can no longer drift from production on path resolution. The rest of
# the slice (shell validation + override write) remains duplicated here
# because it is a few lines and easier to test in isolation than
# spawning run_case.ps1 end-to-end (which requires sonicterm-windows).
function Test-Shell-Selection {
  param([object]$Case, [string]$ConfigPath)
  $caseShell = if ($Case.PSObject.Properties.Name -contains 'shell' -and $Case.shell) {
    [string]$Case.shell
  } else { 'cross' }
  if ($AllowedShells -notcontains $caseShell) {
    return @{ ExitCode = 1; Reason = "FATAL invalid shell '$caseShell'" }
  }
  $appliesTo = ($Case.applies_to -join ',')
  if ($caseShell -eq 'bash' -and $appliesTo -match '(^|,)windows(,|$)') {
    $bashPath = Resolve-BashExe
    if (-not $bashPath) {
      return @{ ExitCode = 77; Reason = 'bash_unavailable' }
    }
    $tomlPath = $bashPath -replace '\\','\\\\'
    $override = "[terminal]`nshell = `"$tomlPath`"`n"
    Set-Content -Path $ConfigPath -Value $override -Encoding UTF8 -NoNewline
    return @{ ExitCode = 0; Reason = 'override-written'; BashPath = $bashPath }
  }
  return @{ ExitCode = 0; Reason = 'no-override' }
}

# ------------------------------------------------------------------
# Test 1: shell="bash" + bash absent  =>  exit 77, bash_unavailable
# ------------------------------------------------------------------
Write-Host '[test 1] shell=bash, PATH scrubbed -> exit 77 bash_unavailable'
Reset-Env
$env:PATH = Join-Path $TmpRoot 'empty-path'
New-Item -ItemType Directory -Force -Path $env:PATH | Out-Null

$case1 = [pscustomobject]@{ id = 't1'; applies_to = @('windows'); shell = 'bash' }
$cfg1  = Join-Path $env:APPDATA 'SonicTerm\sonicterm.toml'
New-Item -ItemType Directory -Force -Path (Split-Path $cfg1 -Parent) | Out-Null

# Probe: only run this test if git-bash hard-coded paths ALSO absent.
# (On a dev box those may exist regardless of PATH; the test is honest
# about that environment-skip rather than spuriously failing.)
$hardCoded = @('C:\Program Files\Git\bin\bash.exe','C:\Program Files (x86)\Git\bin\bash.exe') |
  Where-Object { Test-Path $_ }
if ($hardCoded) {
  Write-Host "  SKIP test 1 — git-bash present at hard-coded path: $($hardCoded[0])" -ForegroundColor Yellow
} else {
  $r1 = Test-Shell-Selection -Case $case1 -ConfigPath $cfg1
  Assert ($r1.ExitCode -eq 77) "exit code 77 (got $($r1.ExitCode))"
  Assert ($r1.Reason -eq 'bash_unavailable') "reason='bash_unavailable' (got '$($r1.Reason)')"
  Assert (-not (Test-Path $cfg1)) "no config override written when skipping"
}

# ------------------------------------------------------------------
# Test 2: shell="bash" + stub bash on PATH  =>  override written
# ------------------------------------------------------------------
Write-Host '[test 2] shell=bash, stub bash.exe on PATH -> override written'
Reset-Env
$stubDir = Join-Path $TmpRoot ("stub-" + [Guid]::NewGuid().ToString('N').Substring(0,6))
New-Item -ItemType Directory -Force -Path $stubDir | Out-Null
# Use a .exe shim — a tiny .cmd renamed .exe is enough for Get-Command bash.exe
# to find it. We never *execute* it in this test.
$stubBash = Join-Path $stubDir 'bash.exe'
Set-Content -Path $stubBash -Value "@echo stub-bash %*" -Encoding ASCII
# Put stub FIRST so it wins even if real bash is on PATH; and chop
# hard-coded git-bash off the test by using a sentinel order.
$env:PATH = "$stubDir;$env:PATH"

$case2 = [pscustomobject]@{ id = 't2'; applies_to = @('windows'); shell = 'bash' }
$cfg2  = Join-Path $env:APPDATA 'SonicTerm\sonicterm.toml'
New-Item -ItemType Directory -Force -Path (Split-Path $cfg2 -Parent) | Out-Null
$r2 = Test-Shell-Selection -Case $case2 -ConfigPath $cfg2
Assert ($r2.ExitCode -eq 0) "exit code 0 (got $($r2.ExitCode))"
Assert ($r2.Reason -eq 'override-written') "reason='override-written' (got '$($r2.Reason)')"
Assert (Test-Path $cfg2) "config override file created"
if (Test-Path $cfg2) {
  $body = Get-Content -Raw $cfg2
  Assert ($body -match 'bash') "config override mentions 'bash'"
  Assert ($body -match '\[terminal\]') "config override contains [terminal] section"
}

# ------------------------------------------------------------------
# Test 3: shell="invalid"  =>  exit 1 with validation error
# ------------------------------------------------------------------
Write-Host '[test 3] shell=invalid -> exit 1, validation FATAL'
Reset-Env
$case3 = [pscustomobject]@{ id = 't3'; applies_to = @('windows'); shell = 'pwsh-but-typoed' }
$cfg3  = Join-Path $env:APPDATA 'SonicTerm\sonicterm.toml'
$r3 = Test-Shell-Selection -Case $case3 -ConfigPath $cfg3
Assert ($r3.ExitCode -eq 1) "exit code 1 (got $($r3.ExitCode))"
Assert ($r3.Reason -match 'FATAL invalid shell') "reason contains 'FATAL invalid shell' (got '$($r3.Reason)')"

# ------------------------------------------------------------------
# Cleanup
# ------------------------------------------------------------------
$env:APPDATA = $origAppData
$env:PATH    = $origPath
try { Remove-Item -Recurse -Force $TmpRoot -ErrorAction SilentlyContinue } catch { }
Pop-Location

if ($fail -eq 0) {
  Write-Host ""
  Write-Host "[Test-ShellDialect] all tests PASS" -ForegroundColor Green
  exit 0
} else {
  Write-Host ""
  Write-Host "[Test-ShellDialect] FAIL ($fail assertion(s))" -ForegroundColor Red
  exit 1
}
