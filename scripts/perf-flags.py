#!/usr/bin/env python3
"""Compare the candidate flags of two performance-comparison runs from their downloaded artifacts.

Each run directory holds one run's `perf-comparison-*` artifacts. Every artifact's run-identity.json binds its
accepted runs to the run, refs, harness, settings and flag-metric definitions; only attempts classified valid
count, a duplicated artifact counts once, and a rerun attempt replaces the earlier one. The flags are recomputed
from each accepted run's result.json with perf-compare.py's own rules; no Markdown is parsed.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
import importlib.util
import json
from pathlib import Path
import re
import sys
from types import SimpleNamespace

SPEC = importlib.util.spec_from_file_location("perf_compare_for_flags", Path(__file__).resolve().with_name(
    "perf-compare.py"))
compare = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = compare
SPEC.loader.exec_module(compare)

FLAG_METRICS_VERSION = compare.FLAG_METRICS_VERSION
# The flag-metric definitions this script implements; a run declaring any other version cannot be read by its rules.
SUPPORTED_FLAG_METRICS = (FLAG_METRICS_VERSION,)
EXIT_NOT_COMPARABLE = 2
# The identity fields every artifact of one run must share, compared after decoding: `guard_api` is the run's one
# effective cfg decision (null for an identity older than it), so two artifacts recording different decisions are
# refused before any duplicate attempt is dropped.
SHARED_IDENTITY = ("run_id", "head_sha", "base_sha", "harness_hash", "flag_metrics_version", "capabilities",
                   "guard_api")
# result.json's validator names hosts by sys.platform; run identities name them as the decision does.
VALIDATOR_PLATFORMS = {"macos": "darwin", "windows": "win32"}
# The files that make up one attempt's evidence; duplicates must agree on every one of them.
EVIDENCE_FILES = (compare.CLASSIFICATION_FILE, "outcome.json", "scratch/result.json")
ATTEMPT_DIRECTORY = re.compile(r"(\d+)-(base|head)")
SHA = re.compile(r"[0-9a-f]{40}")
HARNESS_HASH = re.compile(r"[0-9a-f]{64}")
LABEL = re.compile(r"[A-Za-z0-9]+/[A-Za-z0-9_-]+")
DATASETS = ("timed", "laps", "counters", "alloc")
SIDE_NAMES = ("base", "head")


class NotComparable(Exception):
    """The inputs cannot be bound to one run each, or the two runs cannot be compared."""


@dataclass
class LoadedRun:
    """One run's shared identity, settings per platform, and every flag check keyed (platform, label, dataset,
    phase, metric, statistic)."""

    root: Path
    identity: dict
    settings: dict = field(default_factory=dict)
    checks: dict = field(default_factory=dict)


def _read(path: Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise NotComparable(f"{path}: {error}") from error


def _settings_problem(settings: object) -> str | None:
    """Why a run's dataset settings cannot be compared, or None: two booleans, features per side, and a profile."""
    if not isinstance(settings, dict) or set(settings) != {"short", "counters", "features", "profile"}:
        return f"settings {settings!r} are not short, counters, features and profile"
    if not all(isinstance(settings[key], bool) for key in ("short", "counters")):
        return f"settings short and counters {settings['short']!r}, {settings['counters']!r} are not booleans"
    features = settings["features"]
    if not isinstance(features, dict) or set(features) != set(SIDE_NAMES) \
            or not all(isinstance(named, list) and all(isinstance(item, str) for item in named)
                       for named in features.values()):
        return f"settings features {features!r} do not list each side's features"
    if not isinstance(settings["profile"], dict):
        return f"settings profile {settings['profile']!r} is not an object"
    return None


