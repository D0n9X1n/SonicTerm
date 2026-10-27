"""Rust comment lexer tests."""

from __future__ import annotations
import unittest

import rust_comment_test_fixture


class LexerTests(unittest.TestCase):
    def test_nested_comments_and_all_string_families_hide_rust_tokens(self):
        source = rust_comment_test_fixture.rust(r'''
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
        lexed = rust_comment_test_fixture.checker.lex_rust(source)
        words = [token.text for token in lexed.tokens if token.kind == "IDENT"]
        self.assertEqual(words.count("if"), 0)
        self.assertEqual(words.count("unsafe"), 0)
        self.assertEqual(words.count("return"), 0)
        self.assertEqual(sum(comment.text.startswith("/*") for comment in lexed.comments), 1)
        self.assertEqual(sum(token.kind == "STRING" for token in lexed.tokens), 8)

    def test_chars_lifetimes_unicode_columns_and_crlf_are_distinct(self):
        source = "fn f<'a>(x: &'a str) {\r\n    let c = '中'; let q = b'\\n';\r\n    α();\r\n}\r\n"
        lexed = rust_comment_test_fixture.checker.lex_rust(source)
        lifetimes = [token.text for token in lexed.tokens if token.kind == "LIFETIME"]
        chars = [token.text for token in lexed.tokens if token.kind == "CHAR"]
        alpha = next(token for token in lexed.tokens if token.text == "α")
        self.assertEqual(lifetimes, ["'a", "'a"])
        self.assertEqual(chars, ["'中'", "b'\\n'"])
        self.assertEqual((alpha.line, alpha.column), (3, 5))

    def test_unary_block_after_keyword_is_not_hidden_as_a_macro_tree(self):
        source = "fn f(ready: bool) { if !{ ready } { return; } }\n"
        lexed = rust_comment_test_fixture.checker.lex_rust(source)
        words = [token.text for token in lexed.tokens]
        self.assertIn("ready", words)
        self.assertIn("return", words)
        self.assertEqual(words.count("{"), 3)

    def test_raw_identifier_macro_tree_remains_opaque(self):
        source = "fn f() { r#match! { if fake { return; } } }\n"
        lexed = rust_comment_test_fixture.checker.lex_rust(source)
        words = [token.text for token in lexed.tokens]
        self.assertEqual(words.count("if"), 0)
        self.assertEqual(words.count("return"), 0)

    def test_attributes_and_macro_trees_do_not_create_phantom_constructs(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])


class DocumentationTests(unittest.TestCase):
    def test_outer_docs_attach_before_after_and_between_stacked_attributes(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report, "public-doc"), [])

    def test_block_and_content_doc_attributes_count_but_metadata_does_not(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "public-doc"),
            ["src/lib.rs:9:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_unrelated_item_breaks_doc_attachment(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
                /// This belongs to the constant.
                pub const LIMIT: usize = 1;
                pub fn undocumented() {}
            '''),
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "public-doc"),
            ["src/lib.rs:3:1 [public-doc] effectively public function needs purpose rustdoc"],
        )

    def test_public_unsafe_function_needs_purpose_and_safety_heading(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "safety-doc"),
            ["src/lib.rs:3:1 [safety-doc] effectively public unsafe function rustdoc needs a # Safety section"],
        )
        self.assertNotIn("safety", rust_comment_test_fixture.rule_names(report))

    def test_unsafe_extern_function_is_one_safety_construct(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
                /// Invokes a C-compatible callback.
                ///
                /// # Safety
                /// The caller must satisfy the callback contract.
                // SAFETY: The declaration exposes the documented C ABI contract.
                pub unsafe extern "C" fn callback(_value: u32) {}
            '''),
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["unsafe"]["required"], 1)
