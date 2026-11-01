#!/usr/bin/env python3
"""Require the pinned runtime pair and packaged-selection log from a Windows smoke."""
from __future__ import annotations

import argparse
import hashlib
from pathlib import Path

VERSION = "1.25.260930003"
PINS = {
    "conpty.dll": (119608, "feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a"),
    "OpenConsole.exe": (1089376, "3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586"),
}


def verify(directory: Path, logs: Path) -> None:
    """Fail if staged bytes differ or the smoke did not positively select the packaged runtime."""
    for name, (size, digest) in PINS.items():
        member = directory / name
        metadata = member.lstat()
        if not member.is_file() or metadata.st_size != size or getattr(metadata, "st_file_attributes", 0) & 0x400 or member.is_symlink():
            raise ValueError(f"invalid ConPTY runtime member: {name}")
        if hashlib.sha256(member.read_bytes()).hexdigest() != digest:
            raise ValueError(f"ConPTY runtime digest mismatch: {name}")
    records = [line for path in logs.glob("sonicterm.log*") if path.is_file()
               for line in path.read_text(encoding="utf-8", errors="replace").splitlines()
               if "Windows console runtime selected" in line]
    if len(records) != 1 or 'kind="packaged"' not in records[0] or f'version="{VERSION}"' not in records[0]:
        raise ValueError("smoke did not select exactly one pinned packaged ConPTY runtime")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime-dir", required=True, type=Path)
    parser.add_argument("--logs", required=True, type=Path)
    args = parser.parse_args()
    try:
        verify(args.runtime_dir, args.logs)
    except (OSError, ValueError) as error:
        parser.exit(1, f"ConPTY verification failed: {error}\n")
    print(f"ConPTY packaged runtime verified: {VERSION}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
