"""Perf scenarios."""

from __future__ import annotations
from dataclasses import asdict, dataclass, field
import json
import os
from pathlib import Path
import platform
import re
import sys
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_harness_sources
import perf_latency_schema
import perf_settings
import perf_timeline_schema



# --- Builds, scenario list and run command --------------------------------------------------

# Seconds a harness run may exceed its scenario timeout, for startup, finish_session and exit. Reaching
# the bound is unresolved cleanup whatever the harness's exit; classify_outcome says why.
RUN_MARGIN_S = 30
# The smoke caps every run's bound at this many seconds; reaching it is a run_step deadline like any other.
SMOKE_RUN_CAP_S = 100
LIST_TIMEOUT_S = 60
GIT_TIMEOUT_S = 300
_NAME = re.compile(r"[A-Za-z0-9_.-]+")


@dataclass(frozen=True)
class Scenario:
    """One entry of the harness's `--list` output."""

    id: str
    variants: tuple[str, ...]
    title: str
    timeout_s: int
    short_timeout_s: int
    # Per-variant caps on a --short comparison's valid runs, as (variant, cap) pairs; empty for none.
    run_caps: tuple[tuple[str, int], ...] = ()
    # The latency split schema the harness declares in `capabilities`; None for a harness that predates it.
    latency_split_schema: int | None = None
    # The phase-kinds schema the harness declares; None for a harness that does not classify its phases.
    phase_kinds: int | None = None
    # The S10 attribution record schema the harness declares in `capabilities`; None for one that predates it.
    attribution_schema: int | None = None
    # The echo-timeline schema the harness declares in `capabilities`; None for one that predates it.
    echo_timeline_schema: int | None = None
    # The guard-correlation contract the harness declares in `capabilities`; None for one that predates it.
    guard_correlation_schema: int | None = None

    def cap(self, variant: str) -> int | None:
        """This variant's short-mode run cap, or None when it has none."""
        return dict(self.run_caps).get(variant)


def capped_runs(scenario: Scenario, variant: str, requested: int, short: bool) -> int:
    """Valid runs a set takes: min(requested, cap) under --short (the PR budget), else every requested run."""
    cap = scenario.cap(variant)
    if not short or cap is None:
        # When: a release comparison, or an uncapped variant, runs everything it asked for.
        return requested
    return min(requested, cap)


def _run_caps(entry: dict, variants: Sequence[str]) -> tuple[tuple[str, int], ...] | None:
    """An entry's `run_caps` as pairs: absent is none; anything but listed variants mapped to counts >= 1 is None."""
    caps = entry.get("run_caps", {})
    if not isinstance(caps, dict):
        return None
    if not all(variant in variants and perf_latency_schema._is_int(cap) and cap >= 1 for variant, cap in caps.items()):
        return None
    return tuple(caps.items())


def build_argv(example: str, release: bool, counters: bool = False,
               features: Sequence[str] = ()) -> tuple[str, ...]:
    """Return the locked build of one harness example; Cargo's JSON messages name the built binary.

    `features` are the perf features a tree supports, in `PERF_FEATURES` order; `counters` alone is
    perf-counters, for a tree that declares only it.
    """
    profile = ("--release",) if release else ()
    chosen = tuple(features) or ((perf_harness_sources.COUNTERS_FEATURE,) if counters else ())
    features = ("--features", ",".join(chosen)) if chosen else ()
    return ("cargo", "build", "--locked", *profile, "-p", "sonicterm-app", "--example", example,
            "--message-format=json-render-diagnostics", *features)


def build_passed(result) -> bool:
    """Whether a build step finished: PASS, or exit 0 after the gate cleaned a compiler's lingering helper.

    The gate reports CLEANED_NOT_NATURAL only for its reviewed compile-only steps, with verified custody.
    """
    return result.status == "PASS" or (result.status == "CLEANED_NOT_NATURAL" and result.exit_code == 0)


def artifact_executable(log_text: str, example: str) -> Path | None:
    """Return the executable Cargo reported for an example, whatever target directory it chose."""
    found = None
    for line in log_text.splitlines():
        if not line.startswith("{"):
            continue
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if not isinstance(message, dict) or message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target")
        if (isinstance(target, dict) and target.get("name") == example
                and "example" in (target.get("kind") or []) and isinstance(message.get("executable"), str)):
            found = Path(message["executable"])
    return found


def read_log(log_path: Path) -> str:
    """Read a step log as text; undecodable bytes are replaced, never fatal."""
    try:
        return log_path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        return f"[perf-compare] cannot read {log_path}: {error}"


def log_tail(text: str, count: int = 12) -> str:
    """Return the last human-readable lines of a step log, without Cargo's JSON messages."""
    lines = [line for line in text.splitlines() if line.strip() and not line.startswith("{")]
    return "\n".join(lines[-count:])


