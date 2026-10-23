#!/usr/bin/env python3
"""Tests for scripts/install-pinned-tool.sh, against a fake release served from a local file.

Each test copies the script beside its own manifest, so the committed manifest and the network
are never touched. Wrappers for `curl` and `tar` placed first on PATH record whether the script
reached its download and its extraction, so a refused install can be shown to stop before them.
"""

from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import tarfile
import tempfile
import unittest

_HERE = Path(__file__).resolve().parent
SCRIPT = _HERE / "install-pinned-tool.sh"
TOOL = "fake-tool"
VERSION = "1.2.3"


def host_os() -> str:
    """The manifest spelling of this host's OS, as the script reads `uname -s`."""
    return {"Darwin": "macos", "Linux": "linux"}.get(platform.system(), platform.system().lower())


def host_arch() -> str:
    """The manifest spelling of this host's CPU, as the script reads `uname -m`."""
    machine = platform.machine()
    return {"arm64": "aarch64", "amd64": "x86_64"}.get(machine.lower(), machine)


def release_archive(version_output: str) -> bytes:
    """A gzipped tar holding `fake-tool-<version>/fake-tool`, a script that prints `version_output`."""
    program = f"#!/bin/sh\necho '{version_output}'\n".encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        member = tarfile.TarInfo(f"{TOOL}-{VERSION}/{TOOL}")
        member.size = len(program)
        member.mode = 0o755
        archive.addfile(member, io.BytesIO(program))
    return buffer.getvalue()


@unittest.skipIf(os.name == "nt", "the installer runs only on the Linux and macOS CI hosts")
class InstallPinnedToolTests(unittest.TestCase):
    """The installer takes only a tool name and installs only a verified, pinned release."""

    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="install-pinned-tool-test."))
        self.addCleanup(shutil.rmtree, self.work, True)
        self.scripts = self.work / "scripts"
        self.scripts.mkdir()
        shutil.copy2(SCRIPT, self.scripts / SCRIPT.name)
        self.archive = self.work / "release" / f"{TOOL}-{VERSION}.tar.gz"
        self.archive.parent.mkdir()
        self.archive.write_bytes(release_archive(f"{TOOL} {VERSION}"))
        self.cargo_home = self.work / "cargo-home"
        self.temp = self.work / "tmp"
        self.temp.mkdir()
        self.markers = self.work / "markers"
        self.markers.mkdir()
        self.wrappers = self.work / "wrappers"
        self.wrappers.mkdir()
        for command in ("curl", "tar"):
            real = shutil.which(command)
            wrapper = self.wrappers / command
            wrapper.write_text(f'#!/bin/sh\ntouch "{self.markers / command}"\nexec "{real}" "$@"\n')
            wrapper.chmod(wrapper.stat().st_mode | stat.S_IEXEC)

    def entry(self, **changes) -> dict:
        """A manifest entry for the fake release on this host, with `changes` applied."""
        entry = {
            "version": VERSION,
            "os": host_os(),
            "arch": host_arch(),
            "url": self.archive.as_uri().replace(VERSION, "{version}"),
            "sha256": hashlib.sha256(self.archive.read_bytes()).hexdigest(),
            "member": f"{TOOL}-{{version}}/{TOOL}",
            "version_args": ["--version"],
            "version_output": f"{TOOL} {{version}}",
        }
        entry.update(changes)
        return entry

    def install(self, *arguments: str, entry: dict | None = None):
        """Run the installer with `arguments` against a manifest holding `entry` for the fake tool."""
        manifest = {TOOL: entry if entry is not None else self.entry()}
        (self.scripts / "pinned-tools.json").write_text(json.dumps(manifest), encoding="utf-8")
        environment = dict(os.environ)
        environment.update(
            {
                "CARGO_HOME": str(self.cargo_home),
                "TMPDIR": str(self.temp),
                "PATH": f"{self.wrappers}{os.pathsep}{environment.get('PATH', '')}",
                # Pins in the environment are never read: only the manifest decides.
                "FAKE_TOOL_VERSION": "9.9.9",
                "FAKE_TOOL_SHA256": "0" * 64,
                "CARGO_DENY_VERSION": "latest",
            }
        )
        return subprocess.run(
            ["bash", str(self.scripts / SCRIPT.name), *arguments],
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )

    def installed(self) -> Path:
        """Where the fake tool lands."""
        return self.cargo_home / "bin" / TOOL

    def reached(self, command: str) -> bool:
        """Whether the run invoked `command` (`curl` or `tar`)."""
        return (self.markers / command).exists()

    def assert_refused_before(self, completed, command: str, message: str):
        """A refused install: nonzero, `message` on stderr, nothing installed, `command` never run."""
        self.assertNotEqual(completed.returncode, 0, completed.stdout + completed.stderr)
        self.assertIn(message, completed.stderr)
        self.assertFalse(self.installed().exists())
        self.assertFalse(self.reached(command), f"{command} ran: {completed.stderr}")
        self.assertEqual(list(self.temp.iterdir()), [], "the download directory is removed")

    def test_a_pinned_release_installs_its_binary(self):
        # Protect the normal path: download, verify, extract and install the manifest's release,
        # whatever the environment says the version or digest is.
        completed = self.install(TOOL)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertTrue(os.access(self.installed(), os.X_OK))
        output = subprocess.run([str(self.installed()), "--version"], capture_output=True, text=True)
        self.assertEqual(output.stdout.strip(), f"{TOOL} {VERSION}")
        self.assertTrue(self.reached("curl") and self.reached("tar"))
        self.assertEqual(list(self.temp.iterdir()), [], "the download directory is removed")

    def test_a_wrong_digest_fails_before_extraction_and_installs_nothing(self):
        # Protect the order: the downloaded file is checked before tar ever reads it.
        completed = self.install(TOOL, entry=self.entry(sha256="0" * 64))
        self.assertTrue(self.reached("curl"))
        self.assert_refused_before(completed, "tar", "SHA-256")

    def test_an_unknown_tool_fails_before_any_download(self):
        # Protect the manifest as the only source: a name it does not list is refused.
        self.assert_refused_before(self.install("cargo-unknown"), "curl", "not in")

    def test_an_unpinned_version_or_a_malformed_digest_fails_before_any_download(self):
        # Protect the pins themselves: only an exact X.Y.Z and 64 lowercase hex digits are used.
        for case, entry, message in (
            ("latest", self.entry(version="latest"), "X.Y.Z"),
            ("two-part version", self.entry(version="1.2"), "X.Y.Z"),
            ("short digest", self.entry(sha256="abc123"), "64 lowercase hex"),
            ("uppercase digest", self.entry(sha256=self.entry()["sha256"].upper()), "64 lowercase hex"),
        ):
            with self.subTest(case=case):
                for marker in self.markers.iterdir():
                    marker.unlink()
                self.assert_refused_before(self.install(TOOL, entry=entry), "curl", message)

    def test_the_tool_name_is_the_only_input(self):
        # Protect the interface: no URL, version or digest can be passed as an argument.
        for arguments in ((), (TOOL, "https://example.com/other.tar.gz"), (TOOL, "--version", "9.9.9")):
            with self.subTest(arguments=arguments):
                self.assert_refused_before(self.install(*arguments), "curl", "usage")

    def test_a_binary_reporting_another_version_is_removed(self):
        # Protect against an archive whose binary is not the pinned release.
        self.archive.write_bytes(release_archive(f"{TOOL} 0.0.1"))
        completed = self.install(TOOL)
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("reports", completed.stderr)
        self.assertFalse(self.installed().exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
