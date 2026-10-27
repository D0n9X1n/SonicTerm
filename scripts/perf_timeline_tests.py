"""Perf timeline tests."""

from __future__ import annotations
import copy
import inspect
import json
import unittest

import perf_build_tests
import perf_counter_rows
import perf_counter_schema_tests
import perf_driver
import perf_driver_tests
import perf_latency_tests
import perf_memory_tests
import perf_metrics
import perf_outcomes_tests
import perf_result_schema
import perf_result_tests
import perf_scenarios
import perf_timeline_schema



# The seven integer-ns parts of a 10 ms credited sample, and the two tail sub-parts of the seventh.
TIMELINE_PARTS = {"input_to_parse": 2_000_000, "parse_to_publication": 3_000_000,
                  "publication_to_frame_request": 1_000_000, "frame_request_to_entry": 1_000_000,
                  "entry_to_render": 500_000, "render": 1_500_000, "render_exit_to_credited_end": 1_000_000}
# Every schema-1 field null, the shape of a timeline that derived nothing.
TIMELINE_NULLS = {key: None for key in perf_timeline_schema.TIMELINE_KEYS}
# Readiness found by an output check before the credited entry.
TIMELINE_READINESS = {"outcome": "native_request", "loop_seq": 4, "site": "output_service"}
# A bound pair that fell back: no permit consumed, so no identity and no tick.
TIMELINE_FALLBACK_PAIR = {"credited_dispatch_seq": 3, "admission": "fallback", "permit_identity": "none",
                          "tick_qualified": False}
# A sample whose own split reason is set, so it has no split.
OWN_REASON = {"split_reason": "unsupported", "split": None}


def timeline(variant="ordered", **overrides):
    """One credited sample's schema-1 `echo_timeline` as the harness writes it for `variant`; overrides replace
    keys and DROP removes one."""
    identity = {"schema": 1, "token": 7, "window": 1, "pane": 2}
    ordered = {**TIMELINE_NULLS, **identity, "availability": "recorded", "ordering": "ordered",
               "ready_before_publication": False, "overflow": False, "parts_ns": dict(TIMELINE_PARTS),
               "render_exit_to_dispatch_return_ns": 600_000, "dispatch_return_to_credited_end_ns": 400_000,
               "latest_read_to_parse_ns": 50_000, "read_stamp": "stamped", "readiness": dict(TIMELINE_READINESS),
               "admission": "permit", "permit_present_ns": 700_000, "permit_absent_ns": 300_000,
               "permit_identity": "known", "tick_qualified": True, "tick_to_entry_ns": 900_000,
               "flood_services": 0, "m3_complete": True, "credited_dispatch_seq": 3}
    recorded = {**TIMELINE_NULLS, **identity, "availability": "recorded", "overflow": False,
                "read_stamp": "stamped", "latest_read_to_parse_ns": 50_000}
    # A chain interval reversed against the harness's credited end: bound, read, but not ordered.
    event_order = {**recorded, "ordering": "clock-order", "ordering_reason": "event-order",
                   "ready_before_publication": False, "readiness": dict(TIMELINE_READINESS), **TIMELINE_FALLBACK_PAIR}
    variants = {
        "ordered": ordered,
        # Ordered through the fallback: no permit was consumed, so no occupancy, no tick and no B2.
        "ordered-fallback": {**ordered, "admission": "fallback", "permit_present_ns": None, "permit_absent_ns": None,
                             "permit_identity": "none", "tick_qualified": False, "tick_to_entry_ns": None,
                             "flood_services": 3, "m3_complete": False, "read_stamp": "missing-or-unrepresentable",
                             "latest_read_to_parse_ns": None},
        "during-dispatch": {**event_order, "ordering": "published_during_dispatch", "ordering_reason": None},
        "split-only": {**recorded, "ordering": "split_only", "ordering_reason": "no-credited-pair"},
        # Bound, but readiness came after the entry: the pair's fields with no occupancy and no flood interval.
        "after-entry": {**recorded, "ordering": "split_only", "ordering_reason": "readiness-after-entry",
                        "ready_before_publication": False, "credited_dispatch_seq": 3, "admission": "admitted",
                        "permit_identity": "none", "tick_qualified": False,
                        "readiness": {"outcome": "marked_in_flight", "loop_seq": 9, "site": None}},
        "overflow": {**recorded, "overflow": True, "ordering": "split_only", "ordering_reason": "overflow"},
        "malformed": {**recorded, "ordering": "clock-order", "ordering_reason": "tick-sequence"},
        "event-order": event_order,
        # An unrepresentable instant before binding derives nothing; one in the chain keeps the pair and readiness.
        "unrepresentable-early": {**recorded, "ordering": "clock-order", "ordering_reason": "unrepresentable-instant"},
        "unrepresentable-late": {**event_order, "ordering_reason": "unrepresentable-instant",
                                 "ready_before_publication": None},
        # The sample's own split reason, kept as the timeline's.
        "own-reason": {**recorded, "ordering": "split_only", "ordering_reason": "unsupported",
                       "latest_read_to_parse_ns": None},
        "unavailable": {**TIMELINE_NULLS, **identity, "availability": "unavailable", "unavailable_reason": "cfg-off"},
        "rejected": {**TIMELINE_NULLS, **identity, "availability": "rejected", "rejection_reason": "mismatch"},
    }
    body = {**variants[variant], **overrides}
    return {key: value for key, value in body.items() if value is not perf_latency_tests.DROP}


