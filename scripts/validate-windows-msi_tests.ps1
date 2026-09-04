$ErrorActionPreference = "Stop"

. "$PSScriptRoot\validate-windows-msi.ps1" -MsiPath "unused" -ExpectedVersion "0.0.0"

function New-ValidMetadata {
    $properties = @{
        ProductVersion = "1.2.8"
        UpgradeCode = "{B9F8C0F4-1F4F-4F2E-AE3C-7A9D7D6E3F11}"
        ProductCode = "{25C28633-1F4F-4F36-8FA8-FDFD53A63D1D}"
    }
    $components = @{}
    foreach ($component in $ExpectedComponents) {
        $components[$component] = 256
    }
    @{
        Properties = $properties
        Template = "x64;1033"
        Components = $components
        Features = @{ Binaries = "SonicTerm" }
        FeatureComponents = [string[]]$ExpectedComponents.Clone()
    }
}

function Copy-Metadata {
    param([hashtable]$Source)

    @{
        Properties = $Source.Properties.Clone()
        Template = $Source.Template
        Components = $Source.Components.Clone()
        Features = $Source.Features.Clone()
        FeatureComponents = [string[]]$Source.FeatureComponents.Clone()
    }
}

function Assert-ThrowsLike {
    param(
        [scriptblock]$Action,
        [string]$Pattern
    )

    try {
        & $Action
    }
    catch {
        if ($_.Exception.Message -notlike $Pattern) {
            throw "Expected error like '$Pattern', got '$($_.Exception.Message)'"
        }
        return
    }
    throw "Expected error like '$Pattern', but the action succeeded"
}

function Test-WixSourceContract {
    param([xml]$Wix)

    $product = @($Wix.SelectNodes("//*[local-name()='Product']"))
    if ($product.Count -ne 1 -or [guid]$product[0].GetAttribute("UpgradeCode") -ne [guid]$ExpectedUpgradeCode) {
        throw "Validator UpgradeCode drifted from main.wxs"
    }
    if ($product[0].GetAttribute("Version") -cne '$(var.Version)') {
        throw 'main.wxs Product/@Version must bind $(var.Version)'
    }
    $features = @(
        $Wix.SelectNodes("//*[local-name()='Feature']") |
            Where-Object { $_.GetAttribute("Id") -ceq $ExpectedFeature }
    )
    if ($features.Count -ne 1) {
        throw "main.wxs must contain exactly one feature '$ExpectedFeature'"
    }
    $expectedSorted = @($ExpectedComponents | Sort-Object)
    $componentNodes = @($Wix.SelectNodes("//*[local-name()='Component']"))
    $sourceComponents = @(
        $componentNodes |
            ForEach-Object { $_.GetAttribute("Id") } |
            Sort-Object
    )
    if (Compare-Object $expectedSorted $sourceComponents -CaseSensitive) {
        throw "main.wxs component set differs from the validator contract"
    }
    $sourceFeatureComponents = @(
        $features[0].SelectNodes(".//*[local-name()='ComponentRef']") |
            ForEach-Object { $_.GetAttribute("Id") } |
            Sort-Object
    )
    if (Compare-Object $expectedSorted $sourceFeatureComponents -CaseSensitive) {
        throw "main.wxs Binaries references differ from the validator contract"
    }
    foreach ($component in $ExpectedComponents) {
        $node = $componentNodes | Where-Object { $_.GetAttribute("Id") -ceq $component }
        if (@($node).Count -ne 1 -or $node.GetAttribute("Win64") -cne "yes") {
            throw "main.wxs is missing 64-bit component '$component'"
        }
    }
}

[xml]$wix = Get-Content -LiteralPath "$PSScriptRoot\..\crates\sonicterm-windows\wix\main.wxs" -Raw
Test-WixSourceContract $wix

[xml]$hardcodedVersionWix = $wix.OuterXml
$hardcodedVersionProduct = $hardcodedVersionWix.SelectSingleNode("//*[local-name()='Product']")
$hardcodedVersionProduct.SetAttribute("Version", "1.2.3")
Assert-ThrowsLike {
    Test-WixSourceContract $hardcodedVersionWix
} "*Product/@Version must bind*"

