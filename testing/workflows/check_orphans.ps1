# check_orphans.ps1 — Windows port of check_orphans.sh.
#
# Verifies PtyHandle::Drop actually killed every shell that
# sonicterm-windows spawned. The text contract (stdout / stderr /
# exit code) MUST match check_orphans.sh, because run_case.{sh,ps1}
# parses `orphans=<N>` from stdout and ignores everything else.
#
# Usage:
#   check_orphans.ps1 snapshot <sonic-pid> <out-file>
#       Walk the descendant tree of <sonic-pid>, keep any PID whose
#       executable name matches the shell allowlist, write one PID
#       per line to <out-file>. Run while sonicterm-windows is still
#       alive with shells spawned.
#
#   check_orphans.ps1 check <snapshot-file>
#       For each PID in <snapshot-file>, test whether the process is
#       still alive. Print a single line `orphans=<N>` to stdout.
#       Always exits 0 — caller decides pass/fail from the count.
#
# Notes:
#   * Win32_Process is enumerated ONCE per invocation and turned into
#     a PPID->children hash map. A repeated Get-Process / WMI query
#     per descendant would be O(N*procs); the BFS below is O(procs).
#   * Shell allowlist is intentionally narrow:
#       pwsh.exe powershell.exe cmd.exe bash.exe sh.exe
#       zsh.exe fish.exe dash.exe
#     conhost.exe / OpenConsole.exe are NOT in the allowlist; they are
#     conpty plumbing and are not what PtyHandle::Drop is responsible
#     for cleaning up. (They are still walked by the BFS so their own
#     children, if any, are reachable.)
#   * Exit 0 always (count is reported via stdout). Mirrors bash.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('snapshot', 'check')]
    [string]$Cmd,

    [Parameter(Mandatory = $true, Position = 1)]
    [string]$Arg1,

    [Parameter(Position = 2)]
    [string]$Arg2
)

$ErrorActionPreference = 'Stop'

$ShellAllowlist = @(
    'pwsh.exe', 'powershell.exe', 'cmd.exe',
    'bash.exe', 'sh.exe', 'zsh.exe', 'fish.exe', 'dash.exe'
)

function Get-ProcMap {
    # One CIM query, build PPID -> [pscustomobject]@{ Pid; Name }[] map.
    $procs = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue
    $map = @{}
    foreach ($p in $procs) {
        $ppid = [int]$p.ParentProcessId
        if (-not $map.ContainsKey($ppid)) { $map[$ppid] = New-Object System.Collections.ArrayList }
        [void]$map[$ppid].Add([pscustomobject]@{
            Pid  = [int]$p.ProcessId
            Name = $p.Name
        })
    }
    return $map
}

function Get-Descendants {
    param([int]$Root, [hashtable]$Map)
    # BFS. Returns [pscustomobject]@{ Pid; Name }[] EXCLUDING the root.
    $out  = New-Object System.Collections.ArrayList
    $seen = @{}
    $queue = New-Object System.Collections.Queue
    $queue.Enqueue($Root)
    $seen[$Root] = $true
    while ($queue.Count -gt 0) {
        $cur = $queue.Dequeue()
        if ($Map.ContainsKey($cur)) {
            foreach ($child in $Map[$cur]) {
                if (-not $seen.ContainsKey($child.Pid)) {
                    $seen[$child.Pid] = $true
                    [void]$out.Add($child)
                    $queue.Enqueue($child.Pid)
                }
            }
        }
    }
    return ,$out
}

switch ($Cmd) {
    'snapshot' {
        $sonicPid = [int]$Arg1
        $outFile  = $Arg2
        if ([string]::IsNullOrWhiteSpace($outFile)) {
            [Console]::Error.WriteLine('out file required')
            exit 2
        }
        # Truncate the snapshot file (matches `: > "$out"` in bash).
        Set-Content -Path $outFile -Value $null -Encoding ascii
        # Clear it — Set-Content with $null writes nothing but creates the file.
        if (Test-Path $outFile) { [System.IO.File]::WriteAllText($outFile, '') }

        $map = Get-ProcMap
        $descendants = Get-Descendants -Root $sonicPid -Map $map
        $kept = 0
        $sw = [System.IO.StreamWriter]::new($outFile, $false, [System.Text.Encoding]::ASCII)
        try {
            foreach ($d in $descendants) {
                if ($ShellAllowlist -contains $d.Name.ToLowerInvariant()) {
                    $sw.WriteLine($d.Pid)
                    $kept++
                }
            }
        } finally {
            $sw.Dispose()
        }
        [Console]::Error.WriteLine("snapshot wrote $kept pids to $outFile")
        exit 0
    }
    'check' {
        $snap = $Arg1
        if (-not (Test-Path $snap)) {
            # Mirrors bash: missing snapshot => orphans=0 + WARN, exit 0.
            [Console]::Out.WriteLine('orphans=0')
            [Console]::Error.WriteLine("WARN: snapshot file $snap missing — check skipped")
            exit 0
        }
        $n = 0
        foreach ($line in Get-Content -Path $snap -ErrorAction SilentlyContinue) {
            $pidStr = $line.Trim()
            if ([string]::IsNullOrEmpty($pidStr)) { continue }
            $pidNum = 0
            if (-not [int]::TryParse($pidStr, [ref]$pidNum)) { continue }
            $live = Get-Process -Id $pidNum -ErrorAction SilentlyContinue
            if ($null -ne $live) {
                $n++
                $ppidStr = '?'
                try {
                    $cim = Get-CimInstance Win32_Process -Filter "ProcessId=$pidNum" -ErrorAction SilentlyContinue
                    if ($cim) { $ppidStr = "$($cim.ParentProcessId)" }
                } catch { }
                [Console]::Error.WriteLine("ORPHAN pid=$pidNum ppid=$ppidStr name=$($live.ProcessName)")
            }
        }
        [Console]::Out.WriteLine("orphans=$n")
        exit 0
    }
}
