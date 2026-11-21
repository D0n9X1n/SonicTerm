"""The Performance comparison workflow's partitions: which variants each comparison job measures, and how.

`scripts/perf-shards.json` holds three profiles. `core` is a pull request labelled `perf`, `full` one labelled
`perf-full`, and `release` a release tag. Each profile names its sampling (timed runs a side, counters runs a side
and, when a partition declares laps, laps runs a side) and, per platform, a list of partitions: a name, the
variants it runs, the variants that also run the separate laps set and, optionally, the only variants that run the
counters set (without that list every variant does). A bare scenario ID is its default variant. Every variant must
have a dataset it may run: a counters-only variant runs nothing outside the counters set, so its partition must
give it that set and may not ask laps of it.

perf.yml's plan job runs `python3 scripts/perf_shards.py <profile>`, which checks the whole file and prints
`macos=<json>` and `windows=<json>` for GITHUB_OUTPUT: each a list of matrix entries holding the platform, the
partition name and `selection`, the perf-compare.py options that select and sample that partition. A file that
breaks a rule exits 1, so no runner builds anything for a layout that cannot run.
"""

from __future__ import annotations
import argparse
import json
from pathlib import Path
import re
import sys
from typing import NamedTuple, Sequence

import perf_result_schema

SHARDS_FILE = Path(__file__).with_name("perf-shards.json")
SCHEMA_VERSION = 1
PROFILES = ("core", "full", "release")
PLATFORMS = ("macOS", "Windows")
# The GITHUB_OUTPUT key each platform's matrix is published under.
OUTPUT_KEYS = {"macOS": "macos", "Windows": "windows"}
# A partition name is part of a check name and an artifact name, so it stays a plain lowercase word.
_PARTITION_NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
# `S<n>` or `S<n>/<variant>`, the spelling perf-compare.py's --scenario takes.
_VARIANT = re.compile(r"S[0-9]+(?:/[a-z0-9]+(?:-[a-z0-9]+)*)?")
_PROFILE_KEYS = frozenset({"description", "runs", "counters_runs", "laps_runs", "partitions"})
_PARTITION_KEYS = frozenset({"name", "variants", "laps", "counters"})


class ShardError(ValueError):
    """perf-shards.json breaks a rule; the message names the profile, platform and partition."""


class Partition(NamedTuple):
    """One comparison job's share of a platform: its name, its variants in run order, its laps variants and the
    variants that run the counters set, None when every variant does."""

    name: str
    variants: tuple[str, ...]
    laps: tuple[str, ...]
    counters: tuple[str, ...] | None = None


class Profile(NamedTuple):
    """One profile's sampling and its partitions per platform."""

    name: str
    description: str
    runs: int
    counters_runs: int
    laps_runs: int | None
    partitions: dict[str, tuple[Partition, ...]]


def variant_label(variant: str) -> str:
    """The `ID/variant` label of a selection; a bare ID is its default variant."""
    return variant if "/" in variant else f"{variant}/default"


def permitted_datasets(partition: Partition, variant: str) -> tuple[str, ...]:
    """The sets one of a partition's variants runs: timed, laps and counters, as perf-compare.py plans them.

    A counters-only variant (`perf_result_schema.COUNTERS_ONLY_VARIANTS`) runs only the counters set, and only when
    the partition gives it one.
    """
    label = variant_label(variant)
    counters_only = tuple(label.split("/", 1)) in perf_result_schema.COUNTERS_ONLY_VARIANTS
    datasets = []
    if not counters_only:
        datasets.append("timed")
        if label in {variant_label(item) for item in partition.laps}:
            datasets.append("laps")
    if partition.counters is None or label in {variant_label(item) for item in partition.counters}:
        datasets.append("counters")
    return tuple(datasets)


def _positive(value: object, where: str) -> int:
    """An integer of at least 1; a boolean, which Python counts as an integer, is refused."""
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        raise ShardError(f"{where} is {value!r}, not a positive integer")
    return value


def _variant_list(value: object, where: str) -> tuple[str, ...]:
    """A nonempty list of distinct variant spellings."""
    if not isinstance(value, list) or not value:
        raise ShardError(f"{where} is not a nonempty list")
    for item in value:
        if not isinstance(item, str) or not _VARIANT.fullmatch(item):
            raise ShardError(f"{where} holds {item!r}, which is not S<n> or S<n>/<variant>")
    labels = [variant_label(item) for item in value]
    if len(set(labels)) != len(labels):
        raise ShardError(f"{where} names a variant twice")
    return tuple(value)


def _partition(entry: object, where: str) -> Partition:
    """Read one partition: its name, its variants and the laps variants, which it must also run."""
    if not isinstance(entry, dict) or not {"name", "variants"} <= set(entry) or not set(entry) <= _PARTITION_KEYS:
        raise ShardError(f"{where} needs name and variants, and takes only laps and counters besides")
    name = entry["name"]
    if not isinstance(name, str) or not _PARTITION_NAME.fullmatch(name):
        raise ShardError(f"{where} has name {name!r}, not a lowercase word with hyphens")
    variants = _variant_list(entry["variants"], f"{where} ({name}) variants")
    runs = {variant_label(run) for run in variants}
    laps: tuple[str, ...] = ()
    if "laps" in entry:
        laps = _variant_list(entry["laps"], f"{where} ({name}) laps")
        missing = [item for item in laps if variant_label(item) not in runs]
        if missing:
            raise ShardError(f"{where} ({name}) runs laps for {', '.join(missing)}, which it does not run")
    counters: tuple[str, ...] | None = None
    if "counters" in entry:
        # Nonempty: every partition of a profile requests the counters set, so each artifact's run-identity.json
        # records the same settings and perf-flags.py pools them.
        counters = _variant_list(entry["counters"], f"{where} ({name}) counters")
        missing = [item for item in counters if variant_label(item) not in runs]
        if missing:
            raise ShardError(f"{where} ({name}) runs counters for {', '.join(missing)}, which it does not run")
    partition = Partition(name, variants, laps, counters)
    for variant in variants:
        label = variant_label(variant)
        counters_only = tuple(label.split("/", 1)) in perf_result_schema.COUNTERS_ONLY_VARIANTS
        # When: a counters-only variant is asked for laps, the laps set would be dropped without a word.
        if counters_only and label in {variant_label(item) for item in laps}:
            raise ShardError(f"{where} ({name}) asks laps of {label}, which runs only the counters set")
        if not permitted_datasets(partition, variant):
            raise ShardError(f"{where} ({name}) gives {label} no set to run: it runs only the counters set, so "
                             "name it in counters")
    return partition


