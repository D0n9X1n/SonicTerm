#!/usr/bin/env python3
"""Contract tests for check-history-references.py: what it flags, what it leaves as data, and its exits."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location("history_references", Path(__file__).with_name(
    "check-history-references.py"))
history_references = importlib.util.module_from_spec(_SPEC)
# Registered before it runs, as an import would be: `dataclasses` resolves the module's annotations through it.
sys.modules[_SPEC.name] = history_references
_SPEC.loader.exec_module(history_references)


def flagged_lines(path: str, source: str) -> list[int]:
    """The line numbers the checker flags in `source` read as `path`."""
    return [finding.line for finding in history_references.scan_file(path, source)]


class PatternTests(unittest.TestCase):
    """Each citation form is flagged; colours, GUIDs, versions and upstream links are not."""

    def test_issue_pr_and_run_citations_are_flagged(self):
        # Every form a reason should not take: hash, named and linked numbers, and CI run or job ids.
        for text in ("# see #1607 for why", "// fixed in issue 1607", "# PR 1890 moved it",
                     "// https://github.com/D0n9X1n/SonicTerm/pull/1670", "# run 42970532300 measured it",
                     "// (run 37128040244): 23.7 min", "# job 121555774360 line 9", "# actions/runs/36087392875",
                     "# ids 37318119545, 37325514262"):
            with self.subTest(text=text):
                suffix = ".rs" if text.startswith("//") else ".py"
                self.assertEqual(flagged_lines(f"src/module{suffix}", text + "\n"), [1])

    def test_data_that_looks_numeric_is_not_flagged(self):
        # Hex colours, GUID segments, versions, character references and upstream tracker links are not citations.
        for text in ('# bar_bg = "#101010"', "# accent #1a2b3c", "<!-- Guid='C1A21A01-0000-0000-0000-000000000105' -->",
                     "# v1.4.7 ships it", "// &#1234; is an entity", "# step 3 of 4",
                     "# (https://github.com/microsoft/terminal/issues/19926) upstream bug"):
            with self.subTest(text=text):
                suffix = {"<": ".wxs", "/": ".rs"}.get(text[0], ".py")
                self.assertEqual(flagged_lines(f"src/module{suffix}", text + "\n"), [])


class ScopeTests(unittest.TestCase):
    """Which files are read, and which parts of a test source are data."""

    def test_vendored_and_generated_sources_are_out_of_scope(self):
        # Upstream trees and the generated bindings keep their own text.
        for path in ("crates/sonicterm-harfbuzz/harfbuzz/src/hb.cc.rs", "crates/sonicterm-winit/src/lib.rs",
                     "crates/sonicterm-freetype/src/lib.rs", "assets/themes/a.json"):
            with self.subTest(path=path):
                self.assertFalse(history_references.in_scope(path))
        self.assertTrue(history_references.in_scope("wiki/Usage.md"))
        self.assertTrue(history_references.in_scope(".github/workflows/perf.yml"))

    def test_a_test_source_reads_comments_not_string_data(self):
        # A fixture run id in a test string is data; the same id in a test comment is a citation.
        source = 'RUN = "37118041050"\n# the run 37118041050 shows it\n'
        self.assertEqual(flagged_lines("scripts/perf_prebuilt_tests.py", source), [2])
        self.assertEqual(flagged_lines("crates/x/src/a_tests.rs", 'let run = "run 37118041050";\n'), [])

    def test_a_production_script_reads_its_strings(self):
        # Help text and report headings are prose a reader sees, so a script's strings are read.
        source = 'HEADING = "## attribution (#1607)"\n'
        self.assertEqual(flagged_lines("scripts/perf_report.py", source), [1])

    def test_python_docstrings_are_read_in_test_sources(self):
        # A test docstring describes the test, so it follows the rule like a comment.
        source = 'def test_case():\n    """#1584: the join of raw takes."""\n'
        self.assertEqual(flagged_lines("scripts/perf_guard_tests.py", source), [2])


class AllowedTests(unittest.TestCase):
    """The data exceptions cover only what they name, and an unused one fails."""

    def test_an_allowed_pattern_covers_only_its_file_and_text(self):
        # The exception for a recorded table does not excuse the same text elsewhere.
        finding = history_references.Finding("a.rs", 1, 'helper_row("job 121555774360 line 9")')
        other = history_references.Finding("b.rs", 1, "// job 121555774360 line 9")
        allowed = (history_references.Allowed("a.rs", r"job [0-9]{12} line", "recorded provenance"),)
        remaining, unused = history_references.apply_allowed([finding, other], allowed)
        self.assertEqual((remaining, unused), ([other], []))

    def test_an_exception_that_matches_nothing_is_reported(self):
        # An exception must not outlive its data.
        allowed = (history_references.Allowed("gone.rs", r".", "removed data"),)
        self.assertEqual(history_references.apply_allowed([], allowed), ([], list(allowed)))


class CommandTests(unittest.TestCase):
    """The command's exits over a real repository."""

    def make_repo(self, files: dict[str, str]) -> Path:
        """A git repository holding `files`, tracked."""
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(root)], check=False))
        for name, text in files.items():
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text(text, encoding="utf-8")
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        subprocess.run(["git", "-C", str(root), "add", "."], check=True)
        return root

    def run_check(self, root: Path) -> int:
        """Run main over `root` with no allowed exceptions; return its exit code."""
        saved = history_references.ALLOWED
        history_references.ALLOWED = ()
        try:
            return history_references.main(["--root", str(root)])
        finally:
            history_references.ALLOWED = saved

    def test_a_clean_tree_passes_and_a_citation_fails(self):
        # Exit 0 with nothing to report, exit 1 when a tracked comment cites an issue.
        self.assertEqual(self.run_check(self.make_repo({"src/a.rs": "// Why it is needed.\n"})), 0)
        self.assertEqual(self.run_check(self.make_repo({"src/a.rs": "// See #1607.\n"})), 1)

    def test_an_unreadable_listing_exits_two(self):
        # A directory that is not a repository cannot be listed, so the scan never passes.
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(root)], check=False))
        self.assertEqual(self.run_check(root), 2)


if __name__ == "__main__":
    unittest.main()
