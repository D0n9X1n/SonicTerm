"""Gate files tests."""

from __future__ import annotations
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import gate_model
import gate_script_tests
import gate_test_fixture


class GitStateTests(unittest.TestCase):
    """The runner reports Git state around the run and never cleans the tree."""

    def test_a_dirty_tree_is_reported_as_pre_existing_and_left_unchanged(self):
        # Protect contributors' uncommitted work: dirt present before the run is reported, not
        # blamed on the run, and every byte, status line, and index entry survives it.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            (root / "tracked.txt").write_bytes(b"edited before the run\n")
            (root / "untracked.txt").write_bytes(b"untracked\n")
            (root / "staged.txt").write_bytes(b"staged\n")
            gate_test_fixture.git(root, "add", "staged.txt")
            tree = gate_test_fixture.tree_bytes(root)
            status = gate_test_fixture.git(root, "status", "--porcelain=v1", "--untracked-files=all").stdout
            index = gate_test_fixture.git(root, "ls-files", "--stage").stdout

            report, console = gate_test_fixture.run_quietly([gate_test_fixture.python_step("passes", "pass")], root, base / "logs")

            self.assertEqual(report.exit_code, 0)
            self.assertEqual(report.changes, ())
            self.assertEqual(
                {path for path, _code, _fingerprint in report.before.entries},
                {"tracked.txt", "untracked.txt", "staged.txt"},
            )
            self.assertEqual(gate_test_fixture.tree_bytes(root), tree)
            self.assertEqual(
                gate_test_fixture.git(root, "status", "--porcelain=v1", "--untracked-files=all").stdout, status
            )
            self.assertEqual(gate_test_fixture.git(root, "ls-files", "--stage").stdout, index)
            self.assertIn("pre-existing changes, present before the run and not caused by it (3)", console)
            self.assertIn("the run left tracked and untracked state unchanged", console)

    def test_a_change_made_during_the_run_fails_the_gate_and_is_kept(self):
        # Protect the verdict's meaning: a run that edited the tree it tested fails, and the
        # edit, including a further edit to an already-dirty file, stays for review.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            (root / "tracked.txt").write_bytes(b"edited before the run\n")
            code = (
                "import pathlib; "
                "pathlib.Path('tracked.txt').write_bytes(b'edited by the step\\n'); "
                "pathlib.Path('made-by-step.txt').write_bytes(b'new\\n')"
            )
            report, console = gate_test_fixture.run_quietly([gate_test_fixture.python_step("edits", code)], root, base / "logs")

            self.assertEqual(report.results[0].status, gate_model.PASS)
            self.assertEqual(report.exit_code, 1)
            changes = "\n".join(report.changes)
            self.assertRegex(changes, r"tracked\.txt: M file 0[0-7]{3} sha256:\S+ -> M file 0[0-7]{3} sha256:")
            self.assertIn("made-by-step.txt: clean -> ??", changes)
            self.assertEqual((root / "tracked.txt").read_bytes(), b"edited by the step\n")
            self.assertTrue((root / "made-by-step.txt").is_file())
            self.assertIn("nothing was reverted", console)
            self.assertIn("verdict=FAIL", console)

    def test_a_mode_change_to_a_dirty_file_fails_the_gate(self):
        # Protect the snapshot from a chmod that Git's status cannot show: the file is already
        # modified, so its porcelain line stays ` M` and only the fingerprint's mode changes.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            tracked = root / "tracked.txt"
            tracked.write_bytes(b"edited before the run\n")
            code = (
                "import os, stat; "
                "os.chmod('tracked.txt', stat.S_IMODE(os.stat('tracked.txt').st_mode) ^ stat.S_IWUSR)"
            )
            try:
                report, _console = gate_test_fixture.run_quietly([gate_test_fixture.python_step("chmods", code)], root, base / "logs")
            finally:
                # Restore write access so the temporary directory can be removed on every host.
                tracked.chmod(0o644)

            self.assertEqual(report.results[0].status, gate_model.PASS)
            self.assertEqual(report.exit_code, 1)
            self.assertEqual(len(report.changes), 1, report.changes)
            self.assertRegex(report.changes[0], r"^tracked\.txt: M file 0[0-7]{3} .* -> M file 0[0-7]{3} ")

    def test_a_tracked_edit_fails_the_gate_wherever_the_logs_go(self):
        # Protect the snapshot from --log-dir: only the runner's own untracked logs and summaries
        # are left out, so an edit under a tracked log directory still fails the gate.
        for name, relative in (
            ("outside the tree", None),
            ("untracked directory", Path("logs")),
            ("tracked source directory", Path("src")),
            ("under a tracked directory", Path("src") / "logs"),
        ):
            with self.subTest(log_dir=name), tempfile.TemporaryDirectory() as directory:
                base = Path(directory)
                root = gate_test_fixture.fixture_repository(base)
                (root / "src").mkdir()
                (root / "src" / "module.txt").write_bytes(b"committed\n")
                gate_test_fixture.git(root, "add", "src/module.txt")
                gate_test_fixture.commit(root, "source")
                logs = base / "logs" if relative is None else root / relative

                # A passing run leaves logs behind; neither they nor the next run's own logs
                # count as a change.
                clean, _console = gate_test_fixture.run_quietly([gate_test_fixture.python_step("passes", "pass")], root, logs)
                self.assertEqual((clean.exit_code, clean.changes), (0, ()))

                code = (
                    "import pathlib; "
                    "pathlib.Path('src/module.txt').write_bytes(b'edited by the step\\n'); "
                    "pathlib.Path('src/new.txt').write_bytes(b'new\\n')"
                )
                report, _console = gate_test_fixture.run_quietly([gate_test_fixture.python_step("edits", code)], root, logs)
                self.assertEqual(report.results[0].status, gate_model.PASS)
                self.assertEqual(report.exit_code, 1)
                changes = "\n".join(report.changes)
                self.assertIn("src/module.txt: clean -> M file", changes)
                self.assertIn("src/new.txt: clean -> ?? file", changes)

    def test_a_tracked_or_aliased_runner_output_is_refused_before_any_step(self):
        # Protect the final audit: the summaries are written after the last snapshot, so an
        # output path that Git tracks, or a link to tracked content, is refused before a step
        # runs instead of being overwritten under a PASS that reports unchanged state.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = gate_test_fixture.fixture_repository(base)
            (root / "logs").mkdir()
            (root / "logs" / "summary.txt").write_bytes(b"tracked summary\n")
            gate_test_fixture.git(root, "add", "logs/summary.txt")
            gate_test_fixture.commit(root, "track a summary")
            cases = {"tracked summary": root / "logs"}
            if os.name != "nt":
                # Links need no privilege on POSIX; they sit outside the tree and alias tracked.txt.
                symlinked = base / "symlinked"
                symlinked.mkdir()
                (symlinked / "summary.txt").symlink_to(root / "tracked.txt")
                hard = base / "hard"
                hard.mkdir()
                os.link(root / "tracked.txt", hard / "summary.json")
                cases.update({"symlink to tracked content": symlinked, "hard link to tracked content": hard})
            marker = base / "step-ran"
            for case, log_dir in cases.items():
                with self.subTest(case=case):
                    with self.assertRaisesRegex(ValueError, "runner output"):
                        gate_test_fixture.run_quietly([gate_test_fixture.python_step("marks", gate_test_fixture.marker_code(marker))], root, log_dir)
                    self.assertFalse(marker.exists(), "a step ran before the refusal")
                    self.assertEqual((root / "tracked.txt").read_bytes(), b"committed\n")
                    self.assertEqual((root / "logs" / "summary.txt").read_bytes(), b"tracked summary\n")
            # The command line refuses the same log directory with exit 2 before any step runs.
            completed = subprocess.run(
                [sys.executable, str(gate_test_fixture.ROOT / "scripts" / "local-gate.py"), "--root", str(root),
                 "--log-dir", str(root / "logs"), "--step", "no-raw-exit"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60, check=False,
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn(b"is tracked by Git", completed.stderr)
            self.assertEqual((root / "logs" / "summary.txt").read_bytes(), b"tracked summary\n")

    @gate_test_fixture.POSIX_ONLY
    def test_a_link_that_appears_at_an_output_during_the_run_fails_it(self):
        # Protect the final audit from links a step plants mid-run: a symlink or hard link at a
        # summary or a later log, or a replaced log directory, is never written through, tracked
        # content stays intact, no tail is read through a link or a replaced directory, and the
        # run fails; a replaced directory also stops the run before later steps and summaries.
        for case in ("summary symlink", "summary hard link", "next log symlink", "own log symlink",
                     "replaced log directory"):
            with self.subTest(case=case):
                with tempfile.TemporaryDirectory() as directory:
                    base = Path(directory)
                    root = gate_test_fixture.fixture_repository(base)
                    (root / "tracked-logs").mkdir()
                    (root / "tracked-logs" / "summary.txt").write_bytes(b"tracked summary\n")
                    # Unrelated tracked content under the log name of the replaced-directory step.
                    (root / "tracked-logs" / "01-moves.log").write_bytes(b"unrelated tracked log\n")
                    gate_test_fixture.git(root, "add", "tracked-logs/summary.txt", "tracked-logs/01-moves.log")
                    gate_test_fixture.commit(root, "track a summary")
                    logs = base / "logs"
                    tracked = str(root / "tracked.txt")
                    if case == "summary symlink":
                        target = logs / "summary.txt"
                        steps = [gate_test_fixture.python_step("plants", f"import os; os.symlink({tracked!r}, {str(target)!r})")]
                    elif case == "summary hard link":
                        target = logs / "summary.json"
                        steps = [gate_test_fixture.python_step("plants", f"import os; os.link({tracked!r}, {str(target)!r})")]
                    elif case == "next log symlink":
                        target = logs / "02-second.log"
                        steps = [gate_test_fixture.python_step("plants", f"import os; os.symlink({tracked!r}, {str(target)!r})"),
                                 gate_test_fixture.python_step("second", "pass")]
                    elif case == "own log symlink":
                        target = logs / "01-swaps.log"
                        code = (f"import os, sys; os.unlink({str(target)!r}); "
                                f"os.symlink({tracked!r}, {str(target)!r}); sys.exit(1)")
                        steps = [gate_test_fixture.python_step("swaps", code)]
                    else:
                        # The step fails after the swap, so its tail would be read through the new directory.
                        target = logs
                        code = (f"import os, sys; os.rename({str(logs)!r}, {str(base / 'logs-moved')!r}); "
                                f"os.symlink({str(root / 'tracked-logs')!r}, {str(logs)!r}); sys.exit(1)")
                        steps = [gate_test_fixture.python_step("moves", code), gate_test_fixture.python_step("after", gate_test_fixture.marker_code(base / "after"))]
                    report, console = gate_test_fixture.run_quietly(steps, root, logs)

                    self.assertEqual((root / "tracked.txt").read_bytes(), b"committed\n")
                    self.assertEqual((root / "tracked-logs" / "summary.txt").read_bytes(), b"tracked summary\n")
                    self.assertNotIn("committed", console)
                    self.assertNotIn("unrelated tracked log", console)
                    tracked_log = (root / "tracked-logs" / "01-moves.log").read_bytes()
                    self.assertEqual(tracked_log, b"unrelated tracked log\n")
                    self.assertEqual(report.exit_code, 1)
                    self.assertIn(str(target), "\n".join(report.output_problems))
                    if case in ("summary symlink", "summary hard link", "next log symlink"):
                        # The runner replaced the link with its own file instead of writing through it.
                        self.assertFalse(target.is_symlink())
                        self.assertEqual(os.lstat(target).st_nlink, 1)
                    if case == "replaced log directory":
                        # The run stopped: the later step did not run and no summary was written anywhere.
                        self.assertEqual([result.status for result in report.results], [gate_model.FAIL])
                        self.assertFalse((base / "after").exists())
                        self.assertFalse((base / "logs-moved" / "summary.txt").exists())
                        for where in (base / "logs-moved", root / "tracked-logs"):
                            self.assertFalse((where / "summary.json").exists())

    @gate_test_fixture.POSIX_ONLY
    def test_a_hung_fixture_git_is_killed_at_its_deadline(self):
        # Protect the suite from a hung Git: the fixture helper kills and reaps Git's whole
        # tree at its deadline instead of waiting on it forever.
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            survivor = base / "background-survived"
            fake_git = f"#!/bin/sh\n(sleep 2; echo alive > '{survivor}') &\nexec sleep 30\n"
            bin_dir = gate_script_tests._write_tools(base, {"git": fake_git})
            path = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")
            started = time.monotonic()
            with mock.patch.dict(os.environ, {"PATH": path}):
                with self.assertRaises(subprocess.TimeoutExpired):
                    gate_test_fixture.git(base, "status", timeout=0.5)
            self.assertLess(time.monotonic() - started, 10)
            # Outlast the background child's sleep, so a survivor would have written its marker.
            time.sleep(2.5)
            self.assertFalse(survivor.exists(), "the hung git's background child outlived the kill")

    def test_outside_a_git_work_tree_state_is_unavailable_not_fatal(self):
        # Protect exported trees: without Git the run still completes and says what it lacks.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve() / "plain"
            root.mkdir()
            ceiling = {"GIT_CEILING_DIRECTORIES": str(root.parent)}
            with mock.patch.dict(os.environ, ceiling):
                report, console = gate_test_fixture.run_quietly(
                    [gate_test_fixture.python_step("passes", "pass")], root, Path(directory) / "logs"
                )

            self.assertFalse(report.before.available)
            self.assertEqual(report.exit_code, 0)
            self.assertIn("Git state unavailable", console)
