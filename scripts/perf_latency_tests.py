"""Perf latency tests."""

from __future__ import annotations
import inspect
import json
from pathlib import Path
import tempfile
import unittest

import perf_build_tests
import perf_counter_rows
import perf_counter_schema_tests
import perf_driver_tests
import perf_execution
import perf_harness_sources
import perf_home_tests
import perf_latency_schema
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_result_schema
import perf_result_tests
import perf_scenarios
import perf_settings


def split_sample(parts=(2.0, 3.0, 5.0), **overrides):
    """A credited sample split into `parts` (ms), whose latency is their sum; overrides replace sample keys."""
    split = {"input_to_parse_ms": parts[0], "parse_to_publication_ms": parts[1],
             "publication_to_present_ms": parts[2], "delivery_lag_us": 40.0, "delivery": "sent",
             "coalesced": False, "sync_open": False, "echo_generation": 3}
    sample = {"inject_unix_s": 1.0, "latency_ms": sum(parts), "attributed": True, "reason": "credited",
              "split": split, "split_reason": "split"}
    sample.update(overrides)
    return sample


def reason_sample(split_reason, latency_ms=8.0):
    """A sample with no split: credited when `latency_ms` is a number, else uncredited."""
    return {"inject_unix_s": 2.0, "latency_ms": latency_ms, "attributed": latency_ms is not None,
            "reason": "credited" if latency_ms is not None else "no-candidate", "split": None,
            "split_reason": split_reason}


def split_latency(samples):
    """A schema-1 latency object whose aggregates are recomputed from `samples`, as the harness writes it."""
    credited = [sample for sample in samples if sample["latency_ms"] is not None]
    reasons = {}
    for sample in credited:
        reasons[sample["split_reason"]] = reasons.get(sample["split_reason"], 0) + 1
    split_count = reasons.get("split", 0)
    return {"samples": samples, "attributed": len(credited), "total": len(samples),
            "coverage": len(credited) / len(samples) if samples else 0.0, "split_schema": 1,
            "split_count": split_count, "split_reasons": reasons,
            "split_coverage": split_count / len(credited) if credited else None}


def split_problems(latency, schema=1):
    """What validate_result reports for a result carrying `latency`, under the harness's split schema."""
    return perf_result_schema.validate_result(perf_result_tests.valid_result(latency=latency), perf_memory_tests.HARNESS_HASH, 0, latency_split_schema=schema)


LIST_WITH_SPLIT = {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1}}


def split_side(*samples_per_run):
    """A side's valid counters runs, each carrying one latency object built from its samples."""
    return perf_metrics.SideRuns(outcomes=[perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result(latency=split_latency(list(samples))))
                                   for samples in samples_per_run])


LIST_WITH_KINDS = {**perf_build_tests.LIST_JSON, "capabilities": {"latency_split_schema": 1, "phase_kinds": 1}}
# Marks a field kind_phase drops.
DROP = object()


def kind_phase(kind="sustained", **fields):
    """One result.json phase from a phase-kinds harness: valid_result's phase with its kind and presentation
    fields (120 presented frames, the first 1 ms and the last 59 s into the phase, 10 of 130 redraws not
    presenting); `fields` replace keys, and DROP removes one."""
    phase = dict(perf_result_tests.valid_result()["phases"][0], kind=kind, first_present_ms=1.0, last_present_ms=59_000.0,
                 first_present_seq=1, last_present_seq=120, nonpresenting_redraws=10)
    phase.update(fields)
    return {key: value for key, value in phase.items() if value is not DROP}


class PhaseKindTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    def kind_problems(self, phases, schema=1):
        """What validate_result reports for a result carrying `phases`, under the harness's phase-kinds schema;
        a validator that does not take the schema fails the test rather than erroring."""
        try:
            return perf_result_schema.validate_result(perf_result_tests.valid_result(phases=phases), perf_memory_tests.HARNESS_HASH, 0, phase_kinds=schema)
        except TypeError as error:
            # When: validate_result has no phase_kinds parameter, the capability is not validated at all.
            self.fail(f"validate_result takes no phase_kinds: {error}")

    def test_scenario_list_reads_the_phase_kinds_capability(self):
        # A harness that classifies its phases lists phase_kinds 1 beside the split schema; every scenario carries
        # it into its plans. A list with only the split schema is a harness that predates phase kinds.
        try:
            capable = perf_scenarios.parse_scenario_list(json.dumps(LIST_WITH_KINDS))
            legacy = perf_scenarios.parse_scenario_list(json.dumps(LIST_WITH_SPLIT))
            kinds = ([scenario.phase_kinds for scenario in capable], [scenario.phase_kinds for scenario in legacy])
        except (ValueError, AttributeError) as error:
            # When: the list was refused or carries no phase_kinds, the capability is not read.
            self.fail(f"the phase_kinds capability is not read: {error}")
        self.assertEqual(kinds, ([1, 1], [None, None]))
        self.assertEqual([scenario.latency_split_schema for scenario in capable], [1, 1])

    def test_an_unknown_or_incomplete_capability_map_is_refused(self):
        # An unknown phase_kinds schema, a non-integer value, an unknown key or a map without the split schema
        # cannot be validated, so the list is refused rather than read as legacy.
        for capabilities in ({"latency_split_schema": 1, "phase_kinds": 2}, {"latency_split_schema": 1, "phase_kinds": True},
                             {"latency_split_schema": 1, "phase_kinds": 1.0}, {"phase_kinds": 1},
                             {"latency_split_schema": 1, "phase_kinds": 1, "other": 1}):
            with self.subTest(capabilities=capabilities), self.assertRaises(ValueError):
                perf_scenarios.parse_scenario_list(json.dumps({**perf_build_tests.LIST_JSON, "capabilities": capabilities}))

    def test_a_phase_kinds_harness_must_classify_and_trace_every_phase(self):
        # Valid phases of each kind pass; each rule broken alone is a schema failure naming it, never a fallback.
        valid = [kind_phase(), kind_phase("transition", endpoint="sentinel-parsed", completion_ms=40.0),
                 kind_phase("transition", endpoint="sentinel-parsed", completion_missing="expired"),
                 kind_phase("transition", endpoint="sentinel-parsed", completion_missing="incomplete"),
                 kind_phase("hold"),
                 kind_phase("hold", presented_frames=0, first_present_ms=DROP, last_present_ms=DROP,
                            first_present_seq=DROP, last_present_seq=DROP, present_missing="no-presentation")]
        self.assertEqual(self.kind_problems(valid), [])
        transition = {"kind": "transition", "endpoint": "sentinel-parsed", "completion_ms": 40.0}
        no_frames = {"presented_frames": 0, "first_present_ms": DROP, "last_present_ms": DROP,
                     "first_present_seq": DROP, "last_present_seq": DROP}
        cases = {
            "unknown kind": ({"kind": "burst"}, "kind 'burst' is not one of"),
            "no kind": ({"kind": DROP}, "kind None is not one of"),
            "no endpoint": ({**transition, "endpoint": DROP}, "endpoint None is not a transition endpoint"),
            "unknown endpoint": ({**transition, "endpoint": "soon"}, "endpoint 'soon' is not a transition endpoint"),
            "both completions": ({**transition, "completion_missing": "expired"}, "exactly one of completion_ms"),
            "no completion": ({**transition, "completion_ms": DROP}, "exactly one of completion_ms"),
            "negative completion": ({**transition, "completion_ms": -1.0}, "completion_ms -1.0 is not"),
            "infinite completion": ({**transition, "completion_ms": float("inf")}, "completion_ms inf is not"),
            "reason of another endpoint": ({**transition, "completion_ms": DROP, "completion_missing": "not-presented"},
                                           "'not-presented' is not a reason for sentinel-parsed"),
            "hold with a completion": ({"kind": "hold", "completion_ms": 3.0}, "hold phase but carries completion_ms"),
            "sustained with an endpoint": ({"endpoint": "sentinel-parsed"}, "sustained phase but carries endpoint"),
            "frames without a first": ({"first_present_ms": DROP}, "does not record its first and last presentation"),
            "frames with a missing reason": ({"present_missing": "no-presentation"},
                                             "does not record its first and last presentation"),
            "first after last": ({"first_present_ms": 60_000.0}, "first presentation follows its last"),
            "first count after last": ({"first_present_seq": 121}, "first presentation follows its last"),
            "negative offset": ({"first_present_ms": -1.0}, "are not finite non-negative"),
            "fractional count": ({"last_present_seq": 120.5}, "are not finite non-negative"),
            "no frames but a first": ({"presented_frames": 0}, "presented no frame"),
            "no frames without the reason": (no_frames, "presented no frame"),
            "no frames, another reason": ({**no_frames, "present_missing": "late"}, "presented no frame"),
            "non-presenting above redraws": ({"nonpresenting_redraws": 131}, "nonpresenting_redraws 131"),
            "no non-presenting count": ({"nonpresenting_redraws": DROP}, "nonpresenting_redraws None"),
        }
        for name, (fields, expected) in cases.items():
            with self.subTest(case=name):
                problems = self.kind_problems([kind_phase(**fields)])
                self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_a_legacy_harness_keeps_the_old_phase_contract(self):
        # Without the capability a phase carries no kind or presentation fields and none is required.
        self.assertEqual(self.kind_problems(perf_result_tests.valid_result()["phases"], schema=None), [])

    def test_plans_carry_the_head_lists_phase_kinds_into_validation(self):
        # Both sides run the head's overlaid harness, so the head's list decides both plans' phase_kinds, and
        # every result is validated under it.
        try:
            _code, _gate, _calls, plans, _work, _out = self.compare(listing=LIST_WITH_KINDS)
        except ValueError as error:
            # When: the head's list was refused, no plan carries its capability.
            self.fail(f"the head's phase_kinds list was refused: {error}")
        self.assertEqual({getattr(plan, "phase_kinds", None) for plan in plans}, {1})
        # The harness fakes runs above validate_result, so the real run's call is checked in its source.
        self.assertIn("phase_kinds=plan.phase_kinds", inspect.getsource(perf_execution.execute_run))


class LatencySplitTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    def test_scenario_list_reads_the_latency_split_capability(self):
        # A harness that writes the split fields lists the schema; every scenario carries it into its plans.
        scenarios = perf_scenarios.parse_scenario_list(json.dumps(LIST_WITH_SPLIT))
        self.assertEqual([scenario.latency_split_schema for scenario in scenarios], [1, 1])

    def test_a_list_without_capabilities_is_a_legacy_harness(self):
        # A head that predates the split lists no capabilities, and its results keep the old contract.
        scenarios = perf_scenarios.parse_scenario_list(json.dumps(perf_build_tests.LIST_JSON))
        self.assertEqual([scenario.latency_split_schema for scenario in scenarios], [None, None])

    def test_an_unknown_split_capability_is_refused(self):
        # An unknown schema, a non-object, a non-integer value (True and 1.0 included) or an extra key cannot be
        # validated, so the list is refused rather than read as legacy.
        for capabilities in ({"latency_split_schema": 2}, [], "1", {"latency_split_schema": True},
                             {"latency_split_schema": 1.0}, {"latency_split_schema": 1, "other": 1}, {}):
            with self.subTest(capabilities=capabilities), self.assertRaises(ValueError):
                perf_scenarios.parse_scenario_list(json.dumps({**perf_build_tests.LIST_JSON, "capabilities": capabilities}))

    def test_an_older_head_with_a_matching_hash_and_unsplit_latency_is_accepted(self):
        # With no declared schema the latency object is read as before; split fields, even malformed, are not
        # required and not trusted.
        legacy = {"samples": [{"inject_unix_s": 2.0, "latency_ms": 7.5, "attributed": True, "reason": "credited"}],
                  "attributed": 1, "total": 1, "coverage": 1.0}
        self.assertEqual(split_problems(legacy, schema=None), [])
        self.assertEqual(split_problems({**legacy, "split_count": "many"}, schema=None), [])

    def test_a_schema_1_harness_without_the_discriminator_fails(self):
        # A harness that declares schema 1 must say so in every latency object.
        latency = split_latency([split_sample()])
        self.assertEqual(split_problems(latency), [])
        del latency["split_schema"]
        problems = split_problems(latency)
        self.assertTrue(any("split_schema" in problem for problem in problems), problems)
        self.assertEqual(split_problems(latency, schema=None), [])

    def test_a_schema_1_harness_missing_split_fields_fails(self):
        # Every sample, every report field, and an empty list's zero aggregates are required.
        no_reason = split_latency([split_sample(), reason_sample("unsupported")])
        for sample in no_reason["samples"]:
            del sample["split_reason"]
        mixed = split_latency([split_sample(), reason_sample("unsupported")])
        del mixed["samples"][1]["split_reason"]
        empty_with_counts = {**split_latency([]), "attributed": 2, "split_count": 1, "split_reasons": {"split": 1},
                             "split_coverage": 0.5}
        cases = {"every sample": no_reason, "one sample": mixed, "empty with counts": empty_with_counts}
        for field_name in ("split_count", "split_reasons", "split_coverage"):
            missing = split_latency([split_sample()])
            del missing[field_name]
            cases[f"no {field_name}"] = missing
        for name, latency in cases.items():
            with self.subTest(name):
                self.assertTrue(split_problems(latency))

    def test_an_overlaid_base_without_the_feature_still_requires_schema_1(self):
        # Both sides run the head's overlaid harness, so the head's list decides both plans' schema; a base
        # result without the discriminator is then refused like a head result.
        code, _gate, _calls, plans, _work, _out = self.compare(listing=LIST_WITH_SPLIT)
        self.assertEqual(code, 0)
        self.assertEqual({plan.side for plan in plans}, {"base", "head"})
        self.assertEqual({plan.latency_split_schema for plan in plans}, {1})
        base_plan = next(plan for plan in plans if plan.side == "base")
        unmarked = split_latency([reason_sample("unsupported")])
        del unmarked["split_schema"]
        self.assertTrue(perf_result_schema.validate_result(perf_result_tests.valid_result(latency=unmarked), perf_memory_tests.HARNESS_HASH, 0, partial_counters=True,
                                             latency_split_schema=base_plan.latency_split_schema))

    def test_split_validation_rejects_each_rule(self):
        # One valid object (a split, an unsupported and an uncredited sample); each rule broken alone is refused.
        def base():
            return split_latency([split_sample(), reason_sample("unsupported"), reason_sample("not-credited", None)])
        self.assertEqual(split_problems(base()), [])

        def sample_change(position, **changes):
            def change(latency):
                latency["samples"][position].update(changes)
            return change

        def split_change(**changes):
            def change(latency):
                latency["samples"][0]["split"].update(changes)
            return change

        def report_change(**changes):
            def change(latency):
                latency.update(changes)
            return change

        rules = {
            "unknown reason": sample_change(1, split_reason="made-up"),
            "credited reads not-credited": sample_change(1, split_reason="not-credited"),
            "uncredited reads a reason": sample_change(2, split_reason="unsupported"),
            "negative latency": sample_change(1, latency_ms=-1.0),
            "string latency": sample_change(1, latency_ms="slow"),
            "split for another reason": sample_change(1, split=dict(split_sample()["split"])),
            "no split for split": sample_change(0, split=None),
            "negative part": split_change(input_to_parse_ms=-0.5),
            "infinite part": split_change(parse_to_publication_ms=float("inf")),
            "string lag": split_change(delivery_lag_us="slow"),
            "refused delivery": split_change(delivery="refused"),
            "coalesced not a boolean": split_change(coalesced=0),
            "sync_open not a boolean": split_change(sync_open=None),
            "negative generation": split_change(echo_generation=-1),
            "fractional generation": split_change(echo_generation=1.5),
            "boolean generation": split_change(echo_generation=True),
            "parts off the latency": split_change(publication_to_present_ms=5.01),
            "attributed": report_change(attributed=3),
            "reasons": report_change(split_reasons={"split": 1, "unsupported": 1, "not-credited": 1}),
            "boolean reason count": report_change(split_reasons={"split": True, "unsupported": 1}),
            "split count": report_change(split_count=2),
            "coverage": report_change(split_coverage=1.0),
            "null coverage when credited": report_change(split_coverage=None),
        }
        for name, change in rules.items():
            with self.subTest(name):
                latency = base()
                change(latency)
                self.assertTrue(split_problems(latency))
        # Serialized parts are rounded per part, so a sum within 0.001 ms of the latency is accepted.
        within = base()
        within["samples"][0]["split"]["publication_to_present_ms"] = 5.0009
        self.assertEqual(split_problems(within), [])

    def test_a_schema_1_report_must_carry_every_nullable_key(self):
        # A key the schema allows to be null must still be present: dropping it is an incomplete contract, not
        # a null. Explicit nulls in the same places stay valid.
        def without(latency, key, position=None):
            if position is None:
                del latency[key]
            else:
                del latency["samples"][position][key]
            return latency
        self.assertEqual(split_problems(split_latency([])), [])
        self.assertEqual(split_problems(split_latency([reason_sample("not-credited", None),
                                                       reason_sample("unsupported")])), [])
        cases = {
            "empty report without split_coverage": without(split_latency([]), "split_coverage"),
            "uncredited sample without split": without(split_latency([reason_sample("not-credited", None)]),
                                                       "split", 0),
            "uncredited sample without latency_ms": without(split_latency([reason_sample("not-credited", None)]),
                                                            "latency_ms", 0),
            "unsupported credited sample without split": without(split_latency([reason_sample("unsupported")]),
                                                                  "split", 0),
        }
        for name, latency in cases.items():
            with self.subTest(name):
                problems = split_problems(latency)
                self.assertTrue(any("missing" in problem for problem in problems), problems)

    def test_split_validation_rejects_fabricated_aggregates(self):
        # Mutually consistent aggregates that the samples do not support are refused: no sample is split.
        fabricated = split_latency([reason_sample("unsupported")])
        fabricated.update(split_count=10, split_reasons={"split": 10}, split_coverage=1.0, attributed=10)
        problems = split_problems(fabricated)
        for field_name in ("attributed", "split_reasons", "split_count", "split_coverage"):
            self.assertTrue(any(problem.startswith(f"latency {field_name}") for problem in problems), field_name)

    def test_an_empty_split_report_validates(self):
        # No samples: zero counts, no reasons and null coverage.
        self.assertEqual(split_problems(split_latency([])), [])
        self.assertEqual(split_latency([])["split_coverage"], None)

    def test_split_rows_appear_only_for_s2_default(self):
        # The split covers S2/default's typing phase alone, so no other variant gets rows.
        side = split_side([split_sample(), split_sample((1.0, 1.0, 1.0)), reason_sample("unsupported")])
        rows = perf_counter_rows.split_rows("S2/default", side, side, 1)
        self.assertEqual([row[2] for row in rows], [row_label for row_label, _ in perf_counter_rows.SPLIT_ROWS])
        self.assertTrue(all(row[:2] == ["S2/default", "typing"] for row in rows))
        by_label = {row[2]: row for row in rows}
        self.assertEqual(by_label["split input to parse, median (ms)"][4], "1.500 ms (2 splits)")
        self.assertEqual(by_label["split coverage (%)"][4], "66.7% (2/3)")
        self.assertEqual(by_label["split coverage (%)"][5], "+0.0%")
        self.assertIn("split 2, unsupported 1", by_label["split reasons (credited samples)"][4])
        self.assertIn("suppressed 0, coalesced 0, sync_open 0", by_label["split reasons (credited samples)"][4])
        for label in ("S2/flood", "S1/default", "S10/sync"):
            with self.subTest(label):
                self.assertEqual(perf_counter_rows.split_rows(label, side, side, 1), [])

    def test_split_rows_read_unsupported_on_a_base_without_the_feature(self):
        # A base built without perf-echo-trace credits samples but splits none: its cells read n/a (unsupported)
        # and nothing is compared; a head-only set reads n/a.
        base = split_side([reason_sample("unsupported"), reason_sample("unsupported")])
        head = split_side([split_sample(), reason_sample("arm-gate-off")])
        rows = perf_counter_rows.split_rows("S2/default", base, head, 1)
        timing = [row for row in rows if row[2] != "split reasons (credited samples)"]
        self.assertTrue(all(row[3] == "n/a (unsupported)" for row in timing), timing)
        self.assertTrue(all(row[5] == "n/a" for row in rows))
        self.assertEqual(rows[0][4], "2.000 ms (1 splits)")
        head_only = perf_counter_rows.split_rows("S2/default", perf_metrics.SideRuns(blocked=perf_counter_rows.COUNTERS_HEAD_ONLY), head, 1)
        self.assertTrue(all(row[3] == "n/a" for row in head_only))

    def test_split_coverage_reads_zero_unsupported_or_unavailable(self):
        # Credited, supported samples with no split are 0% coverage, a number; only a build whose every credited
        # sample reads unsupported is n/a (unsupported); no credited sample at all is unavailable. Timing cells
        # without a split read n/a (no split), and an empty reason set is never taken as an unsupported build.
        def cells(*samples):
            rows = perf_counter_rows.split_rows("S2/default", split_side(list(samples)), split_side([split_sample()]), 1)
            return {row[2]: row[3] for row in rows}
        zero = cells(reason_sample("no-appearance-observed"), reason_sample("no-appearance-observed"))
        self.assertEqual(zero["split coverage (%)"], "0.0% (0/2)")
        self.assertEqual(zero["split input to parse, median (ms)"], "n/a (no split)")
        unsupported = cells(reason_sample("unsupported"), reason_sample("unsupported"))
        self.assertEqual(unsupported["split coverage (%)"], "n/a (unsupported)")
        self.assertEqual(unsupported["split input to parse, median (ms)"], "n/a (unsupported)")
        nothing = cells(reason_sample("not-credited", None))
        self.assertEqual(nothing["split coverage (%)"], "unavailable")
        self.assertEqual(nothing["split input to parse, median (ms)"], "n/a (no split)")

    def test_split_rows_are_absent_for_a_legacy_head(self):
        # A head harness without the capability has no split to tabulate, whatever its samples say.
        side = split_side([split_sample()])
        self.assertEqual(perf_counter_rows.split_rows("S2/default", side, side, None), [])

    def test_tree_features_adds_echo_trace_only_with_counters(self):
        # perf-echo-trace implies perf-counters: a tree builds with it only when it declares it and supports
        # counters, and a build passes it after the other features.
        echo_table = "\n[features]\nperf-counters = []\nperf-echo-trace = [\"perf-counters\"]\n"

        def tree(manifest, logging_source):
            temporary = tempfile.TemporaryDirectory()
            self.addCleanup(temporary.cleanup)
            root = Path(temporary.name)
            (root / perf_settings.APP_MANIFEST).parent.mkdir(parents=True)
            (root / perf_settings.APP_MANIFEST).write_text(manifest, encoding="utf-8")
            (root / perf_counter_schema_tests.LOGGING_LIB).parent.mkdir(parents=True)
            (root / perf_counter_schema_tests.LOGGING_LIB).write_text(logging_source, encoding="utf-8")
            return root
        self.assertEqual(perf_harness_sources.tree_features(tree(perf_home_tests.HEAD_MANIFEST + echo_table, perf_counter_schema_tests.LOGGING_WITH_FILTER)),
                         ("perf-counters", "perf-echo-trace"))
        self.assertEqual(perf_harness_sources.tree_features(tree(perf_home_tests.HEAD_MANIFEST + echo_table, perf_counter_schema_tests.LOGGING_WITHOUT_FILTER)), ())
        self.assertEqual(perf_harness_sources.tree_features(tree(perf_counter_schema_tests.COUNTERS_MANIFEST, perf_counter_schema_tests.LOGGING_WITH_FILTER)), ("perf-counters",))
        self.assertEqual(perf_scenarios.build_argv("perf_scenarios", True, features=("perf-counters", "perf-echo-trace"))[-1],
                         "perf-counters,perf-echo-trace")

    def test_the_split_reasons_match_the_harness(self):
        # The harness's precedence table is the reason set this script accepts, in the same order.
        record = (Path(__file__).parent.parent / "crates" / "sonicterm-app" / "examples" / "perf_scenarios"
                  / "record.rs").read_text(encoding="utf-8")
        table = record.split("pub(crate) const SPLIT_REASONS: [&str; 22] = [", 1)[1].split("];", 1)[0]
        listed = [name.strip().strip('"') for name in table.split(",") if name.strip()]
        listed = ["split" if name == "SPLIT" else name for name in listed]
        self.assertEqual(tuple(listed), perf_latency_schema.SPLIT_REASONS)