# The latency split schemas this script can validate.
LATENCY_SPLIT_SCHEMAS = (1,)
# The phase-kind schemas this script can validate: every phase classified, transitions with endpoints, and each
# phase's presentation trace.
PHASE_KINDS_SCHEMAS = (1,)
# Every capability a harness may declare, with the values this script can validate; the split schema is required.
# The S10 attribution record schemas this script can validate.
ATTRIBUTION_SCHEMAS = (1,)
# The guard-correlation contract schemas this script can validate.
GUARD_CORRELATION_SCHEMAS = (1,)
HARNESS_CAPABILITIES = {"latency_split_schema": LATENCY_SPLIT_SCHEMAS, "phase_kinds": PHASE_KINDS_SCHEMAS,
                        "s10_attribution": ATTRIBUTION_SCHEMAS, "echo_timeline_schema": perf_timeline_schema.ECHO_TIMELINE_SCHEMAS,
                        "guard_correlation_schema": GUARD_CORRELATION_SCHEMAS}


def _harness_capabilities(data: dict) -> dict:
    """The list's capabilities: empty when the list has none (a harness that predates them), else a map of known
    keys, the split schema among them, each with a known integer value; any other shape, key or value is
    refused, because a contract this script does not know cannot be validated."""
    if "capabilities" not in data:
        return {}
    capabilities = data["capabilities"]
    if not isinstance(capabilities, dict) or "latency_split_schema" not in capabilities \
            or not set(capabilities) <= set(HARNESS_CAPABILITIES):
        raise ValueError(f"scenario list capabilities {capabilities!r} are not a map of "
                         f"{sorted(HARNESS_CAPABILITIES)} with latency_split_schema")
    for name, value in capabilities.items():
        if not perf_latency_schema._is_int(value) or value not in HARNESS_CAPABILITIES[name]:
            raise ValueError(f"scenario list {name} is {value!r}, not one of {HARNESS_CAPABILITIES[name]}")
    return dict(capabilities)


def parse_scenario_list(text: str) -> list[Scenario]:
    """Parse `--list` output: the JSON object line among the launcher's header and footer."""
    data = None
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped.startswith("{"):
            continue
        try:
            candidate = json.loads(stripped)
        except ValueError:
            continue
        if isinstance(candidate, dict) and "scenarios" in candidate:
            data = candidate
    if data is None:
        raise ValueError("the harness printed no scenario list")
    if not perf_latency_schema._is_int(data.get("schema_version")) or data["schema_version"] != perf_settings.SCHEMA_VERSION:
        raise ValueError(f"scenario list schema_version is {data.get('schema_version')!r}")
    capabilities = _harness_capabilities(data)
    entries = data.get("scenarios")
    if not isinstance(entries, list) or not entries:
        raise ValueError("the scenario list is empty")
    scenarios = []
    for entry in entries:
        variants = entry.get("variants") if isinstance(entry, dict) else None
        if (not isinstance(entry, dict) or not isinstance(entry.get("id"), str) or not _NAME.fullmatch(entry["id"])
                or not isinstance(variants, list) or not variants
                or not all(isinstance(variant, str) and _NAME.fullmatch(variant) for variant in variants)
                or not isinstance(entry.get("title"), str)
                or not (perf_latency_schema._is_int(entry.get("timeout_s")) and entry["timeout_s"] > 0)
                or not (perf_latency_schema._is_int(entry.get("short_timeout_s")) and entry["short_timeout_s"] > 0)):
            raise ValueError(f"malformed scenario entry {entry!r}")
        caps = _run_caps(entry, variants)
        if caps is None:
            raise ValueError(f"malformed scenario entry {entry!r}")
        scenarios.append(Scenario(entry["id"], tuple(variants), entry["title"], entry["timeout_s"],
                                  entry["short_timeout_s"], caps, capabilities.get("latency_split_schema"),
                                  capabilities.get("phase_kinds"), capabilities.get("s10_attribution"),
                                  capabilities.get("echo_timeline_schema"),
                                  capabilities.get("guard_correlation_schema")))
    if len({scenario.id for scenario in scenarios}) != len(scenarios):
        raise ValueError("the scenario list repeats an id")
    return scenarios


def select_scenarios(requested: Sequence[str], scenarios: Sequence[Scenario]) -> list[tuple[str, str]]:
    """Expand `--scenario` values in the order given; `all` is every default variant, and repeats are dropped."""
    if not requested:
        raise ValueError("no scenario selected")
    by_id = {scenario.id: scenario for scenario in scenarios}
    selected: list[tuple[str, str]] = []
    for value in requested:
        if value == "all":
            missing = [scenario.id for scenario in scenarios if "default" not in scenario.variants]
            if missing:
                raise ValueError(f"scenarios without a default variant: {', '.join(missing)}")
            pairs = [(scenario.id, "default") for scenario in scenarios]
        else:
            scenario_id, _separator, variant = value.partition("/")
            variant = variant or "default"
            if scenario_id not in by_id or variant not in by_id[scenario_id].variants:
                raise ValueError(f"unknown scenario {value!r}")
            pairs = [(scenario_id, variant)]
        selected.extend(pair for pair in pairs if pair not in selected)
    return selected


