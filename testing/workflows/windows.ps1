<#
.SYNOPSIS
  Driver for testing/cases.toml on Windows.
.DESCRIPTION
  PowerShell port of testing/workflows/mac.sh. See also: mac.sh.
  Includes the 6 focus + multi-PID guards landed for mac in #472
  (see issue #475 for the per-guard mapping table).
.PARAMETER Case
  Run a single case id (alternative to env:CASE_ID).
.PARAMETER All
  Run every windows-applicable case (default).
.PARAMETER Build
  Force `cargo build --release -p sonicterm-windows` before running.
.PARAMETER Help
  Show this help and exit 0.
.EXAMPLE
  pwsh -File testing\workflows\windows.ps1 -All
  pwsh -File testing\workflows\windows.ps1 -Case render-baseline-cells-cursor-cjk-emoji-bg -Build
#>
[CmdletBinding()]
param(
  [string] $Case,
  [switch] $All,
  [switch] $Build,
  [switch] $Help
)

if ($Help) { Get-Help $PSCommandPath -Detailed; exit 0 }

$ErrorActionPreference = 'Stop'
$Root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $Root

# ------------------------------------------------------------------
# Tool checks (python3 mandatory — tomllib + Pillow; tesseract optional
# but recommended for OCR expectations). yq not required on Windows;
# python3 tomllib handles parsing identically to mac.sh fallback.
# ------------------------------------------------------------------
foreach ($tool in 'python3','cargo','git') {
  if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
    Write-Error "FATAL: missing required tool: $tool"; exit 2
  }
}
$pyok = & python3 -c "from PIL import Image" 2>&1
if ($LASTEXITCODE -ne 0) {
  Write-Error "FATAL: Pillow not installed. Run: python3 -m pip install Pillow"; exit 2
}
if (-not (Get-Command tesseract -ErrorAction SilentlyContinue)) {
  Write-Warning "tesseract not on PATH — OCR-based expectations will FAIL. Install: winget install UB-Mannheim.TesseractOCR"
}

# ------------------------------------------------------------------
# Guard 1 — pre-flight: refuse to start if a competing GUI terminal
# application is running. SendKeys go to whatever window is foreground
# at the moment; if a competitor GUI terminal (Windows Terminal,
# alacritty, mintty, wezterm-gui, ...) is alive AND sonicterm-windows
# drops focus mid-case, our SendKeys calls land in that competitor.
# Documented in issues #464, #494, #490.
#
# The list is intentionally restricted to actual GUI terminal apps —
# NOT host shells/console hosts (conhost, cmd, pwsh, powershell). Those
# don't compete for foreground in the way SendKeys cares about, and
# including them means the driver refuses to launch from any pwsh
# session (regression #490).
#
# Override with $env:SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1 to bypass
# entirely. Extend the list at runtime with
# $env:SONICTERM_HARNESS_EXTRA_TERMS='name1,name2' (comma-separated
# process names, no `.exe` suffix; matched case-insensitively).
# ------------------------------------------------------------------
$Competitors = @(
  'WindowsTerminal',  # Windows Terminal main process
  'wt',               # Windows Terminal CLI alias
  'alacritty',
  'mintty',
  'wezterm-gui',
  'wezterm',
  'kitty',
  'tabby',
  'Hyper',
  'ghostty',
  'Warp',
  'rio',
  'ConEmu64',
  'ConEmuC64',
  'FluentTerminal',
  'MobaXterm'
)
# Note: conhost/cmd/pwsh/powershell intentionally EXCLUDED — these are
# host shells / console hosts, not foreground-competing GUI terminals.
# Including them breaks running the harness from any pwsh window (#490).

# Extension point: $env:SONICTERM_HARNESS_EXTRA_TERMS (comma-separated).
if ($env:SONICTERM_HARNESS_EXTRA_TERMS) {
  $extra = $env:SONICTERM_HARNESS_EXTRA_TERMS -split ',' |
    ForEach-Object { $_.Trim() } |
    Where-Object { $_ }
  if ($extra) { $Competitors = @($Competitors) + @($extra) }
}

if (-not $env:SONICTERM_HARNESS_ALLOW_OTHER_TERMS -or $env:SONICTERM_HARNESS_ALLOW_OTHER_TERMS -ne '1') {
  # Case-insensitive match against .ProcessName (no .exe suffix).
  $compLower = $Competitors | ForEach-Object { $_.ToLowerInvariant() }
  $hits = Get-Process -ErrorAction SilentlyContinue | Where-Object {
    $compLower -contains $_.ProcessName.ToLowerInvariant() -and $_.Id -ne $PID
  }
  if ($hits) {
    Write-Host 'FATAL: competing GUI terminal(s) running — keystrokes will leak.' -ForegroundColor Red
    Write-Host 'Quit them, or set $env:SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1 to override.' -ForegroundColor Red
    $hits | ForEach-Object { Write-Host ("  {0} {1}" -f $_.Id, $_.ProcessName) -ForegroundColor Red }
    exit 2
  }
}

