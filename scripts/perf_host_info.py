"""Perf host info."""

from __future__ import annotations
import os
import re
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_focus
import perf_font_diagnostics
import perf_latency_schema
HOST_COMMAND_TIMEOUT_S = 60
UNAVAILABLE = "unavailable"


def _first_line(text: str | None) -> str:
    lines = [line.strip() for line in (text or "").splitlines() if line.strip()]
    return lines[0] if lines else UNAVAILABLE


def _profile_values(text: str, key: str) -> list[str]:
    """Return every `key: value` value of a system_profiler report, in order."""
    prefix = key + ":"
    return [line.strip()[len(prefix):].strip() for line in text.splitlines() if line.strip().startswith(prefix)]


def host_block(outputs: Mapping[str, str], monitor: Mapping | None = None) -> list[str]:
    """Render the host block from raw command outputs; a figure a command did not give reads `unavailable`.

    `monitor` is the display the valid runs shared, which reads `unknown` when none reported one.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_sets
    memory = _first_line(outputs.get("memory"))
    memory_text = f"{int(memory) / 1024 ** 3:.0f} GiB" if memory.isdigit() else UNAVAILABLE
    os_fields = dict(re.findall(r"^[ \t]*(\w+):[ \t]*(.+?)[ \t]*$", outputs.get("os") or "", re.M))
    os_text = (f"{os_fields.get('ProductName', 'macOS')} {os_fields['ProductVersion']} "
               f"({os_fields.get('BuildVersion', '?')})" if "ProductVersion" in os_fields else UNAVAILABLE)
    displays = outputs.get("displays") or ""
    gpus = _profile_values(displays, "Chipset Model")
    looks = _profile_values(displays, "UI Looks like")
    display_lines = []
    for position, resolution in enumerate(_profile_values(displays, "Resolution")):
        text = resolution
        if position < len(looks):
            text += f", looks like {looks[position]}"
            native, logical = re.match(r"(\d+) x \d+", resolution), re.match(r"(\d+) x \d+", looks[position])
            if native and logical and int(logical[1]):
                text += f", scale {int(native[1]) / int(logical[1]):.2f}"
        display_lines.append(text)
    power = re.search(r"Now drawing from '([^']+)'", outputs.get("power") or "")
    low_power = re.search(r"^[ \t]*lowpowermode[ \t]+(\d+)", outputs.get("power_settings") or "", re.M)
    low_power_text = UNAVAILABLE if not low_power else ("off" if low_power[1] == "0" else "on")
    return [
        f"- Machine: {_first_line(outputs.get('model'))}, {_first_line(outputs.get('cpu'))}, {memory_text}",
        f"- OS: {os_text}",
        f"- GPU: {', '.join(gpus) if gpus else UNAVAILABLE}",
        f"- Displays (resolution, logical size and refresh, scale): "
        f"{'; '.join(display_lines) if display_lines else UNAVAILABLE}",
        f"- Measurement display: {perf_sets.describe_display(monitor)}",
        f"- Power: {power[1] if power else UNAVAILABLE}; Low Power Mode {low_power_text}",
    ]


POWERCFG_ARGV = ("powercfg", "/getactivescheme")
_CPU_KEY = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"
_BIOS_KEY = r"HARDWARE\DESCRIPTION\System\BIOS"
_OS_KEY = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"
# Each Windows host figure's key below HKEY_LOCAL_MACHINE and its value name.
WINDOWS_REGISTRY_VALUES = {
    "cpu": (_CPU_KEY, "ProcessorNameString"),
    "manufacturer": (_BIOS_KEY, "SystemManufacturer"),
    "product": (_BIOS_KEY, "SystemProductName"),
    "os_name": (_OS_KEY, "ProductName"),
    "os_version": (_OS_KEY, "DisplayVersion"),
    "os_build": (_OS_KEY, "CurrentBuild"),
    "os_ubr": (_OS_KEY, "UBR"),
}
# The active scheme's name is the parenthesized text at the end of powercfg's line.
_POWER_PLAN = re.compile(r"\(([^()]+)\)\s*$")


def _registry_value(key: str, name: str) -> object:
    """One HKEY_LOCAL_MACHINE value, read-only, or None when it cannot be read."""
    try:
        import winreg
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, key) as handle:
            return winreg.QueryValueEx(handle, name)[0]
    except (ImportError, OSError):
        return None


def _total_memory_bytes() -> int | None:
    """Physical memory from GlobalMemoryStatusEx, or None off Windows or when the call fails."""
    try:
        import ctypes
        from ctypes import wintypes

        class MemoryStatus(ctypes.Structure):
            """MEMORYSTATUSEX from <sysinfoapi.h>."""

            _fields_ = [("dwLength", wintypes.DWORD), ("dwMemoryLoad", wintypes.DWORD)] + [
                (name, ctypes.c_ulonglong) for name in ("ullTotalPhys", "ullAvailPhys", "ullTotalPageFile",
                                                        "ullAvailPageFile", "ullTotalVirtual", "ullAvailVirtual",
                                                        "ullAvailExtendedVirtual")]

        status = MemoryStatus()
        status.dwLength = ctypes.sizeof(status)
        if not ctypes.WinDLL("kernel32", use_last_error=True).GlobalMemoryStatusEx(ctypes.byref(status)):
            return None
        return int(status.ullTotalPhys)
    except (ImportError, OSError, AttributeError):
        return None


def _file_version(path: str) -> str | None:
    """A file's version resource as `a.b.c.d`, or None when it has none or cannot be read."""
    try:
        import ctypes
        from ctypes import wintypes
        version = ctypes.WinDLL("version", use_last_error=True)
        size = version.GetFileVersionInfoSizeW(path, None)
        if not size:
            return None
        buffer = ctypes.create_string_buffer(size)
        if not version.GetFileVersionInfoW(path, 0, size, buffer):
            return None
        pointer, length = ctypes.c_void_p(), wintypes.UINT()
        if not version.VerQueryValueW(buffer, "\\", ctypes.byref(pointer), ctypes.byref(length)):
            return None
        # VS_FIXEDFILEINFO: signature, structure version, then the file version's high and low halves.
        words = ctypes.cast(pointer, ctypes.POINTER(wintypes.DWORD * 13)).contents
        high, low = words[2], words[3]
        return f"{high >> 16}.{high & 0xFFFF}.{low >> 16}.{low & 0xFFFF}"
    except (ImportError, OSError, AttributeError):
        return None


