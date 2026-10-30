"""Pin package-smoke admission without loading the fixture DLLs."""
from __future__ import annotations

import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("conpty_verify", Path(__file__).with_name("verify-windows-conpty.py"))
verify_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify_module)


class RuntimeSmokeTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="sonicterm-conpty-verify-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.runtime = self.root / "conpty"
        self.logs = self.root / "logs"
        self.runtime.mkdir()
        self.logs.mkdir()
        self.log = self.logs / "sonicterm.log.fixture"
        pins = {}
        for name in ("conpty.dll", "OpenConsole.exe"):
            payload = name.encode("ascii")
            (self.runtime / name).write_bytes(payload)
            pins[name] = (len(payload), hashlib.sha256(payload).hexdigest())
        patch = mock.patch.object(verify_module, "PINS", pins)
        patch.start()
        self.addCleanup(patch.stop)
        self.log.write_text(f'INFO Windows console runtime selected kind="packaged" version="{verify_module.VERSION}"\n', encoding="utf-8")

    def test_exact_pair_and_selection_pass(self):
        # Bytes and selection evidence are both mandatory, not interchangeable.
        verify_module.verify(self.runtime, self.logs)

    def test_missing_or_changed_member_fails(self):
        # A valid log cannot excuse an absent or tampered runtime file.
        host = self.runtime / "OpenConsole.exe"
        host.write_bytes(b"changed")
        with self.assertRaises(ValueError):
            verify_module.verify(self.runtime, self.logs)
        host.unlink()
        with self.assertRaises(OSError):
            verify_module.verify(self.runtime, self.logs)

    def test_missing_fallback_duplicate_and_wrong_version_fail(self):
        # A smoke that falls back, never initializes, or selects more than once cannot count as packaged acceptance.
        for lines in ("", 'Windows console runtime selected kind="development-discovery"',
                      'Windows console runtime selected kind="packaged" version="wrong"',
                      self.log.read_text() * 2):
            with self.subTest(lines=lines):
                self.log.write_text(lines, encoding="utf-8")
                with self.assertRaises(ValueError):
                    verify_module.verify(self.runtime, self.logs)


if __name__ == "__main__":
    unittest.main(verbosity=2)
