"""Gate docs tests."""

from __future__ import annotations
import dataclasses
import io
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest

import gate_catalog
import gate_cli
import gate_docs
import gate_model
import gate_test_fixture


class DocParityTests(unittest.TestCase):
    """CLAUDE.md and both wiki files embed exactly the table's rendered form."""

    def test_each_documented_gate_equals_the_rendered_table(self):
        # Protect every documented copy of the gate from drifting from the runnable table.
        self.assertEqual(
            [name for name, _language in gate_docs.DOCUMENTS],
            ["CLAUDE.md", "wiki/Development-and-Release.md", "wiki/Development-and-Release-zh-CN.md"],
        )
        for name, language in gate_docs.DOCUMENTS:
            with self.subTest(document=name):
                text = (gate_test_fixture.ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(gate_docs.doc_parity_problems(name, text, language), [])

    def test_editing_one_documented_gate_line_alone_fails_parity(self):
        # Protect each copy separately: an edit to one document's gate line is caught.
        old = "| `fmt` | `cargo fmt --all --check` |"
        for name, language in gate_docs.DOCUMENTS:
            with self.subTest(document=name):
                text = (gate_test_fixture.ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(text.count(old), 1)
                mutated = text.replace(old, "| `fmt` | `cargo fmt --all` |")
                problems = gate_docs.doc_parity_problems(name, mutated, language)
                self.assertTrue(any("gate block differs from the table" in problem
                                    for problem in problems), problems)

    def test_editing_the_table_alone_fails_every_document(self):
        # Protect the other direction: a table change must be pasted into every document.
        fmt = next(step for step in gate_catalog.STEPS if step.id == "fmt")
        steps = tuple(
            dataclasses.replace(fmt, hosts=("macos",), ci_jobs=("macos-core",)) if step.id == "fmt"
            else step
            for step in gate_catalog.STEPS
        )
        for name, language in gate_docs.DOCUMENTS:
            with self.subTest(document=name):
                text = (gate_test_fixture.ROOT / name).read_text(encoding="utf-8")
                self.assertNotEqual(gate_docs.doc_parity_problems(name, text, language, steps), [])

    def test_the_block_markers_must_appear_exactly_once(self):
        # Protect extraction: a missing or duplicated block cannot pass as the gate.
        block = gate_docs.render_gate_block("en")
        prose = f"{gate_docs.INVOCATION}\n\n"
        self.assertEqual(gate_docs.doc_parity_problems("doc", prose + block, "en"), [])
        self.assertNotEqual(gate_docs.doc_parity_problems("doc", prose + block + "\n" + block, "en"), [])
        self.assertNotEqual(gate_docs.doc_parity_problems("doc", prose, "en"), [])
        self.assertNotEqual(gate_docs.doc_parity_problems("doc", block, "en"), [])

    def test_both_languages_render_the_same_rows(self):
        # Protect equivalent facts across the English and Chinese blocks.
        def rows(language):
            return [line for line in gate_docs.render_gate_block(language).splitlines() if line.startswith("| `")]

        english = rows("en")
        self.assertEqual(len(english), len(gate_catalog.STEPS))
        self.assertEqual([row.replace(", ", "、") for row in english], rows("zh-CN"))


class CommandLineTests(unittest.TestCase):
    """The CLI renders, lists, and rejects invalid selections without running a step."""

    def cli(self, *arguments: str, cwd: Path | None = None) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            [sys.executable, str(gate_test_fixture.ROOT / "scripts" / "local-gate.py"), *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
            cwd=cwd,
        )

    def test_render_prints_the_documented_block(self):
        # Protect the paste source that the docs parity tests compare against.
        for language in ("en", "zh-CN"):
            with self.subTest(language=language):
                completed = self.cli("--render", language)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                self.assertEqual(
                    completed.stdout.decode("utf-8").replace("\r\n", "\n"),
                    gate_docs.render_gate_block(language) + "\n",
                )

    def test_list_shows_another_hosts_steps(self):
        # Protect review of a host's gate from any other host.
        completed = self.cli("--list", "--host", "windows")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        listing = completed.stdout.decode("utf-8")
        self.assertIn("msi-validator-tests [local]", listing)
        self.assertIn(".\\scripts\\validate-windows-msi_tests.ps1", listing)
        self.assertNotIn("logic-coverage", listing)

    def test_invalid_selections_exit_2_without_running_a_step(self):
        # Protect callers from a typo or a foreign host silently running a different gate.
        detected = gate_cli.detect_host()
        foreign = next(host for host in gate_model.HOSTS if host != detected)
        for arguments in (("--host", foreign), ("--step", "no-such-step")):
            with self.subTest(arguments=arguments):
                completed = self.cli(*arguments)
                self.assertEqual(completed.returncode, 2, completed.stderr)

    def test_a_log_dir_at_or_above_the_root_is_refused(self):
        # Protect the snapshot: a log directory at or above the root would contain every path
        # it reads, so the runner exits 2 before it runs a step or writes a log.
        with tempfile.TemporaryDirectory() as directory:
            root = gate_test_fixture.fixture_repository(Path(directory))
            for log_dir in (".", "..", str(root)):
                with self.subTest(log_dir=log_dir):
                    completed = self.cli(
                        "--root", str(root), "--log-dir", log_dir, "--step", "no-raw-exit", cwd=root
                    )
                    self.assertEqual(completed.returncode, 2, completed.stderr)
                    self.assertIn(b"is the repository root or an ancestor of it", completed.stderr)
                    for parent in (root, root.parent):
                        self.assertFalse((parent / "summary.txt").exists())
                        self.assertFalse((parent / "01-no-raw-exit.log").exists())
            with self.assertRaises(ValueError):
                gate_cli.run_gate([gate_test_fixture.python_step("passes", "pass")], root, root, "test", console=io.StringIO())

    @gate_test_fixture.POSIX_ONLY
    def test_an_ignored_sigchld_is_refused_before_any_step(self):
        # Protect exit statuses: with SIGCHLD ignored the kernel can reap each step as it exits, so
        # a failing step could read as 0; the runner exits 2 before it runs a step or writes a log.
        with tempfile.TemporaryDirectory() as directory:
            root = gate_test_fixture.fixture_repository(Path(directory))
            logs = Path(directory) / "logs"
            completed = subprocess.run(
                [sys.executable, str(gate_test_fixture.ROOT / "scripts" / "local-gate.py"), "--root", str(root),
                 "--log-dir", str(logs), "--step", "no-raw-exit"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, check=False,
                preexec_fn=lambda: signal.signal(signal.SIGCHLD, signal.SIG_IGN),
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn(b"SIGCHLD is ignored", completed.stderr)
            self.assertFalse(logs.exists())
