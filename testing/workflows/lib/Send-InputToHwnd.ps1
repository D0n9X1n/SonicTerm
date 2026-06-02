# ----------------------------------------------------------------------
# Send-InputToHwnd.ps1 — three-bucket input delivery for SonicTerm
# Windows test harness (issue #502, Guard-4 RDP SKIP fix).
#
# Bucket A (text payload)      : NamedPipeClientStream → `--harness-
#                                input-pipe` (#506 / #511 consumer wire-up
#                                under #502 R5). See Send-TextToPipe +
#                                Get-HarnessPipeName below.
# Bucket B (named-key, no mod) : WM_KEYDOWN + WM_KEYUP posted to HWND.
#                                Does NOT require foreground.
# Bucket C (real modifier chord): SendInput, AFTER AttachThreadInput +
#                                 re-verified GetForegroundWindow. SKIPs
#                                 only the chord step on foreground fail.
#
# All wire-level senders now CHECK PostMessage's BOOL return value and
# throw with GetLastWin32Error on failure (per PR #505 REVISE blocker 2).
# Extended-key bit policy is per the canonical Win32 contract: regular
# Enter (VK_RETURN) is NOT extended; NumpadEnter shares VK_RETURN but
# IS extended (PR #505 REVISE blocker 3).
#
# Diagnosis chain (verbatim, per #502 step 3):
#   R3 Haiku Step-1 (final):
#     https://github.com/D0n9X1n/SonicTerm/issues/502#issuecomment-4599612663
#   R3 Opus Step-2 APPROVED-DIAG:
#     https://github.com/D0n9X1n/SonicTerm/issues/502#issuecomment-4599624505
# ----------------------------------------------------------------------

if (-not ([System.Management.Automation.PSTypeName]'SonicInput').Type) {
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class SonicInput {
  public const uint WM_KEYDOWN    = 0x0100;
  public const uint WM_KEYUP      = 0x0101;
  public const uint WM_SYSKEYDOWN = 0x0104;
  public const uint WM_SYSKEYUP   = 0x0105;
  public const uint WM_CHAR       = 0x0102;

  public const uint MAPVK_VK_TO_VSC      = 0;
  public const uint KEYEVENTF_KEYUP      = 0x0002;
  public const uint KEYEVENTF_SCANCODE   = 0x0008;
  public const uint KEYEVENTF_EXTENDEDKEY= 0x0001;
  public const uint INPUT_KEYBOARD       = 1;

  // PostMessage returns BOOL (nonzero = message queued, zero = failure;
  // call GetLastError). We marshal as bool and set SetLastError so the
  // PS wrapper can pull GetLastWin32Error() on failure.
  [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

  [DllImport("user32.dll")]
  public static extern uint MapVirtualKey(uint uCode, uint uMapType);

  [DllImport("user32.dll")]
  public static extern short VkKeyScanW(char ch);

  [DllImport("user32.dll")]
  public static extern IntPtr GetMessageExtraInfo();

  [DllImport("user32.dll")]
  public static extern IntPtr GetForegroundWindow();

  [DllImport("user32.dll")]
  public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);

  [DllImport("user32.dll")]
  public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);

  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);

  [DllImport("kernel32.dll")]
  public static extern uint GetCurrentThreadId();

  [DllImport("user32.dll", CharSet = CharSet.Auto)]
  public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount);

  [StructLayout(LayoutKind.Sequential)]
  public struct KEYBDINPUT {
    public ushort wVk;
    public ushort wScan;
    public uint   dwFlags;
    public uint   time;
    public IntPtr dwExtraInfo;
  }

  // Use a sized union via explicit layout so SendInput stride is correct on x64.
  [StructLayout(LayoutKind.Explicit)]
  public struct INPUT {
    [FieldOffset(0)] public uint type;
    // KEYBDINPUT begins at offset 8 on x64 / 4 on x86 (matches Windows headers).
    [FieldOffset(8)]
    public KEYBDINPUT ki;
    [FieldOffset(8)]
    public MOUSEINPUT_PAD pad;  // pad to MOUSEINPUT size (largest union arm)
  }

  // Sized to MOUSEINPUT (24 bytes on x64) so the union covers the largest variant.
  [StructLayout(LayoutKind.Sequential, Size = 24)]
  public struct MOUSEINPUT_PAD { public int _pad; }

  [DllImport("user32.dll", SetLastError = true)]
  public static extern uint SendInput(uint nInputs,
                                      [In] INPUT[] pInputs,
                                      int cbSize);
}
"@ 2>&1 | Out-Null
}

