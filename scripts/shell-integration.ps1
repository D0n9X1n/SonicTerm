# SonicTerm shell integration (PowerShell 7+). Load with: sonicterm-mcp --shell-integration pwsh | Out-String | Invoke-Expression
if ($env:SONICTERM_SHELL_TOKEN -and -not (Test-Path variable:global:__sonicterm_token) -and (Get-Module PSReadLine)) {
  $global:__sonicterm_token = $env:SONICTERM_SHELL_TOKEN
  Remove-Item env:SONICTERM_SHELL_TOKEN
  $global:__sonicterm_read = $false
  $global:__sonicterm_ran = $false
  $global:__sonicterm_error = $null
  $global:__sonicterm_line = ''
  $global:__sonicterm_parses = 0
  $global:__sonicterm_resolutions = 0
  $global:__sonicterm_prompt = $function:prompt
  $global:__sonicterm_readline = $function:PSConsoleHostReadLine
  function global:__sonicterm_mark([string]$kind) { "`e]133;$kind;sonicterm_token=$($global:__sonicterm_token)`a" }
  # Whether $line can run a native program: any command that is not a cmdlet (an application, a
  # function or script that may call one, a dynamic or unknown name) counts. Only cmdlets are certain.
  # It runs only for a failed line, after the line ran. A line over 16,384 UTF-16 code units, one
  # with more than 64 commands, or one that defines, aliases or removes a command is uncertain, so
  # it may run native: names are resolved as they stand now, which such a line may have changed.
  function global:__sonicterm_may_run_native([string]$line) {
    if ($line.Length -gt 16384) { return $true }
    $global:__sonicterm_parses++
    $global:__sonicterm_resolutions = 0
    $tree = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$null, [ref]$null)
    $defines = {
      param($node)
      $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -or
        ($node -is [System.Management.Automation.Language.VariableExpressionAst] -and $node.VariablePath.DriveName -in 'function', 'alias')
    }
    if ($tree.Find($defines, $true)) { return $true }
    $commands = $tree.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] }, $true)
    foreach ($command in $commands) {
      $name = $command.GetCommandName()
      if (-not $name) { return $true }
      if ($global:__sonicterm_resolutions -ge 64) { return $true }
      $global:__sonicterm_resolutions++
      $info = Get-Command $name -ErrorAction Ignore | Select-Object -First 1
      while ($info -is [System.Management.Automation.AliasInfo]) { $info = $info.ResolvedCommand }
      if ($null -eq $info -or $info.CommandType -ne 'Cmdlet') { return $true }
      if ($info.Name -in 'Set-Alias', 'New-Alias', 'Remove-Alias', 'Import-Module') { return $true }
      foreach ($element in $command.CommandElements) {
        if ($element.Extent.Text -match '^[''"]?(function|alias):') { return $true }
      }
    }
    $false
  }
  # D's code for a failed line, read without ever writing $LASTEXITCODE, so commands see their own
  # value. $native and $fresh are taken before anything else runs. A line that can run no native
  # program failed in PowerShell: 1. Otherwise a new error record means a PowerShell failure, and a
  # non-zero $LASTEXITCODE a native one; when both are present, which statement ended the line is
  # unknown, so D carries no code (an empty string here) rather than a guess. A non-zero
  # $LASTEXITCODE alone is the native code; anything else is 1. Only a failed line is parsed.
  function global:__sonicterm_code([bool]$ok, $native, [bool]$fresh) {
    if ($ok) { return 0 }
    if (-not (__sonicterm_may_run_native $global:__sonicterm_line)) { return 1 }
    $native_failed = $null -ne $native -and $native -ne 0
    if ($fresh -and $native_failed) { return '' }
    if ($fresh) { return 1 }
    if ($native_failed) { return $native }
    1
  }
  function global:prompt {
    $ok = $?
    $native = $global:LASTEXITCODE
    $newest = if ($global:Error.Count -gt 0) { $global:Error[0] } else { $null }
    $fresh = $null -ne $newest -and -not [object]::ReferenceEquals($newest, $global:__sonicterm_error) -and
      $newest.Exception.GetType().Name -ne 'NativeCommandExitException'
    $marks = ''
    if ($global:__sonicterm_read) {
      $code = if ($global:__sonicterm_ran) { __sonicterm_code $ok $native $fresh } else { 0 }
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
      $global:__sonicterm_line = $line
      [Console]::Write((__sonicterm_mark 'C'))
    }
    $line
  }
}
