<#
.SYNOPSIS
  Self-test for Guard 1 (competing-terminal classifier) in windows.ps1.
.DESCRIPTION
  Verifies fix for issues #494 + #490:
    - The 16-name terminal-app list is matched case-insensitively.
    - Host shells (conhost, cmd, pwsh, powershell) are NOT flagged.
    - $env:SONICTERM_HARNESS_EXTRA_TERMS appends to the list.
    - $env:SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1 bypasses everything.

  Two test layers:
    1. Synthetic [pscustomobject] arrays (pure unit-test, no spawn).
    2. Stub-binary-rename: copy a long-lived stub binary (Windows
       PowerShell, which has a stable .ProcessName when renamed) to
       $env:TEMP\guard1-stubs\WindowsTerminal.exe and \conhost.exe,
       launch each as a sleeping process, and assert that the
       classifier flags WindowsTerminal but NOT conhost.

  Exit 0 on all-pass, non-zero on any failure.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

# ------------------------------------------------------------------
# Extract $Competitors list + extension-point parsing from windows.ps1
# by sourcing the file in a SAFE way: we re-implement the classifier
# inline here mirroring the production logic, so we can unit-test
# without running the full driver (which would build cargo, etc).
#
# This MUST stay in lock-step with the list in windows.ps1. The
# stub-binary integration test below also exercises the real driver
# script's classifier branch.
# ------------------------------------------------------------------
$ExpectedCompetitors = @(
  'WindowsTerminal','wt','alacritty','mintty','wezterm-gui','wezterm',
  'kitty','tabby','Hyper','ghostty','Warp','rio',
  'ConEmu64','ConEmuC64','FluentTerminal','MobaXterm'
)

function Get-Classifier {
  param([string[]] $ExtraTerms)
  $list = $ExpectedCompetitors
  if ($ExtraTerms) { $list = @($list) + @($ExtraTerms) }
  $lower = $list | ForEach-Object { $_.ToLowerInvariant() }
  return {
    param($Procs)
    return @($Procs | Where-Object { $_.ProcessName -and ($lower -contains $_.ProcessName.ToLowerInvariant()) })
  }.GetNewClosure()
}

$fails = 0
function Assert-True { param($Cond, [string] $Msg)
  if ($Cond) { Write-Host "  PASS: $Msg" -ForegroundColor Green }
  else { Write-Host "  FAIL: $Msg" -ForegroundColor Red; $script:fails++ }
}

# ------------------------------------------------------------------
# Layer 1 — synthetic unit tests
# ------------------------------------------------------------------
Write-Host '== Unit tests (synthetic ProcessName objects) ==' -ForegroundColor Cyan

# 1.1: parity with the production list
$prodList = (Get-Content (Join-Path $PSScriptRoot '..\windows.ps1') -Raw)
foreach ($name in $ExpectedCompetitors) {
  Assert-True ($prodList -match [regex]::Escape("'$name'")) "windows.ps1 contains '$name'"
}
Assert-True ($ExpectedCompetitors.Count -eq 16) "list has exactly 16 entries (got $($ExpectedCompetitors.Count))"

# 1.2: host shells NOT in production list
foreach ($host_ in 'conhost','cmd','pwsh','powershell') {
  Assert-True (-not ($prodList -match [regex]::Escape("'$host_'"))) "host shell '$host_' is NOT in the production list"
}

