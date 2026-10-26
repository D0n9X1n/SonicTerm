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


class OracleTests(unittest.TestCase):
    def test_every_binding_and_payload_is_required(self):
        # A successful producer may be reused by a later consumer attempt, but never another producer,
        # source, font inventory, toolchain, image, run or fixture; corrupt bytes cannot pass the digest.
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            for name in oracle.ROWS:
                fixture, dpi = name.removesuffix(".json").split("-")
                (directory / name).write_text(json.dumps({"schema": 1, "fixture": fixture, "dpi": int(dpi),
                    "family": "Rec Mono St.Helens", "size": "13.0", "tiles": {"face": [24, 16]}}), encoding="utf-8")
            identity = {"source": "source-digest", "fonts": {"system": "font-digest"},
                        "environment": {"GITHUB_SHA": "head", "GITHUB_RUN_ID": "run",
                                        "ImageOS": "windows", "ImageVersion": "image"},
                        "producer_attempt": "1", "rustc": "rustc version", "cargo": "cargo version"}
            raw = json.dumps({"identity": identity, "files": oracle.validate_rows(directory)}).encode()
            manifest = directory / "manifest.json"
            manifest.write_bytes(raw)
            digest = hashlib.sha256(raw).hexdigest()
            oracle.verify(directory, identity, digest)
            for key in identity:
                changed = copy.deepcopy(identity)
                changed[key] = "different"
                with self.subTest(binding=key), self.assertRaises(ValueError):
                    oracle.verify(directory, changed, digest)
            with self.assertRaises(ValueError):
                oracle.verify(directory, identity, "wrong digest")
            manifest.write_bytes(raw + b" ")
            with self.assertRaises(ValueError):
                oracle.verify(directory, identity, digest)
            manifest.write_bytes(raw)
            payload = directory / oracle.ROWS[0]
            original = payload.read_bytes()
            payload.write_bytes(original + b" ")
            with self.assertRaises(ValueError):
                oracle.verify(directory, identity, digest)
            payload.unlink()
            with self.assertRaises(OSError):
                oracle.verify(directory, identity, digest)

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
                original = oracle.identity(root, "1")
                self.assertNotEqual(original, oracle.identity(root, "2"))
                font.write_bytes(b"replacement")
                changed = oracle.identity(root, "1")
                self.assertNotEqual(original["fonts"], changed["fonts"])
                self.assertNotEqual(original["font_registrations"], changed["font_registrations"])
                with patch.dict(os.environ, GITHUB_SHA="other"), self.assertRaises(ValueError):
                    oracle.identity(root, "1")
                with patch.dict(os.environ, ImageVersion=""), self.assertRaises(ValueError):
                    oracle.identity(root, "1")
                font.unlink()
                with self.assertRaises(OSError):
                    oracle.identity(root, "1")


if __name__ == "__main__":
    unittest.main(verbosity=2)
