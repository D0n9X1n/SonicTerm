<#
.SYNOPSIS
Measurement-only lab for Default terminal picker discovery (track 0b).

.DESCRIPTION
Builds a loose-file package layout that declares the console-host and
terminal-host app extension pair under a SonicTerm test identity, registers it
with Developer Mode (`Add-AppxPackage -Register`, no signing), lists what the
picker's catalog query returns, and removes it again. The terminal side is the
`defterm_handoff_probe` example serving its own test CLSID.

.PARAMETER Action
install registers the layout; query lists both extension catalogs and the
pairs the picker would offer; remove unregisters the package.

.PARAMETER Console
Own declares a test console CLSID served by an OpenConsole.exe built from
microsoft/terminal with that CLSID (from -OpenConsoleDir), plus its own
proxy-stub registration; Wt declares Windows Terminal's console CLSID without
serving it.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('install', 'query', 'remove')]
    [string]$Action,
    [ValidateSet('Own', 'Wt')]
    [string]$Console = 'Own',
    [string]$ProbeExe,
    [string]$OpenConsoleDir,
    [switch]$NoProxyStub,
    [string]$ProxyDll,
    [switch]$Sparse,
    [string]$CertThumbprint,
    [string]$Publisher = 'CN=SonicTerm Lab',
    [string]$WorkDir = (Join-Path $env:TEMP 'defterm-1655\package')
)

$ErrorActionPreference = 'Stop'
$packageName = 'SonicTerm.DefTermLab'
$ownConsoleClsid = 'B7E2C4A9-3F1D-4C8E-A5B6-9D0E1F2A3B4C'
$proxyStubClsid = 'D4A7E2B1-9C3F-4E8D-B6A5-0F1E2D3C4B5A'
$wtConsoleClsid = '2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69'
$terminalClsid = 'C5F1A8D2-7E3B-4A9C-8D6E-1B2C3D4E5F60'

