[CmdletBinding()]
param(
    [ValidateSet('All', 'Web', 'Osc8', 'Paths', 'Wrappers', 'Source', 'Negative', 'Wrapping', 'KnownGaps')]
    [string]$Group = 'All'
)

$ErrorActionPreference = 'Stop'
$esc = [char]27
$root = Join-Path ([System.IO.Path]::GetTempPath()) ('sonicterm-link-test-' + [guid]::NewGuid().ToString('N'))
[System.IO.Directory]::CreateDirectory($root) | Out-Null
$folder = Join-Path $root 'Test Folder'
[System.IO.Directory]::CreateDirectory($folder) | Out-Null
foreach ($name in @('flight.html', 'flight.md', 'source.rs', 'flight.html,', 'inert.exe', 'hash#name.txt', 'percent%20.txt')) {
    [System.IO.File]::WriteAllText((Join-Path $root $name), "Harmless reveal fixture. Select this file; never execute it.`r`n")
}
$file = Join-Path $folder 'selected & name.txt'
[System.IO.File]::WriteAllText($file, 'Harmless spaced filename fixture.')
$plain = (Join-Path $root 'flight.html').Replace('\', '/')
$markdown = (Join-Path $root 'flight.md').Replace('\', '/')
$source = (Join-Path $root 'source.rs').Replace('\', '/')
$spaced = $file.Replace('\', '/')
$literalComma = (Join-Path $root 'flight.html,').Replace('\', '/')
$exe = (Join-Path $root 'inert.exe').Replace('\', '/')
$fileUri = ([Uri]::new($file)).AbsoluteUri
$comma = [string][char]0xFF0C
$period = [string][char]0x3002
$prose = [string][char]0x6B63 + [char]0x6587
$script:caseNumber = 0

function Write-Case {
    param([string]$Label, [string]$Text, [string]$Expected)
    $script:caseNumber++
    Write-Output ''
    Write-Output ('CASE {0:D3} - {1}' -f $script:caseNumber, $Label)
    Write-Output "  EXPECT: $Expected"
    Write-Output $Text
}

function Write-Osc8Case {
    param([string]$Label, [string]$Destination, [string]$Expected)
    Write-Case $Label ("{0}]8;;{1}{0}\{0}[4;36m{2}{0}[0m{0}]8;;{0}\" -f $esc, $Destination, $Label) $Expected
    Write-Output "  DESTINATION: $Destination"
}

function Test-Group {
    param([string]$Name)
    return $Group -eq 'All' -or $Group -eq $Name
}

Write-Output 'SonicTerm URL and local-path detection matrix'
Write-Output 'Hold Ctrl while hovering/clicking on Windows/Linux; Cmd on macOS.'
Write-Output 'Check the preview destination first. Local files must be selected, never executed.'
Write-Output 'Web clicks open your browser; mailto opens a draft. Hover-only is enough to check extraction.'
Write-Output 'No target is opened automatically. No clipboard/config changes or network requests are made.'
Write-Output 'This is a broad manual regression matrix, not proof that every possible input is supported.'
Write-Output "FIXTURES: $root"
Write-Output 'Fixtures remain until you delete that exact temporary directory after testing.'
Write-Output 'Run with -Group Web, Osc8, Paths, Wrappers, Source, Negative, Wrapping, or KnownGaps to shorten output.'
Write-Output 'Report CASE ID, preview text, click result, and any error. Keep the whole wrapped target visible.'

if (Test-Group 'Web') {
    Write-Output 'GROUP: Web'
    foreach ($url in @(
        'https://example.com/', 'http://example.com/path', 'https://example.com:8443/path',
        'https://example.com/a/b?x=1&y=2#section', 'https://example.com/a%20b?q=%2Fsrc%2Fmain.rs',
        'HTTPS://example.com/path', 'https://example.com/a-b_c~d', 'mailto:test@example.com'
    )) {
        Write-Case 'Plain supported URI' $url "Preview exactly $url. Web opens browser; mailto opens a draft, not a sent message."
    }
    foreach ($ending in @('.', ',', ';', ':', '!', '?', $comma, $period)) {
        Write-Case 'URI followed by prose punctuation' "(https://example.com/path)$ending" 'Preview https://example.com/path only; omit wrappers and trailing punctuation.'
    }
    Write-Case 'Neighboring URLs' 'https://example.com/first https://example.com/second' 'Each URL has its own exact destination; no combined target.'
    Write-Case 'Styled text is not an explicit hyperlink' ("{0}[36mhttps://example.com/colored{0}[0m" -f $esc) 'Plain-text URL detection still works; color does not choose the destination.'
}

if (Test-Group 'Osc8') {
    Write-Output 'GROUP: Osc8'
    Write-Osc8Case 'Web label' 'https://example.com/actual' 'Preview actual destination, not the visible label.'
    Write-Osc8Case 'https://example.com/display-only' 'https://example.com/actual' 'Preview /actual despite the URL-looking label.'
    Write-Osc8Case 'Local spaced file label' $fileUri 'Decode spaces and select selected & name.txt in Test Folder.'
    Write-Osc8Case 'Local directory label' $folder.Replace('\', '/') 'Navigate into Test Folder.'
    Write-Osc8Case 'Inert executable label' $exe 'Select inert.exe in its folder; do not execute it.'
    Write-Osc8Case 'Source line label' ($source + ':7') 'Select source.rs; do not launch an editor.'
    Write-Osc8Case 'File URI fragment label' (([Uri]::new((Join-Path $root 'source.rs'))).AbsoluteUri + '#L3') 'Select source.rs; #L3 is location metadata.'
    Write-Osc8Case 'Missing explicit destination' ($plain + '.missing') 'Missing-path error on first click without copying; a second click while visible copies this target.'
    Write-Osc8Case 'Remote file URI' 'file://remote.invalid/share/file.txt' 'Reject as nonlocal; do not access a remote share.'
    Write-Osc8Case 'Unsupported editor scheme' ('vscode://file/' + $source + ':7') 'Reject scheme; never invoke an editor.'
}

if (Test-Group 'Paths') {
    Write-Output 'GROUP: Paths'
    Write-Case 'Absolute slash path' $plain 'Select flight.html.'
    Write-Case 'Absolute backslash path' $plain.Replace('/', '\') 'Select the same flight.html.'
    Write-Case 'Absolute path containing spaces' $spaced 'Select selected & name.txt; preserve spaces and ampersand.'
    Write-Case 'Markdown file' $markdown 'Select flight.md, not its associated application.'
    Write-Case 'Executable is reveal-only' $exe 'Select inert.exe; never execute it.'
    Write-Case 'Rooted log field' ('path=' + $exe + ' probe="--version" timeout_seconds=5') 'Select inert.exe only; never execute it or include path= in the active span.'
    Write-Case 'Quoted log field' ('file="' + $spaced + '" next=ready') 'Select the complete spaced filename; field key and quotes remain outside the underline.'
    Write-Case 'Unwrapped file list' ($plain + [char]0x3001 + 'flight.md') 'After confirming the full literal is missing, first member selects flight.html. The short second name uses the pane CWD, never the first parent folder.'
    Write-Case 'Existing directory' $folder.Replace('\', '/') 'Navigate into Test Folder.'
    Write-Case 'Percent-encoded file URI' $fileUri 'Decode %20 once and select selected & name.txt.'
    foreach ($name in @('hash#name.txt', 'percent%20.txt')) {
        Write-Case 'Encoded filename identity' ([Uri]::new((Join-Path $root $name))).AbsoluteUri "Select literal $name; do not interpret it as a fragment or decode twice."
    }
    Write-Case 'Both literal and shorter files exist' $literalComma 'Select flight.html, including its final comma. The shorter flight.html also exists.'
    if ($plain -match '^[A-Za-z]:/') {
        Write-Case 'Doubled drive-root separator' ($plain.Substring(0, 2) + '/' + $plain.Substring(2)) 'Select flight.html without changing drive identity.'
    }
    $cwd = Get-Location
    if ($cwd.Provider.Name -eq 'FileSystem') {
        $relativeFile = Get-ChildItem -LiteralPath $cwd.Path -File | Where-Object { $_.Name -match '^[A-Za-z0-9_.-]+$' } | Select-Object -First 1
        if ($null -ne $relativeFile) {
            Write-Case 'Relative path in your current directory' ('./' + $relativeFile.Name) "Select $($relativeFile.FullName), only if this pane has current local OSC 7 directory state. No process-CWD fallback."
            Write-Case 'Bare filename' $relativeFile.Name 'Same pane-directory requirement; clickable_bare_names must be enabled.'
        }
    }
    Write-Case 'Home-relative existing config directory' '~/.sonicterm' 'Navigate only if this directory exists in the native home.'
}

if (Test-Group 'Wrappers') {
    Write-Output 'GROUP: Wrappers'
    $pairs = @(@('(', ')'), @('[', ']'), @('{', '}'), @("'", "'"), @('"', '"'), @('`', '`'), @('Read(', ')'))
    foreach ($codes in @(@(0xFF08, 0xFF09), @(0x3010, 0x3011), @(0x300A, 0x300B), @(0x300C, 0x300D), @(0x300E, 0x300F), @(0x201C, 0x201D), @(0x2018, 0x2019), @(0x00AB, 0x00BB))) {
        $pairs += ,@([string][char]$codes[0], [string][char]$codes[1])
    }
    foreach ($pair in $pairs) {
        Write-Case 'Paired path with adjacent Chinese prose' ($pair[0] + $plain + $pair[1] + $comma + $prose) 'Select flight.html; underline excludes the pair, separator, and prose.'
    }
    foreach ($ending in @(',', ';', '!', '?', $comma, $period, [string][char]0x3001, [string][char]0xFF1B, [string][char]0xFF1A, [string][char]0x2026, [string][char]0x2014, [string][char]0x060C)) {
        Write-Case 'Outer punctuation without following space' ('(' + $plain + ')' + $ending + $prose) 'Select flight.html only; outer separator never becomes filename content.'
    }
    Write-Case 'Literal punctuation inside wrapper' ('(' + $literalComma + ')' + $comma + $prose) 'Select flight.html, with the literal comma, not flight.html.'
    Write-Case 'Quote inside parentheses' ('("' + $spaced + '").') 'Select the complete spaced file; exclude both wrapper layers from underline.'
    Write-Case 'Timestamp before a wrapped file' ('9:30 (' + $plain + ')') 'Timestamp does not suppress the file.'
    Write-Case 'Letter label before a wrapped file' ('A: item (' + $plain + ')') 'A: is prose, not a rooted drive path.'
}

if (Test-Group 'Source') {
    Write-Output 'GROUP: Source'
    foreach ($suffix in @(':3', ':3:2', ':3-7', (':3' + [char]0x2013 + '7'))) {
        Write-Case 'Single source location' ($source + $suffix) 'Underline includes the location; select only source.rs. No editor is launched.'
    }
    $groupExpected = if ($source.Contains(' ')) { 'Inert: grouped source anchors cannot contain spaces.' } else { 'Select source.rs from its filename or either location; group commas/spaces do not initiate navigation.' }
    Write-Case 'Grouped locations with Unicode prose' ('(' + $source + ':3, :7)' + $comma + $prose) $groupExpected
    Write-Case 'Quoted individual source with spaces' ('"' + $spaced + ':3".') 'Select selected & name.txt; its quoted individual location supports spaces.'
}

if (Test-Group 'Negative') {
    Write-Output 'GROUP: Negative'
    Write-Case 'Missing explicit path' ($plain + '.missing') 'Show the explicit missing path; first click does not copy; second click confirms copying.'
    Write-Case 'Unverified ordinary word' 'this_is_not_a_fixture_name_078ddff9' 'No navigation, preview, failure notification, or clipboard write.'
    foreach ($text in @(
        ('(' + $plain + ').bak'), ('(' + $plain + ')/child'), ('(' + $plain + ')tail'),
        ('(' + $plain + ']'), ('"' + $plain), ('prefix"' + $plain + '"')
    )) {
        Write-Case 'Ambiguous or malformed structure' $text 'Do not select the shorter flight.html by repairing or truncating this text.'
    }
    foreach ($suffix in @(':0', ':7-3', ':3:0', ':3:abc', ':999999999999')) {
        Write-Case 'Invalid source location' ('(' + $source + $suffix + ').') 'Do not reveal source.rs via an invalid location fallback.'
    }
    Write-Osc8Case 'Traversal file URI' 'file:///C:/work/../other.txt' 'Reject traversal; do not navigate.'
    Write-Osc8Case 'Encoded path separator' 'file:///C:/work/a%2Fb.txt' 'Reject encoded structural separator.'
    Write-Osc8Case 'Unsupported command scheme' 'javascript:alert(1)' 'Reject scheme; never execute content.'
    Write-Case 'Invisible character retained' ($plain + [char]0x200B) 'Do not silently delete the invisible character and reveal the shorter file.'
    Write-Case 'Combining mark on outer closer' ('(' + $plain + ')' + [char]0x0301) 'Unsafe boundary must not authorize the inner path.'
}

if (Test-Group 'Wrapping') {
    Write-Output 'GROUP: Wrapping'
    Write-Output 'Resize narrow enough to wrap, but keep the complete target visible. Automatic wrapping is not a hard newline.'
    $longUrl = 'https://example.com/' + ('segment/' * 12) + 'file?q=1&x=2'
    Write-Case 'Automatically wrapped URL' ('(' + $longUrl + ')') 'Every visible fragment previews the complete URL, within eight rows / 4 KiB.'
    Write-Case 'Automatically wrapped local target' ((' ' * 30) + '(' + $spaced + ')' + $comma + $prose) 'Every target fragment selects the same spaced file; surrounding prose stays outside the underline.'
    Write-Case 'Hard-newline local fragments' ("({0}`n{1})" -f $folder.Replace('\', '/'), '/selected & name.txt') 'Never join hard lines into the existing spaced-file target.'
    Write-Case 'Unproven hard-newline URL fragments' "https://example.com/first`nsecond" 'Do not invent https://example.com/firstsecond; independent first-line URI behavior is separate.'
}

if (Test-Group 'KnownGaps') {
    Write-Output 'GROUP: KnownGaps'
    Write-Case 'Unsupported raw Markdown link' ('[report](' + $plain + ')') 'Not a Markdown parser; no promise to detect this literal source form.'
    Write-Case 'Colored prose is not a link' ("{0}[36mordinary colored words{0}[0m" -f $esc) 'Color alone must not create a clickable target.'
    Write-Output 'The original missing-relative-file report still needs its actual pane directory/probe evidence; it is not declared fixed by these samples.'
}

Write-Output ''
Write-Output 'END: report CASE ID + preview + click result. Wait over a path for its background probe before clicking.'
Write-Output "CLEANUP AFTER TESTING: remove only this fixture directory: $root"
