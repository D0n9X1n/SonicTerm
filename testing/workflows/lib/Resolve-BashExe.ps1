<#
.SYNOPSIS
  Resolve a bash.exe path on Windows for the #493 shell-dialect override.
.DESCRIPTION
  Shared by testing/workflows/run_case.ps1 (production) and
  testing/workflows/tests/Test-ShellDialect.ps1 (regression test).

  Lookup order:
    1. Git for Windows 64-bit   ('C:\Program Files\Git\bin\bash.exe')
    2. Git for Windows 32-bit   ('C:\Program Files (x86)\Git\bin\bash.exe')
    3. bash.exe on PATH
    4. wsl.exe on PATH          (invokes default distro's bash)

  All hard-coded paths are SINGLE-QUOTED so PowerShell does not interpret
  '\b' as a backspace escape — that bug previously corrupted the literal
  to 'Git\x08in\x08ash.exe' (PR #500 revise blocker).

  Returns the full path string, or $null if none available.
#>
function Resolve-BashExe {
  $candidates = @(
    'C:\Program Files\Git\bin\bash.exe',
    'C:\Program Files (x86)\Git\bin\bash.exe'
  )
  foreach ($c in $candidates) { if (Test-Path $c) { return $c } }
  $cmd = Get-Command bash.exe -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  $wsl = Get-Command wsl.exe -ErrorAction SilentlyContinue
  if ($wsl) { return $wsl.Source }   # wsl.exe with default distro invokes its bash
  return $null
}