def _inventory_problem(sets: object) -> str | None:
    """Why an artifact's set inventory cannot be read, or None: one entry per set, each side with a status and
    the attempt directories it accepted, and no side both ended blocked or failed and accepted a run."""
    if not isinstance(sets, list):
        return f"sets {sets!r} is not a list"
    seen = set()
    for entry in sets:
        if not isinstance(entry, dict) or not isinstance(entry.get("label"), str) \
                or not LABEL.fullmatch(entry["label"]) or entry.get("dataset") not in DATASETS:
            return f"set entry {entry!r} names no label and dataset"
        key = (entry["label"], entry["dataset"])
        if key in seen:
            return f"set {key} is listed twice"
        seen.add(key)
        for side_name in SIDE_NAMES:
            side = entry.get(side_name)
            if not isinstance(side, dict) or not isinstance(side.get("status"), str) \
                    or not isinstance(side.get("accepted"), list):
                return f"set {key} {side_name} {side!r} has no status and accepted list"
            named = side["accepted"]
            if not all(isinstance(name, str) and ATTEMPT_DIRECTORY.fullmatch(name) and name.endswith(side_name)
                       for name in named) or len(set(named)) != len(named):
                return f"set {key} {side_name} accepts {named!r}, not distinct {side_name} attempt directories"
            if side["status"] and named:
                return f"set {key} {side_name} ended {side['status']!r} but accepts {named!r}"
    return None


# Capabilities added after a recorded identity shape existed: an identity written before one was added lacks
# exactly that key, which decodes as null (not declared). Every other key is required.
ADDED_CAPABILITIES = ("guard_correlation_schema",)


def decode_capabilities(capabilities: object) -> tuple[dict | None, str | None]:
    """A run's recorded capabilities, decoded backward-compatibly: the current key set, or the previous one that
    lacks only the keys added since, each missing key normalized to null. Unknown keys, any other missing key and
    malformed values are refused. Returns the normalized map, or None with why it cannot be read."""
    known = compare.HARNESS_CAPABILITIES
    previous = set(known) - set(ADDED_CAPABILITIES)
    if not isinstance(capabilities, dict) or set(capabilities) not in (set(known), previous):
        return None, f"capabilities {capabilities!r} are not exactly {sorted(known)} (or the earlier {sorted(previous)})"
    normalized = {name: capabilities.get(name) for name in known}
    problem = _capabilities_problem(normalized)
    return (None, problem) if problem else (normalized, None)


def _capabilities_problem(capabilities: object) -> str | None:
    """Why a run's recorded capabilities cannot be read, or None: exactly the known keys, each null (the head did
    not declare it) or a value this script validates."""
    known = compare.HARNESS_CAPABILITIES
    if not isinstance(capabilities, dict) or set(capabilities) != set(known):
        return f"capabilities {capabilities!r} are not exactly {sorted(known)}"
    for name, value in capabilities.items():
        if value is not None and not (compare._is_int(value) and value in known[name]):
            return f"capability {name} {value!r} is not null or one of {known[name]}"
    if capabilities["echo_timeline_schema"] is not None and capabilities["latency_split_schema"] is None:
        # When: the timeline schema is declared without the split schema its parts and reasons extend.
        return "capability echo_timeline_schema is declared without latency_split_schema"
    return None


def _identity(path: Path) -> dict:
    """An artifact's run-identity.json, refused unless every field is present, typed and supported."""
    identity = _read(path)
    if not isinstance(identity, dict) or identity.get("schema_version") != compare.RUN_IDENTITY_SCHEMA:
        raise NotComparable(f"{path}: not a run identity of schema {compare.RUN_IDENTITY_SCHEMA}")
    attempt = identity.get("run_attempt")
    problems = [
        None if isinstance(identity.get("run_id"), str) else f"run_id {identity.get('run_id')!r} is not text",
        None if compare._is_int(attempt) and attempt >= 0 else f"run_attempt {attempt!r} is not a count",
        None if isinstance(identity.get("platform"), str) and identity["platform"]
        else f"platform {identity.get('platform')!r} is not named",
        *(None if isinstance(identity.get(key), str) and SHA.fullmatch(identity[key])
          else f"{key} {identity.get(key)!r} is not a commit SHA" for key in ("base_sha", "head_sha")),
        None if isinstance(identity.get("harness_hash"), str) and HARNESS_HASH.fullmatch(identity["harness_hash"])
        else f"harness_hash {identity.get('harness_hash')!r} is not a harness hash",
        None if compare._is_int(identity.get("flag_metrics_version"))
        and identity["flag_metrics_version"] in SUPPORTED_FLAG_METRICS
        else f"flag metrics v{identity.get('flag_metrics_version')!r} is not one of {SUPPORTED_FLAG_METRICS}",
        _settings_problem(identity.get("settings")),
        _inventory_problem(identity.get("sets")),
    ]
    capabilities, capability_problem = decode_capabilities(identity.get("capabilities"))
    problems.append(capability_problem)
    # guard_api is the effective guard-correlation cfg decision; an identity older than it has none (null). A
    # head that declares the guard contract records it, so availability is bound to the build, never inferred.
    guard_api = identity.get("guard_api")
    if guard_api is not None and not isinstance(guard_api, bool):
        problems.append(f"guard_api {guard_api!r} is not a boolean")
    elif capabilities is not None and capabilities["guard_correlation_schema"] is not None and guard_api is None:
        problems.append("guard_api is not recorded, but the head declares guard_correlation_schema")
    found = [problem for problem in problems if problem]
    if found:
        raise NotComparable(f"{path}: {'; '.join(found)}")
    return {**identity, "capabilities": capabilities, "guard_api": guard_api}


