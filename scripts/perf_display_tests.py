"""Perf display tests."""

from __future__ import annotations
from perf_test_fixture import perf
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import perf_home
import perf_host_info
import perf_host_tests
import perf_memory_tests
import perf_outcomes
import perf_outcomes_tests
import perf_result_schema
import perf_result_tests
import perf_scenarios
import perf_sets
import perf_settings
import perf_smoke
import perf_smoke_tests
import perf_watcher_tests


def display_run(refresh_mhz, scale=2.0, name="Built-in Display"):
    """A factory for a valid run measured on one display; a None rate means the rate was not reported."""
    def build(plan):
        monitor = {"name": name, "refresh_rate_millihertz": refresh_mhz, "scale_factor": scale}
        return perf_outcomes_tests.make_outcome(plan=plan, result=perf_result_tests.valid_result(monitor=monitor))
    return build


def no_display(plan):
    """A valid run whose result reported no measurement display."""
    return perf_outcomes_tests.make_outcome(plan=plan, result=perf_result_tests.valid_result(monitor=None))


class DisplayCheckTests(unittest.TestCase):
    def run_set(self, answers, runs=1, display=None):
        """Drive one run set whose answers are outcome factories; a side's last factory repeats."""
        calls = []
        plans = {side: perf_outcomes.RunPlan(perf_watcher_tests.IDLE_SCENARIO, "default", side, Path(f"/{side}"), perf_memory_tests.HARNESS_HASH)
                 for side in perf_settings.SIDES}

        def run_case(plan, evidence):
            calls.append(plan.side)
            queue = answers[plan.side]
            return (queue.pop(0) if len(queue) > 1 else queue[0])(plan)
        with contextlib.redirect_stdout(io.StringIO()):
            result = perf_sets.run_set("S1/default", plans, None, runs, run_case, Path("/e"), display=display)
        return result, calls

    def test_another_refresh_rate_or_scale_is_invalid_and_retried(self):
        # Every valid run must share the first valid run's refresh rate and scale; the reason names both displays.
        for other, named in ((display_run(75000), "Built-in Display, 75 Hz, scale 2"),
                             (display_run(60000, scale=1.0), "Built-in Display, 60 Hz, scale 1")):
            with self.subTest(named=named):
                result, calls = self.run_set({"base": [display_run(60000)], "head": [other, display_run(60000)]})
                self.assertEqual(calls, ["base", "head", "head"])
                _side, _evidence, kind, why = result.attempts[1]
                self.assertEqual(kind, "display")
                self.assertIn(named, why[0])
                self.assertIn("Built-in Display, 60 Hz, scale 2", why[0])
                self.assertEqual(perf_outcomes.compare_verdict(kind), "invalid")
                self.assertEqual(len(result.head.outcomes), 1)

    def test_another_display_is_rejected_even_at_the_same_rate_and_scale(self):
        # Display identity is a known difference too: runs on two named displays are not one measurement display.
        result, calls = self.run_set({"base": [display_run(60000, name="DELL U2720Q")],
                                      "head": [display_run(60000, name="LG HDR 4K"),
                                               display_run(60000, name="DELL U2720Q")]})
        self.assertEqual(calls, ["base", "head", "head"])
        _side, _evidence, kind, why = result.attempts[1]
        self.assertEqual(kind, "display")
        self.assertIn("LG HDR 4K", why[0])
        self.assertIn("DELL U2720Q", why[0])

    def test_an_unknown_rate_does_not_hide_a_known_difference(self):
        # Only what a run did not report goes unchecked: a known scale or name must still match the reference.
        result, calls = self.run_set({"base": [display_run(60000)],
                                      "head": [display_run(None, scale=1.0), display_run(60000)]})
        self.assertEqual(calls, ["base", "head", "head"])
        _side, _evidence, kind, why = result.attempts[1]
        self.assertEqual(kind, "display")
        self.assertIn("Built-in Display, unknown Hz, scale 1", why[0])
        for unchecked in (no_display, display_run(None, scale=2.0)):
            with self.subTest(unchecked=unchecked):
                result, _calls = self.run_set({"base": [display_run(60000)], "head": [unchecked]})
                self.assertEqual([attempt[2] for attempt in result.attempts], ["valid", "valid"])
        self.assertEqual(perf_sets.describe_display(perf_sets.display_of(perf_result_tests.valid_result(monitor=None))), "unknown")

    def test_a_reference_learns_a_rate_it_did_not_have(self):
        # A first run with no reported rate sets the reference; the next reported rate is learned, then enforced.
        display = perf_sets.DisplayReference()
        self.run_set({"base": [display_run(None)], "head": [display_run(60000)]}, display=display)
        self.assertEqual(display.monitor["refresh_rate_millihertz"], 60000)
        result, _calls = self.run_set({"base": [display_run(75000), display_run(60000)],
                                       "head": [display_run(60000)]}, display=display)
        self.assertEqual(result.attempts[0][2], "display")

    def test_the_reference_is_the_first_known_display_across_the_comparison(self):
        # One reference spans every run set of a comparison; a run that reported no display never sets it.
        display = perf_sets.DisplayReference()
        self.run_set({"base": [no_display], "head": [display_run(60000)]}, display=display)
        self.assertEqual(display.monitor["refresh_rate_millihertz"], 60000)
        result, calls = self.run_set({"base": [display_run(75000), display_run(60000)],
                                      "head": [display_run(60000)]}, display=display)
        self.assertEqual(calls, ["base", "head", "base"])
        self.assertEqual(result.attempts[0][2], "display")

    def test_the_smoke_does_not_check_the_display(self):
        # The smoke checks schema, focus safety and cleanup only, so runs on different displays still pass.
        rates = iter([60000, 75000])

        def run_case(plan, evidence):
            if plan.kill_at_go:
                return perf_smoke_tests.outcome_of("valid")(plan)
            return display_run(next(rates))(plan)
        with contextlib.redirect_stdout(io.StringIO()):
            code, reasons = perf_smoke.smoke_cases(perf_smoke_tests.SmokeTests.SCENARIOS, Path("/b"), perf_memory_tests.HARNESS_HASH, run_case, Path("/e"),
                                             host_platform="darwin")
        self.assertEqual((code, reasons), (0, []))

    def test_monitor_types_are_checked_when_present(self):
        # The display may be absent or null; when present, each field has its documented type.
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 60000, "scale_factor": 2.0}
        for accepted in (perf_result_tests.valid_result(), perf_result_tests.valid_result(monitor=None), perf_result_tests.valid_result(monitor=monitor),
                         perf_result_tests.valid_result(monitor=dict(monitor, name=None, refresh_rate_millihertz=None))):
            with self.subTest(accepted=accepted.get("monitor", "absent")):
                self.assertEqual(perf_result_schema.validate_result(accepted, perf_memory_tests.HARNESS_HASH, 0), [])
        for broken in ("60 Hz", dict(monitor, name=5), dict(monitor, refresh_rate_millihertz="60000"),
                       dict(monitor, refresh_rate_millihertz=True), dict(monitor, refresh_rate_millihertz=60000.0),
                       dict(monitor, scale_factor=None), {"name": "x", "scale_factor": 2.0}):
            with self.subTest(broken=broken):
                self.assertTrue(perf_result_schema.validate_result(perf_result_tests.valid_result(monitor=broken), perf_memory_tests.HARNESS_HASH, 0))

    def test_host_block_names_the_measurement_display(self):
        # The display the valid runs shared is printed; with none known it reads unknown.
        monitor = {"name": "Built-in Display", "refresh_rate_millihertz": 59940, "scale_factor": 2.0}
        self.assertIn("- Measurement display: Built-in Display, 59.94 Hz, scale 2",
                      perf_host_info.host_block(perf_host_tests.HostBlockTests.OUTPUTS, monitor))
        self.assertIn("- Measurement display: unknown", perf_host_info.host_block(perf_host_tests.HostBlockTests.OUTPUTS))
        unnamed = dict(monitor, name=None, refresh_rate_millihertz=75000, scale_factor=1.0)
        self.assertIn("- Measurement display: unnamed display, 75 Hz, scale 1", perf_host_info.host_block({}, unnamed))


class AssetCheckTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(os.path.realpath(temporary.name))
        self.tree = self.root / "checkout" / "target" / "perf-compare" / "work" / "head"
        self.tree.mkdir(parents=True)
        self.binary = self.root / "cargo-target" / "release" / "examples" / "perf_scenarios"
        self.binary.parent.mkdir(parents=True)

    def fonts(self, assets):
        """Give an assets directory the tracked font the harness configures."""
        (assets / "fonts").mkdir(parents=True)
        (assets / "fonts" / "SymbolsNerdFontMono-Regular.ttf").write_bytes(b"font")

    def problem(self):
        return perf_scenarios.asset_problem(self.binary, self.tree, platform="darwin")

    def test_the_trees_own_assets_with_fonts_pass(self):
        # A run started in the tree resolves that tree's assets and its tracked fonts.
        self.fonts(self.tree / "assets")
        self.assertIsNone(self.problem())

    def test_assets_found_only_above_the_tree_or_nowhere_are_refused(self):
        # An ancestor's assets would load another tree's fonts, and none at all falls back to another font.
        self.assertIn("assets", self.problem())
        self.fonts(self.root / "checkout" / "assets")
        self.assertIn(str(self.root / "checkout" / "assets"), self.problem())

    def test_assets_without_fonts_are_refused(self):
        # The App loads the configured font from assets/fonts.
        (self.tree / "assets").mkdir()
        self.assertIn("fonts", self.problem())

    def test_packaged_assets_beside_the_binary_take_precedence_and_are_refused(self):
        # asset_dir() prefers assets beside the executable, which are not the tree's own.
        self.fonts(self.tree / "assets")
        self.fonts(self.binary.parent / "assets")
        self.assertIn(str(self.binary.parent / "assets"), self.problem())


