#!/usr/bin/env python3
"""Validate and consolidate release assets without platform-specific publish logic."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

SCHEMA_VERSION = 1
HASH_PATTERN = re.compile(r"[0-9a-f]{64}\Z")
TAG_PATTERN = re.compile(r"v?([0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?)\Z")
RELEASE_SUFFIXES = (".dmg", ".msi", ".deb", ".tar.gz")
REQUIRED = {
    ("macos", "aarch64", "dmg"),
    ("macos", "x86_64", "dmg"),
    ("windows", "x86_64", "msi"),
    ("linux", "x86_64", "deb"),
    ("linux", "x86_64", "tar.gz"),
}


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"release assets: {message}")


def release_version(tag: str) -> str:
    matched = TAG_PATTERN.fullmatch(tag)
    if not matched:
        fail(f"invalid release tag {tag!r}")
    return matched.group(1)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_json(path: Path) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {path}: {error}")
    if not isinstance(value, dict):
        fail(f"{path} must contain a JSON object")
    return value


def check_version(args: argparse.Namespace) -> None:
    expected = release_version(args.tag)
    root = Path(args.repo_root).resolve()
    try:
        output = subprocess.check_output(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=root,
            text=True,
        )
        metadata = json.loads(output)
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        fail(f"cannot read Cargo workspace metadata: {error}")
    members = set(metadata["workspace_members"])
    versions = {
        package["name"]: package["version"]
        for package in metadata["packages"]
        if package["id"] in members
    }
    mismatched = sorted(name for name, version in versions.items() if version != expected)
    if mismatched:
        details = ", ".join(f"{name}={versions[name]}" for name in mismatched)
        fail(f"tag {args.tag} expects workspace version {expected}; mismatches: {details}")
    print(f"release tag {args.tag} matches {len(versions)} workspace packages")


def validate_flat_name(value: object, field: str) -> str:
    if not isinstance(value, str) or not value or Path(value).name != value or "/" in value or "\\" in value:
        fail(f"{field} must be one flat filename")
    if value in {".", ".."}:
        fail(f"{field} cannot be {value!r}")
    return value


def write_fragment(args: argparse.Namespace) -> None:
    release_version(args.tag)
    asset = Path(args.asset)
    if not asset.is_file():
        fail(f"asset does not exist: {asset}")
    name = validate_flat_name(asset.name, "asset name")
    fragment = {
        "schema_version": SCHEMA_VERSION,
        "tag": args.tag,
        "asset": {
            "name": name,
            "path": name,
            "platform": args.platform,
            "arch": args.arch,
            "kind": args.kind,
            "sha256": sha256(asset),
        },
    }
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(fragment, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def validated_fragment(path: Path, tag: str, dist: Path) -> dict:
    fragment = load_json(path)
    if fragment.get("schema_version") != SCHEMA_VERSION:
        fail(f"{path} has unsupported schema_version")
    if fragment.get("tag") != tag:
        fail(f"{path} tag does not match {tag}")
    asset = fragment.get("asset")
    if not isinstance(asset, dict):
        fail(f"{path} has no asset object")
    name = validate_flat_name(asset.get("name"), f"{path} asset.name")
    relative = validate_flat_name(asset.get("path"), f"{path} asset.path")
    if relative != name:
        fail(f"{path} asset.path must equal asset.name")
    for field in ("platform", "arch", "kind"):
        value = asset.get(field)
        if not isinstance(value, str) or not value or not re.fullmatch(r"[a-z0-9][a-z0-9.+_-]*", value):
            fail(f"{path} asset.{field} is invalid")
    digest = asset.get("sha256")
    if not isinstance(digest, str) or not HASH_PATTERN.fullmatch(digest):
        fail(f"{path} asset.sha256 is invalid")
    asset_path = dist / relative
    if not asset_path.is_file():
        fail(f"registered asset is missing: {asset_path}")
    actual = sha256(asset_path)
    if actual != digest:
        fail(f"registered hash differs for {asset_path}")
    return asset


def consolidate(args: argparse.Namespace) -> None:
    release_version(args.tag)
    dist = Path(args.dist)
    fragments = sorted(dist.glob("*.asset.json"))
    if not fragments:
        fail(f"no *.asset.json fragments in {dist}")
    assets = [validated_fragment(path, args.tag, dist) for path in fragments]
    names: set[str] = set()
    tuples: set[tuple[str, str, str]] = set()
    for asset in assets:
        name = asset["name"]
        key = (asset["platform"], asset["arch"], asset["kind"])
        if name in names:
            fail(f"duplicate asset name {name}")
        if key in tuples:
            fail(f"duplicate platform/arch/kind tuple {key}")
        names.add(name)
        tuples.add(key)
    missing = sorted(REQUIRED - tuples)
    if missing:
        fail(f"missing required asset tuples: {missing}")
    unregistered = sorted(
        path.name
        for path in dist.iterdir()
        if path.is_file()
        and path.name.endswith(RELEASE_SUFFIXES)
        and path.name not in names
    )
    if unregistered:
        fail(f"unregistered release-like files: {unregistered}")

    assets.sort(key=lambda asset: (asset["platform"], asset["arch"], asset["kind"], asset["name"]))
    manifest_path = dist / args.manifest
    manifest = {"schema_version": SCHEMA_VERSION, "tag": args.tag, "assets": assets}
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    checksum_entries = [(asset["name"], asset["sha256"]) for asset in assets]
    checksum_entries.append((manifest_path.name, sha256(manifest_path)))
    checksum_entries.sort()
    checksum_path = dist / args.checksums
    checksum_path.write_text(
        "".join(f"{digest}  {name}\n" for name, digest in checksum_entries),
        encoding="utf-8",
    )

    upload_paths = [dist / asset["name"] for asset in assets]
    upload_paths.extend([manifest_path, checksum_path])
    upload_path = dist / args.upload_list
    upload_path.write_text("".join(f"{path.as_posix()}\n" for path in upload_paths), encoding="utf-8")
    print(f"validated {len(assets)} release assets")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)

    version = commands.add_parser("check-version")
    version.add_argument("--tag", required=True)
    version.add_argument("--repo-root", default=".")
    version.set_defaults(handler=check_version)

    fragment = commands.add_parser("fragment")
    fragment.add_argument("--tag", required=True)
    fragment.add_argument("--asset", required=True)
    fragment.add_argument("--platform", required=True)
    fragment.add_argument("--arch", required=True)
    fragment.add_argument("--kind", required=True)
    fragment.add_argument("--output", required=True)
    fragment.set_defaults(handler=write_fragment)

    combined = commands.add_parser("consolidate")
    combined.add_argument("--tag", required=True)
    combined.add_argument("--dist", default="dist")
    combined.add_argument("--manifest", default="release-assets.json")
    combined.add_argument("--checksums", default="SHA256SUMS.txt")
    combined.add_argument("--upload-list", default="release-upload-paths.txt")
    combined.set_defaults(handler=consolidate)
    return root


def main() -> None:
    args = parser().parse_args()
    args.handler(args)


if __name__ == "__main__":
    main()
