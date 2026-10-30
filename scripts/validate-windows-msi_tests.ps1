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
    # The pinned transport, host, and attribution install together with the DLL as their sole key path.
    $runtime = @($Wix.SelectNodes("//*[local-name()='Component' and @Id='conpty_runtime']"))
    if ($runtime.Count -ne 1 -or
        $runtime[0].ParentNode.GetAttribute("Id") -cne "ConptyDir" -or
        $runtime[0].ParentNode.GetAttribute("Name") -cne "conpty" -or
        $runtime[0].ParentNode.ParentNode.GetAttribute("Id") -cne "APPLICATIONFOLDER") {
        throw "main.wxs must install conpty_runtime in APPLICATIONFOLDER/conpty"
    }
    if ($runtime[0].GetAttribute("Guid") -cne "E4352FB5-77F4-4B4C-8451-07E3BE3B83E9") {
        throw "main.wxs runtime component GUID differs from the contract"
    }
    $runtimeFiles = @{
        conpty_dll = @("conpty.dll", '..\..\target\x86_64-pc-windows-msvc\release\conpty\conpty.dll', "yes")
        conpty_host = @("OpenConsole.exe", '..\..\target\x86_64-pc-windows-msvc\release\conpty\OpenConsole.exe', "")
        conpty_license = @("LICENSE", '..\..\target\x86_64-pc-windows-msvc\release\conpty\LICENSE', "")
        conpty_notice = @("NOTICE.md", '..\..\scripts\NOTICE-conpty.md', "")
    }
    $fileNodes = @($runtime[0].SelectNodes("*[local-name()='File']"))
    if ($fileNodes.Count -ne $runtimeFiles.Count) {
        throw "main.wxs runtime file set differs from the contract"
    }
    foreach ($fileId in $runtimeFiles.Keys) {
        $file = @($fileNodes | Where-Object { $_.GetAttribute("Id") -ceq $fileId })
        $expected = $runtimeFiles[$fileId]
        if ($file.Count -ne 1 -or $file[0].GetAttribute("Name") -cne $expected[0] -or
            $file[0].GetAttribute("Source") -cne $expected[1] -or
            $file[0].GetAttribute("KeyPath") -cne $expected[2]) {
            throw "main.wxs runtime file '$fileId' differs from the contract"
        }
    }
    # Statically linked winit attribution follows the executable without changing its component or key path.
    $license = @($Wix.SelectNodes("//*[local-name()='File' and @Id='license_winit']"))
    if ($license.Count -ne 1 -or
        $license[0].GetAttribute("Name") -cne "LICENSE-winit-Apache-2.0" -or
        $license[0].GetAttribute("Source") -cne '..\..\crates\sonicterm-winit\LICENSE' -or
        $license[0].ParentNode.GetAttribute("Id") -cne "binary0" -or
        $license[0].GetAttribute("KeyPath") -ceq "yes") {
        throw "main.wxs must include the winit license in binary0 without replacing its key path"
    }
}

[xml]$wix = Get-Content -LiteralPath "$PSScriptRoot\..\crates\sonicterm-windows\wix\main.wxs" -Raw
Test-WixSourceContract $wix

# Missing, renamed, redirected, or key-path license entries cannot silently pass source validation.
[xml]$missingLicenseWix = $wix.OuterXml
$missingLicense = $missingLicenseWix.SelectSingleNode("//*[local-name()='File' and @Id='license_winit']")
[void]$missingLicense.ParentNode.RemoveChild($missingLicense)
Assert-ThrowsLike { Test-WixSourceContract $missingLicenseWix } "*must include the winit license in binary0*"
foreach ($attribute in @("Name", "Source", "KeyPath")) {
    [xml]$changedLicenseWix = $wix.OuterXml
    $changedLicense = $changedLicenseWix.SelectSingleNode("//*[local-name()='File' and @Id='license_winit']")
    $changedLicense.SetAttribute($attribute, $(if ($attribute -eq "KeyPath") { "yes" } else { "unexpected" }))
    Assert-ThrowsLike { Test-WixSourceContract $changedLicenseWix } "*must include the winit license in binary0*"
}

[xml]$movedLicenseWix = $wix.OuterXml
$movedLicense = $movedLicenseWix.SelectSingleNode("//*[local-name()='File' and @Id='license_winit']")
$otherComponent = $movedLicenseWix.SelectSingleNode("//*[local-name()='Component' and @Id='asset_themes']")
[void]$otherComponent.AppendChild($movedLicense)
Assert-ThrowsLike { Test-WixSourceContract $movedLicenseWix } "*must include the winit license in binary0*"

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