def _contained_file(artifact: Path, path: Path) -> Path:
    """`path`, refused unless it is a regular file reached without a symbolic link and inside `artifact`: a link
    could make one attempt's evidence another's, or reach outside the download."""
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise NotComparable(f"{path}: {error}") from error
    # Every component from the artifact down is checked, so neither an attempt directory nor a file can alias
    # another attempt's evidence or reach outside the download.
    linked = any(part.is_symlink() for part in [path, *path.parents] if part != part.parent
                 and artifact in (part, *part.parents))
    if linked or not resolved.is_file():
        raise NotComparable(f"{path}: not a regular file of {artifact} (a link, an escape or not a file)")
    return path


def _set_evidence(artifact: Path, label: str, dataset: str) -> dict[str, dict]:
    """Every physical attempt of one set: its directory name mapped to the raw bytes of each evidence file it has
    (None for an absent one). Each attempt must be a real directory inside the artifact with a final
    classification; an alias, an escape or an unreadable file is refused."""
    scenario, variant = label.split("/", 1)
    set_dir = artifact / "runs" / f"{scenario}-{variant}" / dataset
    attempts: dict[str, dict] = {}
    try:
        children = sorted(set_dir.iterdir()) if set_dir.exists() else []
    except OSError as error:
        raise NotComparable(f"{set_dir}: {error}") from error
    for child in children:
        if not child.is_dir() or not ATTEMPT_DIRECTORY.fullmatch(child.name):
            raise NotComparable(f"{child}: not an attempt directory of {label} {dataset}")
        files = {}
        for name in EVIDENCE_FILES:
            path = child / name
            if path.exists() or path.is_symlink():
                try:
                    files[name] = _contained_file(artifact, path).read_bytes()
                except OSError as error:
                    raise NotComparable(f"{path}: {error}") from error
            else:
                files[name] = None
        if files[compare.CLASSIFICATION_FILE] is None:
            raise NotComparable(f"{child}: no final classification")
        attempts[child.name] = files
    return attempts


def _unlisted_sets(artifact: Path, sets: list) -> list[str]:
    """The physical `runs/<scenario-variant>/<dataset>` directories of `artifact` that its inventory does not list.
    A listed set that never created a directory (a side blocked before any run) is not one of them."""
    listed = {(entry["label"].replace("/", "-", 1), entry["dataset"]) for entry in sets}
    runs = artifact / "runs"
    try:
        found = [(label_dir.name, set_dir.name) for label_dir in sorted(runs.iterdir())
                 for set_dir in sorted(label_dir.iterdir())] if runs.is_dir() else []
    except OSError as error:
        # When: a runs entry is not a readable directory, the artifact's sets cannot be enumerated.
        raise NotComparable(f"{runs}: {error}") from error
    return [f"{label_dir}/{set_dir}" for label_dir, set_dir in found if (label_dir, set_dir) not in listed]


def _parsed(where: str, raw: bytes | None) -> object:
    """One evidence file's JSON, refused when it is absent or not JSON."""
    if raw is None:
        raise NotComparable(f"{where} is missing")
    try:
        return json.loads(raw)
    except ValueError as error:
        raise NotComparable(f"{where}: {error}") from error


