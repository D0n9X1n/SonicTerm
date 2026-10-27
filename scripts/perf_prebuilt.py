"""Perf prebuilt."""

from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
from typing import Callable, Iterable, Mapping, NamedTuple, Sequence

import perf_build_policy
import perf_focus
import perf_settings
_RELEASE_PROFILE = re.compile(r"\s*\[\s*profile\.release\s*\]\s*(?:#.*)?")
_LTO_KEY = re.compile(r"\s*lto\s*=\s*(.+?)\s*(?:#.*)?")
_FILE_CHUNK_BYTES = 1024 * 1024


def executable_name(example: str, platform_name: str = sys.platform) -> str:
    """The file name Cargo gives an example's executable on a platform."""
    return example + ".exe" if platform_name == "win32" else example


def file_sha256(path: Path) -> str:
    """The sha256 of a file's bytes, read in chunks."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(_FILE_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def release_lto(root: Path, environ: Mapping[str, str]) -> str:
    """The LTO setting a release build of `root` uses: the environment override, else `[profile.release]` lto.

    Returns `unset` when neither names one. Only the workspace manifest's own table is read.
    """
    override = environ.get("CARGO_PROFILE_RELEASE_LTO")
    if override is not None:
        return override
    try:
        lines = (root / "Cargo.toml").read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError):
        return "unset"
    in_release = False
    for line in lines:
        if line.lstrip().startswith("["):
            in_release = _RELEASE_PROFILE.fullmatch(line) is not None
            continue
        match = _LTO_KEY.fullmatch(line) if in_release else None
        if match:
            return match.group(1).strip('"\'')
    return "unset"


def build_identity(trees: Mapping[str, Path], features: Mapping[str, Sequence[str]], host_run: Callable,
                   environ: Mapping[str, str], harness_cfgs: Sequence[str] = (),
                   rustflags: Sequence[str] = ()) -> dict[str, object]:
    """What a build depends on beyond the SHAs: target, toolchain, profile, features and runner image.

    The producer records it in the manifest; each consumer derives its own from its own job and
    refuses an artifact that differs, so a toolchain or image rollover fails closed.
    """
    toolchain = {}
    for name, argv in (("rustc", ("rustc", "-vV")), ("cargo", ("cargo", "-V"))):
        record = host_run(argv)
        failure = perf_focus._command_failure(record)
        if failure:
            raise ValueError(f"cannot read the toolchain: {failure}: {record.stderr.strip()}")
        toolchain[name] = record.stdout.strip()
    target = next((line.split(":", 1)[1].strip() for line in toolchain["rustc"].splitlines()
                   if line.startswith("host:")), "")
    return {
        "target": target,
        "toolchain": toolchain,
        "profile": {"lto": {side: release_lto(trees[side], environ) for side in perf_settings.SIDES},
                    "overrides": perf_build_policy.release_profile_overrides(environ)},
        # Each side's cargo features, a list so a later feature joins without a schema change.
        "features": {side: list(features[side]) for side in perf_settings.SIDES},
        # The harness API cfgs both sides build with, and the compiler flags their builds compose at launch.
        "harness_cfgs": list(harness_cfgs),
        "harness_cfg_scope": "example-target-v1",
        "rustflags": list(rustflags),
        "runner_image": {"os": environ.get("ImageOS", ""), "version": environ.get("ImageVersion", "")},
    }


def publish_binaries(destination: Path, builds: Mapping[str, Mapping[str, object]], shas: Mapping[str, str],
                     digest: str, identity: Mapping[str, object], environ: Mapping[str, str]) -> int:
    """Copy each built binary under `destination/<side>/` and write manifest.json; print its sha256.

    Only executables and the manifest are published: each consumer runs a ref from its own tree, which
    supplies that ref's assets.
    """
    binaries: dict[str, dict[str, dict[str, str]]] = {}
    for side in perf_settings.SIDES:
        binaries[side] = {}
        for example, built in builds[side].items():
            relative = f"{side}/{executable_name(example)}"
            target = destination / relative
            if os.path.lexists(target):
                raise ValueError(f"{target} already exists; a producer never overwrites a published binary")
            target.parent.mkdir(parents=True, exist_ok=True)
            # copy2 keeps the executable bit, which the tarball then carries to the consumer.
            shutil.copy2(built, target)
            binaries[side][example] = {"path": relative, "sha256": file_sha256(target)}
    manifest = {"schema_version": perf_build_policy.PREBUILT_SCHEMA_VERSION, "run_id": environ.get("GITHUB_RUN_ID", ""),
                "run_attempt": environ.get("GITHUB_RUN_ATTEMPT", ""), "base_sha": shas["base"],
                "head_sha": shas["head"], "harness_hash": digest, "binaries": binaries, **identity}
    data = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8")
    (destination / perf_build_policy.MANIFEST_FILE).write_bytes(data)
    print(f"manifest_sha256={hashlib.sha256(data).hexdigest()}", flush=True)
    return perf_settings.EXIT_PASS


def _refuse(reason: str) -> ValueError:
    """The error for an artifact this job must not measure."""
    return ValueError(f"refusing the prebuilt binaries: {reason}")


def load_prebuilt(args: argparse.Namespace, shas: Mapping[str, str], digest: str, identity: Mapping[str, object],
                  examples: Sequence[str], copies: Path) -> dict[str, dict[str, Path]]:
    """Check the producer's binaries against this job and copy them under `copies/<side>/`.

    Refused, in order: a missing directory or manifest or another schema; a manifest whose sha256 is not
    the published one; another run or producer attempt; another base or head SHA; another harness hash;
    other features, harness cfgs or compiler flags; another target, toolchain or runner image; another profile; a binary that is missing,
    a symlink, not executable or misdigested, or an example a selected set needs that was not built. The
    caller then checks that each copy lists and finds its tree's assets.
    """
    directory = Path(args.prebuilt)
    if directory.is_symlink() or not directory.is_dir():
        raise _refuse(f"no directory {directory}")
    manifest_path = directory / perf_build_policy.MANIFEST_FILE
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise _refuse(f"no {perf_build_policy.MANIFEST_FILE} in {directory}")
    data = manifest_path.read_bytes()
    try:
        manifest = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as error:
        raise _refuse(f"{perf_build_policy.MANIFEST_FILE} is not JSON: {error}") from error
    if not isinstance(manifest, dict) or manifest.get("schema_version") != perf_build_policy.PREBUILT_SCHEMA_VERSION:
        found = manifest.get("schema_version") if isinstance(manifest, dict) else None
        raise _refuse(f"manifest schema {found!r} is not {perf_build_policy.PREBUILT_SCHEMA_VERSION}")
    actual = hashlib.sha256(data).hexdigest()
    if actual != args.prebuilt_manifest_sha256:
        raise _refuse(f"manifest sha256 {actual} is not the producer's {args.prebuilt_manifest_sha256}")
    if str(manifest.get("run_id")) != args.prebuilt_run_id:
        raise _refuse(f"manifest run id {manifest.get('run_id')} is not this run's {args.prebuilt_run_id}")
    if str(manifest.get("run_attempt")) != args.prebuilt_attempt:
        raise _refuse(f"manifest run attempt {manifest.get('run_attempt')} is not the producer's "
                      f"attempt {args.prebuilt_attempt}")
    for side in perf_settings.SIDES:
        if manifest.get(f"{side}_sha") != shas[side]:
            raise _refuse(f"{side} SHA {manifest.get(f'{side}_sha')} is not this job's {shas[side]}")
    if manifest.get("harness_hash") != digest:
        raise _refuse(f"harness hash {manifest.get('harness_hash')} is not this job's {digest}")
    checks = (("features", "features"), ("harness_cfgs", "harness cfgs"),
              ("harness_cfg_scope", "harness cfg scope"), ("rustflags", "compiler flags"),
              ("target", "target"), ("toolchain", "toolchain"), ("runner_image", "runner image"),
              ("profile", "profile"))
    # Cfg names and compiler flags are lists of strings, checked by type before equality, so a number or boolean
    # that Python would compare equal to another value is refused for its type.
    for key in ("harness_cfgs", "rustflags"):
        value = manifest.get(key)
        if not (isinstance(value, list) and all(isinstance(item, str) for item in value)):
            raise _refuse(f"{key} {value!r} is not a list of strings")
    for key, name in checks:
        if manifest.get(key) != identity[key]:
            raise _refuse(f"{name} {manifest.get(key)!r} is not this job's {identity[key]!r}")
    published = manifest.get("binaries") if isinstance(manifest.get("binaries"), dict) else {}
    builds: dict[str, dict[str, Path]] = {side: {} for side in perf_settings.SIDES}
    for side in perf_settings.SIDES:
        entries = published.get(side) if isinstance(published.get(side), dict) else {}
        if (directory / side).is_symlink():
            raise _refuse(f"{side}: {directory / side} is a symlink")
        for example in examples:
            entry = entries.get(example)
            relative = f"{side}/{executable_name(example)}"
            if not isinstance(entry, dict):
                raise _refuse(f"{side} {example}: the manifest has no such binary")
            if entry.get("path") != relative or not perf_build_policy._HEX_SHA256.fullmatch(str(entry.get("sha256"))):
                raise _refuse(f"{side} {example}: entry {entry!r} does not name {relative} and its sha256")
            source = directory / relative
            if source.is_symlink():
                raise _refuse(f"{side} {example}: {relative} is a symlink")
            if not source.is_file():
                raise _refuse(f"{side} {example}: {relative} is missing")
            # When: on Windows the executable bit does not exist, so only POSIX hosts check it.
            if os.name != "nt" and not os.access(source, os.X_OK):
                raise _refuse(f"{side} {example}: {relative} is not executable")
            found = file_sha256(source)
            if found != entry["sha256"]:
                raise _refuse(f"{side} {example}: {relative} sha256 digest {found} differs from the manifest's "
                              f"{entry['sha256']}")
            # The copy has no `assets` sibling, so asset_dir() resolves the run's tree, as for a local build.
            target = copies / side / executable_name(example)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            if file_sha256(target) != found:
                raise _refuse(f"{side} {example}: the copy at {target} differs from {relative}")
            builds[side][example] = target
    return builds
