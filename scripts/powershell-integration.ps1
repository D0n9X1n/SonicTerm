if ($PSVersionTable.PSVersion -lt [version]'7.2' -or $ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') { return }
if (-not $Host.UI.SupportsVirtualTerminal -or -not (Get-Variable PSStyle -ErrorAction SilentlyContinue)) { return }
if (Get-Command Format-SonicTermFileItem -ErrorAction SilentlyContinue) { return }
foreach ($type in @('System.IO.DirectoryInfo', 'System.IO.FileInfo')) {
    $definition = Get-FormatData -TypeName $type
    if ($definition.FormatViewDefinition.Count -ne 4) { return }
    foreach ($view in $definition.FormatViewDefinition) {
        if ($view.Name -notin @('children', 'childrenWithHardlink')) { return }
        if ($view.Control -is [System.Management.Automation.TableControl]) {
            $expected = if ($view.Name -eq 'childrenWithHardlink') { 'Mode|LastWriteTimeString|LengthString|NameString' } else { 'ModeWithoutHardLink|LastWriteTimeString|LengthString|NameString' }
            if ($view.Control.Rows.Count -ne 1 -or -not $view.Control.Rows[0].Wrap) { return }
            if ((($view.Control.Rows[0].Columns | ForEach-Object { $_.DisplayEntry.Value }) -join '|') -ne $expected) { return }
            if ($view.Control.GroupBy.Expression.Value -ne 'PSParentPath') { return }
            foreach ($column in $view.Control.Rows[0].Columns) {
                if ($column.DisplayEntry.ValueType.ToString() -ne 'Property') { return }
            }
        }
    }
    $member = (Get-TypeData $type).Members['NameString']
    if ($member -isnot [System.Management.Automation.Runspaces.CodePropertyData] -or $member.GetCodeReference.DeclaringType -ne [Microsoft.PowerShell.Commands.FileSystemProvider]) { return }
}
function global:Format-SonicTermFileItem {
    param([System.IO.FileSystemInfo]$Item, [int]$Width = $Host.UI.RawUI.WindowSize.Width)
    $native = [Microsoft.PowerShell.Commands.FileSystemProvider]::NameString([psobject]$Item)
    $plain = [System.Management.Automation.Internal.StringDecorated]::new($native).ToString([System.Management.Automation.OutputRendering]::PlainText)
    $prefix = '{0,-7}{1,26} {2,14} ' -f $Item.ModeWithoutHardLink, $Item.LastWriteTimeString, $Item.LengthString
    $width = [Math]::Max(2, $Width)
    $prefixWidth = $Host.UI.RawUI.LengthInBufferCells($prefix)
    $available = $width - $prefixWidth - 1
    $lines = [Collections.Generic.List[string]]::new()
    if ($available -lt 4) {
        $lines.Add($prefix.TrimEnd())
        $prefix = ''
        $prefixWidth = 0
        $available = $width - 1
    }
    $uri = $null
    $safe = $PSStyle.OutputRendering -ne 'PlainText' -and [uri]::TryCreate($Item.FullName, [UriKind]::Absolute, [ref]$uri) -and $uri.IsFile -and -not $uri.IsUnc -and $Item.PSProvider.Name -eq 'FileSystem'
    foreach ($ch in $Item.FullName.ToCharArray()) { if ([char]::IsControl($ch)) { $safe = $false } }
    $style = if ($PSStyle.OutputRendering -eq 'PlainText') { '' } else { [regex]::Match($native, '^(?:\x1b\[[0-9;]*m)+').Value }
    $reset = if ($style) { $PSStyle.Reset } else { '' }
    $part = [Text.StringBuilder]::new()
    $used = 0
    $elements = [Globalization.StringInfo]::GetTextElementEnumerator($plain)
    while ($elements.MoveNext()) {
        $element = $elements.GetTextElement()
        $cells = $Host.UI.RawUI.LengthInBufferCells($element)
        if ($used -gt 0 -and $used + $cells -gt $available) {
            $label = $style + $part.ToString() + $reset
            if ($safe) {
                $label = ([string][char]27) + ']8;;' + $uri.AbsoluteUri + ([string][char]27) + '\' + $label + ([string][char]27) + ']8;;' + ([string][char]27) + '\'
            }
            $lines.Add($prefix + $label)
            $prefix = ' ' * $prefixWidth
            [void]$part.Clear()
            $used = 0
        }
        [void]$part.Append($element)
        $used += $cells
    }
    if ($part.Length -gt 0) {
        $label = $style + $part.ToString() + $reset
        if ($safe) {
            $label = ([string][char]27) + ']8;;' + $uri.AbsoluteUri + ([string][char]27) + '\' + $label + ([string][char]27) + ']8;;' + ([string][char]27) + '\'
        }
        $lines.Add($prefix + $label)
    }
    $lines -join "`n"
}
$formats = [runspace]::DefaultRunspace.InitialSessionState.Formats
$original = @($formats)
try {
    $headerBuilder = [System.Management.Automation.CustomControl]::Create($false)
    $headerEntry = $headerBuilder.StartEntry($null, $null)
    [void]$headerEntry.AddScriptBlockExpressionBinding('"    Directory: " + $_.PSParentPath.Replace("Microsoft.PowerShell.Core\FileSystem::", "")', $false, $null, '$true', $null)
    [void]$headerEntry.AddNewline(2)
    [void]$headerEntry.AddText(('Mode{0}LastWriteTime{1}Length Name' -f (' ' * 16), (' ' * 9)))
    [void]$headerEntry.AddNewline(1)
    $builder = [System.Management.Automation.CustomControl]::Create($false)
    [void]$builder.GroupByProperty('PSParentPath', $headerEntry.EndEntry().EndControl(), $null)
    $entry = $builder.StartEntry($null, $null)
    [void]$entry.AddScriptBlockExpressionBinding('Format-SonicTermFileItem $_', $false, $null, '$true', $null)
    [void]$entry.AddNewline(1)
    $view = [System.Management.Automation.FormatViewDefinition]::new('SonicTerm Linked Files', $entry.EndEntry().EndControl())
    $data = [System.Management.Automation.ExtendedTypeDefinition]::new('System.IO.DirectoryInfo', [System.Management.Automation.FormatViewDefinition[]]@($view))
    $data.TypeNames.Add('System.IO.FileInfo')
    $formats.Clear()
    $formats.Add([System.Management.Automation.Runspaces.SessionStateFormatEntry]::new($data))
    foreach ($item in $original) { $formats.Add($item) }
    Update-FormatData -ErrorAction Stop
} catch {
    $formats.Clear()
    foreach ($item in $original) { $formats.Add($item) }
    Update-FormatData -ErrorAction SilentlyContinue
    Remove-Item Function:\Format-SonicTermFileItem -ErrorAction SilentlyContinue
    Write-Warning 'SonicTerm file links could not initialize; native PowerShell formatting is retained.'
}