def _reconciled(set_dir: str, entry: dict, attempts: dict[str, dict]) -> None:
    """Refuse an inventory that does not match the attempts on disk: every attempt's final classification names
    its own side, and a healthy side accepts exactly its attempts classified valid. A blocked or failed side
    accepts none, whatever valid attempts it discarded."""
    valid: dict[str, set] = {side_name: set() for side_name in SIDE_NAMES}
    for name, files in attempts.items():
        side_name = ATTEMPT_DIRECTORY.fullmatch(name)[2]
        classification = _parsed(f"{set_dir}/{name} classification", files[compare.CLASSIFICATION_FILE])
        if not isinstance(classification, dict) or classification.get("side") != side_name \
                or not isinstance(classification.get("kind"), str):
            raise NotComparable(f"{set_dir}/{name}: final classification {classification!r} is not of its side")
        if classification["kind"] == "valid":
            valid[side_name].add(name)
    for side_name in SIDE_NAMES:
        accepted = set(entry[side_name]["accepted"])
        if not entry[side_name]["status"] and accepted != valid[side_name]:
            # When: a healthy side's inventory and its valid attempts differ, one of them is not the comparison's.
            raise NotComparable(f"{set_dir} {side_name}: inventory accepts {sorted(accepted)}, but the attempts "
                                f"classified valid are {sorted(valid[side_name])}")


def _accepted_result(where: str, identity: dict, label: str, dataset: str, side_name: str, files: dict) -> dict:
    """The result of one accepted attempt: its outcome must be a valid, exit-0 run of this label and side, and
    its result must pass perf-compare's own validator under the run's capabilities, then name this run."""
    scenario, variant = label.split("/", 1)
    outcome = _parsed(f"{where} outcome", files["outcome.json"])
    if not isinstance(outcome, dict) or outcome.get("kind") != "valid" or outcome.get("side") != side_name \
            or (outcome.get("scenario"), outcome.get("variant")) != (scenario, variant) \
            or not compare._is_int(outcome.get("exit_code")) or outcome["exit_code"] != 0:
        raise NotComparable(f"{where} has an outcome that is not a valid, exit-0 {side_name} run of {label}")
    result = _parsed(f"{where} result", files["scratch/result.json"])
    capabilities = identity["capabilities"]
    try:
        problems = compare.validate_result(
            result, identity["harness_hash"], 0, counters=dataset == "counters",
            partial_counters=side_name == "base",
            platform_name=VALIDATOR_PLATFORMS.get(identity["platform"], identity["platform"]),
            latency_split_schema=capabilities["latency_split_schema"], phase_kinds=capabilities["phase_kinds"],
            attribution_schema=capabilities["s10_attribution"],
            echo_timeline_schema=capabilities["echo_timeline_schema"],
            guard_correlation_schema=capabilities["guard_correlation_schema"], scope=(scenario, variant))
    except (TypeError, AttributeError, ValueError, KeyError, OverflowError) as error:
        # When: the validator itself cannot read the result, the evidence is malformed, never a crash.
        raise NotComparable(f"{where} result cannot be validated: {type(error).__name__}: {error}") from error
    if problems:
        raise NotComparable(f"{where} result breaks the result schema: {problems[0]}")
    if capabilities["guard_correlation_schema"] is not None:
        # When: the head declares the guard contract, the phases' availability must be the one the build's cfg and
        # the counter gate decide, by the same rule the live join applies; unavailable is valid, a mix is not.
        availability = compare.guard_availability(identity["guard_api"], compare.frame_counter_state(result))
        fields = [phase.get("guard_correlation") for phase in result["phases"] if isinstance(phase, dict)]
        problem = compare.guard_availability_problem(fields, availability)
        if problem:
            raise NotComparable(f"{where} result's guard correlation is inconsistent: {problem}")
    expected = {"status": "valid", "scenario": scenario, "variant": variant,
                "short": identity["settings"]["short"]}
    for key, value in expected.items():
        found = result.get(key)
        # A boolean must be a boolean: 1 == True, so equality alone would accept a number.
        if found != value or isinstance(found, bool) != isinstance(value, bool):
            raise NotComparable(f"{where} result {key} {found!r} is not {value!r}")
    if capabilities["phase_kinds"] is None and any("kind" in phase for phase in result["phases"]):
        raise NotComparable(f"{where} result records phase kinds its harness never declared")
    # The validator checks attribution records only under a declared schema, so an undeclared one is refused here.
    if capabilities["s10_attribution"] is None and any("s10_attribution" in phase for phase in result["phases"]):
        raise NotComparable(f"{where} result records S10 attribution its harness never declared")
    return result


