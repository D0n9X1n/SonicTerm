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
        Write-Output "Validated MSI ${resolvedMsi}: version=$numericVersion template=$template components=$($ExpectedComponents.Count)"
    }
    finally {
        foreach ($comObject in @($summary, $database, $installer)) {
            if ($null -ne $comObject -and [System.Runtime.InteropServices.Marshal]::IsComObject($comObject)) {
                [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($comObject)
            }
        }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-MsiValidation
}
