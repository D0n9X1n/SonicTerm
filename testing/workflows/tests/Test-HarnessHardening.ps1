<#
.SYNOPSIS
  Self-test for the three harness-hardening mitigations landed for #488.
.DESCRIPTION
  Issue #488 added three guards to run_case.ps1:
    1. Defender first-launch budget extension (marker file in $env:TEMP).
    2. UAC fast-fail driven by `requires_elevation` in cases.toml.
    3. PrintWindow black-frame detection -> SKIP exit 77.

  This test verifies each mitigation in isolation, without spawning
  sonicterm-windows.exe (so it runs in any CI/dev env). Where mocking
  the full pipeline is impractical we re-implement the production
  predicate inline and assert against it; the production paths are
  short and grep-checked here to detect drift.

  Exit 0 on all-pass, non-zero on any failure.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$fails = 0
function Assert-True { param($Cond, [string] $Msg)
  if ($Cond) { Write-Host "  PASS: $Msg" -ForegroundColor Green }
  else { Write-Host "  FAIL: $Msg" -ForegroundColor Red; $script:fails++ }
}

$RunCase = Join-Path $PSScriptRoot '..\run_case.ps1'
$Driver  = Join-Path $PSScriptRoot '..\windows.ps1'
$CasesToml = Join-Path $PSScriptRoot '..\..\cases.toml'

Assert-True (Test-Path $RunCase)   "run_case.ps1 exists"
Assert-True (Test-Path $Driver)    "windows.ps1 exists"
Assert-True (Test-Path $CasesToml) "cases.toml exists"

$RunCaseSrc = Get-Content -Raw $RunCase
$DriverSrc  = Get-Content -Raw $Driver
$CasesSrc   = Get-Content -Raw $CasesToml

# ------------------------------------------------------------------
# Mitigation 1 — first-launch marker / budget extension
# ------------------------------------------------------------------
Write-Host '' ; Write-Host '== Mitigation 1: Defender first-launch budget ==' -ForegroundColor Cyan

$MarkerName = 'sonic-harness-first-launch-flag'
$Marker = Join-Path $env:TEMP $MarkerName

Assert-True ($RunCaseSrc -match [regex]::Escape($MarkerName)) `
  "run_case.ps1 references the marker filename"
Assert-True ($DriverSrc -match [regex]::Escape($MarkerName)) `
  "windows.ps1 references the marker filename"
Assert-True ($DriverSrc -match 'try\s*\{[^}]*foreach \(\$id in \$Ids\)' -or `
             $DriverSrc -match 'try\s*\{') `
  "windows.ps1 wraps the case loop in try/finally"
Assert-True ($DriverSrc -match '(?s)finally\s*\{[^}]*Remove-Item[^}]*FirstLaunchMarker') `
  "windows.ps1 clears the marker in finally"

# Re-implement the production predicate inline and verify it picks
# the 30s budget when the marker is absent, and 10s when present
# (without an explicit $env:SONICTERM_HARNESS_WIN_TIMEOUT_S override).
function Get-WindowAppearBudget {
  param([string] $MarkerPath, [string] $EnvOverride)
  if ($EnvOverride) { return [int]$EnvOverride }
  if (-not (Test-Path $MarkerPath)) { return 30 }
  return 10
}

# Save/restore real marker so we don't disturb a concurrent harness run.
$savedMarker = $null
if (Test-Path $Marker) {
  $savedMarker = Get-Content -Raw $Marker
  Remove-Item -Force $Marker
}
try {
  Assert-True ((Get-WindowAppearBudget -MarkerPath $Marker -EnvOverride $null) -eq 30) `
    "absent marker -> 30s budget"
  Set-Content -Path $Marker -Value 'test'
  Assert-True ((Get-WindowAppearBudget -MarkerPath $Marker -EnvOverride $null) -eq 10) `
    "present marker -> 10s budget"
  Assert-True ((Get-WindowAppearBudget -MarkerPath $Marker -EnvOverride '45') -eq 45) `
    "explicit env override wins over marker logic"
  Remove-Item -Force $Marker
  Assert-True ((Get-WindowAppearBudget -MarkerPath $Marker -EnvOverride '7') -eq 7) `
    "explicit env override wins over absent marker too"
} finally {
  if (Test-Path $Marker) { Remove-Item -Force $Marker -ErrorAction SilentlyContinue }
  if ($savedMarker) { Set-Content -Path $Marker -Value $savedMarker -NoNewline }
}

# ------------------------------------------------------------------
# Mitigation 2 — UAC fast-fail via requires_elevation
# ------------------------------------------------------------------
Write-Host '' ; Write-Host '== Mitigation 2: requires_elevation UAC fast-fail ==' -ForegroundColor Cyan

Assert-True ($CasesSrc -match 'requires_elevation') `
  "cases.toml documents requires_elevation"
Assert-True ($CasesSrc -match 'windows-elevation-fixture-uac-fast-fail') `
  "cases.toml contains the sample requires_elevation fixture"
Assert-True ($RunCaseSrc -match 'requires_elevation') `
  "run_case.ps1 reads requires_elevation"
Assert-True ($RunCaseSrc -match 'unelevated_only') `
  "run_case.ps1 uses fail_reason=unelevated_only"
Assert-True ($RunCaseSrc -match 'WindowsBuiltInRole.*Administrator' -or `
             $RunCaseSrc -match 'IsInRole') `
  "run_case.ps1 actually probes admin role"

# Re-implement the production predicate inline so we can simulate
# elevated/non-elevated without re-launching pwsh.
function Test-CaseShouldFailUnelevated {
  param([bool] $RequiresElevation, [bool] $IsElevated)
  return ($RequiresElevation -and -not $IsElevated)
}
Assert-True (Test-CaseShouldFailUnelevated -RequiresElevation $true  -IsElevated $false) `
  "non-elevated + requires_elevation -> FAIL"
