"""Rust comment markers tests."""

from __future__ import annotations
import unittest

import rust_comment_test_fixture


class MarkerContractTests(unittest.TestCase):
    def test_nested_function_contracts_are_not_counted_by_the_parent(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            rust_comment_test_fixture.formatted(report),
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
        passing = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        self.assertEqual(rust_comment_test_fixture.formatted(passing), [])

        missing_lock = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                // Lock order: first lock -> second lock.
                fn locks(first: Guard, second: Guard, third: Guard) {
                    first.lock(); second.lock(); third.lock();
                }
            '''),
        })
        self.assertTrue(
            any("must name every acquired lock identifier" in item for item in rust_comment_test_fixture.formatted(missing_lock, "lock-order")),
            rust_comment_test_fixture.formatted(missing_lock, "lock-order"),
        )

        missing_ordering = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                // Ordering: state load is sequentially consistent.
                fn atomic(state: AtomicBool) { state.load(Ordering::SeqCst); }
            '''),
        })
        self.assertTrue(
            any("must name each atomic receiver and exact ordering variant" in item for item in rust_comment_test_fixture.formatted(missing_ordering, "ordering")),
            rust_comment_test_fixture.formatted(missing_ordering, "ordering"),
        )

    def test_repeated_single_lock_does_not_require_lock_order(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": "fn repeated(queue: Queue) { queue.lock(); queue.lock(); }\n",
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report, "lock-order"), [])
        self.assertEqual(report.inventory()["counts"]["lock_order"]["required"], 0)

    def test_lifecycle_marker_names_type_and_release_token(self):
        passing = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                // Lifecycle: CommittedReservation uses charge take for exact release.
                impl Drop for CommittedReservation {
                    fn drop(&mut self) { self.charge.take(); }
                }
            '''),
        })
        missing = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                // Lifecycle: CommittedReservation remains responsible until teardown.
                impl Drop for CommittedReservation {
                    fn drop(&mut self) { self.charge.take(); }
                }
            '''),
        })
        self.assertEqual(rust_comment_test_fixture.formatted(passing, "lifecycle"), [])
        self.assertTrue(
            any("must name the Drop type and a release token" in item for item in rust_comment_test_fixture.formatted(missing, "lifecycle")),
            rust_comment_test_fixture.formatted(missing, "lifecycle"),
        )

    def test_contract_marker_prelude_rejects_docs_between_marker_and_item(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                // Ordering: epoch fetch_add uses Release.
                /// This doc line breaks the marker prelude.
                fn bump(epoch: AtomicUsize) { epoch.fetch_add(1, Ordering::Release); }
            '''),
        })
        relevant = rust_comment_test_fixture.formatted(report, "ordering")
        self.assertTrue(any("immediately above" in item for item in relevant), relevant)
        self.assertTrue(any("not attached" in item for item in relevant), relevant)

    def test_marker_length_cap_applies_per_stacked_instance(self):
        long_body = "epoch Release " + "x" * 148
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust(f'''
                // Ordering: epoch Release publishes state.
                // Ordering: {long_body}
                fn bump(epoch: AtomicUsize) {{ epoch.fetch_add(1, Ordering::Release); }}
            '''),
        })
        self.assertTrue(
            any("marker exceeds 2 lines or 160 characters" in item for item in rust_comment_test_fixture.formatted(report, "ordering")),
            rust_comment_test_fixture.formatted(report, "ordering"),
        )

    def test_io_read_write_calls_with_arguments_do_not_count_as_locks(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                fn io(reader: Reader, writer: Writer, buf: &mut [u8]) {
                    reader.read(buf);
                    writer.write(buf);
                    lock.read(&mut guard);
                    lock.write(value);
                }
            '''),
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report, "lock-order"), [])

    def test_zero_arg_lock_methods_require_one_substantive_sequence(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            rust_comment_test_fixture.formatted(report, "lock-order"),
            [
                "src/lib.rs:8:1 [lock-order] // Lock order: marker must state an acquisition sequence",
                "src/lib.rs:14:1 [lock-order] function with multiple lock identifiers needs // Lock order: immediately above",
            ],
        )

    def test_lock_and_ordering_markers_stack_on_one_function(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                // Lock order: state -> cache.
                // Ordering: store uses Release after both guards commit.
                fn combined() {
                    state.lock();
                    cache.lock();
                    store(value, Ordering::Release);
                }
            '''),
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])
        counts = report.inventory()["counts"]
        self.assertEqual(counts["lock_order"]["required"], 1)
        self.assertEqual(counts["ordering"]["required"], 1)

    def test_all_non_seqcst_atomic_orderings_require_one_function_marker(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            rust_comment_test_fixture.formatted(report, "ordering"),
            ["src/lib.rs:14:1 [ordering] function using non-SeqCst atomic ordering needs // Ordering: immediately above"],
        )

    def test_lifecycle_marker_belongs_above_drop_impl_not_drop_method(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
            rust_comment_test_fixture.formatted(report, "lifecycle"),
            [
                "src/lib.rs:7:1 [lifecycle] Drop impl needs // Lifecycle: immediately above",
                "src/lib.rs:8:5 [lifecycle] // Lifecycle: marker is not attached to a Drop impl",
            ],
        )

    def test_item_markers_attach_across_stacked_attributes(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
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
        self.assertEqual(rust_comment_test_fixture.formatted(report), [])

    def test_safety_above_each_construct_is_accepted_including_empty_impl(self):
        operations = "\n".join("        operation_{0}();".format(index) for index in range(24))
        source = rust_comment_test_fixture.rust('''
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
        report = rust_comment_test_fixture.analyze({"src/lib.rs": source})
        self.assertEqual(rust_comment_test_fixture.formatted(report, "safety"), [])
        self.assertEqual(report.inventory()["counts"]["unsafe"]["required"], 4)

    def test_safety_inside_only_is_rejected(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                fn bad() {
                    unsafe {
                        // SAFETY: Too late because this is inside the block.
                        raw();
                    }
                }
            '''),
        })
        self.assertEqual(
            rust_comment_test_fixture.formatted(report, "safety"),
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
                relevant = rust_comment_test_fixture.formatted(rust_comment_test_fixture.analyze({"src/lib.rs": removed}), rule)
                self.assertEqual(len(relevant), 1, relevant)
                self.assertIn(missing_message, relevant[0])

            if misbound_line is not None:
                misbound = base.replace(marker_line, misbound_line)
                with self.subTest(rule=rule, mutation="misbinding"):
                    relevant = rust_comment_test_fixture.formatted(rust_comment_test_fixture.analyze({"src/lib.rs": misbound}), rule)
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
                relevant = rust_comment_test_fixture.formatted(rust_comment_test_fixture.analyze({"src/lib.rs": relocated}), rule)
                self.assertGreaterEqual(len(relevant), 2, relevant)
                self.assertTrue(any(missing_message in diagnostic for diagnostic in relevant), relevant)
                self.assertTrue(any("not attached" in diagnostic for diagnostic in relevant), relevant)

    def test_stacked_markers_are_valid_not_duplicates(self):
        report = rust_comment_test_fixture.analyze({
            "src/lib.rs": rust_comment_test_fixture.rust('''
                // Ordering: first load uses Acquire.
                // Ordering: second store uses Release.
                fn atomic(first: AtomicBool, second: AtomicBool) {
                    first.load(Ordering::Acquire);
                    second.store(true, Ordering::Release);
                }
            '''),
        })
        self.assertEqual(rust_comment_test_fixture.formatted(report, "ordering"), [])
