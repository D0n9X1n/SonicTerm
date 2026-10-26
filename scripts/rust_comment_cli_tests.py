"""Rust comment cli tests."""

from __future__ import annotations
import json
import unittest

import rust_comment_test_fixture


class PathCliAndInventoryTests(unittest.TestCase):
    def test_paths_absolute_caller_relative_repo_relative_and_backslash_are_identical(self):
        files = {
            "src/lib.rs": "pub fn undocumented() {}\n",
            "scripts/.keep": "",
        }
        with rust_comment_test_fixture.repository(files) as root:
            expected = "src/lib.rs:1:1 [public-doc] effectively public function needs purpose rustdoc\n"
            invocations = [
                (root, str(root / "src/lib.rs")),
                (root / "src", "lib.rs"),
                (root / "src", "src/lib.rs"),
                (root / "src", r"src\lib.rs"),
            ]
            outputs = []
            for cwd, path in invocations:
                completed = rust_comment_test_fixture.cli(cwd, "--check", "--paths", path)
                self.assertEqual(completed.returncode, 1, completed.stderr)
                outputs.append(completed.stdout)
            self.assertEqual(outputs, [expected] * len(outputs))

    def test_paths_constrain_diagnostics_but_full_corpus_resolves_modules(self):
        with rust_comment_test_fixture.repository({
            "src/lib.rs": "pub mod api;\npub fn root_missing() {}\n",
            "src/api.rs": "pub fn api_missing() {}\n",
        }) as root:
            report = rust_comment_test_fixture.checker.analyze_repository(
                root,
                paths=["src/api.rs"],
                caller_cwd=root,
            )
        self.assertEqual(
            rust_comment_test_fixture.formatted(report),
            ["src/api.rs:1:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_inventory_is_nonzero_stable_json_with_counts_paths_and_diagnostics(self):
        with rust_comment_test_fixture.repository({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                pub fn undocumented() {}
                fn branches(ready: bool) {
                    if ready { work(); }
                    if ready { return; }
                }
                unsafe fn raw() {}
                fn locks() { one.lock(); two.lock(); }
                fn atomic() { load(Ordering::Acquire); }
                impl Drop for Thing { fn drop(&mut self) {} }
            '''),
            "scripts/.keep": "",
            "crates/.keep": "",
        }) as root:
            outputs = []
            for cwd in (root, root / "scripts", root / "crates"):
                completed = rust_comment_test_fixture.cli(cwd, "--inventory")
                self.assertEqual(completed.returncode, 0, completed.stderr)
                outputs.append(completed.stdout)
            self.assertEqual(outputs[0], outputs[1])
            self.assertEqual(outputs[1], outputs[2])
            inventory = json.loads(outputs[0])
        self.assertGreater(inventory["counts"]["files"]["resolution"], 0)
        self.assertGreater(inventory["counts"]["public_docs"]["candidates"], 0)
        self.assertGreater(inventory["counts"]["branches"]["mandatory"], 0)
        self.assertGreater(inventory["counts"]["branches"]["advisory"], 0)
        self.assertGreater(inventory["counts"]["unsafe"]["required"], 0)
        self.assertGreater(inventory["counts"]["lock_order"]["required"], 0)
        self.assertGreater(inventory["counts"]["ordering"]["required"], 0)
        self.assertGreater(inventory["counts"]["lifecycle"]["required"], 0)
        self.assertEqual(inventory["paths"]["resolution"], ["src/lib.rs"])
        self.assertTrue(inventory["diagnostics"])

    def test_semantic_candidates_mode_is_stable_and_non_failing(self):
        with rust_comment_test_fixture.repository({"src/lib.rs": "fn f(ready: bool) { if ready { work(); } }\n"}) as root:
            completed = rust_comment_test_fixture.cli(root, "--semantic-candidates")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(
            completed.stdout,
            "src/lib.rs:1:21 [when-advisory] ordinary branch has no mandatory // When: requirement\n",
        )

    def test_no_in_scope_rust_fails_closed(self):
        with rust_comment_test_fixture.repository({"README": "fixture\n"}) as root:
            completed = rust_comment_test_fixture.cli(root, "--check")
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("no tracked Rust files", completed.stderr)
