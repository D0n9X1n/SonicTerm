#!/usr/bin/env pwsh
# scripts/check-no-harness-in-release.ps1
#
# Issue #506 gate: the default release build of sonicterm-windows MUST
# NOT carry the test-only `harness_pipe` symbols. Build without
# `--features harness` and dump the import/export tables.
#
# Exit 0 if no harness symbols present, non-zero otherwise.
$ErrorActionPreference = "Stop"

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    Write-Host "[#506] Building sonicterm-windows --release (no features)…"
    & cargo build --release -p sonicterm-windows 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $exe = "target/release/sonicterm-windows.exe"
    if (-not (Test-Path $exe)) { throw "expected $exe to exist" }

    # The CLI rejection message intentionally mentions
    # `--harness-input-pipe` even in stripped builds, so only fail on
    # the underscore-form symbols + the SDDL pipe-name prefix that can
    # only originate from compiled harness_pipe code.
    $needles = @('harness_pipe', 'sonic-harness-pipe', 'sonicterm-harness-')
    $dumpbin = Get-Command dumpbin -ErrorAction SilentlyContinue
    $hits = if ($dumpbin) {
        & dumpbin /SYMBOLS $exe 2>$null | Select-String -Pattern ($needles -join '|')
    } else {
        $bytes = [System.IO.File]::ReadAllBytes($exe)
        $text  = [System.Text.Encoding]::ASCII.GetString($bytes)
        $needles | Where-Object { $text -match $_ }
    }

    if ($hits) {
        Write-Error "[#506] FAIL: harness symbols present in default release build:"
        $hits | ForEach-Object { Write-Host "  $_" }
        exit 1
    }
    Write-Host "[#506] OK: no harness symbols in default release build."
} finally {
    Pop-Location
}