# ------------------------------------------------------------------
# B2 boundary-verify support: snapshot the user's pre-existing
# sonicterm-windows PIDs once. Anything OUTSIDE this set after a run
# is either a harness-tracked PID we failed to reap (warn + force-kill
# in run_case.ps1) or a user-launched instance mid-run (log only — not
# ours to kill). Exported so run_case.ps1 can read it.
# ------------------------------------------------------------------
$prePids = @(Get-Process -Name 'sonicterm-windows' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
$env:PRE_RUN_USER_PIDS = ($prePids -join ',')

# ------------------------------------------------------------------
# Arg normalisation
# ------------------------------------------------------------------
$Filter = if ($Case) { $Case } elseif ($env:CASE_ID) { $env:CASE_ID } else { 'all' }
if ($All) { $Filter = 'all' }

$Sha = (& git rev-parse --short HEAD 2>$null); if (-not $Sha) { $Sha = 'nogit' }
$Out = Join-Path 'testing\results' ("win-{0}" -f $Sha)
New-Item -ItemType Directory -Force -Path $Out | Out-Null

# ------------------------------------------------------------------
# Build
# ------------------------------------------------------------------
$Bin = 'target\release\sonicterm-windows.exe'
if ($Build -or -not (Test-Path $Bin)) {
  Write-Host '[build] cargo build --release -p sonicterm-windows'
  & cargo build --release -p sonicterm-windows
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

# ------------------------------------------------------------------
# Enumerate matching ids (python3 tomllib — same as mac.sh)
# ------------------------------------------------------------------
$py = @"
import sys, tomllib
flt = sys.argv[1]
with open('testing/cases.toml','rb') as f:
    d = tomllib.load(f)
for c in d['case']:
    if 'windows' not in c.get('applies_to', []):
        continue
    if flt != 'all' and c['id'] != flt:
        continue
    print(c['id'])
"@
$Ids = @($py | python3 - $Filter)
$Ids = $Ids | Where-Object { $_ -and $_.Trim() }
if ($Ids.Count -eq 0) { Write-Error "no matching cases for filter='$Filter'"; exit 1 }

Write-Host "[plan] $($Ids.Count) case(s) to run; results -> $Out"

$Pass = 0; $Fail = 0; $Skip = 0
foreach ($id in $Ids) {
  Write-Host ''
  Write-Host "=== $id ==="
  & pwsh -NoProfile -File (Join-Path $PSScriptRoot 'run_case.ps1') $id $Out
  switch ($LASTEXITCODE) {
    0  { $Pass++ }
    77 { $Skip++ }
    default { $Fail++ }
  }
}

Write-Host ''
Write-Host "[done] pass=$Pass fail=$Fail skip=$Skip / total=$($Ids.Count)"

# Lightweight inline summary (mac.sh shells out to summarize.sh; we
# inline so windows.ps1 stays standalone — summarize.sh is bash-only).
$report = Join-Path $Out 'report.md'
@(
  '# Visual test report (windows)',
  '',
  "- dir: $Out",
  "- total: $($Ids.Count)",
  "- pass: $Pass",
  "- fail: $Fail",
  "- skip: $Skip",
  '',
  '| status | case | screenshot |',
  '|---|---|---|'
) | Set-Content $report
Get-ChildItem -Directory $Out | ForEach-Object {
  $st = if (Test-Path "$($_.FullName)\status") { Get-Content "$($_.FullName)\status" -Raw } else { 'UNKNOWN' }
  $shot = if (Test-Path "$($_.FullName)\screen.png") { "$($_.FullName)\screen.png" } else { '' }
  Add-Content $report "| $($st.Trim()) | $($_.Name) | $shot |"
}
Get-Content $report

# ------------------------------------------------------------------
# Guard 6 epilogue — re-park focus on the Explorer taskbar to swallow
# any stray keystrokes that leaked between cases. Mirrors mac.sh's
# `tell Finder to close every window` epilogue.
# ------------------------------------------------------------------
try {
  (New-Object -ComObject Shell.Application).MinimizeAll() | Out-Null
} catch { }

if ($Fail -ne 0) { exit 1 } else { exit 0 }