def select_laps_scenarios(requested: Sequence[str], scenarios: Sequence[Scenario],
                          selected: Sequence[tuple[str, str]]) -> list[tuple[str, str]]:
    """Expand `--laps-scenario` values: a bare ID is its default variant, and each must be listed and selected."""
    by_id = {scenario.id: scenario for scenario in scenarios}
    chosen: list[tuple[str, str]] = []
    for value in requested:
        scenario_id, _separator, variant = value.partition("/")
        pair = (scenario_id, variant or "default")
        if scenario_id not in by_id or pair[1] not in by_id[scenario_id].variants:
            raise ValueError(f"--laps-scenario names an unknown scenario {value!r}")
        if pair not in selected:
            raise ValueError(f"--laps-scenario {value!r} is not selected by --scenario")
        if pair not in chosen:
            chosen.append(pair)
    return chosen


def harness_argv(binary: Path, scenario_id: str, variant: str, harness_hash: str, scratch: Path, *,
                 short: bool = False, laps: bool = False, counters: bool = False) -> tuple[str, ...]:
    """Return one managed harness run's command line; `counters` makes the harness force the gate on."""
    flags = (("--short",) if short else ()) + (("--laps",) if laps else ()) + (("--counters",) if counters else ())
    return (str(binary), "--run", scenario_id, "--variant", variant, *flags, "--managed",
            "--harness-hash", harness_hash, str(scratch))


def harness_environment(base: Mapping[str, str]) -> dict[str, str]:
    """Return a run's environment: NO_COLOR and RUST_LOG are dropped; HOME and the rest are kept."""
    return {key: value for key, value in base.items() if key not in ("NO_COLOR", "RUST_LOG")}


def new_scratch_path(temp_root: Path, scenario_id: str, variant: str) -> Path:
    """Return a path under the OS temp directory that does not exist yet; the harness creates it."""
    for _attempt in range(100):
        candidate = temp_root / f"sonicterm-perf-{scenario_id}-{variant}-{os.urandom(6).hex()}"
        if not os.path.lexists(candidate):
            return candidate
    raise OSError(f"no unused scratch path under {temp_root}")


def run_timeout_s(scenario: Scenario, smoke: bool, short: bool = False) -> int:
    """Return a run's run_step bound: the scenario timeout plus a margin, capped in the smoke.

    A short comparison run (`--short`) uses the scenario's short timeout, without the smoke's cap.
    """
    if smoke:
        return min(scenario.short_timeout_s + RUN_MARGIN_S, SMOKE_RUN_CAP_S)
    if short:
        return scenario.short_timeout_s + RUN_MARGIN_S
    return scenario.timeout_s + RUN_MARGIN_S


FONT_SUFFIXES = (".ttf", ".otf", ".ttc")
LINUX_SHARED_ASSETS = Path("/usr/share/sonicterm/assets")


def resolve_asset_dir(binary: Path, cwd: Path, platform: str = sys.platform,
                      exists: Callable[[Path], bool] = os.path.exists) -> Path:
    """Mirror sonicterm-cfg's asset_dir() for a harness binary started in `cwd`.

    Packaged paths win: a macOS bundle's Contents/Resources/assets, then assets beside the
    executable, then Linux's /usr/share/sonicterm/assets. Only then the nearest `assets` among
    the working directory and its ancestors, or `<cwd>/assets` when there is none.
    """
    executable_dir = binary.parent
    candidates = [executable_dir / "assets"]
    if executable_dir.parent != executable_dir:
        candidates.insert(0, executable_dir.parent / "Resources" / "assets")
    for candidate in candidates:
        if exists(candidate):
            return candidate
    if platform.startswith("linux") and exists(LINUX_SHARED_ASSETS):
        return LINUX_SHARED_ASSETS
    for directory in (cwd, *cwd.parents):
        if exists(directory / "assets"):
            return directory / "assets"
    return cwd / "assets"


def asset_problem(binary: Path, source_root: Path, platform: str = sys.platform) -> str | None:
    """Explain why a harness started in its source tree would not load that tree's assets, or return None.

    The App loads fonts, themes and keymaps from asset_dir(), so the directory it resolves must be
    the tree's own `assets` and hold its tracked fonts; an ancestor's or a packaged one belongs to
    another tree, and none at all makes the App substitute another font.
    """
    tree = Path(os.path.realpath(source_root))
    expected = tree / "assets"
    resolved = resolve_asset_dir(binary, tree, platform)
    if resolved != expected:
        return f"asset_dir() would resolve {resolved}, not the tree's own {expected}"
    fonts = expected / "fonts"
    if not fonts.is_dir() or not any(entry.suffix.lower() in FONT_SUFFIXES for entry in fonts.iterdir()):
        return f"{fonts} holds no font, so the App would load another font"
    return None


# --- Run watcher: harness pid, sessions, checkpoints and the deadline case -----------------

FOOTPRINT_BINARY = "/usr/bin/footprint"