# MSI rows must bind the runtime files to the intended directory, component, sizes, and DLL key path.
$runtimeContract = @{
    conpty_dll = @{ Name = "conpty.dll"; Size = 119608; Sha256 = "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a" }
    conpty_host = @{ Name = "OpenConsole.exe"; Size = 1089376; Sha256 = "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586" }
    conpty_license = @{ Name = "LICENSE"; Size = 1116; Sha256 = "5d177f23ecfeb0ea8e050b6a5a16355e1ae9a0b286436ca8f83ed08b3795be6b" }
    conpty_notice = @{ Name = "NOTICE.md"; Size = 99; Sha256 = "a" * 64 }
}
function New-RuntimeRows {
    @{
        Files = @($runtimeContract.Keys | ForEach-Object {
            [pscustomobject]@{ Values = @($_, "conpty_runtime", $runtimeContract[$_].Name, [string]$runtimeContract[$_].Size) }
        })
        Components = @([pscustomobject]@{ Values = @("conpty_runtime", "ConptyDir", "conpty_dll") })
        Directories = @(
            [pscustomobject]@{ Values = @("TARGETDIR", "", "SourceDir") }
            [pscustomobject]@{ Values = @("ProgramFiles64Folder", "TARGETDIR", "PFiles") }
            [pscustomobject]@{ Values = @("APPLICATIONFOLDER", "ProgramFiles64Folder", "SonicTerm") }
            [pscustomobject]@{ Values = @("ConptyDir", "APPLICATIONFOLDER", "conpty") }
        )
    }
}
$runtimeRows = New-RuntimeRows
Test-MsiRuntimeMetadata $runtimeRows.Files $runtimeRows.Components $runtimeRows.Directories $runtimeContract
foreach ($fileId in $runtimeContract.Keys) {
    foreach ($field in @(1, 2, 3)) {
        $changedRows = New-RuntimeRows
        $changedFile = $changedRows.Files | Where-Object { $_.Values[0] -ceq $fileId }
        $changedFile.Values[$field] = $(if ($field -eq 3) { "1" } else { "unexpected" })
        Assert-ThrowsLike {
            Test-MsiRuntimeMetadata $changedRows.Files $changedRows.Components $changedRows.Directories $runtimeContract
        } "*runtime file '$fileId'*"
    }
    $missingRows = New-RuntimeRows
    $missingRows.Files = @($missingRows.Files | Where-Object { $_.Values[0] -cne $fileId })
    Assert-ThrowsLike {
        Test-MsiRuntimeMetadata $missingRows.Files $missingRows.Components $missingRows.Directories $runtimeContract
    } "*runtime file set*"
}
foreach ($field in @(1, 2)) {
    $changedRows = New-RuntimeRows
    $changedRows.Components[0].Values[$field] = "unexpected"
    Assert-ThrowsLike {
        Test-MsiRuntimeMetadata $changedRows.Files $changedRows.Components $changedRows.Directories $runtimeContract
    } "*runtime component*"
}
foreach ($directoryId in @("TARGETDIR", "ProgramFiles64Folder", "APPLICATIONFOLDER", "ConptyDir")) {
    foreach ($field in @(1, 2)) {
        $changedRows = New-RuntimeRows
        $changedDirectory = $changedRows.Directories | Where-Object { $_.Values[0] -ceq $directoryId }
        $changedDirectory.Values[$field] = "unexpected"
        Assert-ThrowsLike {
            Test-MsiRuntimeMetadata $changedRows.Files $changedRows.Components $changedRows.Directories $runtimeContract
        } "*runtime directory '$directoryId'*"
    }
}
$extraRows = New-RuntimeRows
$extraRows.Files += [pscustomobject]@{ Values = @("unexpected", "conpty_runtime", "extra.dll", "1") }
Assert-ThrowsLike {
    Test-MsiRuntimeMetadata $extraRows.Files $extraRows.Components $extraRows.Directories $runtimeContract
} "*runtime file set*"
$shortNameRows = New-RuntimeRows
foreach ($file in $shortNameRows.Files) { $file.Values[2] = "SHORT|$($file.Values[2])" }
foreach ($directory in $shortNameRows.Directories) {
    $directory.Values[2] = "SHORT|$($directory.Values[2]):SOURCE|Source"
}
Test-MsiRuntimeMetadata $shortNameRows.Files $shortNameRows.Components $shortNameRows.Directories $runtimeContract

