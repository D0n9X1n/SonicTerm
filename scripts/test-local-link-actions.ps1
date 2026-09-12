[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$esc = [char]27
$root = Join-Path ([System.IO.Path]::GetTempPath()) ('sonicterm-link-test-' + [guid]::NewGuid().ToString('N'))
[System.IO.Directory]::CreateDirectory($root) | Out-Null
$folder = Join-Path $root 'Test Folder'
[System.IO.Directory]::CreateDirectory($folder) | Out-Null
$file = Join-Path $folder 'selected & name.txt'
$source = Join-Path $folder 'main.rs'
$blocked = Join-Path $folder 'blocked.exe'
[System.IO.File]::WriteAllText($file, "Local file reveal test. This file should be selected, not opened.`r`n")
[System.IO.File]::WriteAllText($source, ((1..7 | ForEach-Object { "// Fixture line $_" }) -join "`r`n"))
[System.IO.File]::WriteAllText($blocked, 'Non-executable text fixture with a blocked extension.')

function Write-LinkCase {
    param([string]$Number, [string]$Label, [string]$Target, [string]$Expected)
    Write-Output ""
    Write-Output ("[{0}] {1}]8;;{2}{1}\{1}[4;36m{3}{1}[0m{1}]8;;{1}\" -f $Number, $esc, $Target, $Label)
    Write-Output "    Expect: $Expected"
    Write-Output "    Target: $Target"
}

$folderTarget = $folder.Replace('\', '/')
$fileTarget = $file.Replace('\', '/')
$sourceTarget = $source.Replace('\', '/')
# Double only the drive-root separator to reproduce C:// links without changing the file.
$doubleSlashSource = $sourceTarget.Substring(0, 2) + '/' + $sourceTarget.Substring(2) + ':7'
$fileUri = ([System.Uri]::new($file)).AbsoluteUri

Write-Output 'SonicTerm local-link test'
Write-Output 'Hold Ctrl and hover to inspect; Ctrl+click each numbered label.'
Write-Output 'Files must be selected in Explorer, not opened in their default application.'
Write-Output "Fixtures are kept here: $root"
Write-Output 'This script prints links only; it does not open them or change your configuration.'

Write-LinkCase '1' 'Test Folder' $folderTarget 'Open Test Folder itself in Explorer.'
Write-LinkCase '2' 'selected & name.txt' $fileTarget 'Open Test Folder and select selected & name.txt.'
Write-LinkCase '3' 'main.rs:7 (C:// target)' $doubleSlashSource 'Open Test Folder and select main.rs; do not launch an editor.'
Write-LinkCase '4' 'selected & name.txt (file URI)' $fileUri 'Same selection as case 2; percent-encoded spaces must decode correctly.'
Write-LinkCase '5' 'missing-file.txt' ($folderTarget + '/missing-file.txt') 'Show the explicit filepath and missing reason; first click leaves clipboard unchanged; second click copies.'
Write-LinkCase '6' 'blocked.exe (inert fixture)' $blocked.Replace('\', '/') 'Select blocked.exe in Explorer; never execute it.'
Write-LinkCase '7' 'remote file URI' 'file://remote.invalid/share/main.rs' 'Show the explicit target and rejection reason; do not access the remote share or copy on first click.'
Write-LinkCase '8' 'unsupported editor link' ('vscode://file/' + $sourceTarget + ':7') 'Show a scheme-not-allowed reason; do not launch an editor.'

Write-Output ""
Write-Output '[9] Plain-text absolute source reference (not an OSC 8 label):'
Write-Output ($sourceTarget + ':7')
Write-Output '    Expect: open Test Folder and select main.rs.'
$driveFileUri = 'file:' + $fileUri.Substring(8, 2) + '/' + $fileUri.Substring(10)
Write-LinkCase '10' 'file:c:// file target' $driveFileUri 'Open Test Folder and select selected & name.txt (requires the updated build).'
Write-Output ""
Write-LinkCase '11' 'main.rs line 3 (#L3)' ($sourceTarget + '#L3') 'Select main.rs; #L3 is line metadata, not the filename.'
$sourceUri = ([System.Uri]::new($source)).AbsoluteUri
$driveSourceUri = 'file:' + $sourceUri.Substring(8, 2) + '/' + $sourceUri.Substring(10)
Write-LinkCase '12' 'main.rs line 3 (file:c:// + #3)' ($driveSourceUri + '#3') 'Select main.rs; #3 is line metadata.'
Write-Output 'Report by case number: preview text, click result, and any visible error.'
Write-Output 'After an actionable link fails, first verify the error includes its path and leaves the clipboard unchanged.'
Write-Output 'Click that same link again while the error is visible, then paste to check the copied path.'
Write-Output 'For example: 3 - selected main.rs; 5 - filepath and missing reason, no first-click copy.'
Write-Output 'If validation is pending, keep Ctrl held over the link briefly, then click again.'