class SmokeSetupTests(unittest.TestCase):
    def test_a_checkout_without_assets_fails_before_any_run(self):
        # The smoke runs from the repository root; without its assets the App would load another font.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact = {"reason": "compiler-artifact", "target": {"name": "perf_scenarios", "kind": ["example"]},
                        "executable": str(root / "debug" / "perf_scenarios")}
            gate = perf_watcher_tests.FakeGate(lambda step: ("PASS", 0, json.dumps(artifact) + "\n"))
            gate.sigchld_problem = lambda: None
            gate.leader_watches = lambda: ("kqueue",)
            evidence = root / "evidence"
            evidence.mkdir()
            with mock.patch.object(perf_settings, "load_gate", return_value=gate), \
                    mock.patch.object(perf_settings, "ROOT", root / "checkout"):
                code, reasons = perf_smoke.run_smoke(evidence)
        self.assertEqual(code, perf_settings.EXIT_FAIL)
        self.assertTrue(any("assets" in reason for reason in reasons), reasons)
        self.assertEqual(len(gate.steps), 1)


class HomeSymlinkTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        self.home, self.elsewhere = root / ".sonicterm", root / "dot-config"
        self.home.mkdir()
        self.elsewhere.mkdir()
        # Later than every file and link the test creates, so only a changed entry or target is a candidate.
        self.sentinel_ns = perf.time.time_ns() + 10 ** 15

    def link(self, relative, target, directory=False):
        """Create a symlink under the home, or skip on a host that cannot create one."""
        path = self.home / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            path.symlink_to(target, target_is_directory=directory)
        except (OSError, NotImplementedError):
            self.skipTest("this host cannot create a symlink")
        return path

    def write(self, path, data):
        """Write a file with an old modification time."""
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        os.utime(path, ns=(1_000_000_000, 1_000_000_000))

    def violations(self, before):
        return perf_home.home_violations(self.home, before, perf_home.snapshot_home(self.home), self.sentinel_ns,
                                    perf_watcher_tests.HARNESS_PID, False, ["/scratch-run"])

    def test_a_write_through_a_linked_file_is_a_candidate(self):
        # The main config may be a symlink into a dot-config tree; a write to its target is a write to the home.
        target = self.elsewhere / "sonicterm.toml"
        self.write(target, b"font = 1\n")
        self.link("sonicterm.toml", target)
        before = perf_home.snapshot_home(self.home)
        self.write(target, b"font = 2, changed\n")
        self.assertEqual(self.violations(before), ["sonicterm.toml: changed"])

    def test_the_sentinel_comparison_uses_the_link_target(self):
        # A target written after the sentinel is a candidate, however old the link itself is.
        target = self.elsewhere / "apollo.toml"
        self.write(target, b"x")
        self.link("themes/apollo.toml", target)
        # 1 s later: a filesystem stores file times at its own resolution, 100 ns on NTFS and 1 s on HFS+.
        later_ns = self.sentinel_ns + 1_000_000_000
        os.utime(target, ns=(later_ns, later_ns))
        self.assertEqual(self.violations(perf_home.snapshot_home(self.home)), ["themes/apollo.toml: newer than the sentinel"])

    def test_a_linked_directory_is_walked(self):
        # ~/.sonicterm/logs may point elsewhere; a file written there is a file written under the home.
        (self.elsewhere / "logs").mkdir()
        self.link("logs", self.elsewhere / "logs", directory=True)
        before = perf_home.snapshot_home(self.home)
        self.write(self.elsewhere / "logs" / "crash-1.log", b"crash")
        self.assertEqual(self.violations(before), ["logs/crash-1.log: added"])

    def test_a_dangling_link_records_only_its_target_text(self):
        # A link to nothing has no size or time to compare; retargeting it is still a change.
        link = self.link("keymaps/sonicterm-macos.toml", self.elsewhere / "missing-a.toml")
        before = perf_home.snapshot_home(self.home)
        self.assertEqual(before["keymaps/sonicterm-macos.toml"], (None, None, os.readlink(link)))
        link.unlink()
        self.link("keymaps/sonicterm-macos.toml", self.elsewhere / "missing-b.toml")
        self.assertEqual(self.violations(before), ["keymaps/sonicterm-macos.toml: changed"])

    def test_a_link_cycle_is_recorded_and_its_directory_walked_once(self):
        # A link back into the home is recorded, and the real directory it names is not walked again.
        self.write(self.home / "themes" / "apollo.toml", b"x")
        self.link("themes/loop", self.home / "themes", directory=True)
        self.assertEqual(sorted(perf_home.snapshot_home(self.home)), ["themes/apollo.toml", "themes/loop"])

    def test_a_walk_past_its_bound_is_unresolved(self):
        # The walk is bounded; reaching the bound makes the check unresolved instead of skipping the rest.
        for index in range(5):
            self.write(self.home / "logs" / f"sonicterm.log.{index}", b"x")
        with self.assertRaises(perf_home.HomeSnapshotUnresolved):
            perf_home.snapshot_home(self.home, max_entries=3)
        self.assertEqual(len(perf_home.snapshot_home(self.home, max_entries=6)), 5)

    @unittest.skipIf(os.name == "nt" or (hasattr(os, "geteuid") and os.geteuid() == 0),
                     "needs POSIX permissions as a non-root user")
    def test_an_unreadable_link_target_is_unresolved(self):
        # A target that cannot be read could hide a write, so the check is unresolved, never skipped.
        locked = self.elsewhere / "locked"
        self.write(locked / "sonicterm.toml", b"x")
        self.link("sonicterm.toml", locked / "sonicterm.toml")
        locked.chmod(0)
        self.addCleanup(locked.chmod, 0o700)
        with self.assertRaises(perf_home.HomeSnapshotUnresolved):
            perf_home.snapshot_home(self.home)