# ----------------------------------------------------------------------
# Virtual-key table for named, non-modifier keys (Bucket B).
# NumpadEnter shares VK_RETURN (0x0D) with regular Enter, distinguished
# ONLY by the extended-key bit in lParam / KEYEVENTF_EXTENDEDKEY.
# ----------------------------------------------------------------------
$script:SonicNamedVK = @{
  'enter'        = 0x0D
  'return'       = 0x0D
  'numpadenter'  = 0x0D
  'numpad-enter' = 0x0D
  'tab'          = 0x09
  'escape'       = 0x1B
  'esc'          = 0x1B
  'backspace'    = 0x08
  'space'        = 0x20
  'up'           = 0x26
  'down'         = 0x28
  'left'         = 0x25
  'right'        = 0x27
  'home'         = 0x24
  'end'          = 0x23
  'pageup'       = 0x21
  'page-up'      = 0x21
  'pagedown'     = 0x22
  'page-down'    = 0x22
  'insert'       = 0x2D
  'delete'       = 0x2E
  'f1'  = 0x70; 'f2'  = 0x71; 'f3'  = 0x72; 'f4'  = 0x73
  'f5'  = 0x74; 'f6'  = 0x75; 'f7'  = 0x76; 'f8'  = 0x77
  'f9'  = 0x78; 'f10' = 0x79; 'f11' = 0x7A; 'f12' = 0x7B
}

# Win32 truly-extended-key set (lParam bit 24 / KEYEVENTF_EXTENDEDKEY).
# Per https://learn.microsoft.com/windows/win32/inputdev/about-keyboard-input
# the extended set is: Right Alt, Right Ctrl, Insert, Delete, Home, End,
# PageUp, PageDown, all 4 arrows, Numpad / (VK_DIVIDE), NumpadEnter,
# NumLock, Break (Ctrl+Pause), PrintScreen.
# Regular Enter (VK_RETURN) is NOT extended. Numpad number keys, F1-F12,
# Tab, Backspace, Space, Escape, alpha keys are NOT extended.
$script:SonicExtendedVK = @(
  0x21, # VK_PRIOR  (PageUp)
  0x22, # VK_NEXT   (PageDown)
  0x23, # VK_END
  0x24, # VK_HOME
  0x25, # VK_LEFT
  0x26, # VK_UP
  0x27, # VK_RIGHT
  0x28, # VK_DOWN
  0x2D, # VK_INSERT
  0x2E, # VK_DELETE
  0x6F, # VK_DIVIDE  (Numpad /)
  0x90, # VK_NUMLOCK
  0xA3, # VK_RCONTROL
  0xA5  # VK_RMENU   (right Alt)
)

# Named-key overrides where the extended bit cannot be derived from the
# VK alone (because the VK is shared with a non-extended key). Today this
# is only NumpadEnter (VK_RETURN shared with Enter).
$script:SonicNamedExtended = @{
  'numpadenter'  = $true
  'numpad-enter' = $true
}

function Get-SonicVK {
  param([Parameter(Mandatory=$true)][string]$Key)
  $k = $Key.ToLower()
  if ($script:SonicNamedVK.ContainsKey($k)) { return [int]$script:SonicNamedVK[$k] }
  if ($k.Length -eq 1) {
    $vks = [SonicInput]::VkKeyScanW([char]$k.ToUpper())
    if ($vks -eq -1) { return $null }
    return ([int]($vks -band 0xFF))
  }
  return $null
}

function Test-SonicVKExtended {
  param(
    [Parameter(Mandatory=$true)][int]$VK,
    [string]$KeyName = $null
  )
  if ($KeyName) {
    $kn = $KeyName.ToLower()
    if ($script:SonicNamedExtended.ContainsKey($kn)) {
      return [bool]$script:SonicNamedExtended[$kn]
    }
  }
  return ($script:SonicExtendedVK -contains $VK)
}

function _Build-KeyLParam {
  param(
    [int]$VK,
    [bool]$KeyUp,
    [string]$KeyName = $null
  )
  $scan = [SonicInput]::MapVirtualKey([uint32]$VK, [SonicInput]::MAPVK_VK_TO_VSC)
  $lp = 1   # repeat count = 1
  $lp = $lp -bor (([int]$scan -band 0xFF) -shl 16)
  if (Test-SonicVKExtended -VK $VK -KeyName $KeyName) {
    $lp = $lp -bor (1 -shl 24)
  }
  if ($KeyUp) {
    # bit 30: previous state = 1 (was down); bit 31: transition = 1 (release)
    $lp = $lp -bor (1 -shl 30)
    $lp = $lp -bor (1 -shl 31)
  }
  return [IntPtr]$lp
}