# Source mutations cannot redirect, rename, omit, or split the paired runtime and attribution.
foreach ($fileId in $runtimeContract.Keys) {
    foreach ($attribute in @("Name", "Source", "KeyPath")) {
        [xml]$changedRuntimeWix = $wix.OuterXml
        $changedFile = $changedRuntimeWix.SelectSingleNode("//*[local-name()='File' and @Id='$fileId']")
        $changedFile.SetAttribute($attribute, "unexpected")
        Assert-ThrowsLike { Test-WixSourceContract $changedRuntimeWix } "*runtime file '$fileId'*"
    }
    [xml]$missingRuntimeWix = $wix.OuterXml
    $missingFile = $missingRuntimeWix.SelectSingleNode("//*[local-name()='File' and @Id='$fileId']")
    [void]$missingFile.ParentNode.RemoveChild($missingFile)
    Assert-ThrowsLike { Test-WixSourceContract $missingRuntimeWix } "*runtime file set*"
}
[xml]$changedRuntimeGuidWix = $wix.OuterXml
$changedRuntimeGuidWix.SelectSingleNode("//*[local-name()='Component' and @Id='conpty_runtime']").SetAttribute("Guid", "unexpected")
Assert-ThrowsLike { Test-WixSourceContract $changedRuntimeGuidWix } "*runtime component GUID*"
foreach ($attribute in @("Id", "Name")) {
    [xml]$changedRuntimeWix = $wix.OuterXml
    $runtimeDirectory = $changedRuntimeWix.SelectSingleNode("//*[local-name()='Directory' and @Id='ConptyDir']")
    $runtimeDirectory.SetAttribute($attribute, "unexpected")
    Assert-ThrowsLike { Test-WixSourceContract $changedRuntimeWix } "*install conpty_runtime in APPLICATIONFOLDER/conpty*"
}

# Payload checks read extracted bytes, so a correct File table cannot conceal changed cabinet contents.
$payloadRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("sonicterm-msi-payload-" + [guid]::NewGuid().ToString("N"))
[void][System.IO.Directory]::CreateDirectory($payloadRoot)
try {
    $extractedRuntime = Join-Path $payloadRoot "PFiles/SonicTerm/conpty"
    [void][System.IO.Directory]::CreateDirectory($extractedRuntime)
    $payloadContract = @{}
    foreach ($fileId in $runtimeContract.Keys) {
        $filePath = Join-Path $extractedRuntime $runtimeContract[$fileId].Name
        [System.IO.File]::WriteAllText($filePath, "original-$fileId")
        $payloadContract[$fileId] = @{
            Name = $runtimeContract[$fileId].Name
            Size = (Get-Item -LiteralPath $filePath).Length
            Sha256 = (Get-FileHash -LiteralPath $filePath -Algorithm SHA256).Hash
        }
    }
    Test-MsiRuntimePayload $payloadRoot $payloadContract
    $extraPayload = Join-Path $extractedRuntime "unexpected.dll"
    [System.IO.File]::WriteAllText($extraPayload, "unreviewed")
    Assert-ThrowsLike { Test-MsiRuntimePayload $payloadRoot $payloadContract } "*runtime payload file set*"
    Remove-Item -LiteralPath $extraPayload
    foreach ($fileId in $payloadContract.Keys) {
        $filePath = Join-Path $extractedRuntime $payloadContract[$fileId].Name
        [System.IO.File]::WriteAllText($filePath, "tampered-$fileId")
        Assert-ThrowsLike { Test-MsiRuntimePayload $payloadRoot $payloadContract } "*runtime payload*SHA256*"
        [System.IO.File]::WriteAllText($filePath, "original-$fileId-extra")
        Assert-ThrowsLike { Test-MsiRuntimePayload $payloadRoot $payloadContract } "*runtime payload*size*"
        Remove-Item -LiteralPath $filePath
        Assert-ThrowsLike { Test-MsiRuntimePayload $payloadRoot $payloadContract } "*runtime payload*missing*"
        [System.IO.File]::WriteAllText($filePath, "original-$fileId")
    }
}
finally {
    Remove-Item -LiteralPath $payloadRoot -Recurse -Force
}

