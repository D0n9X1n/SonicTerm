#!/usr/bin/env python3
"""Deterministic cross-platform resource soak/stress harness (skeleton).

This harness drives a resource scenario for a fixed number of logical ticks,
records a per-tick resource sample, and computes plateau/slope analysis from the
fixed sample series. It always emits a schema-versioned JSON result -- including
on hard-stop (signal or ceiling) or internal error -- so an outer gate can make
a decision from the artifact regardless of how the run terminated.

Design constraints:

* Standard library only; runs under python3 on macOS and Windows.
* Control/no-op scenarios are fully deterministic: a virtual clock and a
  seed-derived synthetic series with no sleeps and no real time or memory in any
  asserted field. All synthetic math uses integers and exact rationals
  (fractions.Fraction) and avoids transcendental functions, so the samples and
  analysis are byte-identical across platforms and Python builds for the same
  seed and parameters.
* Live scenarios do best-effort real capture and truthfully report per-metric
  availability in the top-level capabilities map; unavailable metrics are null.

The harness is a skeleton: GPU, child-process, and governor-ledger capture are
intentionally null in live mode until later work packages provide real sources.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import signal
import sys
import time
from fractions import Fraction

# Bumping any observable JSON contract (fields, semantics) requires a new
# schema_version so downstream consumers can gate on it.
SCHEMA_VERSION = "soak-harness/1"

# SHA-256 of the canonical (deterministic-subset) bytes of the default control
# run: `--scenario control --seed 0 --duration 64 --warmup 8 --cadence 10`.
# This is the shared cross-platform reference: every OS must reproduce these
# exact bytes, so a local gate on each OS compares against the same constant
# rather than only re-running itself. If the synthetic model or JSON contract
# changes intentionally, regenerate both this constant and the golden file with:
#   python3 scripts/soak-harness.py --scenario control --canonical \
#     --out scripts/soak-harness.golden.json
#   python3 -c "import hashlib;print(hashlib.sha256(open(
#     'scripts/soak-harness.golden.json','rb').read()).hexdigest())"
GOLDEN_CONTROL_SHA256 = (
    "725085eab7042fed6d8647a3c01d32b86d7084b897fe294c94b8145d5801fedc"
)

# Keys of the deterministic subset: everything whose value is a pure function of
# seed + params. Excludes environment (platform/machine/python/timestamp) and
# capabilities (OS-dependent), which vary legitimately across hosts.
CANONICAL_KEYS = (
    "schema_version",
    "status",
    "stop_reason",
    "error",
    "scenario",
    "data_source",
    "params",
    "sample_count",
    "samples",
    "analysis",
)

# Metric fields captured per sample and analyzed. Order is fixed and used for
# deterministic JSON emission and analysis iteration.
METRIC_FIELDS = (
    "rss_bytes",
    "private_bytes",
    "gpu_estimate_bytes",
    "thread_count",
    "handle_or_fd_count",
    "process_count",
    "ledger_value",
)

SCENARIOS = ("control", "noop")

# Fixed synthetic baseline for control/no-op scenarios (bytes / counts). These
# are arbitrary but stable; determinism depends only on them being constant.
_BASE_RSS = 100_000_000
_BASE_PRIVATE = 80_000_000
_BASE_GPU = 16_000_000
_BASE_THREADS = 8
_BASE_FDS = 24
_BASE_PROCS = 1
_BASE_LEDGER = 100_000_000

# Control-scenario growth shape: a saturating curve baseline + amp*t/(t+tau)
# with a decaying seed-derived jitter. Only +, -, *, / on integers/Fractions.
_CTRL_AMPLITUDE = 40_000_000
_CTRL_TAU = 12
_CTRL_JITTER = 500_000

# Fixed analysis constants.
_PLATEAU_EPSILON = Fraction(1, 100)  # 1% relative band around the final value.
_FIXED_PLACES = 6  # Decimal places for fixed-point rational serialization.


class HardStop(Exception):
    """Raised to stop collection at a safety ceiling or on a stop signal."""

    def __init__(self, reason: str) -> None:
        super().__init__(reason)
        self.reason = reason


# Set by signal handlers; polled by the collection loop so a stop is handled at
# a deterministic point (tick boundary) rather than mid-sample.
_STOP = {"requested": False, "reason": ""}


def _request_stop(signum, _frame) -> None:
    _STOP["requested"] = True
    _STOP["reason"] = "signal:{}".format(signum)


# ---------------------------------------------------------------------------
# Deterministic synthetic series (control / no-op)
# ---------------------------------------------------------------------------


def _unit_jitter(seed: int, tick: int, salt: str) -> Fraction:
    """Return a deterministic rational in [0, 1) derived from seed/tick/salt.

    Uses SHA-256 so the value is identical across platforms and Python builds,
    unlike the ``random`` module's internal state which is only stable per
    implementation. The result is an exact Fraction, never a float.
    """
    payload = "{}:{}:{}".format(seed, tick, salt).encode("utf-8")
    digest = hashlib.sha256(payload).digest()
    numerator = int.from_bytes(digest[:8], "big")
    return Fraction(numerator, 1 << 64)


def _synthetic_metrics(scenario: str, seed: int, tick: int) -> dict:
    """Compute the deterministic metric values for one synthetic tick."""
    if scenario == "noop":
        # Flat baseline: zero growth, zero jitter -> zero slope, immediate
        # plateau. Exercises the "no leak" reference path.
        return {
            "rss_bytes": _BASE_RSS,
            "private_bytes": _BASE_PRIVATE,
            "gpu_estimate_bytes": _BASE_GPU,
            "thread_count": _BASE_THREADS,
            "handle_or_fd_count": _BASE_FDS,
            "process_count": _BASE_PROCS,
            "ledger_value": _BASE_LEDGER,
        }

    # control: saturating growth with decaying jitter.
    growth = Fraction(_CTRL_AMPLITUDE * tick, tick + _CTRL_TAU)
    jitter_amp = Fraction(_CTRL_JITTER * _CTRL_TAU, tick + _CTRL_TAU)
    centered = (_unit_jitter(seed, tick, "rss") - Fraction(1, 2)) * 2
    rss = int(_BASE_RSS + growth + centered * jitter_amp)

    # Derive the remaining metrics deterministically from rss/tick so the
    # series has cross-metric variety without extra entropy sources.
    private = int(Fraction(rss * 4, 5))
    gpu = int(Fraction(rss, 8))
    threads = _BASE_THREADS + (tick // 16)
    fds = _BASE_FDS + (tick // 8)
    procs = _BASE_PROCS
    ledger = rss
    return {
        "rss_bytes": rss,
        "private_bytes": private,
        "gpu_estimate_bytes": gpu,
        "thread_count": threads,
        "handle_or_fd_count": fds,
        "process_count": procs,
        "ledger_value": ledger,
    }


# ---------------------------------------------------------------------------
# Best-effort live capture (never used by asserted control/no-op fields)
# ---------------------------------------------------------------------------


def _live_rss_bytes():
    try:
        import resource
    except ImportError:
        return _windows_working_set()
    try:
        usage = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    except (AttributeError, OSError, ValueError):
        return None
    # macOS reports ru_maxrss in bytes; Linux reports kibibytes.
    if sys.platform == "darwin":
        return int(usage)
    return int(usage) * 1024


def _windows_process_memory():
    try:
        import ctypes
        from ctypes import wintypes
    except (ImportError, ValueError):
        return None

    class _CountersEx(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
            ("PrivateUsage", ctypes.c_size_t),
        ]

    try:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        current_process = kernel32.GetCurrentProcess
        current_process.argtypes = []
        current_process.restype = wintypes.HANDLE

        query = getattr(kernel32, "K32GetProcessMemoryInfo", None)
        if query is None:
            psapi = ctypes.WinDLL("psapi", use_last_error=True)
            query = psapi.GetProcessMemoryInfo
        query.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(_CountersEx),
            wintypes.DWORD,
        ]
        query.restype = wintypes.BOOL

        counters = _CountersEx()
        counters.cb = ctypes.sizeof(_CountersEx)
        if not query(current_process(), ctypes.byref(counters), counters.cb):
            return None
        return {
            "rss_bytes": int(counters.WorkingSetSize),
            "private_bytes": int(counters.PrivateUsage),
        }
    except (OSError, AttributeError, TypeError, ctypes.ArgumentError):
        return None


def _windows_working_set():
    memory = _windows_process_memory()
    return memory["rss_bytes"] if memory is not None else None


def _live_private_bytes():
    # Private bytes have a cheap cross-API source only on Windows in this
    # skeleton; POSIX private/anonymous accounting is deferred.
    if sys.platform.startswith("win"):
        return _windows_private_usage()
    return None


def _windows_private_usage():
    memory = _windows_process_memory()
    return memory["private_bytes"] if memory is not None else None


def _live_handle_or_fd_count():
    # POSIX: count open file descriptors. Windows: kernel handle count.
    for fd_dir in ("/proc/self/fd", "/dev/fd"):
        if os.path.isdir(fd_dir):
            try:
                return len(os.listdir(fd_dir))
            except OSError:
                return None
    if sys.platform.startswith("win"):
        return _windows_handle_count()
    return None


def _windows_handle_count():
    try:
        import ctypes
        from ctypes import wintypes
    except (ImportError, ValueError):
        return None
    try:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        current_process = kernel32.GetCurrentProcess
        current_process.argtypes = []
        current_process.restype = wintypes.HANDLE
        query = kernel32.GetProcessHandleCount
        query.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        query.restype = wintypes.BOOL

        count = wintypes.DWORD(0)
        if not query(current_process(), ctypes.byref(count)):
            return None
        return int(count.value)
    except (OSError, AttributeError, TypeError, ctypes.ArgumentError):
        return None


def _live_thread_count():
    # threading.active_count reflects interpreter-managed threads only, which is
    # a lower bound but a stable, dependency-free signal for the skeleton.
    import threading

    return int(threading.active_count())


def _live_metrics():
    if sys.platform.startswith("win"):
        memory = _windows_process_memory()
        rss_bytes = memory["rss_bytes"] if memory is not None else None
        private_bytes = memory["private_bytes"] if memory is not None else None
    else:
        rss_bytes = _live_rss_bytes()
        private_bytes = _live_private_bytes()
    return {
        "rss_bytes": rss_bytes,
        "private_bytes": private_bytes,
        "gpu_estimate_bytes": None,  # Deferred to a later work package.
        "thread_count": _live_thread_count(),
        "handle_or_fd_count": _live_handle_or_fd_count(),
        "process_count": None,  # Child-process accounting deferred.
        "ledger_value": None,  # Governor ledger not wired in the skeleton.
    }


def _capabilities() -> dict:
    """Report which metrics this OS can capture live, by probing once."""
    live = _live_metrics()
    return {field: live[field] is not None for field in METRIC_FIELDS}


# ---------------------------------------------------------------------------
# Sampling
# ---------------------------------------------------------------------------


def _build_sample(config, tick: int, start_ns: int) -> dict:
    if config.data_source == "synthetic":
        metrics = _synthetic_metrics(config.scenario, config.seed, tick)
        # Virtual clock: purely a function of tick and cadence, no real time.
        elapsed_ms = tick * config.cadence
    else:
        metrics = _live_metrics()
        elapsed_ms = (time.monotonic_ns() - start_ns) // 1_000_000
    sample = {"tick": tick, "elapsed_ms": elapsed_ms}
    for field in METRIC_FIELDS:
        sample[field] = metrics[field]
    return sample


def _collect(config, samples) -> None:
    """Append per-tick samples to ``samples`` until done or a hard stop.

    Samples are appended in place (not returned) so a caller catching HardStop
    still sees every sample collected before the stop -- the always-emit
    guarantee depends on partial progress surviving the exception.
    """
    start_ns = time.monotonic_ns()
    for tick in range(config.duration):
        if _STOP["requested"]:
            raise HardStop(_STOP["reason"] or "signal")
        if len(samples) >= config.max_samples:
            raise HardStop("max_samples")

        sample = _build_sample(config, tick, start_ns)
        samples.append(sample)

        rss = sample["rss_bytes"]
        if config.max_rss_bytes is not None and rss is not None:
            if rss > config.max_rss_bytes:
                raise HardStop("max_rss_bytes")

        if config.data_source == "live":
            if config.max_seconds is not None:
                elapsed_s = (time.monotonic_ns() - start_ns) / 1e9
                if elapsed_s >= config.max_seconds:
                    raise HardStop("max_seconds")
            if config.cadence > 0 and tick + 1 < config.duration:
                time.sleep(config.cadence / 1000.0)


# ---------------------------------------------------------------------------
# Analysis (deterministic, exact rational arithmetic)
# ---------------------------------------------------------------------------


def _fixed_point(value: Fraction, places: int = _FIXED_PLACES) -> str:
    """Serialize an exact Fraction as a fixed-point decimal string.

    Avoids float formatting so the textual result is identical across
    platforms. Uses round-half-to-even on the exact rational.
    """
    scale = 10 ** places
    scaled = round(value * scale)  # round(Fraction) -> int, banker's rounding.
    sign = "-" if scaled < 0 else ""
    scaled = abs(scaled)
    whole, frac = divmod(scaled, scale)
    return "{}{}.{:0{}d}".format(sign, whole, frac, places)


def _linreg_slope(points) -> Fraction:
    """Exact least-squares slope over integer (x, y) points."""
    n = len(points)
    if n < 2:
        return Fraction(0)
    sum_x = sum(x for x, _ in points)
    sum_y = sum(y for _, y in points)
    sum_xx = sum(x * x for x, _ in points)
    sum_xy = sum(x * y for x, y in points)
    denominator = n * sum_xx - sum_x * sum_x
    if denominator == 0:
        return Fraction(0)
    return Fraction(n * sum_xy - sum_x * sum_y, denominator)


def _plateau(points):
    """Return (reached, start_tick) for the maximal in-band trailing run.

    Walks backward from the final value while each earlier value stays within a
    fixed relative epsilon band of it. The plateau is "reached" when that
    trailing run covers at least a quarter of the window (min two points).
    """
    n = len(points)
    if n < 2:
        return (False, None)
    last = points[-1][1]
    band = _PLATEAU_EPSILON * max(1, abs(last))
    index = n - 1
    while index - 1 >= 0 and abs(points[index - 1][1] - last) <= band:
        index -= 1
    trailing = n - index
    reached = trailing >= max(2, n // 4)
    start_tick = points[index][0] if reached else None
    return (reached, start_tick)


def _analyze_field(window, field: str) -> dict:
    points = [
        (s["tick"], s[field]) for s in window if s.get(field) is not None
    ]
    if len(points) < 2:
        return {
            "available": False,
            "count": len(points),
            "first": points[0][1] if points else None,
            "last": points[-1][1] if points else None,
            "min": None,
            "max": None,
            "delta": None,
            "mean_fixed6": None,
            "slope_per_tick_fixed6": None,
            "plateau_reached": False,
            "plateau_start_tick": None,
        }
    values = [y for _, y in points]
    first = points[0][1]
    last = points[-1][1]
    mean = Fraction(sum(values), len(values))
    slope = _linreg_slope(points)
    reached, start_tick = _plateau(points)
    return {
        "available": True,
        "count": len(points),
        "first": first,
        "last": last,
        "min": min(values),
        "max": max(values),
        "delta": last - first,
        "mean_fixed6": _fixed_point(mean),
        "slope_per_tick_fixed6": _fixed_point(slope),
        "plateau_reached": reached,
        "plateau_start_tick": start_tick,
    }


def _analyze(config, samples) -> dict:
    window = [s for s in samples if s["tick"] >= config.warmup]
    fields = {field: _analyze_field(window, field) for field in METRIC_FIELDS}
    return {
        "warmup_ticks": config.warmup,
        "analyzed_samples": len(window),
        "total_samples": len(samples),
        "fields": fields,
    }


# ---------------------------------------------------------------------------
# Result assembly and emission
# ---------------------------------------------------------------------------


def _result_params(config) -> dict:
    return {
        "seed": config.seed,
        "duration": config.duration,
        "warmup": config.warmup,
        "cadence": config.cadence,
        "max_samples": config.max_samples,
        "max_rss_bytes": config.max_rss_bytes,
        "max_seconds": config.max_seconds,
    }


def _result_environment() -> dict:
    return {
        "platform": sys.platform,
        "machine": platform.machine(),
        "python_version": platform.python_version(),
        "generated_at_unix": int(time.time()),
    }


def _minimal_result(config, samples, error) -> dict:
    """Build a schema-compatible artifact without fallible derived sections."""
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "error",
        "stop_reason": None,
        "error": error,
        "scenario": config.scenario,
        "data_source": config.data_source,
        "params": _result_params(config),
        "environment": {
            "platform": sys.platform,
            "machine": None,
            "python_version": None,
            "generated_at_unix": None,
        },
        "capabilities": None,
        "sample_count": len(samples),
        "samples": samples,
        "analysis": None,
    }


def _build_result(config, samples, status, reason, error) -> dict:
    return {
        "schema_version": SCHEMA_VERSION,
        "status": status,
        "stop_reason": reason,
        "error": error,
        "scenario": config.scenario,
        "data_source": config.data_source,
        "params": _result_params(config),
        "environment": _result_environment(),
        "capabilities": _capabilities(),
        "sample_count": len(samples),
        "samples": samples,
        "analysis": _analyze(config, samples),
    }


def _canonical_subset(result) -> dict:
    """Project the deterministic subset of a result (seed/params only)."""
    return {key: result[key] for key in CANONICAL_KEYS}


def _render_bytes(payload) -> bytes:
    """Render a payload to platform-independent, byte-identical UTF-8.

    Fixed separators, sorted keys, ASCII escaping, and an explicit trailing
    ``\\n`` mean the bytes depend only on the payload, never on the OS text mode
    (Windows would otherwise translate ``\\n`` to ``\\r\\n``) or locale. All
    analysis numbers are ints or fixed-point strings -- never floats -- so there
    is no cross-platform float-formatting drift.
    """
    text = json.dumps(
        payload,
        indent=2,
        sort_keys=True,
        ensure_ascii=True,
        separators=(",", ": "),
    )
    return (text + "\n").encode("utf-8")


def _write_bytes(data: bytes, out_path) -> None:
    if out_path is None or out_path == "-":
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
    else:
        with open(out_path, "wb") as handle:
            handle.write(data)


def _emit(result, out_path, canonical: bool) -> None:
    payload = _canonical_subset(result) if canonical else result
    _write_bytes(_render_bytes(payload), out_path)


def canonical_sha256(result) -> str:
    """Return the SHA-256 hex of a result's canonical bytes."""
    return hashlib.sha256(_render_bytes(_canonical_subset(result))).hexdigest()