$valid = New-ValidMetadata
$result = Test-MsiMetadata `
    $valid.Properties `
    $valid.Template `
    $valid.Components `
    $valid.Features `
    $valid.FeatureComponents `
    "v1.2.8-rc.1+build.9"
if ($result -ne "1.2.8") {
    throw "Expected numeric SemVer core 1.2.8, got '$result'"
}

foreach ($requiredProperty in @("ProductVersion", "UpgradeCode", "ProductCode")) {
    $missingProperty = Copy-Metadata $valid
    $missingProperty.Properties.Remove($requiredProperty)
    Assert-ThrowsLike {
        Test-MsiMetadata $missingProperty.Properties $missingProperty.Template $missingProperty.Components $missingProperty.Features $missingProperty.FeatureComponents "1.2.8"
    } "*missing nonempty $requiredProperty*"

    $wrongCaseProperty = Copy-Metadata $valid
    $propertyValue = $wrongCaseProperty.Properties[$requiredProperty]
    $wrongCaseProperty.Properties.Remove($requiredProperty)
    $wrongCaseProperty.Properties[$requiredProperty.ToLowerInvariant()] = $propertyValue
    Assert-ThrowsLike {
        Test-MsiMetadata $wrongCaseProperty.Properties $wrongCaseProperty.Template $wrongCaseProperty.Components $wrongCaseProperty.Features $wrongCaseProperty.FeatureComponents "1.2.8"
    } "*missing nonempty $requiredProperty*"
}

Assert-ThrowsLike {
    Test-MsiMetadata $valid.Properties $valid.Template $valid.Components $valid.Features $valid.FeatureComponents "v1.2.9"
} "*ProductVersion*does not match*"
if ((Get-NumericSemVerCore "v255.255.65535") -ne "255.255.65535") {
    throw "Windows Installer maximum ProductVersion was rejected"
}
foreach ($validVersion in @("1.2.8-0", "1.2.8-alpha.0", "1.2.8+01")) {
    if ((Get-NumericSemVerCore $validVersion) -ne "1.2.8") {
        throw "Valid semantic version '$validVersion' was rejected"
    }
}
foreach ($overflowVersion in @("v256.0.0", "v0.256.0", "v0.0.65536")) {
    Assert-ThrowsLike {
        Get-NumericSemVerCore $overflowVersion
    } "*exceeds Windows Installer bounds*"
}
foreach ($invalidVersion in @(
    "1.02.8",
    "1.2.8-",
    "1.2.8+",
    "1.2.8-rc..1",
    "1.2.8-01",
    "1.2.8-alpha.01"
)) {
    Assert-ThrowsLike {
        Test-MsiMetadata $valid.Properties $valid.Template $valid.Components $valid.Features $valid.FeatureComponents $invalidVersion
    } "*not a supported semantic version*"
}

$badUpgrade = Copy-Metadata $valid
$badUpgrade.Properties.UpgradeCode = "{11111111-1111-1111-1111-111111111111}"
Assert-ThrowsLike {
    Test-MsiMetadata $badUpgrade.Properties $badUpgrade.Template $badUpgrade.Components $badUpgrade.Features $badUpgrade.FeatureComponents "1.2.8"
} "*UpgradeCode*does not match*"

foreach ($invalidProductCode in @(
    "not-a-guid",
    "{25C28633-1F4F-4F36-8FA8-FDFD53A63D1D",
    "{{25C28633-1F4F-4F36-8FA8-FDFD53A63D1D}}",
    "{00000000-0000-0000-0000-000000000000}"
)) {
    $badProduct = Copy-Metadata $valid
    $badProduct.Properties.ProductCode = $invalidProductCode
    Assert-ThrowsLike {
        Test-MsiMetadata $badProduct.Properties $badProduct.Template $badProduct.Components $badProduct.Features $badProduct.FeatureComponents "1.2.8"
    } "*ProductCode*not a nonempty GUID*"
}