# The real process adapter must reject nonzero exits, deadlines, and surviving descendants without running an MSI.
$processRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("sonicterm-msi-process-" + [guid]::NewGuid().ToString("N"))
[void][System.IO.Directory]::CreateDirectory($processRoot)
try {
    $pythonPath = (Get-Command python -CommandType Application | Select-Object -First 1).Source
    Invoke-MsiExtractionCommand @($pythonPath, "-I", "-c", "print('bounded child')") $processRoot 10
    Assert-ThrowsLike {
        Invoke-MsiExtractionCommand @($pythonPath, "-I", "-c", "raise SystemExit(7)") $processRoot 10
    } "*MSI administrative extraction failed*"
    Assert-ThrowsLike {
        Invoke-MsiExtractionCommand @($pythonPath, "-I", "-c", "import time; time.sleep(30)") $processRoot 1
    } "*MSI administrative extraction failed*"
    Assert-ThrowsLike {
        Invoke-MsiExtractionCommand @($pythonPath, "-I", "-c", "import subprocess,sys; subprocess.Popen([sys.executable, '-I', '-c', 'import time; time.sleep(30)'])") $processRoot 10
    } "*MSI administrative extraction failed*"
}
finally {
    Remove-Item -LiteralPath $processRoot -Recurse -Force
}

# Administrative extraction is quiet, bounded, and directed to scratch rather than an application install.
$originalExtractionCommand = ${function:Invoke-MsiExtractionCommand}
try {
    function Invoke-MsiExtractionCommand {
        param([string[]]$Command, [string]$StateRoot, [int]$TimeoutSeconds)
        $expectedCommand = @(
            (Join-Path $env:SystemRoot "System32/msiexec.exe"), "/a", "C:\fixture with space.msi", "/qn", "/norestart",
            "TARGETDIR=C:\scratch with space\extracted", "/L*v", "C:\scratch with space\admin.log"
        )
        if (($Command -join "`n") -cne ($expectedCommand -join "`n") -or
            $StateRoot -cne "C:\scratch with space" -or $TimeoutSeconds -ne 120) {
            throw "Administrative extraction command contract changed"
        }
    }
    Invoke-MsiAdminExtraction "C:\fixture with space.msi" "C:\scratch with space\extracted" "C:\scratch with space"
}
finally {
    ${function:Invoke-MsiExtractionCommand} = $originalExtractionCommand
}

# The extraction boundary must verify the returned bytes and remove only its own temporary tree on either result.
$script:extractionMode = "valid"
$script:extractionRoot = $null
function Invoke-MsiAdminExtraction {
    param([string]$PackagePath, [string]$ExtractionRoot, [string]$StateRoot)
    if ($PackagePath -cne "synthetic.msi" -or -not (Test-Path -LiteralPath $StateRoot -PathType Container)) {
        throw "Unexpected extraction inputs"
    }
    $script:extractionRoot = $StateRoot
    if ($script:extractionMode -eq "failed") { throw "Synthetic extraction failure" }
    $runtimePath = Join-Path $ExtractionRoot "PFiles/SonicTerm/conpty"
    [void][System.IO.Directory]::CreateDirectory($runtimePath)
    foreach ($fileId in $payloadContract.Keys) {
        [System.IO.File]::WriteAllText((Join-Path $runtimePath $payloadContract[$fileId].Name), "original-$fileId")
    }
    if ($script:extractionMode -eq "tampered") {
        [System.IO.File]::WriteAllText((Join-Path $runtimePath "conpty.dll"), "tampered-conpty_dll")
    }
}
Test-MsiRuntimePackage "synthetic.msi" $payloadContract
if (Test-Path -LiteralPath $script:extractionRoot) { throw "Successful extraction left temporary files" }
$script:extractionMode = "tampered"
Assert-ThrowsLike { Test-MsiRuntimePackage "synthetic.msi" $payloadContract } "*runtime payload*SHA256*"
if (Test-Path -LiteralPath $script:extractionRoot) { throw "Invalid payload left temporary files" }
$script:extractionMode = "failed"
Assert-ThrowsLike { Test-MsiRuntimePackage "synthetic.msi" $payloadContract } "*Synthetic extraction failure*"
if (Test-Path -LiteralPath $script:extractionRoot) { throw "Failed extraction left temporary files" }

# Trusted source pins, rather than cabinet metadata or staged build files, supply every payload digest.
$trustedContract = Get-MsiRuntimeContract
foreach ($fileId in @("conpty_dll", "conpty_host", "conpty_license")) {
    foreach ($field in @("Name", "Size", "Sha256")) {
        if ($trustedContract[$fileId][$field] -cne $runtimeContract[$fileId][$field]) {
            throw "Trusted runtime contract differs at '$fileId/$field'"
        }
    }
}
$noticePath = Join-Path $PSScriptRoot "NOTICE-conpty.md"
if ($trustedContract.conpty_notice.Name -cne "NOTICE.md" -or
    $trustedContract.conpty_notice.Size -ne (Get-Item -LiteralPath $noticePath).Length -or
    $trustedContract.conpty_notice.Sha256 -ine (Get-FileHash -LiteralPath $noticePath -Algorithm SHA256).Hash) {
    throw "Runtime NOTICE does not match its reviewed source"
}