function Install-LabPackage {
    if (-not $ProbeExe) { throw '-ProbeExe is required for install' }
    $layout = Join-Path $WorkDir 'layout'
    if (Test-Path -LiteralPath $layout) { Remove-Item -LiteralPath $layout -Recurse -Force }
    New-Item -ItemType Directory -Force -Path "$layout\Assets" | Out-Null
    Copy-Item -LiteralPath $ProbeExe -Destination $layout
    $pngDir = Join-Path $PSScriptRoot '..\assets\icons\exports\png'
    Copy-Item -LiteralPath (Join-Path $pngDir 'sonic-48.png') -Destination "$layout\Assets\Square44x44Logo.png"
    Copy-Item -LiteralPath (Join-Path $pngDir 'sonic-256.png') -Destination "$layout\Assets\Square150x150Logo.png"
    Copy-Item -LiteralPath (Join-Path $pngDir 'sonic-128.png') -Destination "$layout\Assets\StoreLogo.png"

    $consoleClsid = $wtConsoleClsid
    $consoleServer = ''
    $proxyStub = ''
    if ($Console -eq 'Own') {
        # OpenConsole built from microsoft/terminal with __CLSID_CConsoleHandoff set to the
        # SonicTerm test CLSID; a copy of Windows Terminal's binary registers Terminal's CLSID.
        if (-not $OpenConsoleDir) { throw '-OpenConsoleDir is required for -Console Own' }
        $consoleClsid = $ownConsoleClsid
        Copy-Item -LiteralPath (Join-Path $OpenConsoleDir 'OpenConsole.exe') -Destination $layout
        Copy-Item -LiteralPath (Join-Path $OpenConsoleDir 'OpenConsoleProxy.dll') -Destination $layout
        # When: -ProxyDll substitutes another proxy DLL to separate the DLL from its registration.
        if ($ProxyDll) { Copy-Item -LiteralPath $ProxyDll -Destination (Join-Path $layout 'OpenConsoleProxy.dll') -Force }
        $consoleServer = @"
            <com:ExeServer DisplayName="SonicTerm lab console" Executable="OpenConsole.exe">
              <com:Class Id="$consoleClsid" />
            </com:ExeServer>
"@
        # When: the variant without a proxy-stub measures whether this package's proxy
        # registration is what breaks other packages' handoffs.
        if ($NoProxyStub) { $proxyStubClsid = '' }
        $proxyStub = if (-not $proxyStubClsid) { '' } else { @"
        <com:Extension Category="windows.comInterface">
          <com:ComInterface>
            <com:ProxyStub Id="$proxyStubClsid" DisplayName="SonicTerm lab handoff proxy" Path="OpenConsoleProxy.dll" />
            <com:Interface Id="E686C757-9A35-4A1C-B3CE-0BCC8B5C69F4" ProxyStubClsid="$proxyStubClsid" />
            <com:Interface Id="6F23DA90-15C5-4203-9DB0-64E73F1B1B00" ProxyStubClsid="$proxyStubClsid" />
            <com:Interface Id="746E6BC0-AB05-4E38-AB14-71E86763141F" ProxyStubClsid="$proxyStubClsid" />
          </com:ComInterface>
        </com:Extension>
"@ }
    }
    $logPath = Join-Path $WorkDir 'packaged-probe.log'
    # A sparse package carries only identity; its executables stay in the external layout
    # folder, which is how identity would attach to the existing MSI install.
    $sparseProperty = if ($Sparse) { '    <uap10:AllowExternalContent>true</uap10:AllowExternalContent>' } else { '' }
    # A win32App runtime behavior must not also declare an EntryPoint.
    $sparseApplication = if ($Sparse) { ' uap10:TrustLevel="mediumIL" uap10:RuntimeBehavior="win32App"' } else { ' EntryPoint="Windows.FullTrustApplication"' }
    $manifest = @"
<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:uap3="http://schemas.microsoft.com/appx/manifest/uap/windows10/3"
         xmlns:com="http://schemas.microsoft.com/appx/manifest/com/windows10"
         xmlns:desktop6="http://schemas.microsoft.com/appx/manifest/desktop/windows10/6"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         xmlns:uap10="http://schemas.microsoft.com/appx/manifest/uap/windows10/10"
         IgnorableNamespaces="uap uap3 com desktop6 rescap uap10">
  <Identity Name="$packageName" Publisher="$Publisher" Version="0.0.1.0" ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>SonicTerm (defterm lab)</DisplayName>
    <PublisherDisplayName>SonicTerm Lab</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
    <desktop6:FileSystemWriteVirtualization>disabled</desktop6:FileSystemWriteVirtualization>
$sparseProperty
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.19041.0" MaxVersionTested="10.0.26100.0" />
  </Dependencies>
  <Resources><Resource Language="en-us" /></Resources>
  <Applications>
    <Application Id="DefTermLab" Executable="defterm_handoff_probe.exe"$sparseApplication>
      <uap:VisualElements DisplayName="SonicTerm (defterm lab)" Description="Default terminal handoff lab"
                          BackgroundColor="transparent" Square150x150Logo="Assets\Square150x150Logo.png"
                          Square44x44Logo="Assets\Square44x44Logo.png" AppListEntry="none" />
      <Extensions>
        <uap3:Extension Category="windows.appExtension">
          <uap3:AppExtension Name="com.microsoft.windows.console.host" Id="SonicTermConsole" DisplayName="SonicTerm lab console" Description="SonicTerm defterm lab console host" PublicFolder="Public">
            <uap3:Properties><Clsid>{$consoleClsid}</Clsid></uap3:Properties>
          </uap3:AppExtension>
        </uap3:Extension>
        <uap3:Extension Category="windows.appExtension">
          <uap3:AppExtension Name="com.microsoft.windows.terminal.host" Id="SonicTermTerminal" DisplayName="SonicTerm lab terminal" Description="SonicTerm defterm lab terminal host" PublicFolder="Public">
            <uap3:Properties><Clsid>{$terminalClsid}</Clsid></uap3:Properties>
          </uap3:AppExtension>
        </uap3:Extension>
        <com:Extension Category="windows.comServer">
          <com:ComServer>
$consoleServer
            <com:ExeServer DisplayName="SonicTerm lab terminal" Executable="defterm_handoff_probe.exe" Arguments="--mode success --timeout-s 40 --clsid {$terminalClsid} --log &quot;$logPath&quot;">
              <com:Class Id="$terminalClsid" />
            </com:ExeServer>
          </com:ComServer>
        </com:Extension>
$proxyStub
      </Extensions>
    </Application>
  </Applications>
  <Capabilities>
    <rescap:Capability Name="runFullTrust" />
    <rescap:Capability Name="unvirtualizedResources" />
  </Capabilities>
</Package>
"@
    New-Item -ItemType Directory -Force -Path "$layout\Public" | Out-Null
    # When: sparse mode packs a signed identity-only package and attaches the layout as external content.
    if ($Sparse) {
        if (-not $CertThumbprint) { throw '-CertThumbprint is required for -Sparse' }
        $kit = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\x64'
        $pkgDir = Join-Path $WorkDir 'sparse-pkg'
        if (Test-Path -LiteralPath $pkgDir) { Remove-Item -LiteralPath $pkgDir -Recurse -Force }
        New-Item -ItemType Directory -Force -Path "$pkgDir\Assets" | Out-Null
        Copy-Item -Path "$layout\Assets\*" -Destination "$pkgDir\Assets"
        Set-Content -LiteralPath "$pkgDir\AppxManifest.xml" -Value $manifest -Encoding utf8
        $msix = Join-Path $WorkDir 'SonicTermDefTermLab.msix'
        & "$kit\makeappx.exe" pack /d $pkgDir /p $msix /nv /o | Select-Object -Last 1
        if (-not (Test-Path -LiteralPath $msix)) { throw "makeappx produced no package at $msix" }
        & "$kit\signtool.exe" sign /fd SHA256 /sha1 $CertThumbprint $msix | Select-Object -Last 1
        if ($LASTEXITCODE -ne 0) { throw "signtool failed with exit code $LASTEXITCODE" }
        Add-AppxPackage -Path $msix -ExternalLocation $layout
    } else {
        Set-Content -LiteralPath "$layout\AppxManifest.xml" -Value $manifest -Encoding utf8
        Add-AppxPackage -Register "$layout\AppxManifest.xml"
    }
    $installed = Get-AppxPackage -Name $packageName
    Write-Host "installed $($installed.PackageFullName) console=$Console"
}

function Show-Catalog {
    # The picker reads these catalogs from conhost's DelegationConfig; Windows PowerShell has the WinRT projection.
    $query = @'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
[void][Windows.ApplicationModel.AppExtensions.AppExtensionCatalog, Windows.ApplicationModel.AppExtensions, ContentType = WindowsRuntime]
$asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' } | Select-Object -First 1
function Wait-Op($op, [Type]$resultType) { $task = $asTask.MakeGenericMethod($resultType).Invoke($null, @($op)); [void]$task.Wait(-1); $task.Result }
$listType = [System.Collections.Generic.IReadOnlyList[Windows.ApplicationModel.AppExtensions.AppExtension]]
$found = @{}
foreach ($name in 'com.microsoft.windows.console.host', 'com.microsoft.windows.terminal.host') {
    $catalog = [Windows.ApplicationModel.AppExtensions.AppExtensionCatalog]::Open($name)
    $found[$name] = foreach ($ext in (Wait-Op ($catalog.FindAllAsync()) $listType)) {
        $props = Wait-Op ($ext.GetExtensionPropertiesAsync()) ([Windows.Foundation.Collections.IPropertySet])
        # The manifest's <Clsid> element arrives as a nested property set holding its text.
        $clsid = foreach ($entry in $props) { if ($entry.Key -eq 'Clsid') { foreach ($inner in $entry.Value) { if ($inner.Key -eq '#text') { $inner.Value } } } }
        [pscustomobject]@{ Family = $ext.Package.Id.FamilyName; Id = $ext.Id; Clsid = $clsid }
        "catalog $name family=$($ext.Package.Id.FamilyName) id=$($ext.Id) clsid=$clsid" | Write-Host
    }
}
foreach ($term in $found['com.microsoft.windows.terminal.host']) {
    $con = $found['com.microsoft.windows.console.host'] | Where-Object Family -eq $term.Family | Select-Object -First 1
    if ($con) { Write-Host "picker_pair family=$($term.Family) console=$($con.Clsid) terminal=$($term.Clsid)" }
}
'@
    & "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -Command $query
}

switch ($Action) {
    'install' { Install-LabPackage; Show-Catalog }
    'query' { Show-Catalog }
    'remove' {
        Get-AppxPackage -Name $packageName | Remove-AppxPackage
        Write-Host "removed=$(-not (Get-AppxPackage -Name $packageName))"
    }
}