def schema() -> dict:
    """Return a machine-readable description of the emitted result contract."""
    return {
        "schema_version": SCHEMA_VERSION,
        "metric_fields": list(METRIC_FIELDS),
        "scenarios": list(SCENARIOS),
        "statuses": ["ok", "hard_stop", "error"],
        "stop_reasons": [
            "max_samples",
            "max_rss_bytes",
            "max_seconds",
            "signal:<n>",
        ],
        "top_level_keys": [
            "schema_version",
            "status",
            "stop_reason",
            "error",
            "scenario",
            "data_source",
            "params",
            "environment",
            "capabilities",
            "sample_count",
            "samples",
            "analysis",
        ],
        "sample_keys": ["tick", "elapsed_ms"] + list(METRIC_FIELDS),
        "field_analysis_keys": [
            "available",
            "count",
            "first",
            "last",
            "min",
            "max",
            "delta",
            "mean_fixed6",
            "slope_per_tick_fixed6",
            "plateau_reached",
            "plateau_start_tick",
        ],
    }


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="soak-harness",
        description="Deterministic cross-platform resource soak/stress harness.",
    )
    parser.add_argument(
        "--scenario", choices=SCENARIOS, default="control",
        help="Scenario to drive (default: control).",
    )
    parser.add_argument("--seed", type=int, default=0, help="Deterministic seed.")
    parser.add_argument(
        "--duration", type=int, default=64,
        help="Number of logical ticks to sample (default: 64).",
    )
    parser.add_argument(
        "--warmup", type=int, default=8,
        help="Leading ticks excluded from analysis (default: 8).",
    )
    parser.add_argument(
        "--cadence", type=int, default=10,
        help="Virtual ms per tick; real sleep only in live mode (default: 10).",
    )
    parser.add_argument("--out", default=None, help="Output path ('-' = stdout).")
    parser.add_argument(
        "--max-samples", type=int, default=100_000,
        help="Hard ceiling on collected samples (default: 100000).",
    )
    parser.add_argument(
        "--max-rss-bytes", type=int, default=None,
        help="Hard-stop when a sample's rss_bytes exceeds this.",
    )
    parser.add_argument(
        "--max-seconds", type=float, default=None,
        help="Wall-clock ceiling (live scenarios only).",
    )
    parser.add_argument(
        "--live", action="store_true",
        help="Use best-effort real capture instead of the synthetic series.",
    )
    parser.add_argument(
        "--fail-on-hard-stop", action="store_true",
        help="Exit non-zero (3) when the run ends in a hard stop.",
    )
    parser.add_argument(
        "--canonical", action="store_true",
        help="Emit only the deterministic subset (for cross-platform gating).",
    )
    parser.add_argument(
        "--print-schema", action="store_true",
        help="Print the result schema as JSON and exit.",
    )
    parser.add_argument(
        "--list-scenarios", action="store_true",
        help="Print available scenarios and exit.",
    )
    return parser