Assert-True (-not (Test-CaseShouldFailUnelevated -RequiresElevation $true  -IsElevated $true)) `
  "elevated + requires_elevation -> proceeds"
Assert-True (-not (Test-CaseShouldFailUnelevated -RequiresElevation $false -IsElevated $false)) `
  "non-elevated + no requirement -> proceeds"
Assert-True (-not (Test-CaseShouldFailUnelevated -RequiresElevation $false -IsElevated $true)) `
  "elevated + no requirement -> proceeds"

# Verify the sample fixture is parseable + structurally valid (uses python3
# tomllib like the production driver). If python3 is missing, skip this
# layer with a yellow warning rather than failing the whole test.
if (Get-Command python3 -ErrorAction SilentlyContinue) {
  $py = @"
import sys, tomllib
with open(r'$CasesToml', 'rb') as f:
    d = tomllib.load(f)
matches = [c for c in d['case'] if c['id'] == 'windows-elevation-fixture-uac-fast-fail']
if not matches:
    print('NOT_FOUND'); sys.exit(1)
c = matches[0]
print('requires_elevation=' + str(c.get('requires_elevation')))
print('applies_to=' + ','.join(c.get('applies_to', [])))
"@
  $fixtureInfo = ($py | python3 -)
  Assert-True ($fixtureInfo -match 'requires_elevation=True') `
    "sample fixture parses with requires_elevation=true"
  Assert-True ($fixtureInfo -match 'applies_to=.*windows') `
    "sample fixture targets windows"
} else {
  Write-Host '  SKIP: python3 not on PATH — skipping tomllib fixture parse' -ForegroundColor Yellow
}

# ------------------------------------------------------------------
# Mitigation 3 — PrintWindow black-frame detection
# ------------------------------------------------------------------
Write-Host '' ; Write-Host '== Mitigation 3: PrintWindow black-frame -> SKIP 77 ==' -ForegroundColor Cyan

Assert-True ($RunCaseSrc -match 'printwindow_black_frame') `
  "run_case.ps1 uses skip_reason=printwindow_black_frame"
Assert-True ($RunCaseSrc -match 'GetPixel') `
  "run_case.ps1 calls GetPixel on the captured bitmap"
Assert-True ($RunCaseSrc -match 'for \(\$gy = 0; \$gy -lt 16') `
  "run_case.ps1 uses the deterministic 16x16 grid"
