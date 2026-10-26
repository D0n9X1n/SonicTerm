"""Rust comment modules tests."""

from __future__ import annotations
import unittest

import rust_comment_test_fixture


class VisibilityAndResolutionTests(unittest.TestCase):
    def test_private_public_and_reexported_modules_define_effective_visibility(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "public-doc"),
            [
                "src/exposed.rs:1:1 [public-doc] effectively public function needs purpose rustdoc",
                "src/glob_reexport.rs:1:1 [public-doc] effectively public function needs purpose rustdoc",
                "src/grouped_reexport.rs:3:1 [public-doc] effectively public function needs purpose rustdoc",
                "src/item_reexport.rs:2:1 [public-doc] effectively public function needs purpose rustdoc",
            ],
        )

    def test_nested_private_child_stays_exempt_and_inline_public_child_is_checked(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "public-doc"),
            ["src/lib.rs:6:9 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_public_trait_declarations_are_checked(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
                pub trait Service {
                    /// Starts the service.
                    fn start(&self);
                    fn stop(&self);
                }
            '''),
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "public-doc"),
            ["src/lib.rs:4:5 [public-doc] public trait function needs purpose rustdoc"],
        )

    def test_nested_default_modules_and_public_inherent_methods_are_checked(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            rust_comment_test_fixture.formatted(report, "public-doc"),
            [
                "src/lib.rs:6:5 [public-doc] effectively public function needs purpose rustdoc",
                "src/outer/child/mod.rs:1:1 [public-doc] effectively public function needs purpose rustdoc",
            ],
        )

    def test_binary_and_build_public_functions_are_doc_exempt(self):
        report = rust_comment_test_fixture.analyze({
            "src/main.rs": "pub fn binary_helper() {}\nfn main() {}\n",
            "build.rs": rust_comment_test_fixture.rust('''
                pub fn build_helper() {
                    first.lock();
                    second.lock();
                    if ready { return; }
                }
                fn main() {}
            '''),
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])

    def test_test_target_and_exact_excluded_target_resolve_without_checks(self):
        report = rust_comment_test_fixture.analyze({
            "crates/app/src/lib.rs": rust_comment_test_fixture.rust(r'''
                #[cfg(test)]
                #[path = "lib_tests.rs"]
                mod lib_tests;
                #[path = "../../sonicterm-freetype/src/lib.rs"]
                pub mod ffi;
            '''),
            "crates/app/src/lib_tests.rs": rust_comment_test_fixture.rust('''
                pub fn helper() { first.lock(); second.lock(); }
            '''),
            "crates/sonicterm-freetype/src/lib.rs": "pub fn generated() {}\n",
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])
        inventory = report.inventory()
        self.assertIn("crates/app/src/lib_tests.rs", inventory["paths"]["test_context"])
        self.assertIn("crates/sonicterm-freetype/src/lib.rs", inventory["paths"]["excluded"])

    def test_missing_tracked_module_is_an_error_even_when_cfg_gated(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                #[cfg(target_os = "plan9")]
                mod absent;
            '''),
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "module-resolution"),
            ["src/lib.rs:2:1 [module-resolution] module absent has no tracked source target"],
        )

    def test_nondefault_path_has_precedence_over_default_target(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                #[path = "chosen.rs"]
                pub mod api;
            '''),
            "src/chosen.rs": "pub fn chosen_needs_docs() {}\n",
            "src/api.rs": "pub fn default_must_not_be_reached() {}\n",
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "public-doc"),
            ["src/chosen.rs:1:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_raw_identifier_module_resolves_to_unprefixed_filename(self):
        # Rust raw identifiers use the keyword's ordinary spelling in source filenames.
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": "pub mod r#async;\n",
            "src/async.rs": "/// Execute one callback.\npub fn execute() {}\n",
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])

    def test_winit_preserved_source_excludes_only_the_exact_vendor_tree(self):
        # Preserved upstream is exempt, but the authored sibling still enforces unsafe rationale.
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": "fn first_party() {}\n",
            "crates/sonicterm-winit/src/lib.rs": "pub fn upstream() { unsafe { ffi(); } }\n",
            "crates/sonicterm-winit/src/platform_impl/windows/keyboard_tests.rs":
                "#[test]\nfn native_contract() { unsafe { ffi(); } }\n",
            "crates/sonicterm-winit-adapter/src/lib.rs": "pub fn adapter() {}\n",
        })
        self.assertIn("crates/sonicterm-winit/src/lib.rs", report.paths["excluded"])
        self.assertNotIn(
            "crates/sonicterm-winit/src/platform_impl/windows/keyboard_tests.rs", report.paths["excluded"]
        )
        self.assertEqual(
            [item.path for item in report.diagnostics if item.rule == "safety"],
            ["crates/sonicterm-winit/src/platform_impl/windows/keyboard_tests.rs"],
        )
        self.assertEqual(
            [item.path for item in report.diagnostics if item.rule == "public-doc"],
            ["crates/sonicterm-winit-adapter/src/lib.rs"],
        )

    def test_resolution_corpus_keeps_exact_harfbuzz_prefix_narrow(self):
        report = rust_comment_test_fixture.analyze({
            "crates/sonicterm-harfbuzz/src/lib.rs": "pub mod wrapper;\n",
            "crates/sonicterm-harfbuzz/src/wrapper.rs": "pub fn first_party() {}\n",
            "crates/sonicterm-harfbuzz/harfbuzz/src/rust/lib.rs": "pub fn vendored() {}\n",
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "public-doc"),
            ["crates/sonicterm-harfbuzz/src/wrapper.rs:1:1 [public-doc] effectively public function needs purpose rustdoc"],
        )


class TestAndBuildContextTests(unittest.TestCase):
    def test_file_and_item_test_context_exempts_every_rule_except_safety(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            "src/lib_tests.rs": rust_comment_test_fixture.rust('''
                pub fn external_helper() { first.lock(); second.lock(); if ready { return; } }
            '''),
            "tests/integration.rs": rust_comment_test_fixture.rust('''
                pub fn integration_helper() { first.lock(); second.lock(); if ready { return; } }
            '''),
            "build.rs": rust_comment_test_fixture.rust('''
                pub fn build_helper() { first.lock(); second.lock(); if ready { return; } }
            '''),
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])

    def test_detached_non_safety_markers_are_ignored_in_exempt_contexts(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            "src/lib_tests.rs": rust_comment_test_fixture.rust('''
                // When: prose in tests may use the policy vocabulary.
                // Lock order: prose in tests may describe acquisition order.
                // Ordering: prose in tests may describe event ordering.
                // Lifecycle: prose in tests may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn external_helper() {}
            '''),
            "tests/integration.rs": rust_comment_test_fixture.rust('''
                // When: prose in tests may use the policy vocabulary.
                // Lock order: prose in tests may describe acquisition order.
                // Ordering: prose in tests may describe event ordering.
                // Lifecycle: prose in tests may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn integration_helper() {}
            '''),
            "build.rs": rust_comment_test_fixture.rust('''
                // When: prose in build scripts may use the policy vocabulary.
                // Lock order: prose in build scripts may describe acquisition order.
                // Ordering: prose in build scripts may describe event ordering.
                // Lifecycle: prose in build scripts may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn build_helper() {}
            '''),
            "src/main.rs": rust_comment_test_fixture.rust('''
                // When: prose in binaries may use the policy vocabulary.
                // Lock order: prose in binaries may describe acquisition order.
                // Ordering: prose in binaries may describe event ordering.
                // Lifecycle: prose in binaries may describe object lifetime.
                // SAFETY: This marker is deliberately detached.
                fn main() {}
            '''),
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report),
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
        report = rust_comment_test_fixture.analyze({
            "src/lib_tests.rs": "unsafe fn helper() {}\n",
            "tests/integration.rs": "fn probe() { unsafe { operation(); } }\n",
            "build.rs": "unsafe impl Send for BuildThing {}\n",
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "safety"),
            [
                "build.rs:1:1 [safety] unsafe impl needs one substantive // SAFETY: marker immediately above",
                "src/lib_tests.rs:1:1 [safety] unsafe function needs one substantive // SAFETY: marker immediately above",
                "tests/integration.rs:1:14 [safety] unsafe block needs one substantive // SAFETY: marker immediately above",
            ],
        )

    def test_counted_parser_lock_helper_acquires_its_argument(self):
        # lock_parser(&x) and lock_counted(&x, ..) lock x, so with another lock they need an order.
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
                fn unordered() {
                    let guard = crate::app::frame_counters::lock_parser(&pane.parser);
                    launches.borrow_mut();
                }

                // Lock order: parser -> launches; the parser guard drops first.
                fn ordered() {
                    let guard = lock_parser(&parser);
                    launches.borrow_mut();
                }

                // Lock order: parser -> cell; the clock cell is borrowed under the parser guard.
                fn counted() {
                    let guard = lock_counted(&parser, &mut now);
                    cell.borrow_mut();
                }

                fn lock_parser<Value>(parser: &Mutex<Value>) {}
            '''),
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "lock-order"),
            ["src/lib.rs:1:1 [lock-order] function with multiple lock identifiers needs // Lock order: immediately above"],
        )

    def test_production_file_that_declares_test_sibling_is_not_test_context(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "lock-order"),
            ["src/lib.rs:5:1 [lock-order] function with multiple lock identifiers needs // Lock order: immediately above"],
        )
