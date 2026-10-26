"""Rust comment branches tests."""

from __future__ import annotations
import unittest

import rust_comment_test_fixture


class BranchRuleTests(unittest.TestCase):
    def test_all_eight_mandatory_branch_shapes_accept_first_content_markers(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 8)

    def test_shared_when_covers_one_if_chain_by_exact_predicate_tokens(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 3)

    def test_shared_when_reports_uncovered_nested_and_duplicate_sites(self):
        uncovered = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            any("shared // When: does not name this branch predicate" in item for item in rust_comment_test_fixture.formatted(uncovered, "when")),
            rust_comment_test_fixture.formatted(uncovered, "when"),
        )

        nested = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            any(diagnostic.startswith("src/lib.rs:5:13 ") for diagnostic in rust_comment_test_fixture.formatted(nested, "when")),
            rust_comment_test_fixture.formatted(nested, "when"),
        )

        duplicate = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            any("covered by both shared and local // When:" in item for item in rust_comment_test_fixture.formatted(duplicate, "when")),
            rust_comment_test_fixture.formatted(duplicate, "when"),
        )

    def test_local_when_does_not_cover_first_nested_construct(self):
        nested_if = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        self.assertEqual(len(rust_comment_test_fixture.formatted(nested_if, "when")), 1)
        self.assertTrue(
            rust_comment_test_fixture.formatted(nested_if, "when")[0].startswith("src/lib.rs:4:9 "),
            rust_comment_test_fixture.formatted(nested_if, "when"),
        )

        nested_match = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(nested_match, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(all(item.startswith(("src/lib.rs:5:13 ", "src/lib.rs:6:13 ")) for item in relevant))

    def test_shared_when_at_head_of_exempt_match_arm_covers_nested_chain(self):
        for prefix in ("", "total += u32::from(number);\n"):
            with self.subTest(prefix=bool(prefix)):
                report = rust_comment_test_fixture.analyze({
                    "src/lib.rs": rust_comment_test_fixture.rust(f'''
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
                self.assertEqual(rust_comment_test_fixture.formatted(report, "when"), [])
                self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 1)

    def test_shared_when_cannot_cover_empty_predicate_tokens(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                fn empty(value: Option<u8>) {
                    // When: option variants select terminal outcomes.
                    match None {
                        Some(_) => return,
                        None => return,
                    }
                }
            '''),
        })
        self.assertGreaterEqual(len(rust_comment_test_fixture.formatted(report, "when")), 2)
        self.assertTrue(
            all("shared // When: cannot cover a tokenless predicate" in item for item in rust_comment_test_fixture.formatted(report, "when")),
            rust_comment_test_fixture.formatted(report, "when"),
        )

    def test_when_marker_length_accepts_160_and_rejects_161_characters(self):
        accepted_body = "ready " + "x" * 154
        rejected_body = "ready " + "x" * 155
        accepted = rust_comment_test_fixture.analyze({
            "src/lib.rs": f"fn f(ready: bool) {{ if ready {{\n// When: {accepted_body}\nreturn;\n}} }}\n",
        })
        rejected = rust_comment_test_fixture.analyze({
            "src/lib.rs": f"fn f(ready: bool) {{ if ready {{\n// When: {rejected_body}\nreturn;\n}} }}\n",
        })
        self.assertEqual(rust_comment_test_fixture.formatted(accepted, "when"), [])
        self.assertTrue(
            any("marker exceeds 2 lines or 160 characters" in item for item in rust_comment_test_fixture.formatted(rejected, "when")),
            rust_comment_test_fixture.formatted(rejected, "when"),
        )

    def test_obvious_ordinary_predicate_is_exempt_and_advisory_only(self):
        report = rust_comment_test_fixture.analyze({"src/lib.rs": "fn f(ready: bool) { if ready { work(); } }\n"})
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["exempt"], 1)
        self.assertEqual(len(report.semantic_candidates), 1)
        self.assertIn("[when-advisory]", report.semantic_candidates[0].format())

    def test_value_selector_exemption_accepts_only_closed_leaf_grammar(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report, "when"), [])
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
                report = rust_comment_test_fixture.analyze({
                    "src/lib.rs": rust_comment_test_fixture.rust(f'''
                        fn selector(ready: bool, retry: bool) {{
                            {statement}
                        }}
                    '''),
                })
                self.assertGreater(len(rust_comment_test_fixture.formatted(report, "when")), 0)
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
                report = rust_comment_test_fixture.analyze({
                    "src/lib.rs": rust_comment_test_fixture.rust(f'''
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
                        for diagnostic in rust_comment_test_fixture.formatted(report, "when")
                    ),
                    rust_comment_test_fixture.formatted(report, "when"),
                )

    def test_value_selector_exemption_is_format_invariant_and_non_interacting(self):
        compact = rust_comment_test_fixture.analyze({
            "src/lib.rs": "fn f(ready: bool) { let x = if ready { state.value } else { values[index] }; }\n",
        })
        expanded = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            self.assertEqual(rust_comment_test_fixture.formatted(report, "when"), [])
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
        self.assertEqual(compact.inventory(), rust_comment_test_fixture.analyze({
            "src/lib.rs": "fn f(ready: bool) { let x = if ready { state.value } else { values[index] }; }\n",
        }).inventory())

    def test_if_body_finder_skips_braces_inside_grouped_conditions(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:2:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:5:5 ") for item in relevant), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 2)
        self.assertEqual(branches["exempt"], 2)

    def test_if_body_finder_keeps_macro_condition_body_braces(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 4, relevant)
        for line in (2, 5, 8, 11):
            self.assertTrue(
                any(item.startswith(f"src/lib.rs:{line}:5 ") for item in relevant),
                relevant,
            )
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 4)

    def test_if_body_finder_skips_direct_block_expressions(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 4, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:2:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:5:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:8:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:13:5 ") for item in relevant), relevant)
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 4)

    def test_if_body_finder_skips_nested_control_expressions(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:2:5 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:8:5 ") for item in relevant), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 2)
        self.assertEqual(branches["exempt"], 4)

    def test_if_body_finder_skips_struct_pattern_braces(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                fn patterned(value: Value) {
                    if let Value { ready, .. } = value {
                        return;
                    }
                }
            '''),
        })
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:2:5 "), relevant)
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 1)

    def test_let_else_scan_skips_balanced_initializer_groups(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 2)

    def test_conditional_let_syntax_is_not_counted_as_let_else(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:4:7 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 1)

    def test_match_arm_body_does_not_suppress_nested_if_chain(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 2, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:6:20 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:8:15 ") for item in relevant), relevant)
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 2)

    def test_nested_match_arms_are_scanned_and_anchor_to_the_inner_arm(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:6:17 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 3)

    def test_match_body_finder_skips_braces_inside_the_scrutinee(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                fn transformed(items: Vec<u8>) {
                    match items.iter().map(|number| { number + 1 }).count() {
                        0 => (),
                        _ => handle(),
                    }
                }
            '''),
        })
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:3:9 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 1)

    def test_match_body_finder_skips_direct_block_scrutinees(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 3, relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:3:9 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:7:9 ") for item in relevant), relevant)
        self.assertTrue(any(item.startswith("src/lib.rs:11:9 ") for item in relevant), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 3)
        self.assertEqual(branches["exempt"], 3)

    def test_match_in_closure_scrutinee_is_scanned_once(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:5:13 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 3)

    def test_nested_match_in_scrutinee_is_not_skipped(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:4:9 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 2)

    def test_direct_nested_match_scrutinee_is_enumerated_exactly_once(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 1, relevant)
        self.assertTrue(relevant[0].startswith("src/lib.rs:6:9 "), relevant)
        branches = report.inventory()["counts"]["branches"]
        self.assertEqual(branches["mandatory"], 1)
        self.assertEqual(branches["exempt"], 3)

    def test_windows_field_name_is_not_a_platform_gate(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": "fn f(state: State) { if state.windows.is_empty() { work(); } }\n",
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 0)
        self.assertEqual(report.inventory()["counts"]["branches"]["exempt"], 1)

    def test_match_arm_shapes_include_direct_and_later_divergence_noop_cfg_discard_unsafe(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 7)

    def test_when_anti_tautology_uses_predicates_patterns_wildcards_and_entire_chain(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "when"),
            [
                "src/lib.rs:3:9 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
                "src/lib.rs:7:9 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
                "src/lib.rs:12:13 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
                "src/lib.rs:15:13 [when] // When: explanation must add meaning beyond the selecting predicate or pattern",
            ],
        )

    def test_else_if_does_not_hide_nested_branches_in_previous_arm(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        relevant = rust_comment_test_fixture.formatted(report, "when")
        self.assertEqual(len(relevant), 3, relevant)
        self.assertTrue(any(diagnostic.startswith("src/lib.rs:2:5 ") for diagnostic in relevant))
        self.assertTrue(any(diagnostic.startswith("src/lib.rs:3:9 ") for diagnostic in relevant))
        self.assertTrue(any(diagnostic.startswith("src/lib.rs:6:12 ") for diagnostic in relevant))
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 3)

    def test_one_new_semantic_token_passes_and_stopword_only_fails(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "when"),
            ["src/lib.rs:7:9 [when] // When: explanation needs at least one semantic token"],
        )

    def test_marker_after_other_body_content_is_missing_and_misplaced(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(r'''
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
            rust_comment_test_fixture.formatted(report, "when"),
            [
                "src/lib.rs:2:5 [when] mandatory branch needs substantive // When: as first body content or at its branch head",
                "src/lib.rs:4:9 [when] // When: marker is not attached to a mandatory branch",
            ],
        )

    def test_break_and_continue_anywhere_in_body_make_branch_mandatory(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report, "when"), [])
        self.assertEqual(report.inventory()["counts"]["branches"]["mandatory"], 2)
