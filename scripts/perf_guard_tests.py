"""Perf guard tests."""

from __future__ import annotations
import copy
import hashlib
import json
import shutil
from pathlib import Path
import tempfile
import unittest

import perf_execution
import perf_guard_schema
import perf_memory_tests
import perf_result_schema
import perf_result_tests
import perf_timeline_tests


def guard_pane(pane_id: int, prev_next: int, next_seq: int, **overrides: object) -> dict:
    """One pane transfer with every §3.2 field, zeroed, then `overrides`."""
    pane = {"pane_id": pane_id, "closed": False, "identities_exhausted": False,
            "prev_next_section_seq": prev_next, "next_section_seq": next_seq, "carried_pending": None,
            "records": [], "pending": None, "abandoned": [], "abandoned_overflow": 0, "abandoned_unlocated": 0,
            "issued_dropped_located": 0, "issued_dropped_unlocated": 0, "losses": [], "loss_events": 0,
            "losses_merged": False, "refused_closed": 0, "refused_exhausted": 0, "refused_unlocated": 0,
            "first_refusal_ns": None}
    pane.update(overrides)
    return pane


def guard_batch(prev_next: int, next_seq: int, **overrides: object) -> dict:
    """One span batch with every §3.2 field, zeroed, then `overrides`."""
    batch = {"spans": [], "spans_issued": 0, "spans_dropped_located": 0, "spans_dropped_unlocated": 0,
             "losses": [], "loss_events": 0, "losses_merged": False, "collections_issued": 0,
             "collections_refused_exhausted": 0, "prev_next_collection_seq": prev_next,
             "next_collection_seq": next_seq, "identities_exhausted": False, "open_collections": 0}
    batch.update(overrides)
    return batch


def guard_run(count: int) -> list[dict]:
    """A valid run of `count` takes, startup first: take k's window starts at k*1000+100; pane 7 publishes section k
    at [+10, +20] and one span holds [+15, +30], so each complete window overlaps by 5 ns."""
    names = ["startup", "flood", "idle", "tail"]
    sidecars = []
    for take in range(1, count + 1):
        start = take * 1000 + 100
        record = {"section_seq": take, "before_lock_ns": start + 10, "locked_at_ns": start + 20}
        span = {"pane_id": 7, "collection_seq": take, "acquired_ns": start + 15, "released_ns": start + 30}
        sidecars.append({
            "schema": 1, "run_nonce": perf_timeline_tests.GUARD_NONCE, "harness_hash": perf_timeline_tests.GUARD_HASH, "phase_index": take - 1,
            "phase_name": names[take - 1], "take_seq": take, "clock": {"pid": 4242, "run_nonce": perf_timeline_tests.GUARD_NONCE},
            "window": {"label": perf_guard_schema.GUARD_WINDOW_LABEL, "start_ns": start, "end_ns": start + 800},
            "taken_at_ns": start + 850,
            "panes": [guard_pane(7, take, take + 1, records=[record])],
            "spans": guard_batch(take, take + 1, spans=[span], spans_issued=1, collections_issued=1),
        })
    return sidecars


def guard_phases(root: Path, sidecars: list[dict]) -> list[dict]:
    """Write each sidecar under `root` as the harness does and return the phase records naming them."""
    (root / perf_guard_schema.GUARD_SIDECAR_DIR).mkdir(parents=True, exist_ok=True)
    phases = []
    for sidecar in sidecars:
        file_name = f"{perf_guard_schema.GUARD_SIDECAR_DIR}/{sidecar['phase_index']}-{sidecar['phase_name']}.json"
        raw = json.dumps(sidecar, separators=(",", ":")).encode()
        (root / file_name).write_bytes(raw)
        phases.append({"name": sidecar["phase_name"], "guard_correlation": {
            "status": "written", "file": file_name, "take_seq": sidecar["take_seq"], "bytes": len(raw),
            "sha256": hashlib.sha256(raw).hexdigest(), "window": dict(sidecar["window"])}})
    return phases


