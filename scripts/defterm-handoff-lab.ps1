<#
.SYNOPSIS
Measurement-only lab for Windows' Default terminal application handoff (track 0a).

.DESCRIPTION
Registers the `defterm_handoff_probe` example as a per-user COM server for its
test CLSID, routes HKCU\Console\%%Startup to it, launches one console client,
waits for the probe's log, and then restores %%Startup and the CLSID key to the
exact snapshot taken before the run. Run it only on a disposable VM or test
account: while the run lasts, every console launch for this user is delegated.

.PARAMETER Mode
Probe behavior: success, fail, hang, crash-after, close-after (the terminal
closes the session like a closed window), exit-early, or unregistered (the
CLSID is routed but never registered).

.PARAMETER Launch
Medium launches the client through `runas /trustlevel:0x20000` (a restricted
token); Task launches it through a limited-run-level interactive scheduled task
(the user's filtered medium token); High launches it from this process, which
must be elevated.
#>
[CmdletBinding()]
param(
    [ValidateSet('success', 'fail', 'hang', 'crash-after', 'close-after', 'exit-early', 'unregistered')]
    [string]$Mode = 'success',
    [ValidateSet('Medium', 'Task', 'High')]
    [string]$Launch = 'Medium',
    [Parameter(Mandatory)]
    [string]$ProbeExe,
    [Parameter(Mandatory)]
    [string]$WorkDir,
    [int]$TimeoutS = 40,
    [string]$DelegationConsole = '{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}',
    [string]$DelegationTerminal,
    [string]$ProbeClsid = '{A3D9E8F1-6B2C-4E7A-9C15-2F8B7D4E0A61}',
    [switch]$Warm
)

$ErrorActionPreference = 'Stop'
# A fresh CLSID per case keeps an activation left pending by one case out of the next.
$probeClsid = '{' + $ProbeClsid.Trim('{', '}').ToUpperInvariant() + '}'
if (-not $DelegationTerminal) { $DelegationTerminal = $probeClsid }
$startupKey = 'HKCU:\Console\%%Startup'
$clsidKey = "HKCU:\Software\Classes\CLSID\$probeClsid"
$valueNames = @('DelegationConsole', 'DelegationTerminal')

New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
$runId = '{0}-{1}-{2}' -f $Mode, $Launch, (Get-Date -Format 'HHmmss')
$logPath = Join-Path $WorkDir "$runId.log"
$labLog = Join-Path $WorkDir "$runId.lab.txt"

function Write-Lab([string]$line) {
    $stamped = '{0} {1}' -f (Get-Date -Format 'HH:mm:ss.fff'), $line
    Add-Content -LiteralPath $labLog -Value $stamped
    Write-Host $stamped
}

# Snapshot the exact delegation values; an absent value is recorded as absent.
$snapshot = @{}
$startupExisted = Test-Path -LiteralPath $startupKey
foreach ($name in $valueNames) {
    $item = if ($startupExisted) { Get-Item -LiteralPath $startupKey } else { $null }
    if ($item -and ($item.GetValueNames() -contains $name)) {
        $snapshot[$name] = @{ Present = $true; Kind = $item.GetValueKind($name).ToString(); Data = $item.GetValue($name) }
    } else {
        $snapshot[$name] = @{ Present = $false }
    }
}
$clsidExisted = Test-Path -LiteralPath $clsidKey
$snapshot | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $WorkDir "$runId.snapshot.json")
Write-Lab "snapshot startup_existed=$startupExisted clsid_existed=$clsidExisted values=$($snapshot | ConvertTo-Json -Compress -Depth 4)"
if ($clsidExisted) { throw "Probe CLSID key already exists; refusing to overwrite $clsidKey" }