foreach ($invalidTemplate in @("Intel;1033", "X64;1033")) {
    $badTemplate = Copy-Metadata $valid
    $badTemplate.Template = $invalidTemplate
    Assert-ThrowsLike {
        Test-MsiMetadata $badTemplate.Properties $badTemplate.Template $badTemplate.Components $badTemplate.Features $badTemplate.FeatureComponents "1.2.8"
    } "*summary template*does not match*"
}

$wrongCaseComponent = Copy-Metadata $valid
$wrongCaseComponent.Components.Remove("asset_fonts")
$wrongCaseComponent.Components.ASSET_FONTS = 256
Assert-ThrowsLike {
    Test-MsiMetadata $wrongCaseComponent.Properties $wrongCaseComponent.Template $wrongCaseComponent.Components $wrongCaseComponent.Features $wrongCaseComponent.FeatureComponents "1.2.8"
} "*Component table differs*"

$missingComponent = Copy-Metadata $valid
$missingComponent.Components.Remove("asset_fonts")
Assert-ThrowsLike {
    Test-MsiMetadata $missingComponent.Properties $missingComponent.Template $missingComponent.Components $missingComponent.Features $missingComponent.FeatureComponents "1.2.8"
} "*Component table differs*"

$extraComponent = Copy-Metadata $valid
$extraComponent.Components.unexpected = 256
Assert-ThrowsLike {
    Test-MsiMetadata $extraComponent.Properties $extraComponent.Template $extraComponent.Components $extraComponent.Features $extraComponent.FeatureComponents "1.2.8"
} "*Component table differs*"

$not64Bit = Copy-Metadata $valid
$not64Bit.Components.asset_fonts = 0
Assert-ThrowsLike {
    Test-MsiMetadata $not64Bit.Properties $not64Bit.Template $not64Bit.Components $not64Bit.Features $not64Bit.FeatureComponents "1.2.8"
} "*asset_fonts*not marked 64-bit*"

$missingFeature = Copy-Metadata $valid
$missingFeature.Features.Remove("Binaries")
Assert-ThrowsLike {
    Test-MsiMetadata $missingFeature.Properties $missingFeature.Template $missingFeature.Components $missingFeature.Features $missingFeature.FeatureComponents "1.2.8"
} "*Feature table is missing*"

$wrongCaseFeature = Copy-Metadata $valid
$wrongCaseFeature.Features.Remove("Binaries")
$wrongCaseFeature.Features.BINARIES = "SonicTerm"
Assert-ThrowsLike {
    Test-MsiMetadata $wrongCaseFeature.Properties $wrongCaseFeature.Template $wrongCaseFeature.Components $wrongCaseFeature.Features $wrongCaseFeature.FeatureComponents "1.2.8"
} "*Feature table is missing*"

$wrongCaseReference = Copy-Metadata $valid
$wrongCaseReference.FeatureComponents = @(
    $wrongCaseReference.FeatureComponents |
        ForEach-Object { if ($_ -ceq "asset_fonts") { "ASSET_FONTS" } else { $_ } }
)
Assert-ThrowsLike {
    Test-MsiMetadata $wrongCaseReference.Properties $wrongCaseReference.Template $wrongCaseReference.Components $wrongCaseReference.Features $wrongCaseReference.FeatureComponents "1.2.8"
} "*does not reference the expected components*"

$missingReference = Copy-Metadata $valid
$missingReference.FeatureComponents = @($missingReference.FeatureComponents | Where-Object { $_ -ne "asset_fonts" })
Assert-ThrowsLike {
    Test-MsiMetadata $missingReference.Properties $missingReference.Template $missingReference.Components $missingReference.Features $missingReference.FeatureComponents "1.2.8"
} "*does not reference the expected components*"

$extraReference = Copy-Metadata $valid
$extraReference.FeatureComponents += "unexpected"
Assert-ThrowsLike {
    Test-MsiMetadata $extraReference.Properties $extraReference.Template $extraReference.Components $extraReference.Features $extraReference.FeatureComponents "1.2.8"
} "*does not reference the expected components*"

Write-Output "validate-windows-msi tests: ok"