@dataclass
class Evidence:
    """One run's validated evidence: its shared identity, its settings per platform, and each set's final accepted
    results keyed (platform, label, dataset), with the artifact, identity, inventory entry and raw attempt files
    they came from. Both this script and perf-compare's analysis modes read evidence only through it."""

    root: Path
    identity: dict
    settings: dict = field(default_factory=dict)
    sets: dict = field(default_factory=dict)


def load_evidence(root: Path) -> Evidence:
    """Load and validate one run's artifacts under `root`.

    Every artifact's identity, settings and capabilities are validated first. Each set's inventory is then
    reconciled with its attempts on disk; two copies of one attempt must agree on every evidence file. The latest
    run attempt's inventory replaces an earlier one's even when it accepted nothing, and only its accepted attempts
    are read, each through the full result validator."""
    identity_paths = sorted(Path(root).rglob(compare.RUN_IDENTITY_FILE))
    if not identity_paths:
        raise NotComparable(f"{root}: no {compare.RUN_IDENTITY_FILE}")
    identities = [(path.parent, _identity(path)) for path in identity_paths]
    shared = {key: identities[0][1][key] for key in SHARED_IDENTITY}
    loaded = Evidence(Path(root), shared)
    for path, identity in identities:
        differing = [key for key in SHARED_IDENTITY if identity[key] != shared[key]]
        if differing:
            raise NotComparable(f"{path}: {', '.join(differing)} differ from the run's other artifacts")
        settings = loaded.settings.setdefault(identity["platform"], identity["settings"])
        if settings != identity["settings"]:
            raise NotComparable(f"{path}: {identity['platform']} settings differ from the run's other artifacts")
    # (platform, label, dataset) -> run_attempt -> (artifact, entry, evidence); every set an artifact lists counts,
    # accepted runs or not, so a rerun's empty inventory replaces the attempt before it.
    inventories: dict[tuple, dict] = {}
    for artifact, identity in identities:
        unlisted = _unlisted_sets(artifact, identity["sets"])
        if unlisted:
            # When: a set ran but its inventory entry is gone, its replacement semantics cannot be known.
            raise NotComparable(f"{artifact}: sets on disk without an inventory entry: {', '.join(unlisted)}")
        for entry in identity["sets"]:
            key = (identity["platform"], entry["label"], entry["dataset"])
            evidence = _set_evidence(artifact, entry["label"], entry["dataset"])
            _reconciled(f"{artifact.name} {entry['label']} {entry['dataset']}", entry, evidence)
            attempts = inventories.setdefault(key, {})
            earlier = attempts.get(identity["run_attempt"])
            if earlier is not None:
                if (earlier[1], earlier[2]) != (entry, evidence):
                    # When: two copies of one attempt differ in any file, neither can be chosen by its name.
                    raise NotComparable(f"{artifact}: {key} attempt {identity['run_attempt']} has two copies "
                                        f"that disagree ({earlier[0].name})")
                continue
            attempts[identity["run_attempt"]] = (artifact, entry, evidence)
    for (platform, label, dataset), attempts in inventories.items():
        artifact, entry, evidence = attempts[max(attempts)]
        identity = dict(identities)[artifact]
        results = {side_name: [(name, _accepted_result(f"{artifact.name} {label} {dataset} {name}", identity, label,
                                                       dataset, side_name, evidence[name]))
                               for name in entry[side_name]["accepted"]]
                   for side_name in SIDE_NAMES}
        loaded.sets[(platform, label, dataset)] = SimpleNamespace(artifact=artifact, identity=identity, entry=entry,
                                                                  evidence=evidence, results=results)
    return loaded


def load_run(root: Path) -> LoadedRun:
    """Load one run's artifacts under `root` through load_evidence and compute its flag checks per platform. A
    blocked or failed side keeps its status and accepts no run."""
    evidence = load_evidence(root)
    loaded = LoadedRun(evidence.root, evidence.identity, evidence.settings)
    per_platform: dict[str, list] = {}
    for (platform, label, dataset), found in evidence.sets.items():
        sides = {side_name: compare.SideRuns([SimpleNamespace(result=result, evidence=found.artifact / "runs" / name)
                                              for name, result in found.results[side_name]],
                                             blocked=found.entry[side_name]["status"] or None)
                 for side_name in SIDE_NAMES}
        per_platform.setdefault(platform, []).append(compare.SetResult(label, dataset, sides["base"], sides["head"]))
    for platform, results in per_platform.items():
        for check in compare.flag_checks(results):
            loaded.checks[(platform, check.label, check.dataset, check.phase, check.metric, check.statistic)] = check
    return loaded