class _Config:
    def __init__(self, args) -> None:
        self.scenario = args.scenario
        self.seed = args.seed
        self.duration = args.duration
        self.warmup = args.warmup
        self.cadence = args.cadence
        self.out = args.out
        self.max_samples = args.max_samples
        self.max_rss_bytes = args.max_rss_bytes
        self.max_seconds = args.max_seconds
        self.fail_on_hard_stop = args.fail_on_hard_stop
        # Control and no-op are deterministic synthetic scenarios unless the
        # operator explicitly opts into live capture.
        self.data_source = "live" if args.live else "synthetic"


def _validate(config) -> str:
    if config.duration < 1:
        return "--duration must be >= 1"
    if config.warmup < 0:
        return "--warmup must be >= 0"
    if config.cadence < 0:
        return "--cadence must be >= 0"
    if config.max_samples < 1:
        return "--max-samples must be >= 1"
    if config.max_rss_bytes is not None and config.max_rss_bytes < 1:
        return "--max-rss-bytes must be >= 1"
    return ""


def _install_signal_handlers() -> None:
    for name in ("SIGINT", "SIGTERM"):
        signum = getattr(signal, name, None)
        if signum is None:
            continue  # SIGTERM is absent on some Windows Python builds.
        try:
            signal.signal(signum, _request_stop)
        except (ValueError, OSError):
            # Not on the main thread, or unsupported: best-effort only.
            pass


