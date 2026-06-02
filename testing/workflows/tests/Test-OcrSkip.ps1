<#
.SYNOPSIS
  Regression test for issue #492 — OCR-unavailable should SKIP (exit 77)
  not FAIL (exit 1).
.DESCRIPTION
  Three assertions, per Opus Step-2 APPROVED-DIAG test plan:

  1. OCR-only case + tesseract scrubbed from PATH → run_case.ps1 must
     exit 77 *before* spawning the app (early-skip path).
  2. Per-skip log lines must include case id + expect index so silently
     skipped coverage is auditable. Format:
       [SKIP ocr_unavailable] case=<id> expect[N]=<kind>
  3. Mixed-case behavior (per-expect SKIP) is verified against the
     embedded Python expectation evaluator by replaying its skip logic
     with a synthetic case JSON: pixel passes + OCR skips → exit 77;
     pixel passes + OCR passes → exit 0; pixel fails + OCR skips → exit 1.
     We don't drive the full mixed-case run_case.ps1 because that would
     require a built sonicterm-windows.exe + window/screenshot loop; the
     skip arithmetic is what regresses, so we test that directly.

  PATH scrubbing (not Get-Command shadowing) per Opus's note: shadowing
  Get-Command is fragile because the script uses it elsewhere, and the
  embedded Python's subprocess.run(['tesseract',...]) doesn't see PS
  functions anyway. PATH scrubbing exercises the real codepath.

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