# ----------------------------------------------------------------------
# PostMessage wrapper — REVISE blocker 2. Checks the BOOL return and
# throws with the Win32 error code on failure. Callers either catch
# (probe path) or let it propagate.
# ----------------------------------------------------------------------
function _Invoke-PostMessage {
  param(
    [Parameter(Mandatory=$true)][IntPtr]$Hwnd,
    [Parameter(Mandatory=$true)][uint32]$Msg,
    [Parameter(Mandatory=$true)][IntPtr]$WParam,
    [Parameter(Mandatory=$true)][IntPtr]$LParam
  )
  $ok = [SonicInput]::PostMessage($Hwnd, $Msg, $WParam, $LParam)
  if (-not $ok) {
    $err = [System.Runtime.InteropServices.Marshal]::GetLastWin32Error()
    throw ("PostMessage FAILED: hwnd=0x{0:X} msg=0x{1:X} Win32Err={2}" -f `
      $Hwnd.ToInt64(), $Msg, $err)
  }
}

# ----------------------------------------------------------------------
# Bucket A — text payload via the `--harness-input-pipe` named pipe
# (PR #506 / #511). The Rust side spawns a NamedPipe accept loop and
# prints "harness pipe ready: \\.\pipe\sonicterm-harness-<stem>" on
# stdout once the pipe truly exists; we extract the name from
# `sonicterm.out.log` and connect with NamedPipeClientStream.
#
# The pipe is single-instance (max=1, PIPE_TYPE_BYTE + PIPE_WAIT) and
# is recreated after every disconnect, so each Send-TextToPipe call
# performs a full Connect/Write/Flush/Dispose cycle. Failing to
# Dispose() leaves the previous client attached and the next call
# blocks the 5s Connect timeout.
# ----------------------------------------------------------------------

# Poll the sonicterm stdout log for the "harness pipe ready: ..." line.
# Breaks early if the spawned process has already exited so we don't
# burn the full timeout on a binary that crashed at startup.
function Get-HarnessPipeName {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory=$true)][string]$LogPath,
    [Parameter(Mandatory=$true)][System.Diagnostics.Process]$Proc,
    [int]$TimeoutSec = 10
  )
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  $rx = [regex]'harness pipe ready: (\\\\\.\\pipe\\sonicterm-harness-\S+)'
  while ((Get-Date) -lt $deadline) {
    if (Test-Path $LogPath) {
      try {
        $text = Get-Content -LiteralPath $LogPath -Raw -ErrorAction Stop
        $m = $rx.Match($text)
        if ($m.Success) { return $m.Groups[1].Value.Trim() }
      } catch { }
    }
    try { $Proc.Refresh() } catch { }
    if ($Proc.HasExited) {
      $tail = if (Test-Path $LogPath) {
        (Get-Content -LiteralPath $LogPath -Tail 20 -ErrorAction SilentlyContinue) -join "`n"
      } else { '<no log>' }
      throw ("Get-HarnessPipeName: sonicterm exited (code={0}) before announcing pipe. Tail:`n{1}" -f $Proc.ExitCode, $tail)
    }
    Start-Sleep -Milliseconds 100
  }
  $tail = if (Test-Path $LogPath) {
    (Get-Content -LiteralPath $LogPath -Tail 20 -ErrorAction SilentlyContinue) -join "`n"
  } else { '<no log>' }
  throw ("Get-HarnessPipeName: timed out after {0}s waiting for 'harness pipe ready:' in {1}. Tail:`n{2}" -f $TimeoutSec, $LogPath, $tail)
}

# Connect → write UTF-8 bytes → flush → dispose. Throws on any I/O
# failure (caller is expected to FAIL the case, per Opus caveat 3 —
# Bucket A is no longer SKIP-able now that the pipe exists).
function Send-TextToPipe {
  [CmdletBinding()]
  param(
    [Parameter(Mandatory=$true)][string]$PipeName,
    [Parameter(Mandatory=$true)][AllowEmptyString()][string]$Text
  )
  # .NET NamedPipeClientStream wants the stem only (no `\\.\pipe\` prefix).
  $stem = $PipeName -replace '^\\\\\.\\pipe\\',''
  $client = New-Object System.IO.Pipes.NamedPipeClientStream(
    '.', $stem, [System.IO.Pipes.PipeDirection]::Out)
  try {
    $client.Connect(5000)
    if ([string]::IsNullOrEmpty($Text)) { return }
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $client.Write($bytes, 0, $bytes.Length)
    $client.Flush()
  } finally {
    # MANDATORY — pipe is single-instance, leaking the client blocks
    # the next Connect() for the full timeout.
    $client.Dispose()
  }
}