def produce(config) -> dict:
    """Run collection and assemble a result, always returning an artifact.

    Never raises for a scenario-level failure: a safety ceiling or stop signal
    yields ``status="hard_stop"`` and any other exception yields
    ``status="error"`` with a partial sample series, so callers always get a
    schema-versioned result to emit.
    """
    status = "ok"
    reason = None
    error = None
    samples = []
    try:
        _collect(config, samples)
    except HardStop as stop:
        status = "hard_stop"
        reason = stop.reason
    except Exception as exc:  # noqa: BLE001 - always emit an artifact.
        status = "error"
        error = "{}: {}".format(type(exc).__name__, exc)
    try:
        return _build_result(config, samples, status, reason, error)
    except Exception as exc:  # noqa: BLE001 - preserve the partial artifact.
        return _minimal_result(
            config,
            samples,
            "assembly: {}: {}".format(type(exc).__name__, exc),
        )


def main(argv=None) -> int:
    args = _parser().parse_args(argv)

    if args.print_schema:
        _write_bytes(_render_bytes(schema()), None)
        return 0
    if args.list_scenarios:
        sys.stdout.write("\n".join(SCENARIOS) + "\n")
        return 0

    config = _Config(args)
    message = _validate(config)
    if message:
        sys.stderr.write("soak-harness: {}\n".format(message))
        return 2

    _install_signal_handlers()

    result = produce(config)
    try:
        _emit(result, config.out, args.canonical)
    except Exception as exc:  # noqa: BLE001 - retry the artifact on stdout.
        result = _minimal_result(
            config,
            result["samples"],
            "emit: {}: {}".format(type(exc).__name__, exc),
        )
        _emit(result, None, False)

    status = result["status"]
    if status == "error":
        return 1
    if status == "hard_stop" and config.fail_on_hard_stop:
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
