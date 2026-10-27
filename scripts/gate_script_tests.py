"""Gate script tests."""

from __future__ import annotations
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

import gate_test_fixture


_FAKE_CARGO = """#!/bin/sh
# Record CARGO_TARGET_DIR and every argument, separated by the unit separator.
{
  printf '%s' "${CARGO_TARGET_DIR-<unset>}"
  for argument in "$@"; do printf '\\037%s' "$argument"; done
  printf '\\n'
} >> "$FAKE_LOG"
if [ "$1" = metadata ]; then
  cat "$FAKE_METADATA"
  exit 0
fi
if [ -n "$FAKE_FAIL" ]; then
  case " $* " in
    *"$FAKE_FAIL"*) exit 7 ;;
  esac
fi
exit 0
"""
_FAKE_SUCCESS = "#!/bin/sh\nexit 0\n"


def _write_tools(directory: Path, tools: dict[str, str]) -> Path:
    """Write executable fake tools into a directory that goes first on PATH."""
    bin_dir = directory / "bin"
    bin_dir.mkdir()
    for name, body in tools.items():
        path = bin_dir / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)
    return bin_dir


def _calls(log: Path) -> list[tuple[str, list[str]]]:
    """Read the fake cargo's (CARGO_TARGET_DIR, arguments) record, one entry per call."""
    if not log.exists():
        return []
    calls = []
    for line in log.read_text(encoding="utf-8").splitlines():
        target_dir, *arguments = line.split("\x1f")
        calls.append((target_dir, arguments))
    return calls


def _tool_environment(bin_dir: Path, log: Path, target_dir: str | None, **extra: str) -> dict[str, str]:
    """Return an environment that finds the fake tools first and records into log."""
    env = dict(os.environ)
    env.pop("CARGO_TARGET_DIR", None)
    if target_dir is not None:
        env["CARGO_TARGET_DIR"] = target_dir
    env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")
    env["FAKE_LOG"] = str(log)
    env.update(extra)
    return env