def guard_rewrite(root: Path, phase: dict, mutate) -> None:
    """Change a written phase's sidecar with `mutate` and re-record its size and digest, leaving the phase's own
    window as it was."""
    target = root / phase["guard_correlation"]["file"]
    sidecar = json.loads(target.read_bytes())
    mutate(sidecar)
    raw = json.dumps(sidecar, separators=(",", ":")).encode()
    target.write_bytes(raw)
    phase["guard_correlation"].update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())


def guard_status(name: str, take_seq: int | None = None) -> dict:
    """A no-sidecar phase field of status `name`, with an unchanged window."""
    return {"status": name, "file": None, "take_seq": take_seq, "bytes": None, "sha256": None,
            "window": {"label": perf_guard_schema.GUARD_WINDOW_LABEL, "start_ns": 100, "end_ns": 900}}


class GuardCorrelationTests(unittest.TestCase):
    """#1584 D2.1b: the chained validation, window completeness and two-pointer join of raw guard takes."""

    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="guard-join-"))
        self.addCleanup(shutil.rmtree, self.root, True)

    def verdicts(self, sidecars: list[dict], expected_hash: str | None = perf_timeline_tests.GUARD_HASH) -> list[dict]:
        # Malformed or inconsistent evidence must come back as named reasons; an exception is a test failure.
        try:
            return perf_guard_schema.guard_correlation_verdicts(guard_phases(self.root, sidecars), self.root, expected_hash)
        except Exception as error:  # noqa: BLE001 - any raise is the defect under test
            self.fail(f"the join raised instead of naming a problem: {type(error).__name__}: {error}")

    def kinds(self, verdicts: list[dict]) -> list[str]:
        return [verdict["verdict"] for verdict in verdicts]

    def test_overlap_at_each_end_containment_and_touching(self) -> None:
        # J1: partial overlap at either end, a span inside the wait and the wait inside a span; touching is 0.
        self.assertEqual(perf_guard_schema.guard_overlap([(10, 20)], [(5, 12)]), (2, 1, 1))
        self.assertEqual(perf_guard_schema.guard_overlap([(10, 20)], [(18, 30)]), (2, 1, 1))
        self.assertEqual(perf_guard_schema.guard_overlap([(10, 20)], [(12, 15)]), (3, 1, 1))
        self.assertEqual(perf_guard_schema.guard_overlap([(10, 20)], [(0, 40)]), (10, 1, 1))
        self.assertEqual(perf_guard_schema.guard_overlap([(10, 20)], [(0, 10), (20, 30)]), (0, 0, 0))
        # A zero-length span strictly inside a wait shares no time, so it is neither a section nor a span.
        self.assertEqual(perf_guard_schema.guard_overlap([(10, 20)], [(15, 15)]), (0, 0, 0))

    def test_several_spans_in_one_wait_count_their_union_once(self) -> None:
        # J2: disjoint spans inside one wait sum to their union; the section counts once, each span once.
        self.assertEqual(perf_guard_schema.guard_overlap([(0, 100)], [(10, 20), (30, 50), (60, 61)]), (31, 1, 3))

    def test_overlapping_spans_of_one_pane_fail_validation(self) -> None:
        # J3: two spans of one pane cannot overlap; the take does not validate.
        sidecars = guard_run(1)
        batch = sidecars[0]["spans"]
        batch["spans"].append(dict(batch["spans"][0], collection_seq=2, acquired_ns=1120))
        batch.update(spans_issued=2, next_collection_seq=3)
        verdict = self.verdicts(sidecars)[0]
        self.assertEqual(verdict["verdict"], "incomplete")
        self.assertTrue(any("overlap" in reason for reason in verdict["reasons"]), verdict)

    def test_panes_are_isolated_and_a_span_needs_its_pane(self) -> None:
        # J4: another pane's span never counts against pane 7's waits; a span whose pane has no container fails.
        sidecars = guard_run(1)
        sidecars[0]["panes"].append(guard_pane(8, 1, 1))
        sidecars[0]["spans"]["spans"][0]["pane_id"] = 8
        verdict = self.verdicts(sidecars)[0]
        self.assertEqual((verdict["verdict"], verdict["observed_overlap_ns"]), ("complete", 0))
        sidecars = guard_run(1)
        sidecars[0]["spans"]["spans"][0]["pane_id"] = 9
        verdict = self.verdicts(sidecars)[0]
        self.assertIn("spans: a span's pane has no container", verdict["reasons"])

    def test_identity_ranges_carries_and_sticky_flags(self) -> None:
        # J5: a carry completing with an unchanged next validates; a carried identity twice, MAX without the
        # flag, and a sticky flag turning false do not; abandoned_unlocated counts in the identity.
        sidecars = guard_run(2)
        first, second = sidecars[0]["panes"][0], sidecars[1]["panes"][0]
        first.update(next_section_seq=3, pending={"section_seq": 2, "registered_ns": 1150})
        second.update(prev_next_section_seq=3, next_section_seq=3, carried_pending=2,
                      records=[{"section_seq": 2, "before_lock_ns": 2110, "locked_at_ns": 2120}])
        verdicts = self.verdicts(sidecars)
        self.assertEqual(self.kinds(verdicts), ["incomplete", "complete"], verdicts)
        self.assertIn("pane 7: section 2 is pending", verdicts[0]["reasons"])
        twice = copy.deepcopy(sidecars)
        twice[1]["panes"][0]["abandoned"] = [{"section_seq": 2, "registered_ns": 2101, "abandoned_ns": 2102}]
        self.assertEqual(self.kinds(self.verdicts(twice))[1], "incomplete")
        sentinel = guard_run(1)
        sentinel[0]["panes"][0].update(next_section_seq=perf_guard_schema.GUARD_U64_MAX)
        self.assertTrue(any("u64::MAX" in reason for reason in self.verdicts(sentinel)[0]["reasons"]))
        sticky = guard_run(2)
        sticky[0]["panes"][0]["identities_exhausted"] = True
        self.assertTrue(any("sticky" in reason for reason in self.verdicts(sticky)[1]["reasons"]))
        # Two identities issued with one disposition fails only the accounting identity.
        undisposed = guard_run(1)
        undisposed[0]["panes"][0].update(next_section_seq=3)
        reasons = self.verdicts(undisposed)[0]["reasons"]
        self.assertIn("pane 7: 1 dispositions for 2 issued identities", reasons)
        unlocated = guard_run(1)
        unlocated[0]["panes"][0].update(records=[], abandoned_unlocated=1)
        verdict = self.verdicts(unlocated)[0]
        self.assertEqual(verdict["reasons"], ["pane 7: unlocated abandoned_unlocated"], verdict)

    def test_binding_mismatches_fail(self) -> None:
        # J6: a different pid, a substituted nonce, a clock nonce that differs, a reversed window and a
        # non-increasing take time each fail, and every later take with them.
        for mutate in (lambda sidecar: sidecar["clock"].update(pid=1),
                       lambda sidecar: sidecar.update(run_nonce="f" * 32) or sidecar["clock"].update(run_nonce="f" * 32),
                       lambda sidecar: sidecar["clock"].update(run_nonce="e" * 32),
                       lambda sidecar: sidecar["window"].update(start_ns=sidecar["window"]["end_ns"] + 1),
                       lambda sidecar: sidecar.update(taken_at_ns=1)):
            sidecars = guard_run(3)
            mutate(sidecars[1])
            self.assertEqual(self.kinds(self.verdicts(sidecars)), ["complete", "incomplete", "incomplete"])

    def test_a_null_harness_hash_unbinds_every_take(self) -> None:
        # J6b: a null hash at the first, middle or last take makes every take incomplete, the earlier ones too.
        for position in range(3):
            sidecars = guard_run(3)
            sidecars[position]["harness_hash"] = None
            verdicts = self.verdicts(sidecars)
            self.assertEqual(self.kinds(verdicts), ["incomplete"] * 3, position)
            self.assertTrue(all(verdict["reasons"][0] == "harness_unbound" for verdict in verdicts))
            self.assertTrue(all("observed_overlap_ns" not in verdict for verdict in verdicts))
        sidecars = guard_run(2)
        self.assertEqual(self.kinds(self.verdicts(sidecars, expected_hash="cd" * 32)), ["incomplete"] * 2)

    def test_unsorted_input_gives_the_same_result(self) -> None:
        # J7: records and spans in reverse order are sorted, validated and joined deterministically.
        self.assertEqual(perf_guard_schema.guard_overlap([(50, 60), (0, 20)], [(55, 70), (10, 15)]),
                         perf_guard_schema.guard_overlap([(0, 20), (50, 60)], [(10, 15), (55, 70)]))

    def test_distinct_sections_and_spans(self) -> None:
        # J8: two waits overlapping one span count two sections and one span.
        self.assertEqual(perf_guard_schema.guard_overlap([(0, 10), (20, 30)], [(5, 25)]), (10, 2, 1))

    def test_each_completeness_rule(self) -> None:
        # J9: each §5.2 rule makes the window incomplete; losses_merged alone does not.
        rules = {
            "unknown registered_ns": lambda pane, batch: pane.update(
                next_section_seq=3, pending={"section_seq": 2, "registered_ns": None}),
            "refused_unlocated": lambda pane, batch: pane.update(refused_closed=1, refused_unlocated=1),
            "refusal with no other issue": lambda pane, batch: pane.update(refused_closed=1, first_refusal_ns=1500),
            "open collection": lambda pane, batch: batch.update(open_collections=1),
            "loss meets window": lambda pane, batch: pane.update(
                next_section_seq=3, issued_dropped_located=1, loss_events=1,
                losses=[{"from_ns": 1200, "to_ns": 1300}]),
            "loss_events without losses": lambda pane, batch: pane.update(
                next_section_seq=3, issued_dropped_located=1, loss_events=1),
            "crossing record": lambda pane, batch: pane["records"][0].update(before_lock_ns=1050),
        }
        for rule, mutate in rules.items():
            sidecars = guard_run(1)
            mutate(sidecars[0]["panes"][0], sidecars[0]["spans"])
            verdict = self.verdicts(sidecars)[0]
            self.assertEqual(verdict["verdict"], "incomplete", rule)
            self.assertNotIn("observed_overlap_ns", verdict, rule)
        merged = guard_run(1)
        merged[0]["panes"][0]["losses_merged"] = True
        self.assertEqual(self.verdicts(merged)[0]["verdict"], "complete")

    def test_report_lines_never_print_a_number_when_not_complete(self) -> None:
        # J10: an incomplete phase prints its reasons and no number; an unavailable one prints n/a.
        incomplete = {"phase": "flood", "verdict": "incomplete", "status": "written", "reasons": ["harness_unbound"]}
        self.assertEqual(perf_guard_schema.format_guard_verdict(incomplete),
                         "[perf-compare] guard correlation flood: incomplete: harness_unbound")
        unavailable = {"phase": "flood", "verdict": "n/a", "status": "unavailable", "reasons": []}
        self.assertTrue(perf_guard_schema.format_guard_verdict(unavailable).endswith("n/a (unavailable)"))
        complete = dict(guard_run(1)[0])
        verdict = self.verdicts([complete])[0]
        self.assertIn("observed overlap 5 ns, 1 sections, 1 spans", perf_guard_schema.format_guard_verdict(verdict))

    def test_a_late_record_is_counted_in_neither_window(self) -> None:
        # J11: a section pending at take 1 and published in take 2 inside window 1 is excluded from window 2,
        # and window 1 stays incomplete.
        sidecars = guard_run(2)
        sidecars[0]["panes"][0].update(next_section_seq=3, pending={"section_seq": 2, "registered_ns": 1140})
        late = {"section_seq": 2, "before_lock_ns": 1140, "locked_at_ns": 1180}
        sidecars[1]["panes"][0].update(prev_next_section_seq=3, next_section_seq=4, carried_pending=2,
                                       records=[late, {"section_seq": 3, "before_lock_ns": 2110, "locked_at_ns": 2120}])
        verdicts = self.verdicts(sidecars)
        self.assertEqual(self.kinds(verdicts), ["incomplete", "complete"], verdicts)
        self.assertEqual(verdicts[1]["observed_overlap_ns"], 5)

    def test_the_chain_rejects_gaps_and_misnamed_takes(self) -> None:
        # J12: a take_seq gap invalidates it and every later take; distinct phase names validate; a sidecar
        # naming another phase than its own record is rejected.
        self.assertEqual(self.kinds(self.verdicts(guard_run(3))), ["complete"] * 3)
        gapped = guard_run(3)
        gapped[1]["take_seq"] = 5
        self.assertEqual(self.kinds(self.verdicts(gapped)), ["complete", "incomplete", "incomplete"])
        misnamed = guard_run(2)
        phases = guard_phases(self.root, misnamed)
        phases[1]["name"] = "other"
        verdicts = perf_guard_schema.guard_correlation_verdicts(phases, self.root, perf_timeline_tests.GUARD_HASH)
        self.assertEqual(self.kinds(verdicts), ["complete", "incomplete"])

    def test_a_bad_second_take_invalidates_it_and_every_later_one(self) -> None:
        # H5: three distinct phases, startup first, all validate; deleting, truncating or mismatching take 2,
        # or recording it write_failed, take_exhausted or with no status, leaves phase 1 complete and makes
        # phases 2 and 3 incomplete.
        def deleted(root: Path, phases: list[dict]) -> None:
            (root / phases[1]["guard_correlation"]["file"]).unlink()

        def truncated(root: Path, phases: list[dict]) -> None:
            target = root / phases[1]["guard_correlation"]["file"]
            target.write_bytes(target.read_bytes()[:-5])

        def mismatched(root: Path, phases: list[dict]) -> None:
            phases[1]["guard_correlation"]["sha256"] = "0" * 64

        def status(name: str):
            phases_take = None
            return lambda root, phases: phases[1]["guard_correlation"].update(
                status=name, file=None, bytes=None, sha256=None,
                take_seq=phases[1]["guard_correlation"]["take_seq"] if name == "write_failed" else phases_take)

        def missing(root: Path, phases: list[dict]) -> None:
            del phases[1]["guard_correlation"]

        for label, damage in (("deleted", deleted), ("truncated", truncated), ("mismatched", mismatched),
                              ("write_failed", status("write_failed")), ("take_exhausted", status("take_exhausted")),
                              ("missing", missing)):
            root = self.root / label
            phases = guard_phases(root, guard_run(3))
            damage(root, phases)
            verdicts = perf_guard_schema.guard_correlation_verdicts(phases, root, perf_timeline_tests.GUARD_HASH)
            self.assertEqual(self.kinds(verdicts), ["complete", "incomplete", "incomplete"], label)
        # take_exhausted issues no take_seq, so the next take is consecutive and only the chain rule fails it.
        exhausted_run = guard_run(3)
        exhausted_run[2].update(take_seq=2)
        phases = guard_phases(self.root / "exhausted", exhausted_run)
        status("take_exhausted")(self.root / "exhausted", phases)
        verdicts = perf_guard_schema.guard_correlation_verdicts(phases, self.root / "exhausted", perf_timeline_tests.GUARD_HASH)
        self.assertEqual(self.kinds(verdicts), ["complete", "incomplete", "incomplete"])
        self.assertTrue(any("broke the chain" in reason for reason in verdicts[2]["reasons"]), verdicts[2])
        gate_off = [{"name": name, "guard_correlation": guard_status("gate_off")} for name in ("startup", "flood")]
        verdicts = perf_guard_schema.guard_correlation_verdicts(gate_off, self.root, perf_timeline_tests.GUARD_HASH, "gate_off")
        self.assertEqual(self.kinds(verdicts), ["n/a", "n/a"], "a genuine gate-off run consumes no take")

    def test_the_sidecar_window_is_bound_to_its_phase_record(self) -> None:
        # Finding 1: a header window changed with its digest while the phase keeps its own is rejected; a window
        # ending after its take, or one with a null end, is not complete; the next take still validates.
        for label, mutate in (("moved", lambda sidecar: sidecar["window"].update(start_ns=1500, end_ns=1800)),
                              ("after the take", lambda sidecar: sidecar.update(taken_at_ns=1050))):
            root = self.root / label.replace(" ", "-")
            phases = guard_phases(root, guard_run(2))
            guard_rewrite(root, phases[0], mutate)
            verdicts = perf_guard_schema.guard_correlation_verdicts(phases, root, perf_timeline_tests.GUARD_HASH)
            self.assertEqual(self.kinds(verdicts), ["incomplete", "incomplete"], label)
        unknown = guard_run(2)
        unknown[0]["window"]["end_ns"] = None
        verdicts = self.verdicts(unknown)
        self.assertEqual(self.kinds(verdicts), ["incomplete", "complete"], verdicts)
        self.assertEqual(verdicts[0]["reasons"], ["the window is unknown"])

    def test_the_chain_keeps_clocks_and_registrations_across_takes(self) -> None:
        # Finding 2: an unknown take time breaks the chain; a carried pending section keeps its registration,
        # null included; an earlier take's interval reused under a later section or collection is rejected.
        unknown = guard_run(3)
        unknown[0]["taken_at_ns"] = None
        self.assertEqual(self.kinds(self.verdicts(unknown)), ["incomplete"] * 3)
        for label, registered, later in (("null to number", None, 99999), ("number to null", 1150, None),
                                         ("number changed", 1150, 1160)):
            carried = guard_run(2)
            carried[0]["panes"][0].update(next_section_seq=3, pending={"section_seq": 2, "registered_ns": registered})
            carried[1]["panes"][0].update(prev_next_section_seq=3, carried_pending=2,
                                          pending={"section_seq": 2, "registered_ns": later})
            carried[1]["panes"][0]["records"][0]["section_seq"] = 3
            carried[1]["panes"][0]["next_section_seq"] = 4
            verdicts = self.verdicts(carried)
            self.assertIn("pane 7: carried section 2 changed its registration time", verdicts[1]["reasons"], label)
        reused = guard_run(2)
        reused[1]["panes"][0]["records"][0].update(before_lock_ns=1110, locked_at_ns=1120)
        self.assertIn("pane 7: section 2 predates the previous take", self.verdicts(reused)[1]["reasons"])
        reused_span = guard_run(2)
        reused_span[1]["spans"]["spans"][0].update(acquired_ns=1115, released_ns=1130)
        self.assertIn("spans: a span predates the previous take", self.verdicts(reused_span)[1]["reasons"])

    def test_malformed_fields_are_named_never_raised(self) -> None:
        # Finding 3: a missing frozen field is distinct from null, and a bool or float schema is not 1; each is a
        # named problem, and the take and every later one are incomplete.
        cases = {
            "pending": lambda sidecar: sidecar["panes"][0].pop("pending"),
            "carried_pending": lambda sidecar: sidecar["panes"][0].pop("carried_pending"),
            "first_refusal_ns": lambda sidecar: sidecar["panes"][0].pop("first_refusal_ns"),
            "schema true": lambda sidecar: sidecar.update(schema=True),
            "schema float": lambda sidecar: sidecar.update(schema=1.0),
            "float count": lambda sidecar: sidecar["spans"].update(spans_issued=1.0),
        }
        for label, mutate in cases.items():
            sidecars = guard_run(3)
            mutate(sidecars[1])
            verdicts = self.verdicts(sidecars)
            self.assertEqual(self.kinds(verdicts), ["complete", "incomplete", "incomplete"], label)
            self.assertTrue(verdicts[1]["reasons"], label)

    def test_contradictory_counts_and_crossing_spans_are_not_complete(self) -> None:
        # Finding 4: a refusal with no time and none unlocated, a reversed span loss, a recorded collection with
        # collections_issued 0, and a span crossing the window's start each keep the window from being complete.
        cases = {
            "refusal": lambda sidecar: sidecar["panes"][0].update(refused_closed=1),
            "reversed loss": lambda sidecar: sidecar["spans"].update(
                spans_issued=2, spans_dropped_located=1, loss_events=1, losses=[{"from_ns": 3000, "to_ns": 1}]),
            "no collection": lambda sidecar: sidecar["spans"].update(collections_issued=0),
            "crossing span": lambda sidecar: sidecar["spans"]["spans"][0].update(acquired_ns=1050),
        }
        for label, mutate in cases.items():
            sidecars = guard_run(1)
            mutate(sidecars[0])
            verdict = self.verdicts(sidecars)[0]
            self.assertEqual(verdict["verdict"], "incomplete", label)
            self.assertNotIn("observed_overlap_ns", verdict, label)

    def test_availability_is_bound_to_the_run(self) -> None:
        # Finding 5: a no-take status naming a file or take is malformed; a written run with one middle gate_off
        # is an impossible availability change; a status the build rules out is impossible; a declared contract
        # with every field missing is incomplete; a genuine cfg-off run is n/a and consumes nothing.
        check = perf_guard_schema.guard_field_problems
        self.assertTrue(check(dict(guard_status("gate_off"), file="guard-correlation/1-flood.json")))
        self.assertTrue(check(dict(guard_status("unavailable"), take_seq=2)))
        self.assertTrue(check(dict(guard_status("take_exhausted"), take_seq=2)))
        self.assertFalse(check(guard_status("write_failed", take_seq=2)))
        for middle in ("gate_off", "unavailable"):
            phases = guard_phases(self.root / middle, guard_run(3))
            phases[1]["guard_correlation"] = guard_status(middle)
            verdicts = perf_guard_schema.guard_correlation_verdicts(phases, self.root / middle, perf_timeline_tests.GUARD_HASH)
            self.assertEqual(self.kinds(verdicts), ["incomplete"] * 3, middle)
            self.assertTrue(verdicts[1]["reasons"][0].startswith("availability changes"), verdicts[1])
        phases = guard_phases(self.root / "ruled-out", guard_run(2))
        verdicts = perf_guard_schema.guard_correlation_verdicts(phases, self.root / "ruled-out", perf_timeline_tests.GUARD_HASH, "unavailable")
        self.assertEqual(self.kinds(verdicts), ["incomplete"] * 2)
        missing = perf_guard_schema.guard_correlation_verdicts([{"name": "startup"}, {"name": "flood"}], self.root, perf_timeline_tests.GUARD_HASH,
                                                  "take")
        self.assertEqual(self.kinds(missing), ["incomplete"] * 2)
        unavailable = [{"name": name, "guard_correlation": guard_status("unavailable")} for name in ("startup", "flood")]
        self.assertEqual(self.kinds(perf_guard_schema.guard_correlation_verdicts(unavailable, self.root, perf_timeline_tests.GUARD_HASH, "unavailable")),
                         ["n/a", "n/a"])
        self.assertEqual(perf_guard_schema.guard_availability(None, "on"), None)
        self.assertEqual(perf_guard_schema.guard_availability(False, "on"), "unavailable")
        self.assertEqual(perf_guard_schema.guard_availability(True, "off"), "gate_off")
        self.assertEqual(perf_guard_schema.guard_availability(True, "on"), "take")
        result = perf_result_tests.valid_result()
        problems = perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0, guard_correlation_schema=1)
        self.assertIn("phase 'workload' lacks guard_correlation", problems)
        result["phases"][0]["guard_correlation"] = guard_status("unavailable")
        self.assertIn("phase 'workload' records guard_correlation its harness never declared",
                      perf_result_schema.validate_result(result, perf_memory_tests.HARNESS_HASH, 0))

    def test_evidence_keeps_the_sidecars_and_the_phase_fields_are_typed(self) -> None:
        # H5 retention: the scratch's guard-correlation directory is copied into the evidence; a malformed
        # guard_correlation field is a schema problem.
        scratch, kept = self.root / "scratch", self.root / "kept"
        guard_phases(scratch, guard_run(1))
        perf_execution._keep_scratch(scratch, kept)
        self.assertTrue((kept / perf_guard_schema.GUARD_SIDECAR_DIR / "0-startup.json").is_file())
        check = perf_result_schema._PHASE_FIELDS["guard_correlation"]
        self.assertTrue(check(guard_status("unavailable")))
        self.assertFalse(check({"status": "written"}))
        self.assertFalse(check(dict(guard_status("unavailable"), status="zero")))
        self.assertFalse(check({key: value for key, value in guard_status("gate_off").items() if key != "window"}))
        self.assertFalse(perf_result_schema._PHASE_FIELDS["guard_correlation_transport_ns"](-1))
