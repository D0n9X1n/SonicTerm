# SonicTerm shell integration (PowerShell 7+). Load with: sonicterm-mcp --shell-integration pwsh | Out-String | Invoke-Expression
if ($env:SONICTERM_SHELL_TOKEN -and -not (Test-Path variable:global:__sonicterm_token) -and (Get-Module PSReadLine)) {
  $global:__sonicterm_token = $env:SONICTERM_SHELL_TOKEN
  Remove-Item env:SONICTERM_SHELL_TOKEN
  $global:__sonicterm_read = $false
  $global:__sonicterm_ran = $false
  $global:__sonicterm_error = $null
  $global:__sonicterm_may_native = $true
  $global:__sonicterm_prompt = $function:prompt
  $global:__sonicterm_readline = $function:PSConsoleHostReadLine
  function global:__sonicterm_mark([string]$kind) { "`e]133;$kind;sonicterm_token=$($global:__sonicterm_token)`a" }
  # Whether $line can run a native program: any command that is not a cmdlet (an application, a
  # function or script that may call one, a dynamic or unknown name) counts. Only cmdlets are certain.
  function global:__sonicterm_may_run_native([string]$line) {
    $tree = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$null, [ref]$null)
    $commands = $tree.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] }, $true)
    foreach ($command in $commands) {
      $name = $command.GetCommandName()
      if (-not $name) { return $true }
      $info = Get-Command $name -ErrorAction Ignore | Select-Object -First 1
      while ($info -is [System.Management.Automation.AliasInfo]) { $info = $info.ResolvedCommand }
      if ($null -eq $info -or $info.CommandType -ne 'Cmdlet') { return $true }
    }
    $false
  }
  # D's code for a failed line, read without ever writing $LASTEXITCODE, so commands see their own
  # value. A line that can run no native program failed in PowerShell: 1. Otherwise a new error
  # record means a PowerShell failure, and a non-zero $LASTEXITCODE a native one; when both are
  # present, which statement ended the line is unknown, so D carries no code (an empty string here)
  # rather than a guess. A non-zero $LASTEXITCODE alone is the native code; anything else is 1.
  function global:__sonicterm_code([bool]$ok) {
    if ($ok) { return 0 }
    if (-not $global:__sonicterm_may_native) { return 1 }
    $newest = if ($global:Error.Count -gt 0) { $global:Error[0] } else { $null }
    $fresh = $null -ne $newest -and -not [object]::ReferenceEquals($newest, $global:__sonicterm_error) -and
      $newest.Exception.GetType().Name -ne 'NativeCommandExitException'
    $native = $global:LASTEXITCODE
    $native_failed = $null -ne $native -and $native -ne 0
    if ($fresh -and $native_failed) { return '' }
    if ($fresh) { return 1 }
    if ($native_failed) { return $native }
    1
  }
  function global:prompt {
    $ok = $?
    $marks = ''
    if ($global:__sonicterm_read) {
      $code = if ($global:__sonicterm_ran) { __sonicterm_code $ok } else { 0 }
      $marks += if ($code -is [string]) { __sonicterm_mark 'D' } else { __sonicterm_mark "D;$code" }
    }
    $global:__sonicterm_read = $false
    $global:__sonicterm_ran = $false
    $marks += __sonicterm_mark 'A'
    $marks + (& $global:__sonicterm_prompt) + (__sonicterm_mark 'B')
  }
  function global:PSConsoleHostReadLine {
    $line = & $global:__sonicterm_readline
    $global:__sonicterm_read = $true
    if (-not [string]::IsNullOrWhiteSpace($line)) {
      $global:__sonicterm_ran = $true
      # The newest error record is taken before anything else here could add one.
      $global:__sonicterm_error = if ($global:Error.Count -gt 0) { $global:Error[0] } else { $null }
      $global:__sonicterm_may_native = __sonicterm_may_run_native $line
      [Console]::Write((__sonicterm_mark 'C'))
    }
    $line
  }
}