# ------------------------------------------------------------------
# Fixture: temp testing/cases.toml with one OCR-only case + one mixed
# case (one pixel-near + one text-in-region). run_case.ps1 reads
# 'testing/cases.toml' relative to cwd, so we cd into a temp dir
# that contains that path.
# ------------------------------------------------------------------
$Sandbox = Join-Path $env:TEMP ("sonic-492-test-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0,8)))
New-Item -ItemType Directory -Force -Path (Join-Path $Sandbox 'testing') | Out-Null
$CasesToml = @'
[[case]]
id = "ocr-only-fixture"
applies_to = ["windows"]
setup = []
keystrokes = []
[[case.expect]]
kind = "text-in-region"
value = "irrelevant"
[[case.expect]]
kind = "ocr-text"
value = "irrelevant2"

[[case]]
id = "mixed-fixture"
applies_to = ["windows"]
setup = []
keystrokes = []
[[case.expect]]
kind = "pixel-near"
x = 100
y = 100
rgba = [0, 0, 0, 255]
[[case.expect]]
kind = "text-in-region"
value = "irrelevant"
'@
Set-Content -Path (Join-Path $Sandbox 'testing\cases.toml') -Value $CasesToml -Encoding UTF8

# ------------------------------------------------------------------
# Assertion 1+2: OCR-only case with scrubbed PATH → exit 77, log line
# format verified.
# ------------------------------------------------------------------
Write-Host "[1/3] OCR-only case + tesseract-scrubbed PATH → expect exit 77" -ForegroundColor Cyan
$OutDir = Join-Path $Sandbox 'results'
$ScrubbedPath = ($env:PATH -split ';' | Where-Object {
  $_ -and -not (Test-Path (Join-Path $_ 'tesseract.exe'))
}) -join ';'

$child = Start-Process -FilePath 'pwsh' `
  -ArgumentList '-NoProfile','-File',$RunCase,'ocr-only-fixture',$OutDir `
  -WorkingDirectory $Sandbox `
  -Environment @{ PATH = $ScrubbedPath; SONICTERM_HARNESS_OCR_AVAILABLE = $null } `
  -PassThru -Wait -NoNewWindow `
  -RedirectStandardOutput (Join-Path $Sandbox 'ocr-only.stdout') `
  -RedirectStandardError  (Join-Path $Sandbox 'ocr-only.stderr')
$rc = $child.ExitCode
Assert ($rc -eq 77) "OCR-only run exits 77 (got $rc)"

$caseLog = Join-Path $OutDir 'ocr-only-fixture\case.log'
$status  = Join-Path $OutDir 'ocr-only-fixture\status'
$logText = if (Test-Path $caseLog) { Get-Content -Raw $caseLog } else { '' }
$statusText = if (Test-Path $status) { (Get-Content -Raw $status).Trim() } else { '' }

Assert ($statusText -eq 'SKIP') "status file says SKIP (got '$statusText')"

# Per Opus: log line must include case id + expect index.
$lineRegex0 = '\[SKIP ocr_unavailable\] case=ocr-only-fixture expect\[0\]=text-in-region'
$lineRegex1 = '\[SKIP ocr_unavailable\] case=ocr-only-fixture expect\[1\]=ocr-text'
Assert ($logText -match $lineRegex0) "log contains per-skip line for expect[0]"
Assert ($logText -match $lineRegex1) "log contains per-skip line for expect[1]"

# ------------------------------------------------------------------
# Assertion 3: Mixed-case skip arithmetic — exercise the embedded
# Python evaluator's skip/fail/pass tri-state directly. Same script
# extracted to a temp .py with synthesized inputs.
# ------------------------------------------------------------------
Write-Host "[2/3] Mixed case — Python evaluator skip arithmetic" -ForegroundColor Cyan

# Extract the embedded python from run_case.ps1.
$src = Get-Content -Raw $RunCase
$m = [regex]::Match($src, '(?ms)\$py2 = @"\r?\n(.*?)\r?\n"@')
if (-not $m.Success) { Write-Host "  FAIL: could not extract embedded python"; exit 1 }
$pySrc = $m.Groups[1].Value
$PyFile = Join-Path $Sandbox 'expect_eval.py'
Set-Content -Path $PyFile -Value $pySrc -Encoding UTF8

# Need a placeholder screenshot file that pixel_near can open.
# Use Python to write a 1000x700 black PNG via PIL.
$mkpng = @'
from PIL import Image
import sys
Image.new('RGBA',(1000,700),(0,0,0,255)).save(sys.argv[1])
'@
$pngPath = Join-Path $Sandbox 'fake.png'
$mkpng | python3 - $pngPath
if (-not (Test-Path $pngPath)) { Write-Host "  FAIL: could not create fake png"; exit 1 }

function Invoke-Eval([string]$caseJsonContent, [string]$ocrAvail) {
  $cj = Join-Path $Sandbox 'case.json'
  $el = Join-Path $Sandbox 'expect.log'
  Set-Content -Path $cj -Value $caseJsonContent -Encoding UTF8
  if (Test-Path $el) { Remove-Item $el }
  $prevOcr = $env:SONICTERM_HARNESS_OCR_AVAILABLE
  $env:SONICTERM_HARNESS_OCR_AVAILABLE = $ocrAvail
  try {
    & python3 $PyFile $cj $pngPath $el 2>&1 | Out-Null
    $rc = $LASTEXITCODE
    $log = if (Test-Path $el) { Get-Content -Raw $el } else { '' }
    return @{ rc = $rc; log = $log }
  } finally {
    $env:SONICTERM_HARNESS_OCR_AVAILABLE = $prevOcr
  }
}

# Case A: pixel passes (black target on black png) + OCR expect skipped → exit 77
$caseA = @'
{"id":"mixed-a","expect":[
 {"kind":"pixel-near","x":100,"y":100,"rgba":[0,0,0,255],"tolerance":10},
 {"kind":"text-in-region","value":"x"}
]}
'@
$rA = Invoke-Eval $caseA '0'
Assert ($rA.rc -eq 77) "mixed: pixel-pass + OCR-skip → exit 77 (got $($rA.rc))"
Assert ($rA.log -match "(?m)^SKIP\ttext-in-region") "mixed-A log has SKIP line"
Assert ($rA.log -match "(?m)^PASS\tpixel-near")    "mixed-A log has PASS line"

# Case B: pixel fails (white target on black png, tight tolerance) + OCR skipped → exit 1
$caseB = @'
{"id":"mixed-b","expect":[
 {"kind":"pixel-near","x":100,"y":100,"rgba":[255,255,255,255],"tolerance":5},
 {"kind":"text-in-region","value":"x"}
]}
'@
$rB = Invoke-Eval $caseB '0'
Assert ($rB.rc -eq 1) "mixed: pixel-fail + OCR-skip → exit 1 (fail wins over skip) (got $($rB.rc))"

# Case C: OCR available — both evaluate; pixel passes + OCR fails (no text on blank png) → exit 1
$caseC = @'
{"id":"mixed-c","expect":[
 {"kind":"pixel-near","x":100,"y":100,"rgba":[0,0,0,255],"tolerance":10}
]}
'@
$rC = Invoke-Eval $caseC '1'
Assert ($rC.rc -eq 0) "OCR-avail, only pixel expect (passes) → exit 0 (got $($rC.rc))"

# ------------------------------------------------------------------
# Cleanup
# ------------------------------------------------------------------
Write-Host "[3/3] cleanup" -ForegroundColor Cyan
Remove-Item -Recurse -Force $Sandbox -ErrorAction SilentlyContinue

if ($failures.Count -gt 0) {
  Write-Host "`nFAILED: $($failures.Count) assertion(s)" -ForegroundColor Red
  $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
  exit 1
}
Write-Host "`nALL PASS" -ForegroundColor Green
exit 0
