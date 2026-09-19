[CmdletBinding()]
param(
    [ValidateSet('', 'System.IO.FileInfo', 'System.IO.DirectoryInfo')]
    [string]$CustomType = '',
    [ValidateSet('View', 'NameGetter')]
    [string]$CustomKind = 'View'
)
$ErrorActionPreference = 'Stop'
$env:TERM_PROGRAM = 'WindowsTerminal'
$integration = Join-Path $PSScriptRoot 'powershell-integration.ps1'
if ($CustomType) {
    # A fresh process keeps each customization independent of previously registered format data.
    $root = Join-Path ([IO.Path]::GetTempPath()) ('sonicterm-custom-view-' + [guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($root)
    try {
        if ($CustomKind -eq 'View') {
            $builder = [System.Management.Automation.CustomControl]::Create($false)
            $entry = $builder.StartEntry($null, $null)
            [void]$entry.AddScriptBlockExpressionBinding('"USERVIEW:" + $_.Name', $false, $null, '$true', $null)
            $view = [System.Management.Automation.FormatViewDefinition]::new('UserFileView', $entry.EndEntry().EndControl())
            $data = [System.Management.Automation.ExtendedTypeDefinition]::new($CustomType, [System.Management.Automation.FormatViewDefinition[]]@($view))
            $formats = [runspace]::DefaultRunspace.InitialSessionState.Formats
            $original = @($formats)
            $formats.Clear()
            $formats.Add([System.Management.Automation.Runspaces.SessionStateFormatEntry]::new($data))
            foreach ($format in $original) { $formats.Add($format) }
            Update-FormatData -ErrorAction Stop
        } else {
            Update-TypeData -TypeName $CustomType -MemberType ScriptProperty -MemberName NameString -Value { 'USERVIEW:' + $this.Name } -Force
        }
        $path = if ($CustomType -eq 'System.IO.FileInfo') {
            $file = Join-Path $root 'sample.txt'
            [IO.File]::WriteAllText($file, 'inert')
            $file
        } else { $root }
        $before = Get-Item -LiteralPath $path | Out-String -Width 200
        if (-not $before.Contains('USERVIEW:')) { throw 'custom view fixture did not load' }
        . $integration
        $after = Get-Item -LiteralPath $path | Out-String -Width 200
        if (Get-Command Format-SonicTermFileItem -ErrorAction SilentlyContinue) { throw "custom $CustomType $CustomKind was overwritten" }
        if (-not $after.Contains('USERVIEW:')) { throw 'custom view rendering changed' }
        return
    } finally { Remove-Item -LiteralPath $root -Recurse -Force }
}
foreach ($type in @('System.IO.FileInfo', 'System.IO.DirectoryInfo')) {
    foreach ($kind in @('View', 'NameGetter')) {
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        foreach ($argument in @('-NoProfile', '-NonInteractive', '-File', $PSCommandPath, '-CustomType', $type, '-CustomKind', $kind)) {
            $start.ArgumentList.Add($argument)
        }
        $child = [Diagnostics.Process]::Start($start)
        try {
            if (-not $child.WaitForExit(10000)) {
                $child.Kill($true)
                $child.WaitForExit()
                throw "custom $type $kind check timed out"
            }
            if ($child.ExitCode -ne 0) { throw "custom $type $kind check failed" }
        } finally { $child.Dispose() }
    }
}
. $integration
if (-not (Get-Command Format-SonicTermFileItem -ErrorAction SilentlyContinue)) { throw 'integration did not initialize' }
$root = Join-Path ([IO.Path]::GetTempPath()) ('sonicterm-shell-links-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($root)
try {
    foreach ($name in @('terminalprofiles-worktree-recovery-20260917-145142', 'folder with spaces')) {
        [void][IO.Directory]::CreateDirectory((Join-Path $root $name))
    }
    foreach ($name in @('file with spaces.txt', 'hash#file.txt', 'percent%20.txt', '目录🙂.txt', 'inert.exe')) {
        [IO.File]::WriteAllText((Join-Path $root $name), 'inert')
    }
    $PSStyle.OutputRendering = 'Ansi'
    foreach ($item in Get-ChildItem -LiteralPath $root) {
        foreach ($width in @(30, 80, 200)) {
            $s = Format-SonicTermFileItem $item -Width $width
            $target = ([uri]$item.FullName).AbsoluteUri
            foreach ($line in ($s -split "`n")) {
                $active = ''
                foreach ($match in [regex]::Matches($line, '\x1b\]8;;([^\x1b]*)\x1b\\')) {
                    $active = $match.Groups[1].Value
                    if ($active -and $active -cne $target) { throw "wrong URI identity: $active expected $target" }
                }
                if ($active) { throw 'link reaches generated newline' }
            }
            if (-not $s.Contains(([string][char]27) + ']8;;')) { throw 'missing link' }
        }
    }
    # Header positions must match actual default-view cells, not an independent duplicate of the format expression.
    $shortItem = Get-Item -LiteralPath (Join-Path $root 'inert.exe')
    $display = $shortItem | Out-String -Width 200
    $display = [System.Management.Automation.Internal.StringDecorated]::new($display).ToString([System.Management.Automation.OutputRendering]::PlainText)
    $header = @($display -split "`n" | Where-Object { $_.StartsWith('Mode') })[0]
    $row = @($display -split "`n" | Where-Object { $_.Contains('inert.exe') })[0]
    if ($header.IndexOf('Name') -ne $row.IndexOf('inert.exe')) { throw 'Name header does not align with file cells' }
    $lengthColumn = $header.IndexOf('Length') + 'Length'.Length - 1
    if ($row[$lengthColumn] -ne $shortItem.Length.ToString()[-1]) { throw 'Length header does not align with file cells' }
    foreach ($item in Get-ChildItem -LiteralPath $root) {
        foreach ($width in @(2, 20, 80, 200)) {
            $s = @($item, 'AFTER') | Out-String -Width $width
            $matches = [regex]::Matches($s, '\x1b\]8;;([^\x1b]*)\x1b\\')
            if (-not $matches.Count -or $matches[$matches.Count - 1].Groups[1].Value) { throw 'formatter dropped final closure' }
            # Decode the final formatted stream; another formatter may insert newlines within a linked fragment.
            $active = ''
            $linked = [Text.StringBuilder]::new()
            $unlinked = [Text.StringBuilder]::new()
            $target = ([uri]$item.FullName).AbsoluteUri
            foreach ($token in [regex]::Matches($s, '\x1b\]8;;([^\x1b]*)\x1b\\|\x1b\[[0-9;]*m|[\s\S]')) {
                if ($token.Value.StartsWith(([string][char]27) + ']8;;', [StringComparison]::Ordinal)) {
                    $active = $token.Groups[1].Value
                    if ($active -and $active -cne $target) { throw 'formatted URI identity changed' }
                } elseif (-not $token.Value.StartsWith([string][char]27, [StringComparison]::Ordinal)) {
                    if ($active) {
                        if ($token.Value -notin @("`r", "`n")) { [void]$linked.Append($token.Value) }
                    } else { [void]$unlinked.Append($token.Value) }
                }
            }
            if ($linked.ToString() -cne $item.Name) { throw "formatted link owns padding or loses name text: width=$width linked=$($linked.ToString() | ConvertTo-Json -Compress) expected=$($item.Name | ConvertTo-Json -Compress)" }
            if (($unlinked.ToString() -replace '[\r\n]', '') -notmatch 'AFTER') { throw 'following record remains linked' }
        }
    }
    foreach ($mode in @('Host', 'PlainText')) {
        $PSStyle.OutputRendering = $mode
        $s = Get-ChildItem -LiteralPath $root | Out-String -Width 80
        if ($s.Contains([char]27)) { throw 'redirect has escapes' }
    }
    $plain = Format-SonicTermFileItem $item -Width 80
    if ($plain.Contains([char]27)) { throw 'PlainText helper has escapes' }
    $PSStyle.OutputRendering = 'Ansi'
    $explicit = $item | Format-Table | Out-String -Width 80
    if ($explicit.Contains(([string][char]27) + ']8;')) { throw 'explicit Format-Table altered' }
    $json = $item | Select-Object Name, FullName | ConvertTo-Json -Compress
    if ($json.Contains('\u001b')) { throw 'object pipeline changed' }
    if ((Get-Alias ls).Definition -ne 'Get-ChildItem') { throw 'ls replaced' }
    $views = @((Get-FormatData System.IO.DirectoryInfo).FormatViewDefinition.Name)
    . $integration
    Update-FormatData -ErrorAction Stop
    if (($views -join '|') -ne (@((Get-FormatData System.IO.DirectoryInfo).FormatViewDefinition.Name) -join '|')) { throw 'duplicate view' }
    'PowerShell file links: PASS'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force
}