def _state(run: LoadedRun, key: tuple) -> str:
    check = run.checks.get(key)
    return "not checked" if check is None else "flagged" if check.flagged else "not flagged"


def compare_runs(first: LoadedRun, second: LoadedRun, *, allow_different_heads: bool = False) -> dict:
    """Split the two runs' flags into both runs, one run (the other checked it unflagged) and missing evidence
    (the other has no such check), each key mapped to its two states. Runs with different flag metrics are
    refused; different heads are refused unless allowed, and then their flags are listed side by side."""
    if first.identity["flag_metrics_version"] != second.identity["flag_metrics_version"]:
        raise NotComparable(f"flag metrics v{first.identity['flag_metrics_version']} and "
                            f"v{second.identity['flag_metrics_version']} differ")
    # The metric contract binds even runs of different heads: a platform both ran must use one dataset setting.
    for platform in sorted(set(first.settings) & set(second.settings)):
        if first.settings[platform] != second.settings[platform]:
            raise NotComparable(f"{platform} settings differ: {first.settings[platform]} and "
                                f"{second.settings[platform]}")
    if first.identity["head_sha"] == second.identity["head_sha"]:
        # When: the heads match, the runs must repeat one comparison: the same base and harness.
        differing = [key for key in ("base_sha", "harness_hash") if first.identity[key] != second.identity[key]]
        if differing:
            raise NotComparable(f"same head, but {', '.join(differing)} differ")
    if first.identity["head_sha"] != second.identity["head_sha"]:
        if not allow_different_heads:
            raise NotComparable(f"heads {first.identity['head_sha']} and {second.identity['head_sha']} differ; "
                                "pass --allow-different-heads to list their flags side by side")
        return {"side_by_side": [sorted(key for key, check in run.checks.items() if check.flagged)
                                 for run in (first, second)]}
    split: dict[str, dict] = {"both": {}, "one": {}, "missing": {}}
    flagged = {key for run in (first, second) for key, check in run.checks.items() if check.flagged}
    for key in sorted(flagged):
        states = (_state(first, key), _state(second, key))
        bucket = "both" if states == ("flagged", "flagged") else "missing" if "not checked" in states else "one"
        split[bucket][key] = states
    return split


def _key_text(key: tuple) -> str:
    return " ".join(str(part) for part in key if part)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("runs", nargs=2, type=Path, help="two directories of downloaded perf-comparison artifacts")
    parser.add_argument("--allow-different-heads", action="store_true",
                        help="list flags of runs of different heads side by side instead of refusing")
    args = parser.parse_args(argv)
    try:
        runs = [load_run(root) for root in args.runs]
        split = compare_runs(*runs, allow_different_heads=args.allow_different_heads)
    except NotComparable as error:
        print(f"[perf-flags] not comparable: {error}", file=sys.stderr)
        return EXIT_NOT_COMPARABLE
    for number, run in enumerate(runs, 1):
        identity = run.identity
        print(f"Run {number} ({run.root}): run {identity['run_id']}, head {identity['head_sha']}, "
              f"base {identity['base_sha']}, harness {identity['harness_hash']}, "
              f"flag metrics v{identity['flag_metrics_version']}, settings {json.dumps(run.settings, sort_keys=True)}")
    if "side_by_side" in split:
        for number, keys in enumerate(split["side_by_side"], 1):
            print(f"\nFlagged in run {number}:")
            print("\n".join(f"- {_key_text(key)}" for key in keys) or "- none")
        return 0
    for bucket, title in (("both", "Flagged in both runs"), ("one", "Flagged in one run"),
                          ("missing", "Evidence missing (flagged in one run, not checked in the other)")):
        print(f"\n{title}:")
        print("\n".join(f"- {_key_text(key)}: {states[0]} / {states[1]}" for key, states in split[bucket].items())
              or "- none")
    return 0


if __name__ == "__main__":
    sys.exit(main())