def windows_host_outputs(host_run: Callable[[Sequence[str], int], perf_focus.CommandRecord], *,
                         registry: Callable[[str, str], object] | None = None,
                         memory: Callable[[], int | None] | None = None,
                         file_version: Callable[[str], str | None] | None = None) -> dict[str, str]:
    """Read the Windows host block's figures, read-only; a figure that could not be read is empty.

    The registry, memory status and file version are injectable, so the tests drive fakes.
    """
    registry = registry or _registry_value
    outputs = {}
    for name, (key, value_name) in WINDOWS_REGISTRY_VALUES.items():
        value = registry(key, value_name)
        outputs[name] = "" if value is None else str(value).strip()
    total = (memory or _total_memory_bytes)()
    outputs["memory_bytes"] = str(total) if perf_latency_schema._is_int(total) else ""
    record = host_run(POWERCFG_ARGV, HOST_COMMAND_TIMEOUT_S)
    outputs["power"] = "" if perf_focus._command_failure(record) else record.stdout
    conhost = os.path.join(os.environ.get("SystemRoot", "C:\\Windows"), "System32", "conhost.exe")
    outputs["conhost"] = (file_version or _file_version)(conhost) or ""
    return outputs


def host_block_windows(outputs: Mapping[str, str], monitor: Mapping | None,
                       renderer: Mapping | None) -> list[str]:
    """Render the Windows host block; a figure that could not be read reads `unavailable`.

    `monitor` is the display the valid runs shared and `renderer` the adapter they drew through.
    """
    # Resolve this runtime-only dependency after module initialization.
    import perf_sets
    machine = " ".join(part for part in (outputs.get("manufacturer"), outputs.get("product")) if part)
    memory = outputs.get("memory_bytes") or ""
    memory_text = f"{int(memory) / 1024 ** 3:.0f} GiB" if memory.isdigit() else UNAVAILABLE
    os_text = UNAVAILABLE
    if outputs.get("os_name"):
        os_text = outputs["os_name"] + (f" {outputs['os_version']}" if outputs.get("os_version") else "")
        if outputs.get("os_build"):
            update = f".{outputs['os_ubr']}" if outputs.get("os_ubr") else ""
            os_text += f" (build {outputs['os_build']}{update})"
    plan = _POWER_PLAN.search((outputs.get("power") or "").strip())
    return [
        f"- Machine: {machine or UNAVAILABLE}, {outputs.get('cpu') or UNAVAILABLE}, {memory_text}",
        f"- OS: {os_text}",
        f"- GPU: {perf_font_diagnostics.describe_renderer(renderer) if renderer is not None else UNAVAILABLE}",
        f"- Measurement display: {perf_sets.describe_display(monitor)}",
        f"- Power plan: {plan[1] if plan else UNAVAILABLE}",
        f"- Console host: conhost.exe {outputs.get('conhost') or UNAVAILABLE}",
    ]


RECOVERY_HEADER = "| Frame | Baseline | PR |\n|---|---|---|\n"
RECOVERY_NOTE = ("Each cell sums, over every episode of every accepted counters run, the row-cache misses and hits, "
                 "the shaping requests and the render attempts of that frame: A retries an injected atlas change, B "
                 "is the first recovered presentation, C and D are forced frames of the unchanged scene.")