@gate_test_fixture.POSIX_ONLY
class WorkspaceGateScriptTests(unittest.TestCase):
    """check-workspace-crates.sh keeps a caller's target directory and runs every phase."""

    def run_script(self, directory: Path, target_dir: str | None = None, fail: str = ""):
        bin_dir = _write_tools(
            directory,
            {"cargo": _FAKE_CARGO, "rustfmt": _FAKE_SUCCESS, "python3": _FAKE_SUCCESS},
        )
        log = directory / "calls.log"
        completed = subprocess.run(
            ["bash", str(gate_test_fixture.ROOT / "scripts" / "check-workspace-crates.sh")],
            env=_tool_environment(bin_dir, log, target_dir, FAKE_FAIL=fail),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
        return completed, _calls(log)

    def test_nested_winit_phases_honor_a_caller_target_dir(self):
        # Protect CARGO_TARGET_DIR isolation through the nested pinned-winit builds.
        with tempfile.TemporaryDirectory() as directory:
            isolated = str(Path(directory) / "isolated-target")
            completed, calls = self.run_script(Path(directory), target_dir=isolated)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(len(calls), 3, calls)
            for _target_dir, arguments in calls[:2]:
                self.assertIn("crates/sonicterm-winit/Cargo.toml", arguments)
                self.assertEqual(arguments[arguments.index("--target-dir") + 1], isolated)
            target_dir, arguments = calls[2]
            self.assertEqual(arguments, ["test", "--workspace", "--lib", "--bins", "--tests", "--no-fail-fast"])
            self.assertEqual(target_dir, isolated)

    def test_without_a_caller_target_dir_winit_shares_the_workspace_target(self):
        # Protect the default cache reuse between the workspace and the pinned winit.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_script(Path(directory))

            self.assertEqual(completed.returncode, 0, completed.stderr)
            expected = os.path.join(os.path.realpath(gate_test_fixture.ROOT), "target")
            for _target_dir, arguments in calls[:2]:
                value = arguments[arguments.index("--target-dir") + 1]
                self.assertEqual(os.path.realpath(value), expected)

    def test_a_failing_phase_does_not_stop_later_phases(self):
        # Protect the script's fail-complete contract across its winit and workspace phases.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_script(Path(directory), fail="sonicterm-winit")

            self.assertNotEqual(completed.returncode, 0)
            self.assertEqual(len(calls), 3, calls)
            self.assertEqual(calls[2][1][:2], ["test", "--workspace"])


_FAKE_RUSTC = """#!/bin/sh
if [ "$1" = --print ] && [ "$2" = sysroot ]; then
  printf '%s\\n' "$FAKE_SYSROOT"
  exit 0
fi
exit 1
"""

VERIFIED = (
    "sonicterm-types", "sonicterm-grid", "sonicterm-vt", "sonicterm-cfg", "sonicterm-logging",
    "sonicterm-resource", "sonicterm-text", "sonicterm-ui", "sonicterm-app-core", "sonicterm-io",
    "sonicterm-render-model", "sonicterm-block-glyph", "sonicterm-font-config",
)
EXCLUDED = (
    "sonicterm-freetype", "sonicterm-harfbuzz", "sonicterm-fontconfig", "sonicterm-font",
    "sonicterm-engine", "sonicterm-gpu", "sonicterm-app", "sonicterm-mac", "sonicterm-windows",
    "sonicterm-linux",
)


def _workspace_package_names() -> list[str]:
    """Read the workspace members' package names from the manifests, without Cargo."""
    manifest = (gate_test_fixture.ROOT / "Cargo.toml").read_text(encoding="utf-8")
    members = re.search(r"(?m)^members\s*=\s*\[([^\]]*)\]", manifest).group(1)
    names = []
    for member in re.findall(r'"([^"]+)"', members):
        package = (gate_test_fixture.ROOT / member / "Cargo.toml").read_text(encoding="utf-8")
        names.append(re.search(r'(?m)^name\s*=\s*"([^"]+)"', package).group(1))
    return names


def _fake_metadata(names) -> str:
    """Return the `cargo metadata --no-deps` fields the classification reads."""
    packages = [{"name": name, "id": f"path+file:///fixture/{name}#0.0.0"} for name in names]
    return json.dumps({"packages": packages, "workspace_members": [package["id"] for package in packages]})


def _script_list(name: str) -> list[str]:
    """Read one bash array's names from check-windows-target.sh without running the script."""
    source = (gate_test_fixture.ROOT / "scripts" / "check-windows-target.sh").read_text(encoding="utf-8")
    body = re.search(rf"(?ms)^{name}=\(\n(.*?)^\)", source)
    if body is None:
        raise AssertionError(f"check-windows-target.sh has no {name}=( ... ) list")
    entries = []
    for line in body.group(1).splitlines():
        entry = line.strip().strip('"')
        if entry and not entry.startswith("#"):
            entries.append(entry.split("|", 1)[0])
    return entries


class WindowsTargetClassificationTests(unittest.TestCase):
    """The Windows-target lists classify every workspace member, checked statically on every host."""

    def test_the_script_lists_classify_every_workspace_member(self):
        # Protect classification completeness in CI: the script's own lists, read without
        # running it, match Cargo.toml's members, so a new crate must be classified even
        # though CI never runs the optional windows-target step.
        verified = _script_list("verified")
        excluded = _script_list("excluded")
        self.assertEqual(verified, list(VERIFIED))
        self.assertEqual(excluded, list(EXCLUDED))
        self.assertEqual(set(verified) & set(excluded), set())
        self.assertEqual(sorted(verified + excluded), sorted(_workspace_package_names()))
        self.assertEqual(len(verified), 13)


@gate_test_fixture.POSIX_ONLY
class WindowsTargetCheckTests(unittest.TestCase):
    """check-windows-target.sh classifies every member and pins the checked scope."""

    def run_check(self, directory: Path, *, names=None, installed: bool = True,
                  target_dir: str | None = None, fail: str = ""):
        bin_dir = _write_tools(directory, {"cargo": _FAKE_CARGO, "rustc": _FAKE_RUSTC})
        sysroot = directory / "sysroot"
        if installed:
            (sysroot / "lib" / "rustlib" / "x86_64-pc-windows-msvc" / "lib").mkdir(parents=True)
        else:
            sysroot.mkdir()
        metadata = directory / "metadata.json"
        metadata.write_text(
            _fake_metadata(_workspace_package_names() if names is None else names), encoding="utf-8"
        )
        log = directory / "calls.log"
        env = _tool_environment(
            bin_dir, log, target_dir,
            FAKE_METADATA=str(metadata), FAKE_SYSROOT=str(sysroot), FAKE_FAIL=fail,
        )
        completed = subprocess.run(
            ["bash", str(gate_test_fixture.ROOT / "scripts" / "check-windows-target.sh")],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=60,
            check=False,
        )
        return completed, _calls(log)

    def test_verified_members_get_the_pinned_clippy_and_winit_scope(self):
        # Protect the measured boundary: exactly the verified members are linted with all
        # targets, denied warnings, and --locked, and the pinned winit's Windows tests compile.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory))

            self.assertEqual(completed.returncode, 0, completed.stderr)
            commands = [arguments for _target_dir, arguments in calls]
            self.assertEqual([arguments[0] for arguments in commands], ["metadata", "clippy", "check"])
            clippy = commands[1]
            self.assertEqual(clippy[:4], ["clippy", "--locked", "--target", "x86_64-pc-windows-msvc"])
            packages = [clippy[index + 1] for index, value in enumerate(clippy) if value == "-p"]
            self.assertEqual(packages, list(VERIFIED))
            self.assertEqual(clippy[-4:], ["--all-targets", "--", "-D", "warnings"])
            check = commands[2]
            self.assertEqual(
                check[:6],
                ["check", "--locked", "--manifest-path", "crates/sonicterm-winit/Cargo.toml",
                 "--target", "x86_64-pc-windows-msvc"],
            )
            self.assertEqual(check[-4:], ["--features", "serde", "--lib", "--tests"])
            expected = os.path.join(os.path.realpath(gate_test_fixture.ROOT), "target", "check-windows-target")
            self.assertEqual(
                os.path.realpath(clippy[clippy.index("--target-dir") + 1]),
                os.path.join(expected, "workspace"),
            )
            self.assertEqual(
                os.path.realpath(check[check.index("--target-dir") + 1]),
                os.path.join(expected, "winit"),
            )
            for name in EXCLUDED:
                self.assertRegex(completed.stdout, rf"\[windows-target\]   {re.escape(name)}: \S")
            self.assertIn("nothing runs", completed.stdout)

    def test_check_directories_nest_under_a_caller_target_dir(self):
        # Protect CARGO_TARGET_DIR isolation for the Windows-target artifacts.
        with tempfile.TemporaryDirectory() as directory:
            isolated = str(Path(directory) / "isolated")
            completed, calls = self.run_check(Path(directory), target_dir=isolated)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            clippy = calls[1][1]
            self.assertEqual(
                clippy[clippy.index("--target-dir") + 1],
                os.path.join(isolated, "check-windows-target", "workspace"),
            )

    def test_a_missing_target_prints_the_rustup_hint_and_fails(self):
        # Protect a first run from failing without saying how to install the target.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory), installed=False)

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("rustup target add x86_64-pc-windows-msvc", completed.stderr)
            self.assertEqual([arguments[0] for _target_dir, arguments in calls], ["metadata"])

    def test_a_new_member_in_neither_list_fails_classification(self):
        # Protect the check from silently skipping a crate added to the workspace.
        with tempfile.TemporaryDirectory() as directory:
            names = _workspace_package_names() + ["sonicterm-newcomer"]
            completed, calls = self.run_check(Path(directory), names=names)

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn(
                "workspace member sonicterm-newcomer is in neither the verified nor the excluded list",
                completed.stderr,
            )
            self.assertIn("clippy", [arguments[0] for _target_dir, arguments in calls])

    def test_a_listed_name_that_is_no_longer_a_member_fails_classification(self):
        # Protect the lists from keeping a removed or renamed crate.
        with tempfile.TemporaryDirectory() as directory:
            names = [name for name in _workspace_package_names() if name != "sonicterm-harfbuzz"]
            completed, _calls_made = self.run_check(Path(directory), names=names)

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("sonicterm-harfbuzz is listed but is not a workspace member", completed.stderr)

    def test_a_clippy_failure_fails_the_check_and_still_checks_winit(self):
        # Protect the winit phase from being skipped after a lint failure.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory), fail="clippy")

            self.assertNotEqual(completed.returncode, 0)
            self.assertEqual(
                [arguments[0] for _target_dir, arguments in calls], ["metadata", "clippy", "check"]
            )

    def test_a_winit_check_failure_fails_the_check(self):
        # Protect the winit phase's verdict: a Windows test that fails to compile fails the script.
        with tempfile.TemporaryDirectory() as directory:
            completed, calls = self.run_check(Path(directory), fail="sonicterm-winit")

            self.assertEqual(completed.returncode, 1, completed.stderr)
            self.assertEqual(
                [arguments[0] for _target_dir, arguments in calls], ["metadata", "clippy", "check"]
            )
