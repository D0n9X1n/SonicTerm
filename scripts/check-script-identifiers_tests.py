#!/usr/bin/env python3
"""Contract tests for scripts/check-script-identifiers.py."""

from __future__ import annotations

import ast
from contextlib import contextmanager
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

_HERE = Path(__file__).resolve().parent
_CHECKER_PATH = _HERE / "check-script-identifiers.py"

_spec = importlib.util.spec_from_file_location("check_script_identifiers", _CHECKER_PATH)
checker = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = checker
_spec.loader.exec_module(checker)

# Git variables that would point a fixture command at another repository or index.
_GIT_REDIRECTS = ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_COMMON_DIR")

# A fixture allowlist: `id` is allowed and every other short name is reported.
CONFIG = 'allowed-idents-below-min-chars = ["id"]\n'


def dedented(text: str) -> str:
    """Return a dedented fixture that starts at its first line."""
    return textwrap.dedent(text).lstrip("\n")


def clean_env() -> dict[str, str]:
    """Return the environment without the variables that redirect Git away from a fixture."""
    return {name: value for name, value in os.environ.items() if name not in _GIT_REDIRECTS}


@contextmanager
def repository(tracked: dict[str, str | bytes], untracked: dict[str, str | bytes] | None = None):
    """Create a Git fixture whose `tracked` files are in the index and whose `untracked` files are not."""
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        environment = clean_env()
        subprocess.run(["git", "init", "-q", str(root)], check=True, env=environment,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        for name, content in {**tracked, **(untracked or {})}.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            # Bytes keep a fixture's exact line endings; Path.write_text(newline=) needs Python 3.10.
            path.write_bytes(content if isinstance(content, bytes) else content.encode("utf-8"))
        if tracked:
            subprocess.run(["git", "-C", str(root), "add", "--", *tracked], check=True, env=environment,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        yield root


def run_checker(root: Path) -> subprocess.CompletedProcess:
    """Run the checker as a real process, so the exit status is the one the gate step sees."""
    return subprocess.run(
        [sys.executable, str(_CHECKER_PATH), "--root", str(root)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8",
        env=clean_env(), check=False, timeout=60,
    )


# Every binding form the rule covers, one short name per binding.
FORMS = dedented('''
    import json as js
    from os import path as op
    aa, [bb, *cc] = 1, [2, 3]
    dd += 1
    ee: int = 1
    ff: str
    (gg := 5)
    hh = ii = 0
    for jj, kk in []: pass
    with open("a") as ll, open("b") as (mm, nn): pass
    try: pass
    except OSError as er: pass
    def fn(pa, /, pb=1, *va, kw, **kx): return lambda la, lb=2, *lv, **lk: 0
    class Zz: pass
    async def af(ap):
        async for ar in ap: pass
        async with ap as aw: pass
    [ca for ca in [] for cb in []]
    {da: db for da, db in []}
    {sa for sa in []}
    list(ga for ga in [])
''')

EXPECTED_FORMS = [
    (1, "js"), (2, "op"), (3, "aa"), (3, "bb"), (3, "cc"), (4, "dd"), (5, "ee"), (6, "ff"),
    (7, "gg"), (8, "hh"), (8, "ii"), (9, "jj"), (9, "kk"), (10, "ll"), (10, "mm"), (10, "nn"),
    (12, "er"), (13, "fn"), (13, "kw"), (13, "kx"), (13, "la"), (13, "lb"), (13, "lk"), (13, "lv"),
    (13, "pa"), (13, "pb"), (13, "va"), (14, "Zz"), (15, "af"), (15, "ap"), (16, "ar"), (17, "aw"),
    (18, "ca"), (18, "cb"), (19, "da"), (19, "db"), (20, "sa"), (21, "ga"),
]

# Uses, attribute and subscript stores, call keywords and unaliased imports bind no name the rule owns.
USES = dedented('''
    import os
    from json import loads
    from os import sep as separator
    global gl
    config.xy = 1
    for config.xy in []: pass
    table[kv] = 2
    del table[kv], gl
    call(op=os.sep, js=loads("1"))
    value = "ab = 1"
''')

# `_`, `_`-prefixed names, self and cls are exempt; `id` passes only while the allowlist names it.
EXEMPT = dedented('''
    class Holder:
        def method(self, _unused, __private=None):
            _ = [_ for _ in []]
            _cache = 1
        @classmethod
        def build(cls, id=None):
            return cls
''')


class AllowlistReaderTests(unittest.TestCase):
    """read_allowlist returns exactly the top-level list that Clippy reads from clippy.toml."""

    def test_reads_a_single_line_array(self):
        # The repository writes the list on one line; that spelling parses to exactly its entries.
        text = 'min-ident-chars-threshold = 2\nallowed-idents-below-min-chars = ["id", "\'a", "\'_"]\n'
        self.assertEqual(checker.read_allowlist(text), frozenset({"id", "'a", "'_"}))

    def test_reads_a_multi_line_array_with_comments_and_crlf(self):
        # Comments, blank lines, a trailing comma and CRLF endings are layout, not entries.
        text = ("# header\r\nallowed-idents-below-min-chars = [ # abbreviations\r\n"
                '  "id",\r\n\r\n  # words\r\n  "to",\r\n]\r\n')
        self.assertEqual(checker.read_allowlist(text), frozenset({"id", "to"}))

    def test_decodes_basic_and_literal_strings(self):
        # Basic strings resolve escapes and literal strings keep backslashes, as TOML defines them.
        text = r'''allowed-idents-below-min-chars = ["\u0069d", 'o\k', "q\"", "b\\s"]''' + "\n"
        self.assertEqual(checker.read_allowlist(text), frozenset({"id", "o\\k", 'q"', "b\\s"}))

    def test_skips_other_values_whole_even_when_they_contain_the_key(self):
        # Strings, multi-line strings, arrays and inline tables of other keys cannot pose as the key or a table.
        text = dedented('''
            avoid-breaking-exported-api = false
            doc-valid-idents = ["allowed-idents-below-min-chars = [\\"x\\"]", 'a # b']
            banner = """
            allowed-idents-below-min-chars = ["y"]
            """
            disallowed-methods = [
            { path = "std::process::exit", reason = "use the logged exit" },
            ["[not a table]"],
            ]
            allowed-idents-below-min-chars = ["id"]
        ''')
        self.assertEqual(checker.read_allowlist(text), frozenset({"id"}))

    def test_rejects_a_missing_duplicated_or_table_scoped_key(self):
        # Only one top-level key is the allowlist; a missing, repeated or table-scoped one is refused, not guessed.
        for text in (
            "min-ident-chars-threshold = 2\n",
            'allowed-idents-below-min-chars = ["id"]\nallowed-idents-below-min-chars = ["to"]\n',
            '[lints]\nallowed-idents-below-min-chars = ["id"]\n',
        ):
            with self.subTest(text=text):
                with self.assertRaises(checker.ConfigError):
                    checker.read_allowlist(text)

    def test_rejects_anything_but_a_complete_array_of_strings(self):
        # A malformed value, a bad escape or Clippy's ".." default marker stops the scan instead of guessing a list.
        for text in (
            'allowed-idents-below-min-chars = "id"\n',
            'allowed-idents-below-min-chars = ["id", 2]\n',
            'allowed-idents-below-min-chars = ["id"\n',
            'allowed-idents-below-min-chars = ["id", ".."]\n',
            'allowed-idents-below-min-chars = ["id"] extra\n',
            'allowed-idents-below-min-chars = ["\\q"]\n',
        ):
            with self.subTest(text=text):
                with self.assertRaises(checker.ConfigError):
                    checker.read_allowlist(text)

    def test_repository_allowlist_replaces_clippys_default_list(self):
        # clippy.toml lists the lifetimes, omits Clippy's default single letters, and holds only two-character names.
        allowed = checker.read_allowlist((_HERE.parent / "clippy.toml").read_text(encoding="utf-8"))
        self.assertTrue({"'a", "'_"} <= allowed)
        self.assertFalse(allowed & {"i", "j", "x", "y", "z", "w", "n"})
        self.assertTrue(all(len(name) == 2 for name in allowed))


class ShortNameTests(unittest.TestCase):
    """is_short applies the rule's three shapes, the allowlist and the exemptions."""

    ALLOWED = frozenset({"id", "H1"})

    def test_flags_one_character_letter_digit_and_two_letter_names(self):
        # One character, one letter followed by digits, and two letters outside the allowlist are too short.
        for name in ("s", "n", "x", "C", "p1", "x10", "H2", "CI", "pr", "Zz"):
            with self.subTest(name=name):
                self.assertTrue(checker.is_short(name, self.ALLOWED))

    def test_keeps_allowlisted_exempt_and_descriptive_names(self):
        # Allowlisted and exempt names pass, and so do longer names that end in digits, such as sha256.
        for name in ("id", "H1", "_", "_x", "__private", "self", "cls", "row", "step", "sha256", "kernel32"):
            with self.subTest(name=name):
                self.assertFalse(checker.is_short(name, self.ALLOWED))


class BindingSiteTests(unittest.TestCase):
    """short_bindings finds each covered binding form and nothing else."""

    def test_every_binding_form_is_found_at_its_line(self):
        # Each form the rule names is reported once at its own line, sorted by line and then name.
        self.assertEqual(checker.short_bindings(FORMS, frozenset()), EXPECTED_FORMS)
        # A name bound twice on one line is one finding.
        self.assertEqual(checker.short_bindings("xy = [xy for xy in []]\n", frozenset()), [(1, "xy")])

    def test_uses_stores_keywords_and_plain_imports_are_not_bindings(self):
        # Short names that are read, attribute or subscript stores, call keywords or unaliased imports are not listed.
        self.assertEqual(checker.short_bindings(USES, frozenset()), [])

    def test_exempt_names_pass_and_the_allowlist_decides_the_rest(self):
        # The exemptions hold with an empty allowlist, and an allowlisted name is reported once it is removed.
        self.assertEqual(checker.short_bindings(EXEMPT, frozenset({"id"})), [])
        self.assertEqual(checker.short_bindings(EXEMPT, frozenset()), [(6, "id")])


class CommandLineTests(unittest.TestCase):
    """The command scans the tracked direct children of scripts/ and reports through its exit status."""

    def test_reports_tracked_scripts_only_and_exits_1(self):
        # Untracked, nested and out-of-directory files are outside the scan; findings print as path:line name.
        tracked = {
            "clippy.toml": CONFIG,
            "scripts/good.py": "id = 1\nrow_count = 2\n",
            "scripts/bad.py": "n = 1\ntotal = [p1 for p1 in []]\n",
            "scripts/nested/deep.py": "q = 1\n",
            "crates/build_helper.py": "r = 1\n",
        }
        with repository(tracked, {"scripts/untracked.py": "u = 1\n"}) as root:
            completed = run_checker(root)
        self.assertEqual(completed.returncode, 1, completed.stderr)
        self.assertEqual(completed.stdout, "scripts/bad.py:1 n\nscripts/bad.py:2 p1\n")
        self.assertIn("2 short identifiers", completed.stderr)

    def test_a_clean_scan_exits_0(self):
        # Allowlisted and descriptive names pass, and the summary counts the scanned scripts.
        with repository({"clippy.toml": CONFIG, "scripts/good.py": "id = 1\nrow_count = 2\n"}) as root:
            completed = run_checker(root)
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        self.assertEqual(completed.stdout, "check-script-identifiers: ok (1 script)\n")

    def test_crlf_and_bom_scripts_report_their_lines(self):
        # A Windows checkout's CRLF endings and a UTF-8 BOM neither hide a finding nor shift its line.
        tracked = {"clippy.toml": CONFIG, "scripts/windows.py": b"\xef\xbb\xbfvalue = 1\r\nk = 2\r\n"}
        with repository(tracked) as root:
            completed = run_checker(root)
        self.assertEqual(completed.returncode, 1, completed.stderr)
        self.assertEqual(completed.stdout, "scripts/windows.py:2 k\n")

    def test_an_incomplete_scan_exits_2(self):
        # A scan that cannot read its config, its file list or a tracked script fails closed instead of passing.
        good = "row_count = 1\n"
        cases = {
            "missing config": ({"scripts/good.py": good}, None, "clippy.toml"),
            "missing key": ({"clippy.toml": "min-ident-chars-threshold = 2\n", "scripts/good.py": good},
                            None, "allowed-idents-below-min-chars"),
            "syntax error": ({"clippy.toml": CONFIG, "scripts/broken.py": "def broken(:\n"},
                             None, "scripts/broken.py:1"),
            "deleted script": ({"clippy.toml": CONFIG, "scripts/gone.py": good}, "scripts/gone.py",
                               "scripts/gone.py"),
            "no scripts": ({"clippy.toml": CONFIG}, None, "no tracked scripts"),
        }
        for label, (tracked, removed, expected) in cases.items():
            with self.subTest(case=label):
                with repository(tracked) as root:
                    if removed is not None:
                        (root / removed).unlink()
                    completed = run_checker(root)
                self.assertEqual(completed.returncode, 2, completed.stdout + completed.stderr)
                self.assertIn(expected, completed.stderr)


class CompatibilityTests(unittest.TestCase):
    """The checker runs on Python 3.9, whose standard library has no tomllib."""

    def test_checker_source_is_python_3_9(self):
        # The 3.9 grammar must accept the checker, and it must not import tomllib, which 3.9 and 3.10 lack.
        tree = ast.parse(_CHECKER_PATH.read_text(encoding="utf-8"), filename=str(_CHECKER_PATH),
                         feature_version=(3, 9))
        modules = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                modules.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                modules.add(node.module.split(".")[0])
        self.assertNotIn("tomllib", modules)


if __name__ == "__main__":
    unittest.main(verbosity=2)