def timeline_sample(echo_timeline, **overrides):
    """A credited 10 ms split sample carrying `echo_timeline`; overrides replace sample keys."""
    return perf_latency_tests.split_sample((2.0, 3.0, 5.0), echo_timeline=echo_timeline, **overrides)


# A split whose publication-to-present part disagrees with the timeline's last five parts.
LAST_FIVE_SPLIT = dict(perf_latency_tests.split_sample()["split"], publication_to_present_ms=4.0)


def timeline_latency(samples, coverage=perf_latency_tests.DROP):
    """A schema-1 latency object carrying `samples` and the coverage the harness writes over its credited samples,
    one without a timeline object counted as empty; `coverage` supplies another."""
    latency = perf_latency_tests.split_latency(samples)
    credited = [sample["echo_timeline"] if isinstance(sample.get("echo_timeline"), dict) else {}
                for sample in samples if sample["latency_ms"] is not None]
    latency["echo_timeline_coverage"] = perf_timeline_schema.timeline_coverage(credited) if coverage is perf_latency_tests.DROP else coverage
    return latency


def timeline_problems(latency, schema=1, scope=("S2", "default"), split=1, **result_fields):
    """What validate_result reports for a result carrying `latency`, under the split schema, the declared
    echo-timeline schema and the run's scope."""
    return perf_result_schema.validate_result(perf_result_tests.valid_result(latency=latency, **result_fields), perf_memory_tests.HARNESS_HASH, 0,
                                latency_split_schema=split, echo_timeline_schema=schema, scope=scope)


def timeline_side(*timelines_per_run, coverage=True):
    """A side's valid counters runs, each with one credited sample per timeline (None for a sample without one);
    `coverage=False` is a legacy harness that writes neither the coverage nor any timeline."""
    runs = []
    for timelines in timelines_per_run:
        if coverage:
            latency = timeline_latency([timeline_sample(entry) for entry in timelines])
        else:
            latency = perf_latency_tests.split_latency([perf_latency_tests.split_sample() for _ in timelines])
        runs.append(perf_outcomes_tests.make_outcome(result=perf_counter_schema_tests.counters_result(latency=latency)))
    return perf_metrics.SideRuns(outcomes=runs)


def producer_split_reason(reason, bound=True):
    """The JSON timeline.rs writes for a credited sample whose own split reason is `reason`: the pair is bound and
    its admission, permit and tick read, while readiness, the read-to-parse duration, occupancy and the flood
    interval stay null; `bound=False` is the same record with no render inside the forward."""
    entry = {**TIMELINE_NULLS, "schema": 1, "availability": "recorded", "overflow": False, "read_stamp": "stamped",
             "ordering": "clock-order" if reason == "clock-order" else "split_only", "ordering_reason": reason,
             "token": 11, "window": 7, "pane": 3}
    if bound:
        entry.update(credited_dispatch_seq=1, admission="permit", permit_identity="known", tick_qualified=True,
                     tick_to_entry_ns=350)
    return entry


