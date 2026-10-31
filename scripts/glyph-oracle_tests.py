#!/usr/bin/env python3
"""Offline corruption and provenance tests for the exact-run helper oracle."""
import copy
import hashlib
import importlib.util
import json
import os
import types
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("glyph_oracle", Path(__file__).with_name("glyph-oracle.py"))
oracle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(oracle)
ROOT = Path(__file__).resolve().parent.parent


class OracleTests(unittest.TestCase):
    def test_every_binding_and_payload_is_required(self):
        # A successful producer may be reused by a later consumer attempt, but never another producer,
        # source, font inventory, toolchain, image, run or fixture; corrupt bytes cannot pass the digest.
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            for name in oracle.ROWS:
                fixture, dpi = name.removesuffix(".json").split("-")
                (directory / name).write_text(json.dumps({"schema": 1, "fixture": fixture, "dpi": int(dpi),
                    "family": "Consolas", "size": "13.0", "tiles": {"face": [24, 16]}}), encoding="utf-8")
            identity = {"source": "source-digest", "fonts": {"system": "font-digest"},
                        "environment": {"GITHUB_SHA": "head", "GITHUB_RUN_ID": "run",
                                        "ImageOS": "windows", "ImageVersion": "image"},
                        "producer_attempt": "1", "rustc": "rustc version", "cargo": "cargo version"}
            raw = json.dumps({"identity": identity, "files": oracle.validate_rows(directory, "Consolas")}).encode()
            manifest = directory / "manifest.json"
            manifest.write_bytes(raw)
            digest = hashlib.sha256(raw).hexdigest()
            self.assertTrue(oracle.verify(directory, identity, digest, "Consolas"))
            # Runner bindings differ between runner images; the consumer then measures for itself.
            # Every other binding still refuses the artifact.
            for key in identity:
                changed = copy.deepcopy(identity)
                changed[key] = "different"
                with self.subTest(binding=key):
                    if key in oracle.RUNNER_BINDINGS:
                        self.assertFalse(oracle.verify(directory, changed, digest, "Consolas"))
                    else:
                        with self.assertRaises(ValueError):
                            oracle.verify(directory, changed, digest, "Consolas")
            for name in identity["environment"]:
                changed = copy.deepcopy(identity)
                changed["environment"][name] = "different"
                with self.subTest(environment=name):
                    if name in oracle.RUNNER_ENVIRONMENT:
                        self.assertFalse(oracle.verify(directory, changed, digest, "Consolas"))
                    else:
                        with self.assertRaises(ValueError):
                            oracle.verify(directory, changed, digest, "Consolas")
            # A runner difference never excuses a changed run binding at the same time.
            changed = copy.deepcopy(identity)
            changed["environment"]["ImageVersion"] = "other image"
            changed["producer_attempt"] = "2"
            with self.assertRaises(ValueError):
                oracle.verify(directory, changed, digest, "Consolas")
            with self.assertRaises(ValueError):
                oracle.verify(directory, identity, "wrong digest", "Consolas")
            manifest.write_bytes(raw + b" ")
            with self.assertRaises(ValueError):
                oracle.verify(directory, identity, digest, "Consolas")
            manifest.write_bytes(raw)
            payload = directory / oracle.ROWS[0]
            original = payload.read_bytes()
            payload.write_bytes(original + b" ")
            with self.assertRaises(ValueError):
                oracle.verify(directory, identity, digest, "Consolas")
            payload.unlink()
            with self.assertRaises(OSError):
                oracle.verify(directory, identity, digest, "Consolas")

    def test_rows_bind_the_family_the_harness_pins(self):
        # The validator reads the harness's pinned family from workload.rs, so Windows rows naming the
        # retired bundled face are refused and rows naming Consolas pass; macOS pins Menlo.
        self.assertEqual(oracle.pinned_family(ROOT, "win32"), "Consolas")
        self.assertEqual(oracle.pinned_family(ROOT, "darwin"), "Menlo")
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            for family, accepted in (("Rec Mono St.Helens", False), ("Consolas", True)):
                for name in oracle.ROWS:
                    fixture, dpi = name.removesuffix(".json").split("-")
                    (directory / name).write_text(json.dumps({"schema": 1, "fixture": fixture, "dpi": int(dpi),
                        "family": family, "size": "13.0", "tiles": {"face": [24, 16]}}), encoding="utf-8")
                with self.subTest(family=family):
                    pinned = oracle.pinned_family(ROOT, "win32")
                    if accepted:
                        oracle.validate_rows(directory, pinned)
                    else:
                        with self.assertRaises(ValueError):
                            oracle.validate_rows(directory, pinned)

    def test_source_inventory_hashes_names_and_bytes(self):
        # A checkout SHA alone is insufficient: edited source/font bytes and renamed files change identity.
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "first").write_bytes(b"same")
            (root / "second").write_bytes(b"same")
            original = oracle.digest_files(root, ["first"])
            self.assertNotEqual(original, oracle.digest_files(root, ["second"]))
            (root / "first").write_bytes(b"changed")
            self.assertNotEqual(original, oracle.digest_files(root, ["first"]))

    def test_identity_binds_registry_fonts_and_exact_workflow(self):
        # Windows GDI memory names are reproducible only with identical registered bytes and selection maps.
        # Portable files exercise the real hashing; only OS registry enumeration and command probes are supplied.
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "Windows/Fonts").mkdir(parents=True)
            (root / "Local").mkdir()
            (root / "source.rs").write_text("source", encoding="utf-8")
            font = root / "Windows/Fonts/font.ttf"
            font.write_bytes(b"font")
            class RegistryKey:
                def __enter__(self):
                    return self
                def __exit__(self, *unused):
                    return None
            registry = types.SimpleNamespace(HKEY_LOCAL_MACHINE=1, HKEY_CURRENT_USER=2, REG_SZ=1, REG_EXPAND_SZ=2,
                OpenKey=lambda *unused: RegistryKey(), QueryInfoKey=lambda unused: (0, 1, 0),
                EnumValue=lambda *unused: ("Face", str(font), 1))
            environment = dict(GITHUB_SHA="head", GITHUB_RUN_ID="run", GITHUB_REPOSITORY="owner/repo",
                               ImageOS="windows", ImageVersion="image", WINDIR=str(root / "Windows"),
                               LOCALAPPDATA=str(root / "Local"))
            def probe(unused, *arguments):
                return {("git", "ls-files", "-z"): b"source.rs\0",
                        ("git", "rev-parse", "HEAD"): b"head\n",
                        ("rustc", "-vV"): b"toolchain", ("cargo", "-V"): b"cargo"}[arguments]
            with patch.dict(os.environ, environment), patch.dict("sys.modules", winreg=registry), \
                    patch.object(oracle, "command", probe):
                original = oracle.identity(root, "1", "Consolas")
                self.assertNotEqual(original, oracle.identity(root, "2", "Consolas"))
                font.write_bytes(b"replacement")
                changed = oracle.identity(root, "1", "Consolas")
                self.assertNotEqual(original["fonts"], changed["fonts"])
                self.assertNotEqual(original["font_registrations"], changed["font_registrations"])
                with patch.dict(os.environ, GITHUB_SHA="other"), self.assertRaises(ValueError):
                    oracle.identity(root, "1", "Consolas")
                with patch.dict(os.environ, ImageVersion=""), self.assertRaises(ValueError):
                    oracle.identity(root, "1", "Consolas")
                font.unlink()
                with self.assertRaises(OSError):
                    oracle.identity(root, "1", "Consolas")


if __name__ == "__main__":
    unittest.main(verbosity=2)