def _profile(name: str, entry: object) -> Profile:
    """Read one profile; every variant runs once per platform, in a partition with a unique name."""
    if not isinstance(entry, dict) or not set(entry) <= _PROFILE_KEYS:
        raise ShardError(f"profile {name} takes only {', '.join(sorted(_PROFILE_KEYS))}")
    for key in ("description", "runs", "counters_runs", "partitions"):
        if key not in entry:
            raise ShardError(f"profile {name} has no {key}")
    if not isinstance(entry["description"], str) or not entry["description"].strip():
        raise ShardError(f"profile {name} has no description")
    runs = _positive(entry["runs"], f"profile {name} runs")
    counters_runs = _positive(entry["counters_runs"], f"profile {name} counters_runs")
    laps_runs = _positive(entry["laps_runs"], f"profile {name} laps_runs") if "laps_runs" in entry else None
    platforms = entry["partitions"]
    if not isinstance(platforms, dict) or set(platforms) != set(PLATFORMS):
        raise ShardError(f"profile {name} partitions must name exactly {', '.join(PLATFORMS)}")
    partitions: dict[str, tuple[Partition, ...]] = {}
    for platform in PLATFORMS:
        entries = platforms[platform]
        if not isinstance(entries, list) or not entries:
            raise ShardError(f"profile {name} {platform} has no partitions")
        read = tuple(_partition(item, f"profile {name} {platform} partition {index + 1}")
                     for index, item in enumerate(entries))
        names = [partition.name for partition in read]
        if len(set(names)) != len(names):
            raise ShardError(f"profile {name} {platform} names a partition twice")
        seen: dict[str, str] = {}
        for partition in read:
            for variant in partition.variants:
                label = variant_label(variant)
                if label in seen:
                    raise ShardError(f"profile {name} {platform} runs {label} in both {seen[label]} and "
                                     f"{partition.name}")
                seen[label] = partition.name
        partitions[platform] = read
    declares_laps = any(partition.laps for read in partitions.values() for partition in read)
    # When: laps_runs and the laps declarations disagree, one of them would be silently ignored.
    if declares_laps != (laps_runs is not None):
        raise ShardError(f"profile {name} needs laps_runs exactly when a partition declares laps")
    return Profile(name, entry["description"], runs, counters_runs, laps_runs, partitions)


def load_profiles(path: Path = SHARDS_FILE) -> dict[str, Profile]:
    """Read and check perf-shards.json; any broken rule raises ShardError."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ShardError(f"cannot read {path}: {error}") from error
    if not isinstance(document, dict) or set(document) != {"schema_version", "profiles"}:
        raise ShardError(f"{path.name} must hold exactly schema_version and profiles")
    if document["schema_version"] != SCHEMA_VERSION:
        raise ShardError(f"{path.name} schema_version {document['schema_version']!r} is not {SCHEMA_VERSION}")
    profiles = document["profiles"]
    if not isinstance(profiles, dict) or set(profiles) != set(PROFILES):
        raise ShardError(f"{path.name} profiles must be exactly {', '.join(PROFILES)}")
    return {name: _profile(name, profiles[name]) for name in PROFILES}


def partition_arguments(profile: Profile, partition: Partition) -> list[str]:
    """The perf-compare.py options that select and sample one partition; the run length is the workflow's."""
    arguments = ["--scenario", *partition.variants, "--runs", str(profile.runs)]
    if partition.counters is None:
        arguments += ["--counters"]
    else:
        arguments += ["--counters-scenario", *partition.counters]
    arguments += ["--counters-runs", str(profile.counters_runs)]
    for variant in partition.laps:
        arguments += ["--laps-scenario", variant]
    if partition.laps:
        arguments += ["--laps-runs", str(profile.laps_runs)]
    return arguments


def matrix(profile: Profile, platform: str) -> list[dict[str, str]]:
    """A platform's matrix entries; `selection` splits on spaces, since no variant or option holds one."""
    return [{"platform": platform, "partition": partition.name,
             "selection": " ".join(partition_arguments(profile, partition))}
            for partition in profile.partitions[platform]]


def main(argv: Sequence[str] | None = None) -> int:
    """Print the chosen profile's matrix per platform as GITHUB_OUTPUT lines; 1 when the file breaks a rule."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("profile", choices=PROFILES, help="the profile whose partitions the comparison jobs run")
    parser.add_argument("--shards", type=Path, default=SHARDS_FILE, help="the partition file (default: %(default)s)")
    args = parser.parse_args(argv)
    try:
        profiles = load_profiles(args.shards)
    except ShardError as error:
        print(f"perf-shards: {error}", file=sys.stderr)
        return 1
    for platform in PLATFORMS:
        print(f"{OUTPUT_KEYS[platform]}={json.dumps(matrix(profiles[args.profile], platform), separators=(',', ':'))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