$clientPid = $null
$traceId = "SonicDefterm_$runId"
# Every process start during the run, so client identity and server activations are observable.
Register-CimIndicationEvent -ClassName Win32_ProcessStartTrace -SourceIdentifier $traceId
$warmProbe = $null
try {
    if ($Mode -ne 'unregistered') {
        $probeMode = $Mode
        New-Item -Path "$clsidKey\LocalServer32" -Force | Out-Null
        Set-ItemProperty -LiteralPath "$clsidKey\LocalServer32" -Name '(default)' -Value "`"$ProbeExe`" --mode $probeMode --log `"$logPath`" --timeout-s $TimeoutS --clsid $probeClsid"
    }
    if ($Warm) {
        # Warm: the server is already registered before the client starts. It runs as a
        # limited interactive task because COM only reuses a server at the activator's integrity.
        $warmTask = "SonicTermDeftermWarm_$runId"
        $warmAction = New-ScheduledTaskAction -Execute $ProbeExe -Argument "--mode $Mode --log `"$logPath`" --timeout-s $TimeoutS --clsid $probeClsid"
        $warmPrincipal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
        Register-ScheduledTask -TaskName $warmTask -Action $warmAction -Principal $warmPrincipal -Force | Out-Null
        try { Start-ScheduledTask -TaskName $warmTask } finally { Start-Sleep -Milliseconds 1500; Unregister-ScheduledTask -TaskName $warmTask -Confirm:$false }
        $warmProbe = Get-CimInstance Win32_Process -Filter "Name = 'defterm_handoff_probe.exe'" | Select-Object -First 1
        Write-Lab "warm_probe pid=$($warmProbe.ProcessId)"
    }
    if (-not $startupExisted) { New-Item -Path $startupKey -Force | Out-Null }
    Set-ItemProperty -LiteralPath $startupKey -Name 'DelegationConsole' -Value $DelegationConsole -Type String
    Set-ItemProperty -LiteralPath $startupKey -Name 'DelegationTerminal' -Value $DelegationTerminal -Type String
    Write-Lab "routed console=$DelegationConsole terminal=$DelegationTerminal"

    $nonce = "SONIC_DEFTERM_$runId"
    $hostNames = @('OpenConsole', 'WindowsTerminal', 'conhost', 'defterm_handoff_probe')
    $hostsBefore = @(Get-Process -Name $hostNames -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    $launchedAt = Get-Date
    if ($Launch -eq 'Medium') {
        & "$env:SystemRoot\System32\runas.exe" /trustlevel:0x20000 "cmd.exe /k title $nonce" | Out-Null
    } elseif ($Launch -eq 'Task') {
        # A limited-run-level interactive task gives the client the user's filtered medium token.
        $taskName = "SonicTermDefterm_$runId"
        $action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\cmd.exe" -Argument "/k title $nonce"
        $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
        Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Force | Out-Null
        try { Start-ScheduledTask -TaskName $taskName } finally { Start-Sleep -Milliseconds 500; Unregister-ScheduledTask -TaskName $taskName -Confirm:$false }
    } else {
        Start-Process -FilePath "$env:SystemRoot\System32\cmd.exe" -ArgumentList "/k title $nonce" | Out-Null
    }
    Start-Sleep -Milliseconds 3000
    $newHosts = Get-CimInstance Win32_Process | Where-Object { ($hostNames -contains [IO.Path]::GetFileNameWithoutExtension($_.Name)) -and ($hostsBefore -notcontains $_.ProcessId) } |
        ForEach-Object { "$($_.Name):$($_.ProcessId):parent=$($_.ParentProcessId)" }
    Write-Lab "new_host_processes=$($newHosts -join ',')"
    $client = Get-CimInstance Win32_Process -Filter "Name = 'cmd.exe'" | Where-Object CommandLine -like "*$nonce*" | Select-Object -First 1
    if ($client) {
        $clientPid = $client.ProcessId
        $owner = Get-Process -Id $clientPid -IncludeUserName -ErrorAction SilentlyContinue
        $conhosts = Get-CimInstance Win32_Process -Filter "ParentProcessId = $clientPid" | ForEach-Object { "$($_.Name):$($_.ProcessId)" }
        Write-Lab "client pid=$clientPid user=$($owner.UserName) children=$($conhosts -join ',')"
    } else {
        Write-Lab 'client not found by nonce'
    }

    # Poll until the probe reports a finished session, a fallback window shows the
    # client, or the deadline; record when the window appeared and the client exited.
    $deadline = (Get-Date).AddSeconds($TimeoutS + 10)
    $windowSeen = $false
    $clientGone = $false
    while ((Get-Date) -lt $deadline) {
        $elapsedMs = [int]((Get-Date) - $launchedAt).TotalMilliseconds
        if (-not $windowSeen -and (Get-Process | Where-Object { $_.MainWindowTitle -like "*$nonce*" })) {
            $windowSeen = $true
            Write-Lab "window_appeared after_ms=$elapsedMs"
        }
        if ($clientPid -and -not $clientGone -and -not (Get-Process -Id $clientPid -ErrorAction SilentlyContinue)) {
            $clientGone = $true
            Write-Lab "client_exited after_ms=$elapsedMs"
        }
        if ((Test-Path -LiteralPath $logPath) -and (Select-String -LiteralPath $logPath -Pattern 'event=(session_done|server_stopping|crash|close_done)' -Quiet)) { break }
        # When: no handoff is expected to finish, the fallback window is the observed outcome.
        if ($windowSeen -and $Mode -in 'fail', 'hang', 'exit-early', 'unregistered') { break }
        Start-Sleep -Milliseconds 500
    }
    Write-Lab "wait_done window_seen=$windowSeen client_gone=$clientGone elapsed_ms=$([int]((Get-Date) - $launchedAt).TotalMilliseconds)"
    Start-Sleep -Seconds 2
    $alive = if ($clientPid) { [bool](Get-Process -Id $clientPid -ErrorAction SilentlyContinue) } else { $false }
    $windows = Get-Process | Where-Object { $_.MainWindowTitle -like "*$nonce*" } | ForEach-Object { "$($_.ProcessName):$($_.Id)" }
    Write-Lab "after client_alive=$alive windows_with_nonce=$($windows -join ',')"
    $probes = Get-CimInstance Win32_Process -Filter "Name = 'defterm_handoff_probe.exe'" | ForEach-Object { $_.ProcessId }
    Write-Lab "probe_processes=$($probes -join ',')"
} finally {
    $traced = Get-Event -SourceIdentifier $traceId -ErrorAction SilentlyContinue | ForEach-Object {
        $started = $_.SourceEventArgs.NewEvent
        if ($started.ProcessName -match '^(cmd|conhost|OpenConsole|WindowsTerminal|defterm_handoff_probe)\.exe$') { "$($started.ProcessName):$($started.ProcessID):parent=$($started.ParentProcessID)" }
    }
    Unregister-Event -SourceIdentifier $traceId -ErrorAction SilentlyContinue
    Get-Event -SourceIdentifier $traceId -ErrorAction SilentlyContinue | Remove-Event
    Write-Lab "process_starts=$($traced -join ',')"
    foreach ($name in $valueNames) {
        $saved = $snapshot[$name]
        if ($saved.Present) {
            Set-ItemProperty -LiteralPath $startupKey -Name $name -Value $saved.Data -Type $saved.Kind
        } elseif (Test-Path -LiteralPath $startupKey) {
            Remove-ItemProperty -LiteralPath $startupKey -Name $name -ErrorAction SilentlyContinue
        }
    }
    if (-not $startupExisted -and (Test-Path -LiteralPath $startupKey)) { Remove-Item -LiteralPath $startupKey -Recurse }
    if (Test-Path -LiteralPath $clsidKey) { Remove-Item -LiteralPath $clsidKey -Recurse }
    Get-CimInstance Win32_Process -Filter "Name = 'defterm_handoff_probe.exe'" | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    $restored = Get-Item -LiteralPath $startupKey -ErrorAction SilentlyContinue
    Write-Lab "restored startup_values=$(if ($restored) { $restored.GetValueNames() -join ',' } else { '<no key>' }) clsid_key=$(Test-Path -LiteralPath $clsidKey)"
}
if ($clientPid -and (Get-Process -Id $clientPid -ErrorAction SilentlyContinue)) {
    Write-Lab "client $clientPid still running after restore; leaving it for inspection"
}
