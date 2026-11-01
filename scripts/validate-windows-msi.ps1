[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$MsiPath,

    [Parameter(Mandatory = $true)]
    [string]$ExpectedVersion
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ExpectedUpgradeCode = "{B9F8C0F4-1F4F-4F2E-AE3C-7A9D7D6E3F11}"
$ExpectedComponents = @(
    "binary0",
    "conpty_runtime",
    "script_progids",
    "script_capabilities",
    "script_open_with",
    "asset_themes",
    "asset_keymaps",
    "asset_fonts",
    "asset_icons",
    "start_menu_shortcut",
    "desktop_shortcut"
)
$ExpectedFeature = "Binaries"
$ExpectedTemplate = "x64;1033"
$Component64Bit = 256

function Get-NumericSemVerCore {
    param([string]$Version)

    $prereleaseIdentifier = '(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)'
    $pattern = "^v?(?<major>0|[1-9][0-9]*)\.(?<minor>0|[1-9][0-9]*)\.(?<patch>0|[1-9][0-9]*)(?:-$prereleaseIdentifier(?:\.$prereleaseIdentifier)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
    if ($Version -notmatch $pattern) {
        throw "Expected version '$Version' is not a supported semantic version"
    }

    $major = [uint64]$Matches.major
    $minor = [uint64]$Matches.minor
    $patch = [uint64]$Matches.patch
    if ($major -gt 255 -or $minor -gt 255 -or $patch -gt 65535) {
        throw "MSI ProductVersion core $major.$minor.$patch exceeds Windows Installer bounds (255.255.65535)"
    }
    "$major.$minor.$patch"
}

function Invoke-ComMember {
    param(
        [object]$ComObject,
        [string]$Name,
        [System.Reflection.BindingFlags]$Flags,
        [object[]]$Arguments
    )

    $ComObject.GetType().InvokeMember($Name, $Flags, $null, $ComObject, $Arguments)
}

function Read-MsiRows {
    param(
        [object]$Database,
        [string]$Query,
        [int]$FieldCount
    )

    $view = Invoke-ComMember $Database "OpenView" ([System.Reflection.BindingFlags]::InvokeMethod) @($Query)
    try {
        Invoke-ComMember $view "Execute" ([System.Reflection.BindingFlags]::InvokeMethod) $null | Out-Null
        while ($true) {
            $record = Invoke-ComMember $view "Fetch" ([System.Reflection.BindingFlags]::InvokeMethod) $null
            if ($null -eq $record) {
                break
            }
            try {
                $fields = @()
                for ($index = 1; $index -le $FieldCount; $index++) {
                    $fields += Invoke-ComMember $record "StringData" ([System.Reflection.BindingFlags]::GetProperty) @($index)
                }
                [pscustomobject]@{ Values = [object[]]$fields }
            }
            finally {
                [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($record)
            }
        }
    }
    finally {
        try {
            Invoke-ComMember $view "Close" ([System.Reflection.BindingFlags]::InvokeMethod) $null | Out-Null
        }
        finally {
            [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
        }
    }
}

function Convert-RowsToMap {
    param([object[]]$Rows)

    $result = [System.Collections.Hashtable]::new([System.StringComparer]::Ordinal)
    foreach ($row in $Rows) {
        $result[[string]$row.Values[0]] = [string]$row.Values[1]
    }
    $result
}

function Test-MsiMetadata {
    param(
        [hashtable]$Properties,
        [string]$Template,
        [hashtable]$Components,
        [hashtable]$Features,
        [string[]]$FeatureComponents,
        [string]$Version
    )

    $numericVersion = Get-NumericSemVerCore $Version
    foreach ($required in @("ProductVersion", "UpgradeCode", "ProductCode")) {
        if (@($Properties.Keys) -cnotcontains $required -or [string]::IsNullOrWhiteSpace($Properties[$required])) {
            throw "MSI Property table is missing nonempty $required"
        }
    }
    if ($Properties.ProductVersion -cne $numericVersion) {
        throw "MSI ProductVersion '$($Properties.ProductVersion)' does not match numeric SemVer core '$numericVersion'"
    }
    if ($Properties.UpgradeCode -ine $ExpectedUpgradeCode) {
        throw "MSI UpgradeCode '$($Properties.UpgradeCode)' does not match '$ExpectedUpgradeCode'"
    }
    $parsedProductCode = [guid]::Empty
    if (-not [guid]::TryParse($Properties.ProductCode, [ref]$parsedProductCode) -or $parsedProductCode -eq [guid]::Empty) {
        throw "MSI ProductCode '$($Properties.ProductCode)' is not a nonempty GUID"
    }
    if ($Template -cne $ExpectedTemplate) {
        throw "MSI summary template '$Template' does not match '$ExpectedTemplate'"
    }

    $actualComponents = @($Components.Keys | Sort-Object)
    $expectedSorted = @($ExpectedComponents | Sort-Object)
    if (Compare-Object $expectedSorted $actualComponents -CaseSensitive) {
        throw "MSI Component table differs from the expected set: $($actualComponents -join ', ')"
    }
    foreach ($component in $ExpectedComponents) {
        $attributes = [int]$Components[$component]
        if (($attributes -band $Component64Bit) -eq 0) {
            throw "MSI component '$component' is not marked 64-bit (Attributes=$attributes)"
        }
    }
    if (@($Features.Keys) -cnotcontains $ExpectedFeature) {
        throw "MSI Feature table is missing '$ExpectedFeature'"
    }
    $actualFeatureComponents = @($FeatureComponents | Sort-Object)
    if (Compare-Object $expectedSorted $actualFeatureComponents -CaseSensitive) {
        throw "MSI feature '$ExpectedFeature' does not reference the expected components: $($actualFeatureComponents -join ', ')"
    }

    $numericVersion
}

function Get-MsiRuntimeContract {
    $manifest = Get-Content -LiteralPath "$PSScriptRoot\windows-conpty.json" -Raw | ConvertFrom-Json
    if ($manifest.schema_version -ne 1 -or $manifest.architecture -cne "x64" -or
        $manifest.package_id -cne "Microsoft.Windows.Console.ConPTY") {
        throw "Unsupported Windows ConPTY pin manifest"
    }
    @{
        conpty_dll = @{
            Name = "conpty.dll"
            Size = [long]$manifest.files.'conpty.dll'.size
            Sha256 = [string]$manifest.files.'conpty.dll'.sha256
        }
        conpty_host = @{
            Name = "OpenConsole.exe"
            Size = [long]$manifest.files.'OpenConsole.exe'.size
            Sha256 = [string]$manifest.files.'OpenConsole.exe'.sha256
        }
        conpty_license = @{
            Name = "LICENSE"
            Size = [long]$manifest.license.size
            Sha256 = [string]$manifest.license.sha256
        }
        conpty_notice = @{
            Name = "NOTICE.md"
            Size = 23176
            Sha256 = "e7fbaadee6ab20c28b87730a510ee5f5815d8fb4bd88d1d54d282dc2a74c0726"
        }
    }
}

function Test-MsiRuntimeMetadata {
    param(
        [object[]]$Files,
        [object[]]$Components,
        [object[]]$Directories,
        [hashtable]$Contract
    )

    $runtime = @($Components | Where-Object { [string]$_.Values[0] -ceq "conpty_runtime" })
    if ($runtime.Count -ne 1 -or [string]$runtime[0].Values[1] -cne "ConptyDir" -or
        [string]$runtime[0].Values[2] -cne "conpty_dll") {
        throw "MSI runtime component must use ConptyDir and the conpty_dll key path"
    }
    $expectedDirectories = @{
        TARGETDIR = @("", "SourceDir")
        ProgramFiles64Folder = @("TARGETDIR", "PFiles")
        APPLICATIONFOLDER = @("ProgramFiles64Folder", "SonicTerm")
        ConptyDir = @("APPLICATIONFOLDER", "conpty")
    }
    foreach ($directoryId in $expectedDirectories.Keys) {
        $directory = @($Directories | Where-Object { [string]$_.Values[0] -ceq $directoryId })
        $expected = $expectedDirectories[$directoryId]
        if ($directory.Count -ne 1 -or [string]$directory[0].Values[1] -cne $expected[0] -or
            (([string]$directory[0].Values[2] -split ':')[0] -split '\|')[-1] -cne $expected[1]) {
            throw "MSI runtime directory '$directoryId' differs from the contract"
        }
    }
    foreach ($fileId in $Contract.Keys) {
        $file = @($Files | Where-Object { [string]$_.Values[0] -ceq $fileId })
        if ($file.Count -eq 0) {
            throw "MSI runtime file set is missing '$fileId'"
        }
        if ($file.Count -ne 1 -or [string]$file[0].Values[1] -cne "conpty_runtime" -or
            ([string]$file[0].Values[2] -split '\|')[-1] -cne $Contract[$fileId].Name -or
            [long]$file[0].Values[3] -ne $Contract[$fileId].Size) {
            throw "MSI runtime file '$fileId' differs from the contract"
        }
    }
    $runtimeFiles = @($Files | Where-Object { [string]$_.Values[1] -ceq "conpty_runtime" })
    if ($runtimeFiles.Count -ne $Contract.Count) {
        throw "MSI runtime file set differs from the contract"
    }
}

function Test-MsiRuntimePayload {
    param([string]$ExtractionRoot, [hashtable]$Contract)

    $runtimePath = Join-Path $ExtractionRoot "PFiles/SonicTerm/conpty"
    foreach ($file in $Contract.Values) {
        $filePath = Join-Path $runtimePath $file.Name
        if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) {
            throw "MSI runtime payload '$($file.Name)' is missing"
        }
        $item = Get-Item -LiteralPath $filePath
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "MSI runtime payload '$($file.Name)' is a reparse point"
        }
        if ($item.Length -ne $file.Size) {
            throw "MSI runtime payload '$($file.Name)' size does not match the trusted pin"
        }
        if ((Get-FileHash -LiteralPath $filePath -Algorithm SHA256).Hash -ine $file.Sha256) {
            throw "MSI runtime payload '$($file.Name)' SHA256 does not match the trusted pin"
        }
    }
    $actualNames = @(Get-ChildItem -LiteralPath $runtimePath -Force | ForEach-Object Name)
    $expectedNames = @($Contract.Values | ForEach-Object Name)
    if (Compare-Object $expectedNames $actualNames -CaseSensitive) {
        throw "MSI runtime payload file set differs from the contract"
    }
}

function Invoke-MsiExtractionCommand {
    param([string[]]$Command, [string]$StateRoot, [int]$TimeoutSeconds = 120)

    # Assign-before-launch custody bounds extraction and rejects successful leaders with surviving children.
    $runner = @'
import importlib.util, json, os, pathlib, sys, time
module_path, state_root, timeout_seconds, *command = sys.argv[1:]
spec = importlib.util.spec_from_file_location("msi_process_job", module_path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with (pathlib.Path(state_root) / "process.log").open("wb") as sink:
    result = module.run(command, cwd=pathlib.Path(state_root), env=os.environ, sink=sink,
                        deadline=time.monotonic() + int(timeout_seconds), output_limit_bytes=1048576)
custody = result["custody"]
valid = (result["exit_code"] == 0 and result["natural"] and not result["errors"]
         and not any(result[name] for name in ("interrupted", "timed_out", "launch_failed"))
         and custody["cleanup"] == "none" and not custody["errors"]
         and all(custody[name] for name in ("empty", "bootstrap_reaped", "protocol_complete", "capture_complete")))
print(json.dumps(result))
raise SystemExit(0 if valid else 1)
'@
    $diagnostic = & python -I -c $runner "$PSScriptRoot\windows-process-job.py" $StateRoot $TimeoutSeconds @Command 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "MSI administrative extraction failed: $($diagnostic -join [Environment]::NewLine)"
    }
}

function Invoke-MsiAdminExtraction {
    param([string]$PackagePath, [string]$ExtractionRoot, [string]$StateRoot)

    $command = @(
        (Join-Path $env:SystemRoot "System32/msiexec.exe"), "/a", $PackagePath, "/qn", "/norestart",
        "TARGETDIR=$ExtractionRoot", "/L*v", (Join-Path $StateRoot "admin.log")
    )
    Invoke-MsiExtractionCommand $command $StateRoot 120
}

function Test-MsiRuntimePackage {
    param([string]$PackagePath, [hashtable]$Contract)

    $stateRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("sonicterm-msi-" + [guid]::NewGuid().ToString("N"))
    [void][System.IO.Directory]::CreateDirectory($stateRoot)
    try {
        $extractionRoot = Join-Path $stateRoot "extracted"
        Invoke-MsiAdminExtraction $PackagePath $extractionRoot $stateRoot
        Test-MsiRuntimePayload $extractionRoot $Contract
    }
    finally {
        Remove-Item -LiteralPath $stateRoot -Recurse -Force
    }
}

function Invoke-MsiValidation {
    $resolvedMsi = (Resolve-Path -LiteralPath $MsiPath).Path
    $installer = $null
    $database = $null
    $summary = $null
    try {
        $installer = New-Object -ComObject WindowsInstaller.Installer
        $database = Invoke-ComMember $installer "OpenDatabase" ([System.Reflection.BindingFlags]::InvokeMethod) @($resolvedMsi, 0)
        $properties = Convert-RowsToMap (Read-MsiRows $database 'SELECT `Property`,`Value` FROM `Property`' 2)
        $summary = Invoke-ComMember $installer "SummaryInformation" ([System.Reflection.BindingFlags]::GetProperty) @($resolvedMsi, 0)
        $template = [string](Invoke-ComMember $summary "Property" ([System.Reflection.BindingFlags]::GetProperty) @(7))
        $components = Convert-RowsToMap (Read-MsiRows $database 'SELECT `Component`,`Attributes` FROM `Component`' 2)
        $features = Convert-RowsToMap (Read-MsiRows $database 'SELECT `Feature`,`Title` FROM `Feature`' 2)
        $featureComponentRows = Read-MsiRows $database 'SELECT `Feature_`,`Component_` FROM `FeatureComponents`' 2
        $featureComponents = @(
            $featureComponentRows |
                Where-Object { [string]$_.Values[0] -ceq $ExpectedFeature } |
                ForEach-Object { [string]$_.Values[1] }
        )
        $numericVersion = Test-MsiMetadata $properties $template $components $features $featureComponents $ExpectedVersion
        $runtimeContract = Get-MsiRuntimeContract
        $files = @(Read-MsiRows $database 'SELECT `File`,`Component_`,`FileName`,`FileSize` FROM `File`' 4)
        $runtimeComponents = @(Read-MsiRows $database 'SELECT `Component`,`Directory_`,`KeyPath` FROM `Component`' 3)
        $directories = @(Read-MsiRows $database 'SELECT `Directory`,`Directory_Parent`,`DefaultDir` FROM `Directory`' 3)
        Test-MsiRuntimeMetadata $files $runtimeComponents $directories $runtimeContract
    }
    finally {
        foreach ($comObject in @($summary, $database, $installer)) {
            if ($null -ne $comObject -and [System.Runtime.InteropServices.Marshal]::IsComObject($comObject)) {
                [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($comObject)
            }
        }
    }
    Test-MsiRuntimePackage $resolvedMsi $runtimeContract
    Write-Output "Validated MSI ${resolvedMsi}: version=$numericVersion template=$template components=$($ExpectedComponents.Count)"
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-MsiValidation
}