# Each timeline rule as one case breaking it alone: (rule, the timeline, sample overrides). Every earlier rule
# holds, so the named rule is the first the reader reports.
TIMELINE_RULE_CASES = (
    ("keys", timeline(token=perf_latency_tests.DROP), {}),
    ("keys", timeline(extra=1), {}),
    ("schema", timeline(schema=2), {}),
    ("field-types", timeline(flood_services=-1), {}),
    ("field-types", timeline(token=True), {}),
    ("field-types", timeline(token=2 ** 64), {}),
    ("field-types", timeline(credited_dispatch_seq=2 ** 32), {}),
    ("field-types", timeline(readiness=dict(TIMELINE_READINESS, loop_seq=2 ** 32)), {}),
    ("field-types", timeline(readiness=dict(TIMELINE_READINESS, site="paint")), {}),
    ("field-types", timeline(parts_ns={**TIMELINE_PARTS, "render": 1.5}), {}),
    ("field-types", timeline(parts_ns={**TIMELINE_PARTS, "render": 10 ** 400}), {}),
    ("field-types", timeline(ordering="sideways"), {}),
    ("availability-reason", timeline(unavailable_reason="cfg-off"), {}),
    ("availability-reason", timeline("rejected", rejection_reason=None), {}),
    ("rejection-reason", timeline("rejected", rejection_reason="loop-sequence"), {}),
    ("unavailable-reason", timeline("unavailable", unavailable_reason="later"), {}),
    ("unrecorded-carries-diagnostics", timeline("rejected", m3_complete=True, flood_services=0), {}),
    ("unrecorded-carries-diagnostics", timeline("unavailable", ordering="ordered"), {}),
    ("recorded-fields", timeline(read_stamp=None, latest_read_to_parse_ns=None), {}),
    ("parts-without-ordered", timeline("split-only", parts_ns=dict(TIMELINE_PARTS)), {}),
    ("parts-without-ordered", timeline(parts_ns=None), {}),
    ("ordered-with-overflow", timeline(overflow=True), {}),
    ("ordered-split", timeline(), {"split_reason": "unsupported"}),
    ("ordered-split", timeline(credited_dispatch_seq=None, admission=None, permit_identity=None,
                               tick_qualified=None, tick_to_entry_ns=None), {}),
    ("overflow-event-fields", timeline("overflow", admission="fallback"), {}),
    ("malformed-derives-nothing", timeline("malformed", flood_services=2, m3_complete=True), {}),
    ("malformed-derives-nothing", timeline("malformed", ordering="split_only"), {}),
    ("malformed-derives-nothing", timeline("malformed", **TIMELINE_FALLBACK_PAIR), {}),
    ("reason-known", timeline("split-only", ordering_reason="fiction"), {}),
    ("reason-status", timeline(ordering_reason="sum"), {}),
    ("reason-status", timeline("split-only", ordering_reason=None), {}),
    ("reason-status", timeline("event-order", ordering="split_only"), {}),
    ("reason-precedence", timeline("split-only"), OWN_REASON),
    ("reason-precedence", timeline("own-reason"), {}),
    ("reason-overflow", timeline("split-only", overflow=True), {}),
    ("reason-overflow", timeline("overflow", overflow=False), {}),
    ("bound-pair", timeline("split-only", credited_dispatch_seq=3), {}),
    ("bound-pair", timeline("ordered-fallback", permit_identity="known"), {}),
    ("bound-pair", timeline(permit_identity=None), {}),
    ("reason-binding", timeline("split-only", **TIMELINE_FALLBACK_PAIR), {}),
    ("reason-binding", timeline("split-only", ordering_reason="missing-event:output-check"), {}),
    ("reason-readiness", timeline("after-entry", ordering_reason="missing-event:output-check",
                                  ready_before_publication=None), {}),
    ("reason-readiness", timeline(readiness=None, ready_before_publication=None), {}),
    ("readiness-split", timeline("unrepresentable-late", latest_read_to_parse_ns=None), OWN_REASON),
    ("readiness-split", timeline("unrepresentable-late", latest_read_to_parse_ns=None, flood_services=4,
                                 m3_complete=True), OWN_REASON),
    ("readiness-split", timeline("split-only", readiness=dict(TIMELINE_READINESS)), {"split": None}),
    ("unrepresentable-pair", timeline("unrepresentable-late", readiness=None), {}),
    ("after-entry-intervals", timeline("after-entry", flood_services=1, m3_complete=True), {}),
    ("after-entry-intervals", timeline("after-entry", permit_present_ns=1, permit_absent_ns=1), {}),
    ("ready-before-reason", timeline("after-entry", ordering_reason="ready-before-publication"), {}),
    ("ready-before-reason", timeline("split-only", ready_before_publication=True), {}),
    ("ready-before-reason", timeline("event-order", ready_before_publication=True), {}),
    ("unrepresentable-publication", timeline("unrepresentable-late", ready_before_publication=True), {}),
    ("unrepresentable-publication", timeline("unrepresentable-late", ready_before_publication=False), {}),
    ("ordered-prerequisites", timeline(ready_before_publication=None), {}),
    ("ordered-prerequisites", timeline(flood_services=None, m3_complete=None), {}),
    ("ordered-prerequisites", timeline(permit_present_ns=None, permit_absent_ns=None), {}),
    ("sub-parts", timeline(render_exit_to_dispatch_return_ns=999), {}),
    ("sub-parts", timeline(dispatch_return_to_credited_end_ns=None), {}),
    ("parts-sum", timeline(parts_ns={**TIMELINE_PARTS, "render": 2_000_000}), {}),
    ("split-parts", timeline(parts_ns={**TIMELINE_PARTS, "input_to_parse": 0, "parse_to_publication": 5_000_000}),
     {}),
    ("split-parts", timeline(), {"split": LAST_FIVE_SPLIT}),
    ("occupancy-sum", timeline(permit_present_ns=700_000_000, permit_absent_ns=300_000_000), {}),
    ("tick-to-entry-without-qualified", timeline(tick_qualified=False), {}),
    ("tick-qualified-identity", timeline(permit_identity="unknown"), {}),
    ("permit-durations", timeline(permit_absent_ns=None), {}),
    ("permit-durations", timeline("ordered-fallback", permit_present_ns=500_000, permit_absent_ns=500_000), {}),
    ("read-to-parse-stamp", timeline(read_stamp="not-configured"), {}),
    ("context-prerequisites", timeline("after-entry", ordering_reason="missing-event:output-check", readiness=None,
                                       ready_before_publication=None, flood_services=1, m3_complete=True), {}),
    ("context-prerequisites", timeline("after-entry", ordering_reason="missing-event:output-check", readiness=None,
                                       ready_before_publication=None, permit_present_ns=1, permit_absent_ns=1), {}),
    ("context-prerequisites", timeline("split-only", flood_services=1, m3_complete=True), {}),
    ("context-prerequisites", timeline("unrepresentable-early", flood_services=1, m3_complete=True), {}),
    ("context-prerequisites", dict(producer_split_reason("pane-not-shown"), permit_present_ns=1, permit_absent_ns=1),
     {"split_reason": "pane-not-shown", "split": None}),
    ("read-to-parse-split", timeline("own-reason", latest_read_to_parse_ns=50_000), OWN_REASON),
    ("read-to-parse-split", dict(producer_split_reason("clock-order"), latest_read_to_parse_ns=1),
     {"split_reason": "clock-order", "split": None}),
    ("flood-pair", timeline("event-order", flood_services=1), {}),
    ("ready-before-publication-ordered", timeline(ready_before_publication=True), {}),
)