# A changed source NOTICE must not redefine the trusted digest of the packaged attribution.
try {
    function Get-Item { throw "NOTICE pin must not come from current source metadata" }
    function Get-FileHash { throw "NOTICE pin must not come from current source bytes" }
    $fixedNoticeContract = Get-MsiRuntimeContract
    if ($fixedNoticeContract.conpty_notice.Size -ne 23176 -or
        $fixedNoticeContract.conpty_notice.Sha256 -cne "e7fbaadee6ab20c28b87730a510ee5f5815d8fb4bd88d1d54d282dc2a74c0726") {
        throw "NOTICE pin differs from the reviewed upstream bytes"
    }
}
finally {
    Remove-Item Function:\Get-Item
    Remove-Item Function:\Get-FileHash
}

# The public validator must require runtime metadata and payload validation before reporting success.
$validationRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("sonicterm-msi-validation-" + [guid]::NewGuid().ToString("N"))
[void][System.IO.Directory]::CreateDirectory($validationRoot)
try {
    $MsiPath = Join-Path $validationRoot "synthetic.msi"
    [System.IO.File]::WriteAllText($MsiPath, "synthetic database")
    $ExpectedVersion = "1.2.8"
    $script:validatedRuntimePackage = $false
    $script:validationRows = New-RuntimeRows
    function New-Object { param([string]$ComObject) "synthetic installer" }
    function Invoke-ComMember {
        param($ComObject, $Name, $Flags, $Arguments)
        switch ($Name) {
            "OpenDatabase" { "synthetic database" }
            "SummaryInformation" { "synthetic summary" }
            "Property" { "x64;1033" }
            default { throw "Unexpected COM member '$Name'" }
        }
    }
    function Read-MsiRows {
        param($Database, $Query, $FieldCount)
        switch -Exact ($Query) {
            'SELECT `Property`,`Value` FROM `Property`' {
                foreach ($name in $valid.Properties.Keys) { [pscustomobject]@{ Values = @($name, $valid.Properties[$name]) } }
            }
            'SELECT `Component`,`Attributes` FROM `Component`' {
                foreach ($name in $valid.Components.Keys) { [pscustomobject]@{ Values = @($name, $valid.Components[$name]) } }
            }
            'SELECT `Feature`,`Title` FROM `Feature`' { [pscustomobject]@{ Values = @("Binaries", "SonicTerm") } }
            'SELECT `Feature_`,`Component_` FROM `FeatureComponents`' {
                foreach ($name in $valid.FeatureComponents) { [pscustomobject]@{ Values = @("Binaries", $name) } }
            }
            'SELECT `File`,`Component_`,`FileName`,`FileSize` FROM `File`' { $script:validationRows.Files }
            'SELECT `Component`,`Directory_`,`KeyPath` FROM `Component`' { $script:validationRows.Components }
            'SELECT `Directory`,`Directory_Parent`,`DefaultDir` FROM `Directory`' { $script:validationRows.Directories }
            default { throw "Unexpected MSI query '$Query'" }
        }
    }
    function Get-MsiRuntimeContract { $runtimeContract }
    function Test-MsiRuntimePackage {
        param([string]$PackagePath, [hashtable]$Contract)
        if ($PackagePath -cne $MsiPath -or $Contract -ne $runtimeContract) { throw "Wrong runtime validation inputs" }
        $script:validatedRuntimePackage = $true
    }
    $validationOutput = Invoke-MsiValidation
    if (-not $script:validatedRuntimePackage) { throw "Validator skipped actual runtime payload verification" }
    if ($validationOutput -notlike "Validated MSI *components=11*") { throw "Unexpected validator output" }
    $script:validatedRuntimePackage = $false
    $script:validationRows.Files = @()
    Assert-ThrowsLike { Invoke-MsiValidation } "*runtime file set*"
    if ($script:validatedRuntimePackage) { throw "Invalid MSI metadata reached extraction" }
}
finally {
    Remove-Item -LiteralPath $validationRoot -Recurse -Force
}

Write-Output "validate-windows-msi tests: ok"