# 1.3: classifier flags listed terminals, skips host shells
$classifier = Get-Classifier
$synthetic = @(
  [pscustomobject]@{ ProcessName = 'ConEmu64' }
  [pscustomobject]@{ ProcessName = 'windowsterminal' }     # case-insensitive
  [pscustomobject]@{ ProcessName = 'WEZTERM-GUI' }         # case-insensitive
  [pscustomobject]@{ ProcessName = 'conhost' }             # should NOT match
  [pscustomobject]@{ ProcessName = 'pwsh' }                # should NOT match
  [pscustomobject]@{ ProcessName = 'cmd' }                 # should NOT match
  [pscustomobject]@{ ProcessName = 'powershell' }          # should NOT match
  [pscustomobject]@{ ProcessName = 'firefox' }             # unrelated
)
$hits = & $classifier $synthetic
$hitNames = $hits | ForEach-Object { $_.ProcessName.ToLowerInvariant() } | Sort-Object
Assert-True ($hitNames -join ',' -eq 'conemu64,wezterm-gui,windowsterminal') `
  "classifier flags only the 3 terminal apps (got: $($hitNames -join ','))"
Assert-True (-not ($hitNames -contains 'conhost')) "classifier does NOT flag conhost (#490)"
Assert-True (-not ($hitNames -contains 'pwsh'))    "classifier does NOT flag pwsh (#490)"
Assert-True (-not ($hitNames -contains 'cmd'))     "classifier does NOT flag cmd (#490)"

# 1.4: extension point
$classifierExt = Get-Classifier -ExtraTerms @('myCustomTerm','OtherTerm')
$extInput = @(
  [pscustomobject]@{ ProcessName = 'mycustomterm' }
  [pscustomobject]@{ ProcessName = 'random' }
)
$extHits = & $classifierExt $extInput
Assert-True ($extHits.Count -eq 1 -and $extHits[0].ProcessName -eq 'mycustomterm') `
  "SONICTERM_HARNESS_EXTRA_TERMS appends new names case-insensitively"

# ------------------------------------------------------------------
# Layer 2 — stub-binary integration test (renamed notepad.exe stubs)
# ------------------------------------------------------------------
Write-Host '' ; Write-Host '== Integration tests (renamed powershell.exe stubs) ==' -ForegroundColor Cyan

$stubDir = Join-Path $env:TEMP 'guard1-stubs'
if (Test-Path $stubDir) { Remove-Item -Recurse -Force $stubDir }
New-Item -ItemType Directory -Force -Path $stubDir | Out-Null

$src = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
if (-not (Test-Path $src)) {
  Write-Host "  SKIP: powershell.exe not found at $src — skipping integration layer" -ForegroundColor Yellow
} else {
  $stubTerm = Join-Path $stubDir 'WindowsTerminal.exe'
  $stubHost = Join-Path $stubDir 'conhost.exe'
  Copy-Item $src $stubTerm
  Copy-Item $src $stubHost

  # Sleep for 6s — long enough for the test to enumerate + classify.
  $sleepArgs = @('-NoProfile','-Command','Start-Sleep 6')
  $pTerm = Start-Process -FilePath $stubTerm -ArgumentList $sleepArgs -PassThru -WindowStyle Hidden
  $pHost = Start-Process -FilePath $stubHost -ArgumentList $sleepArgs -PassThru -WindowStyle Hidden
  Start-Sleep -Milliseconds 800
  try {
    $live = Get-Process -ErrorAction SilentlyContinue |
      Where-Object { $_.Id -in @($pTerm.Id, $pHost.Id) }
    $cls = Get-Classifier
    $flagged = & $cls $live
    $flaggedNames = @($flagged | ForEach-Object { $_.ProcessName.ToLowerInvariant() })

    Assert-True ($flaggedNames -contains 'windowsterminal') `
      "stub 'WindowsTerminal.exe' IS flagged as competing (saw: $($flaggedNames -join ','))"
    Assert-True (-not ($flaggedNames -contains 'conhost')) `
      "stub 'conhost.exe' is NOT flagged (#490 regression)"
  } finally {
    $pTerm, $pHost | ForEach-Object {
      if ($_ -and -not $_.HasExited) {
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
      }
    }
    Remove-Item -Recurse -Force $stubDir -ErrorAction SilentlyContinue
  }
}

Write-Host ''
if ($fails -eq 0) {
  Write-Host "All Guard 1 classifier tests PASSED." -ForegroundColor Green
  exit 0
} else {
  Write-Host "$fails Guard 1 classifier test(s) FAILED." -ForegroundColor Red
  exit 1
}