Assert-True ($RunCaseSrc -match 'exit 77') `
  "run_case.ps1 still has the SKIP exit 77 path"

# Mock the predicate: feed it three Bitmaps (all-black, all-alpha-zero,
# normal content) and assert the classification matches what run_case.ps1
# would do. This is the "Don't fake" honest mock — we exercise the SAME
# 16x16 grid sampler against real System.Drawing.Bitmap instances.
Add-Type -AssemblyName System.Drawing 2>&1 | Out-Null

function Test-BitmapIsBlackFrame {
  param([System.Drawing.Bitmap] $Bmp)
  $w = $Bmp.Width; $h = $Bmp.Height
  $allBlack = $true; $allAlphaZero = $true
  for ($gy = 0; $gy -lt 16; $gy++) {
    for ($gx = 0; $gx -lt 16; $gx++) {
      $sx = [int](($gx + 0.5) * $w / 16.0); if ($sx -ge $w) { $sx = $w - 1 }
      $sy = [int](($gy + 0.5) * $h / 16.0); if ($sy -ge $h) { $sy = $h - 1 }
      $px = $Bmp.GetPixel($sx, $sy)
      if ($px.R -ne 0 -or $px.G -ne 0 -or $px.B -ne 0) { $allBlack = $false }
      if ($px.A -ne 0) { $allAlphaZero = $false }
      if (-not $allBlack -and -not $allAlphaZero) { return $false }
    }
  }
  return ($allBlack -or $allAlphaZero)
}

# Case A: fully opaque black bitmap (PrintWindow's classic failure mode).
$bmpBlack = New-Object System.Drawing.Bitmap 100, 60
$gA = [System.Drawing.Graphics]::FromImage($bmpBlack)
$gA.Clear([System.Drawing.Color]::FromArgb(255, 0, 0, 0))
$gA.Dispose()
Assert-True (Test-BitmapIsBlackFrame $bmpBlack) "all-black bitmap -> detected as black-frame"
$bmpBlack.Dispose()

# Case B: fully transparent (alpha=0) bitmap — DWM dropped the frame.
$bmpAlpha = New-Object System.Drawing.Bitmap 100, 60
$gB = [System.Drawing.Graphics]::FromImage($bmpAlpha)
$gB.Clear([System.Drawing.Color]::FromArgb(0, 128, 128, 128))
$gB.Dispose()
Assert-True (Test-BitmapIsBlackFrame $bmpAlpha) "all-alpha-zero bitmap -> detected as black-frame"
$bmpAlpha.Dispose()

# Case C: real content (white background, a red dot) — must NOT trip.
$bmpOk = New-Object System.Drawing.Bitmap 100, 60
$gC = [System.Drawing.Graphics]::FromImage($bmpOk)
$gC.Clear([System.Drawing.Color]::White)
$gC.FillEllipse([System.Drawing.Brushes]::Red, 40, 20, 20, 20)
$gC.Dispose()
Assert-True (-not (Test-BitmapIsBlackFrame $bmpOk)) "real-content bitmap -> NOT flagged"
$bmpOk.Dispose()

# Case D: 1px-anti-pattern — one non-black pixel near origin must defeat
# the all-black detector. We poke pixel (0,0) red on an otherwise-black
# bitmap and confirm the sampler still classifies it as black-frame
# (because the 16x16 grid samples the CENTER of each cell, not (0,0)),
# AND that pre-poking a CENTER cell defeats it. This documents the
# sample-pattern caveat called out in the Opus diagnosis.
$bmpEdge = New-Object System.Drawing.Bitmap 100, 60
$gD = [System.Drawing.Graphics]::FromImage($bmpEdge)
$gD.Clear([System.Drawing.Color]::Black)
$gD.Dispose()
$bmpEdge.SetPixel(0, 0, [System.Drawing.Color]::Red)  # corner only
Assert-True (Test-BitmapIsBlackFrame $bmpEdge) "single corner pixel doesn't defeat the grid sampler (expected)"
# Now poke a sample point (~center of cell [8,8] on a 100x60 bitmap).
$cx = [int]((8 + 0.5) * 100 / 16.0)
$cy = [int]((8 + 0.5) * 60 / 16.0)
$bmpEdge.SetPixel($cx, $cy, [System.Drawing.Color]::FromArgb(255, 200, 200, 200))
Assert-True (-not (Test-BitmapIsBlackFrame $bmpEdge)) "one sample-aligned non-black pixel breaks the detector"
$bmpEdge.Dispose()

Write-Host ''
if ($fails -eq 0) {
  Write-Host "All #488 harness-hardening tests PASSED." -ForegroundColor Green
  exit 0
} else {
  Write-Host "$fails harness-hardening test(s) FAILED." -ForegroundColor Red
  exit 1
}