function Send-TextToHwnd {
  param(
    [Parameter(Mandatory=$true)][IntPtr]$Hwnd,
    [Parameter(Mandatory=$true)][AllowEmptyString()][string]$Text
  )
  # Deprecated as of #502 consumer: HWND is no longer the channel for
  # text payload. Callers should resolve the harness pipe name and use
  # Send-TextToPipe directly. Kept as a clearer error for any stale
  # call site we missed in this PR.
  throw "Send-TextToHwnd is deprecated — use Send-TextToPipe with the case's harness pipe name (#502)"
}

# ----------------------------------------------------------------------
# Bucket B — single named key, no modifier. WM_KEYDOWN + WM_KEYUP.
# Foreground NOT required. Returns $true on success; throws on any
# PostMessage failure (REVISE blocker 2).
# ----------------------------------------------------------------------
function Send-NamedKeyToHwnd {
  param(
    [Parameter(Mandatory=$true)][IntPtr]$Hwnd,
    [Parameter(Mandatory=$true)][string]$Key
  )
  if ($Hwnd -eq [IntPtr]::Zero) { throw 'Send-NamedKeyToHwnd: zero HWND' }
  $vk = Get-SonicVK -Key $Key
  if ($null -eq $vk) { throw "Send-NamedKeyToHwnd: unknown key '$Key'" }
  $lpDown = _Build-KeyLParam -VK $vk -KeyUp:$false -KeyName $Key
  $lpUp   = _Build-KeyLParam -VK $vk -KeyUp:$true  -KeyName $Key
  _Invoke-PostMessage -Hwnd $Hwnd -Msg ([SonicInput]::WM_KEYDOWN) `
    -WParam ([IntPtr]$vk) -LParam $lpDown
  _Invoke-PostMessage -Hwnd $Hwnd -Msg ([SonicInput]::WM_KEYUP) `
    -WParam ([IntPtr]$vk) -LParam $lpUp
  return $true
}

# ----------------------------------------------------------------------
# Chord classifier — does the chord contain a real modifier?
# ----------------------------------------------------------------------
function Test-ChordHasModifier {
  param([Parameter(Mandatory=$true)][string]$Chord)
  $parts = $Chord -split '\+'
  if ($parts.Length -le 1) { return $false }
  $mods = @('ctrl','control','cmd','command','alt','option','shift','win','meta','super')
  foreach ($p in $parts[0..($parts.Length - 2)]) {
    if ($mods -contains $p.ToLower()) { return $true }
  }
  return $false
}

