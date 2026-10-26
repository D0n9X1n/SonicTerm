#!/usr/bin/env python3
"""Subset Rec Mono St.Helens into the test-only fixture faces.

SonicTerm no longer ships Rec Mono St.Helens, but tests that need a fixed,
host-independent monospace face still load it from
`crates/sonicterm-font/test-fonts/rec-mono/`. Those files are subsets of the
four upstream faces (MOSconfig/recursive-code-config v1.2.2, SIL OFL 1.1, no
Reserved Font Name); every face keeps the codepoints listed in
`codepoints.txt`.

The list holds Basic Latin, Latin-1, box drawing, block elements, Powerline,
and every non-ASCII codepoint the upstream face covers that first-party Rust
sources, localized strings, themes, or keymaps use. The styled faces keep the
same list because tests measure bold and italic CJK as well as regular.
Every layout feature, the hinting programs, and the full name table are kept,
and nothing recalculates the head, hhea, OS/2, or post metrics, so cell
metrics, glyph advances, and ligatures match the upstream faces exactly.

Pass the directory that holds the upstream `RecMonoSt.Helens-*.ttf` files:

    python3 scripts/generate-rec-mono-fixture.py /path/to/rec-mono-st-helens

The output is deterministic for a given input: the subsetter keeps the
upstream head timestamps.
"""

from __future__ import annotations

import sys
from pathlib import Path

from fontTools import subset

FIXTURE_DIR = Path(__file__).resolve().parent.parent / "crates/sonicterm-font/test-fonts/rec-mono"
FACES = ["Regular", "Bold", "Italic", "BoldItalic"]


def read_codepoints(list_path: Path) -> list[int]:
    """The codepoints a list file names, one `U+XXXX` entry per line."""
    return [int(line.strip()[2:], 16) for line in list_path.read_text().splitlines() if line.strip()]


def subset_options() -> subset.Options:
    """Options that keep layout, hinting, names, and every metric of the upstream face."""
    options = subset.Options()
    options.layout_features = ["*"]
    options.name_IDs = ["*"]
    options.name_legacy = True
    options.name_languages = ["*"]
    options.hinting = True
    options.notdef_outline = True
    options.legacy_kern = True
    options.prune_unicode_ranges = False
    options.prune_codepage_ranges = False
    # PfEd is a FontForge editor table the subsetter cannot subset; no reader uses it.
    options.drop_tables = ["PfEd"]
    return options


def main(argv: list[str]) -> int:
    """Write each fixture face from the upstream face in `argv[1]`."""
    if len(argv) != 2:
        print(f"usage: {argv[0]} <directory with RecMonoSt.Helens-*.ttf>", file=sys.stderr)
        return 2
    source_dir = Path(argv[1])
    codepoints = read_codepoints(FIXTURE_DIR / "codepoints.txt")
    for face in FACES:
        options = subset_options()
        font = subset.load_font(str(source_dir / f"RecMonoSt.Helens-{face}.ttf"), options)
        subsetter = subset.Subsetter(options)
        subsetter.populate(unicodes=codepoints)
        subsetter.subset(font)
        output = FIXTURE_DIR / f"RecMonoSt.Helens-{face}.ttf"
        subset.save_font(font, str(output), options)
        print(f"{output.name}: {output.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
