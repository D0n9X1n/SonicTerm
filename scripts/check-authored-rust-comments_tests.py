#!/usr/bin/env python3
"""Contract tests for scripts/check-authored-rust-comments.py."""

from __future__ import annotations

from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

_HERE = Path(__file__).resolve().parent
_CHECKER_PATH = _HERE / "check-authored-rust-comments.py"

_spec = importlib.util.spec_from_file_location("check_authored_rust_comments", _CHECKER_PATH)
checker = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = checker
_spec.loader.exec_module(checker)


def rust(source: str) -> str:
    """Return an LF-terminated, dedented Rust fixture."""
    return textwrap.dedent(source).lstrip("\n").rstrip() + "\n"


@contextmanager
def repository(files: dict[str, str | bytes]):
    """Create a tracked, uncommitted Git fixture repository."""
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        subprocess.run(
            ["git", "init", "-q", str(root)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        for name, content in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(content, bytes):
                path.write_bytes(content)
            else:
                with path.open("w", encoding="utf-8", newline="") as handle:
                    handle.write(content)
        subprocess.run(
            ["git", "-C", str(root), "add", "--", "."],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        yield root


def analyze(files: dict[str, str | bytes], paths: list[str] | None = None):
    """Analyze a temporary tracked fixture and return its report."""
    with repository(files) as root:
        return checker.analyze_repository(root, paths=paths, caller_cwd=root)


def formatted(report, rule: str | None = None) -> list[str]:
    """Format diagnostics, optionally selecting one rule."""
    return [
        diagnostic.format()
        for diagnostic in report.diagnostics
        if rule is None or diagnostic.rule == rule
    ]


def rule_names(report) -> list[str]:
    """Return diagnostic rule names in stable report order."""
    return [diagnostic.rule for diagnostic in report.diagnostics]


def cli(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the checker CLI with platform newlines normalized for comparison."""
    return subprocess.run(
        [sys.executable, str(_CHECKER_PATH), *args],
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
        encoding="utf-8",
    )


class LexerTests(unittest.TestCase):
    def test_nested_comments_and_all_string_families_hide_rust_tokens(self):
        source = rust(r'''
            /* outer if fake { /* nested unsafe { nope(); } */ still } */
            fn real() {
                let _ = "if unsafe { return; }";
                let _ = b"else { break; }";
                let _ = c"match x { _ => continue }";
                let _ = r###"if fake { return; }"###;
                let _ = br##"unsafe { fake(); }"##;
                let _ = cr#"else if fake { break; }"#;
                let _ = rc#"match fake { _ => return }"#;
                let _ = r#"@group(0) @binding(0) var<uniform> if: array<u32>;"#;
            }
        ''')
        lexed = checker.lex_rust(source)
        words = [token.text for token in lexed.tokens if token.kind == "IDENT"]
        self.assertEqual(words.count("if"), 0)
        self.assertEqual(words.count("unsafe"), 0)
        self.assertEqual(words.count("return"), 0)
        self.assertEqual(sum(comment.text.startswith("/*") for comment in lexed.comments), 1)
        self.assertEqual(sum(token.kind == "STRING" for token in lexed.tokens), 8)

    def test_chars_lifetimes_unicode_columns_and_crlf_are_distinct(self):
        source = "fn f<'a>(x: &'a str) {\r\n    let c = '中'; let q = b'\\n';\r\n    α();\r\n}\r\n"
        lexed = checker.lex_rust(source)
        lifetimes = [token.text for token in lexed.tokens if token.kind == "LIFETIME"]
        chars = [token.text for token in lexed.tokens if token.kind == "CHAR"]
        alpha = next(token for token in lexed.tokens if token.text == "α")
        self.assertEqual(lifetimes, ["'a", "'a"])
        self.assertEqual(chars, ["'中'", "b'\\n'"])
        self.assertEqual((alpha.line, alpha.column), (3, 5))

    def test_unary_block_after_keyword_is_not_hidden_as_a_macro_tree(self):
        source = "fn f(ready: bool) { if !{ ready } { return; } }\n"
        lexed = checker.lex_rust(source)
        words = [token.text for token in lexed.tokens]
        self.assertIn("ready", words)
        self.assertIn("return", words)
        self.assertEqual(words.count("{"), 3)

    def test_raw_identifier_macro_tree_remains_opaque(self):
        source = "fn f() { r#match! { if fake { return; } } }\n"
        lexed = checker.lex_rust(source)
        words = [token.text for token in lexed.tokens]
        self.assertEqual(words.count("if"), 0)
        self.assertEqual(words.count("return"), 0)

    def test_attributes_and_macro_trees_do_not_create_phantom_constructs(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                #[cfg_attr(any(), doc = "if fake { unsafe { return; } }")]
                /// Runs the macro-bearing implementation.
                pub fn real() {
                    fake_macro! {
                        pub fn ghost() {}
                        if fake { return; }
                        unsafe { fake(); }
                    }
                }
            '''),
        })
        self.assertEqual(formatted(report), [])


class DocumentationTests(unittest.TestCase):
    def test_outer_docs_attach_before_after_and_between_stacked_attributes(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                /// Purpose before attributes.
                #[inline]
                pub fn before() {}

                #[inline]
                /// Purpose after an attribute.
                pub fn after() {}

                #[cfg_attr(
                    any(target_os = "macos", target_os = "windows"),
                    inline
                )]
                /// Purpose between attributes.
                #[allow(dead_code)]
                pub fn interleaved() {}
            '''),
        })
        self.assertEqual(formatted(report, "public-doc"), [])

    def test_block_and_content_doc_attributes_count_but_metadata_does_not(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                /** Explains the block-documented function. */
                pub fn block_doc() {}

                #[doc = "Explains the attribute-documented function."]
                pub fn attr_doc() {}

                #[doc(hidden)]
                #[doc(alias = "concealed")]
                pub fn hidden_only() {}
            '''),
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            ["src/lib.rs:9:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_unrelated_item_breaks_doc_attachment(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                /// This belongs to the constant.
                pub const LIMIT: usize = 1;
                pub fn undocumented() {}
            '''),
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            ["src/lib.rs:3:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_public_unsafe_function_needs_purpose_and_safety_heading(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                /// Calls a raw primitive.
                // SAFETY: Callers uphold the pointer invariant.
                pub unsafe fn missing_heading() {}

                /// Calls a raw primitive.
                ///
                /// # Safety
                /// The pointer must remain valid for the call.
                // SAFETY: The declaration exposes the documented invariant.
                pub unsafe fn complete() {}
            '''),
        })
        self.assertEqual(
            formatted(report, "safety-doc"),
            ["src/lib.rs:3:1 [safety-doc] effectively public unsafe function rustdoc needs a # Safety section"],
        )
        self.assertNotIn("safety", rule_names(report))

    def test_unsafe_extern_function_is_one_safety_construct(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                /// Invokes a C-compatible callback.
                ///
                /// # Safety
                /// The caller must satisfy the callback contract.
                // SAFETY: The declaration exposes the documented C ABI contract.
                pub unsafe extern "C" fn callback(_value: u32) {}
            '''),
        })
        self.assertEqual(formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["unsafe"]["required"], 1)


class VisibilityAndResolutionTests(unittest.TestCase):
    def test_private_public_and_reexported_modules_define_effective_visibility(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                mod private;
                pub mod exposed;
                mod item_reexport;
                mod glob_reexport;
                mod grouped_reexport;
                pub use item_reexport::Api;
                pub use glob_reexport::*;
                pub use grouped_reexport::{Grouped, OTHER};
            '''),
            "src/private.rs": "pub fn exempt() {}\n",
            "src/exposed.rs": "pub fn required() {}\n",
            "src/item_reexport.rs": "pub struct Api;\npub fn required_item() {}\n",
            "src/glob_reexport.rs": "pub fn required_glob() {}\n",
            "src/grouped_reexport.rs": "pub struct Grouped;\npub const OTHER: u8 = 0;\npub fn required_grouped() {}\n",
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            [
                "src/exposed.rs:1:1 [public-doc] effectively public function needs purpose rustdoc",
                "src/glob_reexport.rs:1:1 [public-doc] effectively public function needs purpose rustdoc",
                "src/grouped_reexport.rs:3:1 [public-doc] effectively public function needs purpose rustdoc",
                "src/item_reexport.rs:2:1 [public-doc] effectively public function needs purpose rustdoc",
            ],
        )

    def test_nested_private_child_stays_exempt_and_inline_public_child_is_checked(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                pub mod outer {
                    mod hidden {
                        pub fn exempt() {}
                    }
                    pub mod visible {
                        pub fn required() {}
                    }
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            ["src/lib.rs:6:9 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_public_trait_declarations_are_checked(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                pub trait Service {
                    /// Starts the service.
                    fn start(&self);
                    fn stop(&self);
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            ["src/lib.rs:4:5 [public-doc] public trait function needs purpose rustdoc"],
        )

    def test_nested_default_modules_and_public_inherent_methods_are_checked(self):
        report = analyze({
            "src/lib.rs": rust('''
                pub mod outer;
                pub struct Public;
                struct Private;

                impl Public {
                    pub fn required_method() {}
                }
                impl Private {
                    pub fn unreachable_method() {}
                }

                pub(crate) fn restricted() {}
                fn enclosing() {
                    pub fn nested() {}
                }
            '''),
            "src/outer.rs": "pub mod child;\n",
            "src/outer/child/mod.rs": "pub fn required_nested() {}\n",
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            [
                "src/lib.rs:6:5 [public-doc] effectively public function needs purpose rustdoc",
                "src/outer/child/mod.rs:1:1 [public-doc] effectively public function needs purpose rustdoc",
            ],
        )

    def test_binary_and_build_public_functions_are_doc_exempt(self):
        report = analyze({
            "src/main.rs": "pub fn binary_helper() {}\nfn main() {}\n",
            "build.rs": rust('''
                pub fn build_helper() {
                    first.lock();
                    second.lock();
                    if ready { return; }
                }
                fn main() {}
            '''),
        })
        self.assertEqual(formatted(report), [])

    def test_test_target_and_exact_excluded_target_resolve_without_checks(self):
        report = analyze({
            "crates/app/src/lib.rs": rust(r'''
                #[cfg(test)]
                #[path = "lib_tests.rs"]
                mod lib_tests;
                #[path = "../../sonicterm-freetype/src/lib.rs"]
                pub mod ffi;
            '''),
            "crates/app/src/lib_tests.rs": rust('''
                pub fn helper() { first.lock(); second.lock(); }
            '''),
            "crates/sonicterm-freetype/src/lib.rs": "pub fn generated() {}\n",
        })
        self.assertEqual(formatted(report), [])
        inventory = report.inventory()
        self.assertIn("crates/app/src/lib_tests.rs", inventory["paths"]["test_context"])
        self.assertIn("crates/sonicterm-freetype/src/lib.rs", inventory["paths"]["excluded"])

    def test_missing_tracked_module_is_an_error_even_when_cfg_gated(self):
        report = analyze({
            "src/lib.rs": rust('''
                #[cfg(target_os = "plan9")]
                mod absent;
            '''),
        })
        self.assertEqual(
            formatted(report, "module-resolution"),
            ["src/lib.rs:2:1 [module-resolution] module absent has no tracked source target"],
        )

    def test_nondefault_path_has_precedence_over_default_target(self):
        report = analyze({
            "src/lib.rs": rust('''
                #[path = "chosen.rs"]
                pub mod api;
            '''),
            "src/chosen.rs": "pub fn chosen_needs_docs() {}\n",
            "src/api.rs": "pub fn default_must_not_be_reached() {}\n",
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            ["src/chosen.rs:1:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_resolution_corpus_keeps_exact_harfbuzz_prefix_narrow(self):
        report = analyze({
            "crates/sonicterm-harfbuzz/src/lib.rs": "pub mod wrapper;\n",
            "crates/sonicterm-harfbuzz/src/wrapper.rs": "pub fn first_party() {}\n",
            "crates/sonicterm-harfbuzz/harfbuzz/src/rust/lib.rs": "pub fn vendored() {}\n",
        })
        self.assertEqual(
            formatted(report, "public-doc"),
            ["crates/sonicterm-harfbuzz/src/wrapper.rs:1:1 [public-doc] effectively public function needs purpose rustdoc"],
        )


class TestAndBuildContextTests(unittest.TestCase):
    def test_file_and_item_test_context_exempts_every_rule_except_safety(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                #[cfg(test)]
                mod inline_tests {
                    pub fn helper() {
                        first.lock(); second.lock();
                        if ready { return; }
                    }
                }

                #[test]
                fn direct_test() {
                    first.lock(); second.lock();
                    if ready { return; }
                }

                #[cfg(test)]
                #[path = "lib_tests.rs"]
                mod sibling;
            '''),
            "src/lib_tests.rs": rust('''
                pub fn external_helper() { first.lock(); second.lock(); if ready { return; } }
            '''),
            "tests/integration.rs": rust('''
                pub fn integration_helper() { first.lock(); second.lock(); if ready { return; } }
            '''),
            "build.rs": rust('''
                pub fn build_helper() { first.lock(); second.lock(); if ready { return; } }
            '''),
        })
        self.assertEqual(formatted(report), [])

    def test_detached_non_safety_markers_are_ignored_in_exempt_contexts(self):
        report = analyze({
            "src/lib.rs": rust('''
                #[cfg(test)]
                mod inline_tests {
                    // When: prose in tests may use the policy vocabulary.
                    // Lock order: prose in tests may describe acquisition order.
                    // Ordering: prose in tests may describe event ordering.
                    // Lifecycle: prose in tests may describe object lifetime.
                    // SAFETY: This marker is deliberately detached.
                    fn helper() {}

                    struct TestGuard;
                    impl Drop for TestGuard { fn drop(&mut self) {} }
                }

                #[test]
                fn direct_test() {
                    // When: prose in tests may use the policy vocabulary.
                    // Lock order: prose in tests may describe acquisition order.
                    // Ordering: prose in tests may describe event ordering.
                    // Lifecycle: prose in tests may describe object lifetime.
                    // SAFETY: This marker is deliberately detached.
                    work();
                }
            '''),
            "src/lib_tests.rs": rust('''
                // When: prose in tests may use the policy vocabulary.
                // Lock order: prose in tests may describe acquisition order.
                // Ordering: prose in tests may describe event ordering.
                // Lifecycle: prose in tests may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn external_helper() {}
            '''),
            "tests/integration.rs": rust('''
                // When: prose in tests may use the policy vocabulary.
                // Lock order: prose in tests may describe acquisition order.
                // Ordering: prose in tests may describe event ordering.
                // Lifecycle: prose in tests may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn integration_helper() {}
            '''),
            "build.rs": rust('''
                // When: prose in build scripts may use the policy vocabulary.
                // Lock order: prose in build scripts may describe acquisition order.
                // Ordering: prose in build scripts may describe event ordering.
                // Lifecycle: prose in build scripts may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn build_helper() {}
            '''),
            "src/main.rs": rust('''
                // When: prose in binaries may use the policy vocabulary.
                // Lock order: prose in binaries may describe acquisition order.
                // Ordering: prose in binaries may describe event ordering.
                // Lifecycle: prose in binaries may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn main() {}
            '''),
        })
        self.assertEqual(
            formatted(report),
            [
                "build.rs:5:1 [safety] // SAFETY: marker is not attached to an unsafe construct",
                "src/lib.rs:7:5 [safety] // SAFETY: marker is not attached to an unsafe construct",
                "src/lib.rs:20:5 [safety] // SAFETY: marker is not attached to an unsafe construct",
                "src/lib_tests.rs:5:1 [safety] // SAFETY: marker is not attached to an unsafe construct",
                "src/main.rs:5:1 [safety] // SAFETY: marker is not attached to an unsafe construct",
                "tests/integration.rs:5:1 [safety] // SAFETY: marker is not attached to an unsafe construct",
            ],
        )

    def test_unsafe_safety_remains_required_in_every_test_context(self):
        report = analyze({
            "src/lib_tests.rs": "unsafe fn helper() {}\n",
            "tests/integration.rs": "fn probe() { unsafe { operation(); } }\n",
            "build.rs": "unsafe impl Send for BuildThing {}\n",
        })
        self.assertEqual(
            formatted(report, "safety"),
            [
                "build.rs:1:1 [safety] unsafe impl needs one substantive // SAFETY: marker immediately above",
                "src/lib_tests.rs:1:1 [safety] unsafe function needs one substantive // SAFETY: marker immediately above",
                "tests/integration.rs:1:14 [safety] unsafe block needs one substantive // SAFETY: marker immediately above",
            ],
        )

    def test_production_file_that_declares_test_sibling_is_not_test_context(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                #[cfg(test)]
                #[path = "lib_tests.rs"]
                mod tests;

                fn production() {
                    first.lock();
                    second.lock();
                }
            '''),
            "src/lib_tests.rs": "pub fn helper() {}\n",
        })
        self.assertEqual(
            formatted(report, "lock-order"),
            ["src/lib.rs:5:1 [lock-order] function with multiple lock identifiers needs // Lock order: immediately above"],
        )


class BranchRuleTests(unittest.TestCase):
    def test_all_eight_mandatory_branch_shapes_accept_first_content_markers(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                fn branches(value: Option<u8>, ready: bool) {
                    if ready { work(); } else {
                        // When: ready work is unavailable, recovery preserves the pending operation.
                        recover();
                    }
                    if ready { work(); } else if value.is_some() {
                        // When: value is_some permits retry after the ready path is unavailable.
                        retry();
                    }
                    if ready {
                        // When: ready state deliberately preserves prior output unchanged.
                    }
                    if ready {
                        // When: ready cancellation stops downstream work.
                        return;
                    }
                    let Some(number) = value else {
                        // When: absence cannot produce the required number.
                        return;
                    };
                    if cfg!(target_os = "windows") {
                        // When: target_os integration uses the native windows bridge.
                        platform();
                    }
                    if ready {
                        // When: ready telemetry failure must not abort rendering.
                        let _ = maybe_record();
                    }
                    if ready {
                        // When: ready foreign access requires the raw boundary.
                        // SAFETY: The handle remains valid for this call.
                        unsafe { raw_call(); }
                    }
                }
            '''),
        })
        self.assertEqual(formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 8)

    def test_shared_when_covers_one_if_chain_by_exact_predicate_tokens(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn shared(result: ResultState, amount: Amount) {
                    // When: result releases_charge returns the permit; amount is_zero preserves accounting.
                    if result.releases_charge() {
                        return;
                    } else if amount.is_zero() {
                        return;
                    } else {
                        return;
                    }
                }
            '''),
        })
        self.assertEqual(formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 3)

    def test_shared_when_reports_uncovered_nested_and_duplicate_sites(self):
        uncovered = analyze({
            "src/lib.rs": rust('''
                fn uncovered(result: ResultState, amount: Amount) {
                    // When: result releases_charge returns the permit.
                    if result.releases_charge() {
                        return;
                    } else if amount.is_zero() {
                        return;
                    }
                }
            '''),
        })
        self.assertTrue(
            any("shared // When: does not name this branch predicate" in item for item in formatted(uncovered, "when")),
            formatted(uncovered, "when"),
        )

        nested = analyze({
            "src/lib.rs": rust('''
                fn nested(outer: bool, value: Option<u8>) {
                    // When: outer cancellation stops the enclosing operation.
                    if outer {
                        match value {
                            Some(number) => return,
                            None => recover(),
                        }
                    } else {
                        return;
                    }
                }
            '''),
        })
        self.assertTrue(
            any(diagnostic.startswith("src/lib.rs:5:13 ") for diagnostic in formatted(nested, "when")),
            formatted(nested, "when"),
        )

        duplicate = analyze({
            "src/lib.rs": rust('''
                fn duplicate(ready: bool) {
                    // When: ready cancellation terminates processing.
                    if ready {
                        // When: ready cancellation terminates processing.
                        return;
                    }
                }
            '''),
        })
        self.assertTrue(
            any("covered by both shared and local // When:" in item for item in formatted(duplicate, "when")),
            formatted(duplicate, "when"),
        )

    def test_local_when_does_not_cover_first_nested_construct(self):
        nested_if = analyze({
            "src/lib.rs": rust('''
                fn leak(alpha: bool, beta: bool) -> u32 {
                    if alpha {
                        // When: alpha gate holds so beta fast route runs first.
                        if beta { return 1; }
                        return 2;
                    }
                    0
                }
            '''),
        })
        self.assertEqual(len(formatted(nested_if, "when")), 1)
        self.assertTrue(
            formatted(nested_if, "when")[0].startswith("src/lib.rs:4:9 "),
            formatted(nested_if, "when"),
        )

        nested_match = analyze({
            "src/lib.rs": rust('''
                fn leak(alpha: bool, value: Option<u8>) {
                    if alpha {
                        // When: alpha gate selects value outcomes before return.
                        match value {
                            Some(number) => return,
                            None => (),
                        }
                        return;
                    }
                }
            '''),
        })
        relevant = formatted(nested_match, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(all(item.startswith(("src/lib.rs:5:13 ", "src/lib.rs:6:13 ")) for item in relevant))

    def test_shared_when_at_head_of_exempt_match_arm_covers_nested_chain(self):
        for prefix in ("", "total += u32::from(number);\n"):
            with self.subTest(prefix=bool(prefix)):
                report = analyze({
                    "src/lib.rs": rust(f'''
                        fn nested(value: Option<u8>, alpha: bool) {{
                            let mut total = 0;
                            match value {{
                                Some(number) => {{
                                    {prefix}// When: alpha gate splits accumulation for this arm.
                                    if alpha {{ total += 1; }} else {{ total += 2; }}
                                }}
                                None => total = 9,
                            }}
                        }}
                    '''),
                })
                self.assertEqual(formatted(report, "when"), [])
                self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 1)

    def test_shared_when_cannot_cover_empty_predicate_tokens(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn empty(value: Option<u8>) {
                    // When: option variants select terminal outcomes.
                    match None {
                        Some(_) => return,
                        None => return,
                    }
                }
            '''),
        })
        self.assertGreaterEqual(len(formatted(report, "when")), 2)
        self.assertTrue(
            all("shared // When: cannot cover a tokenless predicate" in item for item in formatted(report, "when")),
            formatted(report, "when"),
        )

    def test_when_marker_length_accepts_160_and_rejects_161_characters(self):
        accepted_body = "ready " + "x" * 154
        rejected_body = "ready " + "x" * 155
        accepted = analyze({
            "src/lib.rs": f"fn f(ready: bool) {{ if ready {{\n// When: {accepted_body}\nreturn;\n}} }}\n",
        })
        rejected = analyze({
            "src/lib.rs": f"fn f(ready: bool) {{ if ready {{\n// When: {rejected_body}\nreturn;\n}} }}\n",
        })
        self.assertEqual(formatted(accepted, "when"), [])
        self.assertTrue(
            any("marker exceeds 2 lines or 160 characters" in item for item in formatted(rejected, "when")),
            formatted(rejected, "when"),
        )

    def test_obvious_ordinary_predicate_is_exempt_and_advisory_only(self):
        report = analyze({"src/lib.rs": "fn f(ready: bool) { if ready { work(); } }\n"})
        self.assertEqual(formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["exempt"], 1)
        self.assertEqual(len(report.semantic_candidates), 1)
        self.assertIn("[when-advisory]", report.semantic_candidates[0].format())

    def test_value_selector_exemption_accepts_only_closed_leaf_grammar(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                fn selectors(ready: bool, base: usize) {
                    let literal = if ready { 1 } else { 0 };
                    let path = if ready { left } else { Enum::Right };
                    let field = if ready { state.left.value } else { fallback.value };
                    let index = if ready { values[index] } else { values[0usize] };
                    let borrow = if ready { &mut left } else { &mut right };
                    let parenthesized = if ready { (((left))) } else { (right) };
                    let operand = base - if ready { amount.bytes } else { 0 };
                }
            '''),
        })
        self.assertEqual(formatted(report, "when"), [])
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 0)
        self.assertEqual(branches["value_selectors"], 7)
        self.assertEqual(branches["value_selectors_by_crate"], {"(root)": 7})
        selector_candidates = [
            item.format()
            for item in report.semantic_candidates
            if "value-selection" in item.message
        ]
        self.assertEqual(len(selector_candidates), 7)

    def test_value_selector_exemption_denies_non_leaf_and_statement_shapes(self):
        cases = {
            "call": "let x = if ready { left() } else { right };",
            "method-call": "let x = if ready { state.left() } else { right };",
            "binary": "let x = if ready { left + right } else { zero };",
            "statement": "let x = if ready { prepare(); left } else { right };",
            "else-if": "let x = if ready { left } else if retry { middle } else { right };",
            "statement-position": "if ready { left } else { right }",
            "return": "let x = if ready { return left } else { right };",
            "attribute": "let x = if ready { #[cfg(any())] left } else { right };",
            "try": "let x = if ready { left? } else { right };",
            "unsafe": "let x = if ready { unsafe { left } } else { right };",
            "await": "let x = if ready { left.await } else { right };",
            "macro": "let x = if ready { left!() } else { right };",
            "call-index": "let x = if ready { values[load()] } else { right };",
            "field-index": "let x = if ready { values[index.next] } else { right };",
            "nested-if": "let x = if ready { if retry { left } else { middle } } else { right };",
            "deref": "let x = if ready { *left } else { right };",
            "negation": "let x = if ready { -left } else { right };",
            "cast": "let x = if ready { left as usize } else { right };",
            "tuple": "let x = if ready { (left, right) } else { fallback };",
            "array": "let x = if ready { [left] } else { fallback };",
            "range": "let x = if ready { left..right } else { fallback };",
            "struct": "let x = if ready { Pair { left, right } } else { fallback };",
            "closure": "let x = if ready { || left } else { fallback };",
            "platform": "let x = if cfg!(windows) { left } else { right };",
        }
        for name, statement in cases.items():
            with self.subTest(name=name):
                report = analyze({
                    "src/lib.rs": rust(f'''
                        fn selector(ready: bool, retry: bool) {{
                            {statement}
                        }}
                    '''),
                })
                self.assertGreater(len(formatted(report, "when")), 0)
                self.assertEqual(
                    report.inventory()["counts"]["branches"].get("value_selectors", 0),
                    0,
                )

    def test_statement_position_selector_stays_blocking_after_block_statements(self):
        preceding = {
            "if": "if retry { prepare(); }",
            "match": "match value { Some(_) => prepare(), None => recover() }",
            "for": "for item in items { consume(item); }",
            "loop": "loop { break; }",
            "block": "{ prepare(); }",
            "inner-attribute": "#![allow(unused)]",
        }
        for name, prefix in preceding.items():
            with self.subTest(name=name):
                report = analyze({
                    "src/lib.rs": rust(f'''
                        fn selector(ready: bool, retry: bool) {{
                            {prefix}
                            if ready {{ left }} else {{ right }};
                        }}
                    '''),
                })
                self.assertEqual(
                    report.inventory()["counts"]["branches"]["value_selectors"],
                    0,
                )
                self.assertTrue(
                    any(
                        "mandatory branch needs substantive // When:" in diagnostic
                        for diagnostic in formatted(report, "when")
                    ),
                    formatted(report, "when"),
                )

    def test_value_selector_exemption_is_format_invariant_and_non_interacting(self):
        compact = analyze({
            "src/lib.rs": "fn f(ready: bool) { let x = if ready { state.value } else { values[index] }; }\n",
        })
        expanded = analyze({
            "src/lib.rs": rust('''
                fn f(ready: bool) {
                    prepare();
                    // Records outside this path contribute the fallback value.
                    let x = if ready {
                        state.value
                    } else {
                        values[index]
                    };
                }
            '''),
        })
        for report in (compact, expanded):
            self.assertEqual(formatted(report, "when"), [])
            self.assertEqual(report.inventory()["counts"]["branches"]["value_selectors"], 1)
        compact_shape = [
            (item.rule, item.message)
            for item in compact.semantic_candidates
        ]
        expanded_shape = [
            (item.rule, item.message)
            for item in expanded.semantic_candidates
        ]
        self.assertEqual(compact_shape, expanded_shape)
        self.assertEqual(compact.inventory(), analyze({
            "src/lib.rs": "fn f(ready: bool) { let x = if ready { state.value } else { values[index] }; }\n",
        }).inventory())

    def test_if_body_finder_skips_braces_inside_grouped_conditions(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn grouped(items: Vec<u8>, value: Option<u8>) {
                    if items.iter().any(|number| { number > 0 }) {
                        return;
                    }
                    if predicate(match value {
                        Some(_) => true,
                        None => false,
                    }) {
                        return;
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:2:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:5:5 ") for item in relevant), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 2)
        self.assertEqual(branches["exempt"], 2)

    def test_if_body_finder_keeps_macro_condition_body_braces(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn macros(mode: Mode, value: Value) {
                    if matches!(mode, Mode::Auto) {
                        return;
                    }
                    if !matches!(mode, Mode::Auto) {
                        return;
                    }
                    if path::matches_mode!(mode) {
                        return;
                    }
                    if matches!(value, Value { ready: true, .. }) {
                        return;
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 4, relevant)
        for line in (2, 5, 8, 11):
            self.assertTrue(
                any(item.startswith(f"src/lib.rs:{line}:5 ") for item in relevant),
                relevant,
            )
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 4)

    def test_if_body_finder_skips_direct_block_expressions(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn blocks(ready: bool) {
                    if { ready } {
                        return;
                    }
                    if ready && { permitted() } {
                        return;
                    }
                    if unsafe { permitted() } {
                        // SAFETY: The probe reads state owned by this thread.
                        unsafe { observe(); }
                        return;
                    }
                    if !{ ready } {
                        return;
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 4, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:2:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:5:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:8:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:13:5 ") for item in relevant), relevant)
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 4)

    def test_if_body_finder_skips_nested_control_expressions(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn nested(value: Option<u8>, ready: bool) {
                    if match value {
                        Some(_) => true,
                        None => false,
                    } {
                        return;
                    }
                    if if ready { true } else { false } {
                        return;
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:2:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:8:5 ") for item in relevant), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 2)
        self.assertEqual(branches["exempt"], 4)

    def test_if_body_finder_skips_struct_pattern_braces(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn patterned(value: Value) {
                    if let Value { ready, .. } = value {
                        return;
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:2:5 "), relevant)
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 1)

    def test_let_else_scan_skips_balanced_initializer_groups(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn grouped(reader: Reader, values: Vec<u8>) {
                    let Ok(number) = reader.read(&mut [0u8; 32]) else {
                        // When: reader failure cannot produce the required number.
                        return;
                    };
                    let Some(value) = values.iter().find(|item| {
                        let candidate = **item;
                        candidate > 3
                    }) else {
                        // When: values contain no candidate that can continue processing.
                        return;
                    };
                    consume(number, value);
                }
            '''),
        })
        self.assertEqual(formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 2)

    def test_conditional_let_syntax_is_not_counted_as_let_else(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn conditional(value: Option<u8>) {
                    if let Some(number) = value {
                        use_number(number);
                    } else {
                        return;
                    }
                    while let Some(number) = next_number() {
                        use_number(number);
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:4:7 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 1)

    def test_match_arm_body_does_not_suppress_nested_if_chain(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn arm_chain(value: Option<u8>, alpha: bool, beta: bool) {
                    match value {
                        Some(number) => {
                            if alpha {
                                first(number);
                            } else if beta {
                                second(number);
                            } else {
                                fallback(number);
                            }
                        }
                        None => recover(),
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:6:20 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:8:15 ") for item in relevant), relevant)
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 2)

    def test_nested_match_arms_are_scanned_and_anchor_to_the_inner_arm(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn outer(value: Option<u8>, inner: Option<u8>) {
                    match value {
                        Some(_) => {
                            match inner {
                                Some(number) => handle(number),
                                None => (),
                            }
                        }
                        None => recover(),
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:6:17 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 3)

    def test_match_body_finder_skips_braces_inside_the_scrutinee(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn transformed(items: Vec<u8>) {
                    match items.iter().map(|number| { number + 1 }).count() {
                        0 => (),
                        _ => handle(),
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:3:9 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 1)

    def test_match_body_finder_skips_direct_block_scrutinees(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn blocks() {
                    match unsafe { ffi_call() } {
                        0 => (),
                        _ => handle(),
                    }
                    match async { fetch().await }.await {
                        None => (),
                        Some(value) => handle(value),
                    }
                    match { calculate() } {
                        0 => (),
                        value => handle(value),
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 3, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:3:9 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:7:9 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:11:9 ") for item in relevant), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 3)
        self.assertEqual(branches["exempt"], 3)

    def test_match_in_closure_scrutinee_is_scanned_once(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn spawned(inner: Option<u8>) {
                    match builder().spawn(move || {
                        match inner {
                            Some(number) => handle(number),
                            None => (),
                        }
                    }) {
                        Ok(_) => started(),
                        Err(_) => failed(),
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:5:13 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 3)

    def test_nested_match_in_scrutinee_is_not_skipped(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn nested(value: Option<u8>) {
                    match (match value {
                        Some(number) => number,
                        None => (),
                    }) {
                        _ => handle(),
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:4:9 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 2)

    def test_direct_nested_match_scrutinee_is_enumerated_exactly_once(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn nested(value: Option<u8>) {
                    match match value {
                        Some(number) => number,
                        None => 0,
                    } {
                        0 => (),
                        number => handle(number),
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:6:9 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 3)

    def test_windows_field_name_is_not_a_platform_gate(self):
        report = analyze({
            "src/lib.rs": "fn f(state: State) { if state.windows.is_empty() { work(); } }\n",
        })
        self.assertEqual(formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 0)
        self.assertEqual(report.inventory()["counts"]["branches"]["exempt"], 1)

    def test_match_arm_shapes_include_direct_and_later_divergence_noop_cfg_discard_unsafe(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                fn arms(value: Option<u8>) {
                    match value {
                        Some(number) =>
                            // When: invalid range stops processing the number.
                            return,
                        None => {
                            // When: value has no selected number, so delayed cleanup stops processing.
                            prepare();
                            if finished() {
                                // When: finished cleanup can stop processing this arm.
                                return;
                            }
                        }
                    }
                    match value {
                        None =>
                            // When: value absence deliberately preserves prior state.
                            (),
                        Some(number) if cfg!(windows) => {
                            // When: native support handles the selected number.
                            platform(number);
                        }
                        Some(number) => {
                            // When: telemetry failure must not discard the number.
                            let _ = record(number);
                        }
                        _ => {
                            // When: value foreign fallback requires raw access.
                            // SAFETY: The value came from the validated decoder.
                            unsafe { raw(); }
                        }
                    }
                }
            '''),
        })
        self.assertEqual(formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 7)

    def test_when_anti_tautology_uses_predicates_patterns_wildcards_and_entire_chain(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                fn tautologies(cacheReady: bool, retry_count: u8, value: Option<u8>) {
                    if cacheReady { work(); } else {
                        // When: cache ready.
                        fallback();
                    }
                    if retry_count > 3 {
                        // When: retry count 3.
                        return;
                    }
                    match value {
                        Some(number) =>
                            // When: Some number.
                            return,
                        _ =>
                            // When: value.
                            (),
                    }
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "when"),
            [
                "src/lib.rs:3:9 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
                "src/lib.rs:7:9 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
                "src/lib.rs:12:13 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
                "src/lib.rs:15:13 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
            ],
        )

    def test_else_if_does_not_hide_nested_branches_in_previous_arm(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn branches(ready: bool, retry: bool, other: bool) {
                    if ready {
                        if retry {
                            return;
                        }
                    } else if other {
                        recover();
                    }
                }
            '''),
        })
        relevant = formatted(report, "when")
        self.assertEqual(len(relevant), 3, relevant)
        self.assertTrue(any(diagnostic.startswith("src/lib.rs:2:5 ") for diagnostic in relevant))
        self.assertTrue(any(diagnostic.startswith("src/lib.rs:3:9 ") for diagnostic in relevant))
        self.assertTrue(any(diagnostic.startswith("src/lib.rs:6:12 ") for diagnostic in relevant))
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 3)

    def test_one_new_semantic_token_passes_and_stopword_only_fails(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                fn semantic(ready: bool) {
                    if ready {
                        // When: ready fallback.
                        return;
                    }
                    if ready {
                        // When: the predicate is true.
                        return;
                    }
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "when"),
            ["src/lib.rs:7:9 [when] // When: explanation needs at least one semantic token"],
        )

    def test_marker_after_other_body_content_is_missing_and_misplaced(self):
        report = analyze({
            "src/lib.rs": rust(r'''
                fn late(ready: bool) {
                    if ready {
                        prepare();
                        // When: ready cancellation stops downstream work.
                        return;
                    }
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "when"),
            [
                "src/lib.rs:2:5 [when] mandatory branch needs substantive // When: as first body content or at its branch head",
                "src/lib.rs:4:9 [when] // When: marker is not attached to a mandatory branch",
            ],
        )

    def test_break_and_continue_anywhere_in_body_make_branch_mandatory(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn loops(ready: bool) {
                    loop {
                        if ready {
                            // When: ready completion ends this loop.
                            finish();
                            break;
                        }
                        if !ready {
                            // When: ready remains unavailable, so deferred work resumes next iteration.
                            prepare();
                            continue;
                        }
                    }
                }
            '''),
        })
        self.assertEqual(formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 2)


class MarkerContractTests(unittest.TestCase):
    def test_nested_function_contracts_are_not_counted_by_the_parent(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn outer() {
                    fn inner(ready: bool) {
                        if ready { return; }
                        first.lock();
                        second.lock();
                        load(Ordering::Acquire);
                    }
                }
            '''),
        })
        self.assertEqual(
            formatted(report),
            [
                "src/lib.rs:2:5 [lock-order] function with multiple lock identifiers needs // Lock order: immediately above",
                "src/lib.rs:2:5 [ordering] function using non-SeqCst atomic ordering needs // Ordering: immediately above",
                "src/lib.rs:3:9 [when] mandatory branch needs substantive // When: as first body content or at its branch head",
            ],
        )
        counts = report.inventory()["counts"]
        self.assertEqual(counts["branches"]["mandatory"], 1)
        self.assertEqual(counts["lock_order"]["required"], 1)
        self.assertEqual(counts["ordering"]["required"], 1)

    def test_identifier_bound_stacked_lock_and_ordering_markers(self):
        passing = analyze({
            "src/lib.rs": rust('''
                // Lock order: parent lock -> child lock.
                // Ordering: epoch fetch_add uses Release; closing load uses Acquire.
                #[inline]
                fn contracts(parent: Guard, child: Guard, epoch: AtomicUsize, closing: AtomicBool) {
                    parent.lock();
                    child.lock();
                    epoch.fetch_add(1, Ordering::Release);
                    closing.load(Ordering::Acquire);
                }
            '''),
        })
        self.assertEqual(formatted(passing), [])

        missing_lock = analyze({
            "src/lib.rs": rust('''
                // Lock order: first lock -> second lock.
                fn locks(first: Guard, second: Guard, third: Guard) {
                    first.lock(); second.lock(); third.lock();
                }
            '''),
        })
        self.assertTrue(
            any("must name every acquired lock identifier" in item for item in formatted(missing_lock, "lock-order")),
            formatted(missing_lock, "lock-order"),
        )

        missing_ordering = analyze({
            "src/lib.rs": rust('''
                // Ordering: state load is sequentially consistent.
                fn atomic(state: AtomicBool) { state.load(Ordering::SeqCst); }
            '''),
        })
        self.assertTrue(
            any("must name each atomic receiver and exact ordering variant" in item for item in formatted(missing_ordering, "ordering")),
            formatted(missing_ordering, "ordering"),
        )

    def test_repeated_single_lock_does_not_require_lock_order(self):
        report = analyze({
            "src/lib.rs": "fn repeated(queue: Queue) { queue.lock(); queue.lock(); }\n",
        })
        self.assertEqual(formatted(report, "lock-order"), [])
        self.assertEqual(report.inventory()["counts"]["lock_order"]["required"], 0)

    def test_lifecycle_marker_names_type_and_release_token(self):
        passing = analyze({
            "src/lib.rs": rust('''
                // Lifecycle: CommittedReservation uses charge take for exact release.
                impl Drop for CommittedReservation {
                    fn drop(&mut self) { self.charge.take(); }
                }
            '''),
        })
        missing = analyze({
            "src/lib.rs": rust('''
                // Lifecycle: CommittedReservation remains responsible until teardown.
                impl Drop for CommittedReservation {
                    fn drop(&mut self) { self.charge.take(); }
                }
            '''),
        })
        self.assertEqual(formatted(passing, "lifecycle"), [])
        self.assertTrue(
            any("must name the Drop type and a release token" in item for item in formatted(missing, "lifecycle")),
            formatted(missing, "lifecycle"),
        )

    def test_contract_marker_prelude_rejects_docs_between_marker_and_item(self):
        report = analyze({
            "src/lib.rs": rust('''
                // Ordering: epoch fetch_add uses Release.
                /// This doc line breaks the marker prelude.
                fn bump(epoch: AtomicUsize) { epoch.fetch_add(1, Ordering::Release); }
            '''),
        })
        relevant = formatted(report, "ordering")
        self.assertTrue(any("immediately above" in item for item in relevant), relevant)
        self.assertTrue(any("not attached" in item for item in relevant), relevant)

    def test_marker_length_cap_applies_per_stacked_instance(self):
        long_body = "epoch Release " + "x" * 148
        report = analyze({
            "src/lib.rs": rust(f'''
                // Ordering: epoch Release publishes state.
                // Ordering: {long_body}
                fn bump(epoch: AtomicUsize) {{ epoch.fetch_add(1, Ordering::Release); }}
            '''),
        })
        self.assertTrue(
            any("marker exceeds 2 lines or 160 characters" in item for item in formatted(report, "ordering")),
            formatted(report, "ordering"),
        )

    def test_io_read_write_calls_with_arguments_do_not_count_as_locks(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn io(reader: Reader, writer: Writer, buf: &mut [u8]) {
                    reader.read(buf);
                    writer.write(buf);
                    lock.read(&mut guard);
                    lock.write(value);
                }
            '''),
        })
        self.assertEqual(formatted(report, "lock-order"), [])

    def test_zero_arg_lock_methods_require_one_substantive_sequence(self):
        report = analyze({
            "src/lib.rs": rust('''
                // Lock order: state -> cache -> stdout.
                fn correct() {
                    state.read();
                    cache.write().expect("cache poisoned");
                    stdout().lock();
                }

                // Lock order: state and cache guards protect shared state.
                fn no_sequence() {
                    state.lock();
                    cache.lock()?;
                }

                fn missing() {
                    state.read();
                    cache.write();
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "lock-order"),
            [
                "src/lib.rs:8:1 [lock-order] // Lock order: marker must state an acquisition sequence",
                "src/lib.rs:14:1 [lock-order] function with multiple lock identifiers needs // Lock order: immediately above",
            ],
        )

    def test_lock_and_ordering_markers_stack_on_one_function(self):
        report = analyze({
            "src/lib.rs": rust('''
                // Lock order: state -> cache.
                // Ordering: store uses Release after both guards commit.
                fn combined() {
                    state.lock();
                    cache.lock();
                    store(value, Ordering::Release);
                }
            '''),
        })
        self.assertEqual(formatted(report), [])
        counts = report.inventory()["counts"]
        self.assertEqual(counts["lock_order"]["required"], 1)
        self.assertEqual(counts["ordering"]["required"], 1)

    def test_all_non_seqcst_atomic_orderings_require_one_function_marker(self):
        report = analyze({
            "src/lib.rs": rust('''
                // Ordering: load uses Relaxed and Acquire; store uses Release; compare_exchange uses AcqRel and Acquire.
                fn atomic_contract() {
                    load(Ordering::Relaxed);
                    load(Ordering::Acquire);
                    store(value, Ordering::Release);
                    compare_exchange(a, b, Ordering::AcqRel, Ordering::Acquire);
                }
                fn exempt() {
                    load(Ordering::SeqCst);
                    compare(std::cmp::Ordering::Less);
                    compare(std::cmp::Ordering::Equal);
                    compare(std::cmp::Ordering::Greater);
                }
                fn missing() { load(Ordering::Acquire); }
            '''),
        })
        self.assertEqual(
            formatted(report, "ordering"),
            ["src/lib.rs:14:1 [ordering] function using non-SeqCst atomic ordering needs // Ordering: immediately above"],
        )

    def test_lifecycle_marker_belongs_above_drop_impl_not_drop_method(self):
        report = analyze({
            "src/lib.rs": rust('''
                // Lifecycle: Good releases handle through close.
                #[cfg(any())]
                impl Drop for Good {
                    fn drop(&mut self) { self.handle.close(); }
                }

                impl Drop for Bad {
                    // Lifecycle: Bad releases handle through close.
                    fn drop(&mut self) { self.handle.close(); }
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "lifecycle"),
            [
                "src/lib.rs:7:1 [lifecycle] Drop impl needs // Lifecycle: immediately above",
                "src/lib.rs:8:5 [lifecycle] // Lifecycle: marker is not attached to a Drop impl",
            ],
        )

    def test_item_markers_attach_across_stacked_attributes(self):
        report = analyze({
            "src/lib.rs": rust('''
                // Lock order: state -> cache.
                #[cfg(any())]
                fn locks() { state.lock(); cache.lock(); }

                // Ordering: store uses Release to publish initialized state.
                #[inline]
                fn atomic() { store(value, Ordering::Release); }

                // SAFETY: The implementation preserves the marker invariant.
                #[cfg(any())]
                unsafe impl Send for Thing {}
            '''),
        })
        self.assertEqual(formatted(report), [])

    def test_safety_above_each_construct_is_accepted_including_empty_impl(self):
        operations = "\n".join("        operation_{0}();".format(index) for index in range(24))
        source = rust('''
            // SAFETY: Callers uphold the raw pointer contract.
            unsafe fn helper() {}

            // SAFETY: The marker trait invariant is maintained by construction.
            unsafe impl Send for Thing {}

            // SAFETY: These declarations match the linked C ABI.
            unsafe extern "C" { fn foreign(); }

            fn body() {
                // SAFETY: The validated handle owns every raw operation below.
                unsafe {
            __OPERATIONS__
                }
            }
        ''').replace("__OPERATIONS__", operations)
        report = analyze({"src/lib.rs": source})
        self.assertEqual(formatted(report, "safety"), [])
        self.assertEqual(report.inventory()["counts"]["unsafe"]["required"], 4)

    def test_safety_inside_only_is_rejected(self):
        report = analyze({
            "src/lib.rs": rust('''
                fn bad() {
                    unsafe {
                        // SAFETY: Too late because this is inside the block.
                        raw();
                    }
                }
            '''),
        })
        self.assertEqual(
            formatted(report, "safety"),
            [
                "src/lib.rs:2:5 [safety] unsafe block needs one substantive // SAFETY: marker immediately above",
                "src/lib.rs:3:9 [safety] // SAFETY: marker is not attached to an unsafe construct",
            ],
        )

    def test_removal_misbinding_and_relocation_fail_every_marker_class(self):
        cases = {
            "safety": (
                "fn f() {\n// SAFETY: Raw handle is validated.\nunsafe { raw(); }\n}\n",
                "unsafe {",
                "unsafe block needs one substantive // SAFETY: marker immediately above",
                None,
            ),
            "lock-order": (
                "// Lock order: first -> second.\nfn f() { first.lock(); second.lock(); }\n",
                "fn f()",
                "function with multiple lock identifiers needs // Lock order: immediately above",
                "// Lock order: first -> unrelated.",
            ),
            "ordering": (
                "// Ordering: state load uses Acquire.\nfn f() { state.load(Ordering::Acquire); }\n",
                "fn f()",
                "function using non-SeqCst atomic ordering needs // Ordering: immediately above",
                "// Ordering: state load uses Release.",
            ),
            "lifecycle": (
                "// Lifecycle: Thing releases handle through take.\nimpl Drop for Thing { fn drop(&mut self) { self.handle.take(); } }\n",
                "impl Drop",
                "Drop impl needs // Lifecycle: immediately above",
                "// Lifecycle: Thing remains responsible during teardown.",
            ),
            "when": (
                "fn f(ready: bool) { if ready {\n// When: ready cancellation stops work.\nreturn;\n} }\n",
                "if ready",
                "mandatory branch needs substantive // When: as first body content or at its branch head",
                "// When: unrelated cancellation stops work.",
            ),
        }
        for rule, (base, target, missing_message, misbound_line) in cases.items():
            marker_line = next(line for line in base.splitlines() if "//" in line)
            removed = base.replace(marker_line + "\n", "")
            with self.subTest(rule=rule, mutation="removal"):
                relevant = formatted(analyze({"src/lib.rs": removed}), rule)
                self.assertEqual(len(relevant), 1, relevant)
                self.assertIn(missing_message, relevant[0])

            if misbound_line is not None:
                misbound = base.replace(marker_line, misbound_line)
                with self.subTest(rule=rule, mutation="misbinding"):
                    relevant = formatted(analyze({"src/lib.rs": misbound}), rule)
                    self.assertGreaterEqual(len(relevant), 1, relevant)
                    self.assertTrue(
                        any(
                            phrase in diagnostic
                            for diagnostic in relevant
                            for phrase in ("must name", "does not name")
                        ),
                        relevant,
                    )

            if rule == "when":
                relocated = base.replace(marker_line + "\nreturn;", "prepare();\n" + marker_line + "\nreturn;")
            elif rule == "safety":
                relocated = base.replace(
                    marker_line + "\nunsafe { raw(); }",
                    "unsafe {\n" + marker_line + "\nraw();\n}",
                )
            else:
                relocated = base.replace(marker_line + "\n", "") + marker_line + "\n"
            with self.subTest(rule=rule, mutation="relocation"):
                relevant = formatted(analyze({"src/lib.rs": relocated}), rule)
                self.assertGreaterEqual(len(relevant), 2, relevant)
                self.assertTrue(any(missing_message in diagnostic for diagnostic in relevant), relevant)
                self.assertTrue(any("not attached" in diagnostic for diagnostic in relevant), relevant)

    def test_stacked_markers_are_valid_not_duplicates(self):
        report = analyze({
            "src/lib.rs": rust('''
                // Ordering: first load uses Acquire.
                // Ordering: second store uses Release.
                fn atomic(first: AtomicBool, second: AtomicBool) {
                    first.load(Ordering::Acquire);
                    second.store(true, Ordering::Release);
                }
            '''),
        })
        self.assertEqual(formatted(report, "ordering"), [])


class PathCliAndInventoryTests(unittest.TestCase):
    def test_paths_absolute_caller_relative_repo_relative_and_backslash_are_identical(self):
        files = {
            "src/lib.rs": "pub fn undocumented() {}\n",
            "scripts/.keep": "",
        }
        with repository(files) as root:
            expected = "src/lib.rs:1:1 [public-doc] effectively public function needs purpose rustdoc\n"
            invocations = [
                (root, str(root / "src/lib.rs")),
                (root / "src", "lib.rs"),
                (root / "src", "src/lib.rs"),
                (root / "src", r"src\lib.rs"),
            ]
            outputs = []
            for cwd, path in invocations:
                completed = cli(cwd, "--check", "--paths", path)
                self.assertEqual(completed.returncode, 1, completed.stderr)
                outputs.append(completed.stdout)
            self.assertEqual(outputs, [expected] * len(outputs))

    def test_paths_constrain_diagnostics_but_full_corpus_resolves_modules(self):
        with repository({
            "src/lib.rs": "pub mod api;\npub fn root_missing() {}\n",
            "src/api.rs": "pub fn api_missing() {}\n",
        }) as root:
            report = checker.analyze_repository(
                root,
                paths=["src/api.rs"],
                caller_cwd=root,
            )
        self.assertEqual(
            formatted(report),
            ["src/api.rs:1:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_inventory_is_nonzero_stable_json_with_counts_paths_and_diagnostics(self):
        with repository({
            "src/lib.rs": rust('''
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
                completed = cli(cwd, "--inventory")
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
        with repository({"src/lib.rs": "fn f(ready: bool) { if ready { work(); } }\n"}) as root:
            completed = cli(root, "--semantic-candidates")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(
            completed.stdout,
            "src/lib.rs:1:21 [when-advisory] ordinary branch has no mandatory // When: requirement\n",
        )

    def test_no_in_scope_rust_fails_closed(self):
        with repository({"README": "fixture\n"}) as root:
            completed = cli(root, "--check")
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("no tracked Rust files", completed.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