# ----------------------------------------------------------------------
# Bucket C — real modifier chord via SendInput. Requires foreground.
# Returns $true on success, $false if foreground unattainable (caller
# should mark the step as `chord_no_foreground` SKIP — NOT the whole case).
# ----------------------------------------------------------------------
function Send-ChordToHwnd {
  param(
    [Parameter(Mandatory=$true)][IntPtr]$Hwnd,
    [Parameter(Mandatory=$true)][string]$Chord
  )
  if ($Hwnd -eq [IntPtr]::Zero) { throw 'Send-ChordToHwnd: zero HWND' }
  $parts = $Chord -split '\+'
  $key   = $parts[-1]
  $modVks = @()
  $usesAlt = $false
  foreach ($p in $parts[0..([Math]::Max(0,$parts.Length - 2))]) {
    if ($parts.Length -le 1) { break }
    switch ($p.ToLower()) {
      'ctrl'    { $modVks += 0x11 }
      'control' { $modVks += 0x11 }
      'cmd'     { $modVks += 0x11 }
      'command' { $modVks += 0x11 }
      'shift'   { $modVks += 0x10 }
      'alt'     { $modVks += 0x12; $usesAlt = $true }
      'option'  { $modVks += 0x12; $usesAlt = $true }
      'win'     { $modVks += 0x5B }
      'meta'    { $modVks += 0x5B }
      'super'   { $modVks += 0x5B }
    }
  }
  $vk = Get-SonicVK -Key $key
  if ($null -eq $vk) { throw "Send-ChordToHwnd: unknown key '$key' in chord '$Chord'" }
  $keyIsExtended = Test-SonicVKExtended -VK $vk -KeyName $key

  # Attach + re-verify foreground immediately before SendInput. This is
  # the core #491/#502 fix: the foreground-lock filter is what makes
  # SetForegroundWindow no-op past the first call in RDP sessions.
  $targetPid = 0
  $targetTid = [SonicInput]::GetWindowThreadProcessId($Hwnd, [ref]$targetPid)
  $curTid    = [SonicInput]::GetCurrentThreadId()
  $attached  = $false
  if ($targetTid -ne 0 -and $targetTid -ne $curTid) {
    $attached = [SonicInput]::AttachThreadInput($curTid, $targetTid, $true)
  }
  try {
    [void][SonicInput]::SetForegroundWindow($Hwnd)
    Start-Sleep -Milliseconds 60
    $fg = [SonicInput]::GetForegroundWindow()
    if ($fg -ne $Hwnd) {
      # Foreground unattainable — SKIP only this chord step.
      return $false
    }

    # Build INPUT[] : mods down, key down, key up, mods up (reverse).
    $inputs = New-Object 'SonicInput+INPUT[]' (($modVks.Length * 2) + 2)
    $i = 0
    foreach ($m in $modVks) {
      $inp = New-Object SonicInput+INPUT
      $inp.type = [SonicInput]::INPUT_KEYBOARD
      $ki = New-Object SonicInput+KEYBDINPUT
      $ki.wVk = [uint16]$m
      $ki.wScan = [uint16]([SonicInput]::MapVirtualKey([uint32]$m, [SonicInput]::MAPVK_VK_TO_VSC))
      $ki.dwFlags = 0
      $ki.dwExtraInfo = [SonicInput]::GetMessageExtraInfo()
      $inp.ki = $ki
      $inputs[$i] = $inp; $i++
    }
    # Key down
    $inp = New-Object SonicInput+INPUT
    $inp.type = [SonicInput]::INPUT_KEYBOARD
    $ki = New-Object SonicInput+KEYBDINPUT
    $ki.wVk = [uint16]$vk
    $ki.wScan = [uint16]([SonicInput]::MapVirtualKey([uint32]$vk, [SonicInput]::MAPVK_VK_TO_VSC))
    $ki.dwFlags = 0
    if ($keyIsExtended) {
      $ki.dwFlags = $ki.dwFlags -bor [SonicInput]::KEYEVENTF_EXTENDEDKEY
    }
    $ki.dwExtraInfo = [SonicInput]::GetMessageExtraInfo()
    $inp.ki = $ki
    $inputs[$i] = $inp; $i++
    # Key up
    $inp = New-Object SonicInput+INPUT
    $inp.type = [SonicInput]::INPUT_KEYBOARD
    $ki = New-Object SonicInput+KEYBDINPUT
    $ki.wVk = [uint16]$vk
    $ki.wScan = [uint16]([SonicInput]::MapVirtualKey([uint32]$vk, [SonicInput]::MAPVK_VK_TO_VSC))
    $ki.dwFlags = [SonicInput]::KEYEVENTF_KEYUP
    if ($keyIsExtended) {
      $ki.dwFlags = $ki.dwFlags -bor [SonicInput]::KEYEVENTF_EXTENDEDKEY
    }
    $ki.dwExtraInfo = [SonicInput]::GetMessageExtraInfo()
    $inp.ki = $ki
    $inputs[$i] = $inp; $i++
    # Mods up (reverse order)
    for ($r = $modVks.Length - 1; $r -ge 0; $r--) {
      $inp = New-Object SonicInput+INPUT
      $inp.type = [SonicInput]::INPUT_KEYBOARD
      $ki = New-Object SonicInput+KEYBDINPUT
      $ki.wVk = [uint16]$modVks[$r]
      $ki.wScan = [uint16]([SonicInput]::MapVirtualKey([uint32]$modVks[$r], [SonicInput]::MAPVK_VK_TO_VSC))
      $ki.dwFlags = [SonicInput]::KEYEVENTF_KEYUP
      $ki.dwExtraInfo = [SonicInput]::GetMessageExtraInfo()
      $inp.ki = $ki
      $inputs[$i] = $inp; $i++
    }

    $cb = [System.Runtime.InteropServices.Marshal]::SizeOf([Type]'SonicInput+INPUT')
    $sent = [SonicInput]::SendInput([uint32]$inputs.Length, $inputs, $cb)
    return ($sent -eq $inputs.Length)
  } finally {
    if ($attached) {
      [void][SonicInput]::AttachThreadInput($curTid, $targetTid, $false)
    }
  }
}

# ----------------------------------------------------------------------
# Convenience: read window title (used by the OSC sentinel probe).
# ----------------------------------------------------------------------
function Get-SonicWindowTitle {
  param([Parameter(Mandatory=$true)][IntPtr]$Hwnd)
  $sb = New-Object System.Text.StringBuilder 1024
  [void][SonicInput]::GetWindowText($Hwnd, $sb, $sb.Capacity)
  return $sb.ToString()
}
