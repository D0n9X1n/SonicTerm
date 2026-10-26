#!/usr/bin/env python3
"""Bind helper-only glyph measurements to the exact checkout, runner, fonts and producer attempt."""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys

SPEC = importlib.util.spec_from_file_location("oracle_smoke", Path(__file__).with_name("native-smoke-runner.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)
ROWS = tuple(f"{fixture}-{dpi}.json" for fixture in ("S9", "S12") for dpi in (72, 144))


def command(root, *arguments):
    """Bound and reap even the small identity probes; failed provenance never produces an identity."""
    result = runner.run_command(arguments, root, 60, os.environ)
    if result.returncode:
        raise ValueError(f"identity probe {arguments}: {result.stderr.decode(errors='replace')}")
    return result.stdout


def digest_files(root, names):
    """Length-framed paths and bytes prevent concatenation ambiguity; missing files fail closed."""
    digest = hashlib.sha256()
    for name in sorted(names):
        for value in (name.encode(), (root / name).read_bytes()):
            digest.update(len(value).to_bytes(8, "big"))
            digest.update(value)
    return digest.hexdigest()


def identity(root, attempt):
    """Hash tracked source bytes, bundled fonts, and every system/user font, not image labels alone."""
    tracked = command(root, "git", "ls-files", "-z").decode().rstrip("\0").split("\0")
    names = {name: os.environ[name] for name in ("GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_REPOSITORY", "ImageOS", "ImageVersion")}
    if not all(names.values()):
        raise ValueError("empty workflow or runner identity")
    head = command(root, "git", "rev-parse", "HEAD").decode().strip()
    if head != names["GITHUB_SHA"] or not attempt.isdigit() or int(attempt) < 1:
        raise ValueError("checkout or producer attempt differs from this workflow")
    font_roots = [Path(os.environ["WINDIR"]) / "Fonts",
                  Path(os.environ["LOCALAPPDATA"]) / "Microsoft/Windows/Fonts"]
    fonts = {}
    # GDI can register file-backed faces outside the normal font directories; bind its selection map too.
    import winreg
    registrations = {}
    for scope, hive in (("machine", winreg.HKEY_LOCAL_MACHINE), ("user", winreg.HKEY_CURRENT_USER)):
        try:
            registry = winreg.OpenKey(hive, r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts")
        except FileNotFoundError:
            if scope == "machine":
                raise ValueError("machine font registry is missing")
            continue
        with registry:
            for index in range(winreg.QueryInfoKey(registry)[1]):
                name, value, kind = winreg.EnumValue(registry, index)
                if kind not in (winreg.REG_SZ, winreg.REG_EXPAND_SZ):
                    raise ValueError(f"unsupported font registration {name}")
                value = os.path.expandvars(value)
                font_path = Path(value)
                if not font_path.is_absolute():
                    font_path = font_roots[0 if scope == "machine" else 1] / font_path
                registrations[f"{scope}:{name}"] = hashlib.sha256(font_path.read_bytes()).hexdigest()
    for index, directory in enumerate(font_roots):
        entries = [str(file.relative_to(directory)) for file in directory.rglob("*") if file.is_file()]
        fonts[str(index)] = digest_files(directory, entries)
    if not list(font_roots[0].iterdir()):
        raise ValueError("system font inventory is empty")
    return {"schema": 1, "environment": names, "producer_attempt": attempt,
            "source": digest_files(root, tracked), "fonts": fonts, "font_registrations": registrations,
            "rustc": command(root, "rustc", "-vV").decode(),
            "cargo": command(root, "cargo", "-V").decode(),
            "rows": list(ROWS), "font_family": "Rec Mono St.Helens", "font_size_pt": 13.0,
            "dpi": [72, 144]}


def validate_rows(directory):
    """All four complete rows are mandatory; no partial producer can be published."""
    files = {}
    for name in ROWS:
        data = (directory / name).read_bytes()
        row = json.loads(data)
        fixture, dpi = name.removesuffix(".json").split("-")
        if not isinstance(row, dict) or row.get("schema") != 1 or row.get("fixture") != fixture or row.get("dpi") != int(dpi) \
                or row.get("family") != "Rec Mono St.Helens" or row.get("size") != "13.0" \
                or not isinstance(row.get("tiles"), dict) or not row["tiles"] or any(not isinstance(size, list) or len(size) != 2
                    or any(type(value) is not int or value <= 0 for value in size)
                    for size in row["tiles"].values()):
            raise ValueError(f"incomplete or misbound helper row: {name}")
        files[name] = hashlib.sha256(data).hexdigest()
    return files


def verify(directory, expected_identity, expected_digest):
    """Verify the out-of-band manifest digest, exact provenance and every payload hash."""
    raw = (directory / "manifest.json").read_bytes()
    if hashlib.sha256(raw).hexdigest() != expected_digest:
        raise ValueError("oracle manifest digest mismatch")
    manifest = json.loads(raw)
    if manifest != {"identity": expected_identity, "files": validate_rows(directory)}:
        raise ValueError("oracle source/font/image/toolchain/run/attempt or payload mismatch")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("prepare", "seal", "verify"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    try:
        directory = Path(os.environ["SONICTERM_GLYPH_ORACLE_EXPORT"] if args.mode != "verify"
                         else os.environ["SONICTERM_GLYPH_ORACLE_DIR"])
        attempt = os.environ["GITHUB_RUN_ATTEMPT"] if args.mode != "verify" else os.environ["ORACLE_ATTEMPT"]
        bound = identity(root, attempt)
        if args.mode == "prepare":
            directory.mkdir(parents=True, exist_ok=False)
            (directory / "identity.json").write_text(json.dumps(bound), encoding="utf-8")
        elif args.mode == "seal":
            if json.loads((directory / "identity.json").read_text(encoding="utf-8")) != bound:
                raise ValueError("producer inputs changed during measurement")
            raw = json.dumps({"identity": bound, "files": validate_rows(directory)}, sort_keys=True).encode()
            (directory / "manifest.json").write_bytes(raw)
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
                output.write(f"digest={hashlib.sha256(raw).hexdigest()}\nattempt={attempt}\n")
        else:
            digest = os.environ["ORACLE_DIGEST"]
            verify(directory, bound, digest)
            with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as output:
                output.write(f"SONICTERM_GLYPH_ORACLE_DIGEST={digest}\n")
    except (ValueError, OSError, KeyError) as error:
        print(f"glyph-oracle: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