def population(numerator, denominator):
    """One coverage population as the harness writes it."""
    return {"numerator": numerator, "denominator": denominator}


class EchoTimelineTests(perf_driver_tests.CompareHarness, unittest.TestCase):
    def test_every_disposition_the_producer_writes_is_accepted(self):
        # Each disposition the harness can write passes every rule: ordered, fallback-ordered, published during the
        # dispatch, named split-only, readiness after the entry, overflowed, malformed and still recorded, chain
        # clock-order, an unrepresentable instant before or after binding, unavailable and rejected.
        for variant in ("ordered", "ordered-fallback", "during-dispatch", "split-only", "after-entry", "overflow",
                        "malformed", "event-order", "unrepresentable-early", "unrepresentable-late", "unavailable",
                        "rejected"):
            with self.subTest(variant=variant):
                self.assertIsNone(perf_timeline_schema._timeline_problem(timeline(variant), timeline_sample(None)))
        legitimate = (
            (timeline(read_stamp="missing-or-unrepresentable", latest_read_to_parse_ns=None), {}),
            (timeline(permit_identity="unknown", tick_qualified=False, tick_to_entry_ns=None), {}),
            (timeline(admission="admitted", permit_identity="none", tick_qualified=False, tick_to_entry_ns=None), {}),
            (timeline("malformed", read_stamp="not-configured", latest_read_to_parse_ns=None), {}),
            (timeline("after-entry", ready_before_publication=True), {}),
            (timeline("rejected", rejection_reason="malformed-initial-permit"), {}),
            (timeline("unavailable", unavailable_reason="exhausted", token=None, window=None), {}),
            # A sample's own split reason stays the timeline's, overflowed or not, below only an unrepresentable
            # instant and a malformed record; a sample whose own reason is clock-order keeps its status.
            (timeline("own-reason"), OWN_REASON),
            (timeline("own-reason", overflow=True), OWN_REASON),
            (timeline("unrepresentable-early", latest_read_to_parse_ns=None), OWN_REASON),
            (timeline("malformed", latest_read_to_parse_ns=None), OWN_REASON),
            (timeline("own-reason", ordering="clock-order", ordering_reason="clock-order"),
             {"split_reason": "clock-order", "split": None}),
        )
        for entry, overrides in legitimate:
            with self.subTest(entry=entry, overrides=overrides):
                self.assertIsNone(perf_timeline_schema._timeline_problem(entry, timeline_sample(None, **overrides)))

    def test_an_unrepresentable_instant_carries_only_what_its_sources_allow(self):
        # Before binding (timeline.rs:1009-1012) it derives nothing; in the chain (875-881) it keeps the pair and the
        # readiness of a valid split but has no publication instant, so no publication flag (1069-1073). Readiness
        # comes only from the sample's split (981, 1054), whatever reason wins. Each holds through validate_result.
        for name, entry, overrides in (
                ("early", timeline("unrepresentable-early"), {}),
                ("early, own reason", timeline("unrepresentable-early", latest_read_to_parse_ns=None), OWN_REASON),
                ("late", timeline("unrepresentable-late"), {})):
            with self.subTest(accepted=name):
                self.assertEqual(timeline_problems(timeline_latency([timeline_sample(entry, **overrides)])), [])
        for name, entry, overrides, rule in (
                ("pair and readiness without a split",
                 timeline("unrepresentable-late", latest_read_to_parse_ns=None), OWN_REASON, "readiness-split"),
                ("and a flood interval",
                 timeline("unrepresentable-late", latest_read_to_parse_ns=None, flood_services=4, m3_complete=True),
                 OWN_REASON, "readiness-split"),
                ("a publication flag", timeline("unrepresentable-late", ready_before_publication=True), {},
                 "unrepresentable-publication")):
            with self.subTest(refused=name):
                problems = timeline_problems(timeline_latency([timeline_sample(entry, **overrides)]))
                self.assertTrue(any(f"echo_timeline {rule}:" in problem for problem in problems), problems)

    def test_a_samples_own_split_reason_keeps_its_bound_context(self):
        # The producer binds the pair and reads its admission, permit and tick before the sample's own split reason
        # suppresses readiness and the parts (timeline.rs:1026-1052), so these exact records are accepted, a known
        # consumed tick outside M4 included, with or without a pair; any readiness or parts are still refused.
        for reason in ("pane-not-shown", "clock-order", "row-identity-changed"):
            for bound in (True, False):
                with self.subTest(reason=reason, bound=bound):
                    entry = producer_split_reason(reason, bound)
                    sample = timeline_sample(entry, split_reason=reason, split=None)
                    self.assertIsNone(perf_timeline_schema._timeline_problem(entry, sample))
                    self.assertFalse(perf_timeline_schema._timeline_in_m4(entry))
        sample = timeline_sample(None, split_reason="pane-not-shown", split=None)
        with_readiness = dict(producer_split_reason("pane-not-shown"), readiness=dict(TIMELINE_READINESS))
        self.assertEqual((perf_timeline_schema._timeline_problem(with_readiness, sample) or "").split(":")[0], "reason-readiness")

    def test_each_rule_names_the_inconsistency_it_rejects(self):
        # Every inconsistent combination is a named problem, never a fallback, and every rule has a case.
        for rule, entry, overrides in TIMELINE_RULE_CASES:
            with self.subTest(rule=rule, entry=entry, overrides=overrides):
                problem = perf_timeline_schema._timeline_problem(entry, timeline_sample(None, **overrides))
                self.assertEqual((problem or "").split(":")[0], rule, problem)
        self.assertEqual({rule for rule, _entry, _overrides in TIMELINE_RULE_CASES},
                         {name for name, _broken, _requirement in perf_timeline_schema.TIMELINE_RULES})

    def test_validation_reports_each_broken_sample_by_position(self):
        # A result with a broken timeline fails validation, naming the sample and the rule.
        samples = [timeline_sample(timeline()), timeline_sample(timeline(overflow=True))]
        self.assertEqual(timeline_problems(timeline_latency(samples)),
                         ["latency sample 1 echo_timeline ordered-with-overflow: an overflowed record is never ordered"])

    def test_impossible_numbers_are_named_problems_never_exceptions(self):
        # Values beyond the producer's integer widths, huge integers and booleans in a count are named schema
        # problems from validate_result, before any float conversion can overflow.
        entries = {"u64 identity": timeline(token=2 ** 80), "u32 dispatch": timeline(credited_dispatch_seq=2 ** 80),
                   "u32 loop": timeline(readiness=dict(TIMELINE_READINESS, loop_seq=2 ** 80)),
                   "huge part": timeline(parts_ns={**TIMELINE_PARTS, "render": 10 ** 400}),
                   "boolean count": timeline(flood_services=True)}
        for name, entry in entries.items():
            with self.subTest(case=name):
                problems = timeline_problems(timeline_latency([timeline_sample(entry)]))
                self.assertTrue(any("echo_timeline field-types" in problem for problem in problems), problems)

    def test_coverage_is_recomputed_from_the_per_sample_fields(self):
        # Ordered, fallback-ordered, split-only, overflowed, malformed, unavailable and rejected credited samples:
        # the ordered denominator and the credited one differ, and an uncredited sample counts in neither.
        entries = [timeline(), timeline("ordered-fallback"), timeline("split-only"), timeline("overflow"),
                   timeline("malformed"), timeline("unavailable"), timeline("rejected")]
        self.assertEqual(perf_timeline_schema.timeline_coverage(entries), {
            "schema": 1, "availability": "available", "credited": 7, "recorded": population(5, 7),
            "ordered": population(2, 7), "b2": population(1, 2), "m4": population(1, 2), "m3": population(1, 2),
            "read_stamped": population(1, 2), "b2_of_credited": population(1, 7),
            "m3_of_credited": population(1, 7)})
        samples = [timeline_sample(entry) for entry in entries] + [
            perf_latency_tests.reason_sample("not-credited", None) | {"echo_timeline": None}]
        self.assertEqual(timeline_problems(timeline_latency(samples)), [])

    def test_every_credited_in_scope_sample_needs_a_timeline_and_counts(self):
        # Under the declared schema a credited S2 sample carries an object, unavailable included: nine null
        # timelines beside one ordered one are nine problems, and the credited denominator is ten, never one.
        samples = [timeline_sample(timeline())] + [timeline_sample(None) for _ in range(9)]
        shrunk = perf_timeline_schema.timeline_coverage([timeline()])
        problems = timeline_problems(timeline_latency(samples, coverage=shrunk))
        self.assertEqual(sum("timeline-missing" in problem for problem in problems), 9, problems)
        self.assertTrue(any("echo_timeline_coverage is" in problem for problem in problems), problems)
        counted = timeline_latency(samples)["echo_timeline_coverage"]
        self.assertEqual((counted["credited"], counted["ordered"]), (10, population(1, 10)))
        unavailable = [timeline_sample(timeline("unavailable", unavailable_reason="not-recorded"))] * 2
        self.assertEqual(timeline_problems(timeline_latency(unavailable)), [])
        # A declared harness that drops every timeline field is not read as one that predates the schema.
        stripped = timeline_problems(perf_latency_tests.split_latency([perf_latency_tests.split_sample()]))
        self.assertIn("latency echo_timeline_coverage is missing", stripped)
        self.assertIn("latency sample 0 is missing echo_timeline", stripped)

    def test_the_scope_decides_which_samples_carry_a_timeline(self):
        # Only a credited S2/default or S2/flood sample has one: an uncredited or out-of-scope sample carries null
        # and its run reports null coverage. The run's scope wins; without one the result's own names decide.
        in_scope = timeline_latency([timeline_sample(timeline())])
        self.assertEqual(timeline_problems(in_scope, scope=("S2", "flood")), [])
        out_of_scope = timeline_latency([timeline_sample(None)], coverage=None)
        self.assertEqual(timeline_problems(out_of_scope, scope=("S1", "default")), [])
        problems = timeline_problems(timeline_latency([timeline_sample(timeline())], coverage=None),
                                     scope=("S1", "default"))
        self.assertTrue(any("timeline-out-of-scope" in problem for problem in problems), problems)
        problems = timeline_problems(in_scope, scope=("S1", "default"))
        self.assertTrue(any("echo_timeline_coverage is" in problem for problem in problems), problems)
        missing = timeline_latency([timeline_sample(None)])
        self.assertEqual(timeline_problems(out_of_scope, scope=None, scenario="S1", variant="default"), [])
        problems = timeline_problems(missing, scope=None, scenario="S2", variant="flood")
        self.assertTrue(any("timeline-missing" in problem for problem in problems), problems)
        self.assertEqual(timeline_problems(out_of_scope, scope=("S1", "default"), scenario="S2", variant="flood"),
                         [])

    def test_an_unavailable_side_has_null_populations_never_zero(self):
        # Nothing recorded is unavailable with null populations; nothing credited is no coverage at all.
        coverage = perf_timeline_schema.timeline_coverage([timeline("unavailable"), timeline("rejected")])
        self.assertEqual(coverage["availability"], "unavailable")
        self.assertEqual(coverage["recorded"], population(0, 2))
        self.assertTrue(all(coverage[name] is None for name in perf_timeline_schema.TIMELINE_POPULATIONS), coverage)
        self.assertIsNone(perf_timeline_schema.timeline_coverage([]))
        zero = perf_timeline_schema.timeline_coverage([timeline("split-only"), timeline("malformed")])
        self.assertEqual((zero["availability"], zero["ordered"], zero["b2"]),
                         ("available", population(0, 2), population(0, 0)))

    def test_supplied_coverage_that_disagrees_with_the_samples_fails(self):
        # The supplied coverage is compared with the recomputation, never trusted; a zero for an unavailable
        # side, a missing coverage, a missing timeline key and a timeline on an uncredited sample all fail.
        samples = [timeline_sample(timeline()), timeline_sample(timeline("split-only"))]
        honest = timeline_latency(samples)
        inflated = copy.deepcopy(honest["echo_timeline_coverage"])
        inflated["ordered"]["numerator"] = 2
        unavailable = [timeline_sample(timeline("unavailable"))]
        zeroed = copy.deepcopy(timeline_latency(unavailable)["echo_timeline_coverage"])
        zeroed["ordered"] = population(0, 1)
        cases = {
            "inflated": (timeline_latency(samples, coverage=inflated), "echo_timeline_coverage is"),
            "zero for unavailable": (timeline_latency(unavailable, coverage=zeroed), "echo_timeline_coverage is"),
            "no coverage": ({key: value for key, value in honest.items() if key != "echo_timeline_coverage"},
                            "echo_timeline_coverage is missing"),
            "no timeline key": (timeline_latency([*samples, perf_latency_tests.split_sample()]), "sample 2 is missing echo_timeline"),
            "uncredited": (timeline_latency([*samples, perf_latency_tests.reason_sample("not-credited", None)
                                             | {"echo_timeline": timeline()}]), "timeline-out-of-scope"),
            "not an object": (timeline_latency([timeline_sample("ordered")]), "object: is neither null"),
        }
        self.assertEqual(timeline_problems(honest), [])
        for name, (latency, expected) in cases.items():
            with self.subTest(case=name):
                problems = timeline_problems(latency)
                self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_the_coverage_shape_is_strict_before_any_equality(self):
        # A bool or a float equals an integer in Python, so the shape is checked first: each of these equals the
        # honest coverage under `==`, or breaks its shape, and is a named problem either way.
        honest = timeline_latency([timeline_sample(timeline())])
        coverage = honest["echo_timeline_coverage"]
        self.assertEqual({**coverage, "schema": True}, coverage)
        broken = {
            "schema bool": {**coverage, "schema": True},
            "credited bool": {**coverage, "credited": True},
            "numerator bool": {**coverage, "ordered": {"numerator": True, "denominator": 1}},
            "denominator float": {**coverage, "recorded": {"numerator": 1, "denominator": 1.0}},
            "huge count": {**coverage, "credited": 10 ** 400},
            "extra key": {**coverage, "extra": 1},
            "missing key": {key: value for key, value in coverage.items() if key != "m4"},
            "unknown availability": {**coverage, "availability": "partly"},
            "null recorded": {**coverage, "recorded": None},
            "half a population": {**coverage, "b2": {"numerator": 1}},
        }
        for name, supplied in broken.items():
            with self.subTest(case=name):
                problems = timeline_problems({**honest, "echo_timeline_coverage": supplied})
                self.assertTrue(any(problem.startswith("latency echo_timeline_coverage") for problem in problems),
                                problems)

    def test_a_harness_that_declares_no_timeline_keeps_the_split_contract(self):
        # A harness without the declaration predates the timeline: nothing is checked when it writes none, and a
        # timeline or a coverage it writes anyway is a problem. Nothing is checked without the split schema.
        self.assertEqual(timeline_problems(perf_latency_tests.split_latency([perf_latency_tests.split_sample()]), schema=None), [])
        problems = timeline_problems(timeline_latency([timeline_sample(timeline())]), schema=None)
        self.assertEqual(problems, ["latency echo_timeline is written, but the harness declares no "
                                    "echo_timeline_schema"])
        broken = timeline_latency([timeline_sample(timeline(overflow=True))])
        self.assertEqual(perf_latency_tests.split_problems(broken, schema=None),
                         ["latency echo_timeline is written, but the harness declares no echo_timeline_schema"])

    def test_the_timeline_does_not_hide_behind_a_null_split_capability(self):
        # The echo-timeline schema depends on the split schema: declared without it is a problem, and its checks
        # still run. With neither declared, a harness that writes a timeline is still refused.
        legacy = {"inject_unix_s": 1.0, "latency_ms": 10.0, "attributed": True, "reason": "credited"}
        overflowed = timeline(overflow=True)
        latency = {"samples": [dict(legacy, echo_timeline=overflowed)], "attributed": 1, "total": 1,
                   "coverage": 1.0, "echo_timeline_coverage": perf_timeline_schema.timeline_coverage([overflowed])}
        problems = timeline_problems(latency, split=None)
        self.assertIn("echo_timeline_schema is declared without latency_split_schema 1", problems)
        self.assertTrue(any("ordered-with-overflow" in problem for problem in problems), problems)
        stripped = {"samples": [legacy], "attributed": 1, "total": 1, "coverage": 1.0}
        problems = timeline_problems(stripped, split=None)
        self.assertIn("latency echo_timeline_coverage is missing", problems)
        self.assertIn("latency sample 0 is missing echo_timeline", problems)
        self.assertEqual(timeline_problems(latency, split=None, schema=None),
                         ["latency echo_timeline is written, but the harness declares no echo_timeline_schema"])
        self.assertEqual(timeline_problems(stripped, split=None, schema=None), [])
        with self.assertRaises(ValueError):
            perf_scenarios.parse_scenario_list(json.dumps({**perf_build_tests.LIST_JSON, "capabilities": {"echo_timeline_schema": 1}}))

    def test_rows_print_each_population_per_side_with_points_and_the_verdict(self):
        # Both sides recorded: every population's named numerator and denominator per side, the change in
        # points, and the two-sided gate passing; only S2's two variants get rows.
        side = timeline_side([timeline(), timeline("ordered-fallback")])
        rows = perf_counter_rows.timeline_rows("S2/default", side, side)
        self.assertEqual([row[2] for row in rows],
                         ["echo timeline availability", *[label for label, _ in perf_counter_rows.TIMELINE_ROWS],
                          "echo timeline coverage gate (ordered >= 80%, gap <= 10 points)"])
        by_label = {row[2]: row[3:] for row in rows}
        self.assertEqual(by_label["echo timeline availability"], ["available", "available", "n/a"])
        self.assertEqual(by_label["echo timeline ordered / credited"], ["2/2 (100.0%)", "2/2 (100.0%)", "+0.0 pts"])
        self.assertEqual(by_label["echo timeline B2 / ordered"], ["1/2 (50.0%)", "1/2 (50.0%)", "+0.0 pts"])
        self.assertEqual(rows[-1][5], "pass (base 100.0%, head 100.0%)")
        self.assertEqual(len(perf_counter_rows.timeline_rows("S2/flood", side, side)), len(rows))
        self.assertEqual(perf_counter_rows.timeline_rows("S1/default", side, side), [])
        self.assertIn("timeline_rows(result.label, result.base, result.head)", inspect.getsource(perf_driver._compare))
        # A credited sample without a timeline object stays in its side's credited denominator.
        gapped = {row[2]: row[3:] for row in perf_counter_rows.timeline_rows("S2/default", timeline_side([timeline(), None]), side)}
        self.assertEqual(gapped["echo timeline ordered / credited"][0], "1/2 (50.0%)")

    def test_the_verdict_is_unavailable_against_a_legacy_harness_base(self):
        # A legacy harness writes no timeline: its cells say so, nothing is compared, and the two-sided verdict is
        # unavailable rather than a pass or a fail; neither side writing one prints no rows.
        legacy = timeline_side([timeline(), timeline()], coverage=False)
        head = timeline_side([timeline(), timeline()])
        rows = perf_counter_rows.timeline_rows("S2/default", legacy, head)
        self.assertTrue(all(row[3] == "n/a (no timeline)" for row in rows), rows)
        self.assertTrue(all(row[5] == "n/a" for row in rows[:-1]), rows)
        self.assertEqual(rows[-1][5], "unavailable (base recorded no timeline)")
        self.assertEqual(perf_counter_rows.timeline_rows("S2/default", legacy, legacy), [])
        head_only = perf_counter_rows.timeline_rows("S2/default", perf_metrics.SideRuns(blocked=perf_counter_rows.COUNTERS_HEAD_ONLY), head)
        self.assertEqual(head_only[-1][3:], ["n/a", "2/2 (100.0%)", "unavailable (base recorded no timeline)"])

    def test_the_verdict_is_unavailable_against_the_stub_base(self):
        # The overlaid harness on the stub App writes an unavailable/not-recorded timeline for every credited
        # sample: the side is unavailable with recorded 0/2, never a measured zero, and the verdict is unavailable.
        stub = timeline_side([timeline("unavailable", unavailable_reason="not-recorded")] * 2)
        rows = {row[2]: row[3:] for row in perf_counter_rows.timeline_rows("S2/default", stub, timeline_side([timeline()] * 2))}
        self.assertEqual(rows["echo timeline availability"][0], "unavailable")
        self.assertEqual(rows["echo timeline recorded / credited"][0], "0/2 (0.0%)")
        self.assertEqual(rows["echo timeline ordered / credited"], ["unavailable", "2/2 (100.0%)", "n/a"])
        self.assertEqual(rows["echo timeline coverage gate (ordered >= 80%, gap <= 10 points)"][2],
                         "unavailable (base recorded no timeline)")

    def test_an_unavailable_side_is_told_apart_from_a_measured_zero(self):
        # A cfg-off base recorded nothing: unavailable, recorded 0/2, verdict unavailable. A base that recorded
        # but ordered nothing is a measured 0.0% and fails the gate.
        head = timeline_side([timeline(), timeline()])
        off = {row[2]: row[3:] for row in perf_counter_rows.timeline_rows(
            "S2/default", timeline_side([timeline("unavailable"), timeline("unavailable")]), head)}
        self.assertEqual(off["echo timeline availability"][0], "unavailable")
        self.assertEqual(off["echo timeline recorded / credited"][0], "0/2 (0.0%)")
        self.assertEqual(off["echo timeline ordered / credited"], ["unavailable", "2/2 (100.0%)", "n/a"])
        self.assertEqual(off["echo timeline coverage gate (ordered >= 80%, gap <= 10 points)"][2],
                         "unavailable (base recorded no timeline)")
        zero = {row[2]: row[3:] for row in perf_counter_rows.timeline_rows(
            "S2/default", timeline_side([timeline("split-only"), timeline("malformed")]), head)}
        self.assertEqual(zero["echo timeline ordered / credited"], ["0/2 (0.0%)", "2/2 (100.0%)", "+100.0 pts"])
        self.assertEqual(zero["echo timeline B2 / ordered"], ["0/0 (n/a)", "2/2 (100.0%)", "n/a"])
        self.assertEqual(zero["echo timeline coverage gate (ordered >= 80%, gap <= 10 points)"][2],
                         "fail (base 0.0%, head 100.0%)")

    def test_the_gate_is_exact_at_both_boundaries(self):
        # Each side at least 80% and at most ten points apart, by integer cross-products: 82/100 against 92/100
        # is exactly ten points and passes in both directions, with equal or unequal denominators; one more
        # point fails; 80% exactly passes and 79% fails.
        def coverage(ordered, credited):
            return perf_timeline_schema.timeline_coverage([timeline()] * ordered + [timeline("split-only")] * (credited - ordered))
        cases = (((82, 100), (92, 100), "pass"), ((92, 100), (82, 100), "pass"), ((41, 50), (92, 100), "pass"),
                 ((92, 100), (41, 50), "pass"), ((4, 5), (9, 10), "pass"), ((80, 100), (4, 5), "pass"),
                 ((82, 100), (93, 100), "fail"), ((93, 100), (82, 100), "fail"), ((41, 50), (93, 100), "fail"),
                 ((4, 5), (91, 100), "fail"), ((79, 100), (80, 100), "fail"), ((8, 10), (19, 20), "fail"))
        for base, head, verdict in cases:
            with self.subTest(base=base, head=head):
                result = perf_counter_rows.timeline_coverage_verdict(coverage(*base), coverage(*head))
                self.assertEqual(result.split(" ")[0], verdict, result)
        self.assertEqual(perf_counter_rows.timeline_coverage_verdict(coverage(82, 100), coverage(92, 100)),
                         "pass (base 82.0%, head 92.0%)")
        self.assertTrue(perf_counter_rows.timeline_coverage_verdict(None, coverage(5, 5)).startswith("unavailable (base"))
        unavailable = perf_timeline_schema.timeline_coverage([timeline("unavailable")])
        self.assertEqual(perf_counter_rows.timeline_coverage_verdict(coverage(5, 5), unavailable),
                         "unavailable (head recorded no timeline)")


GUARD_NONCE = "0123456789abcdef0123456789abcdef"
GUARD_HASH = "ab" * 32
