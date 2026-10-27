"""Coverage updates."""

from __future__ import annotations
import argparse
import json
from pathlib import Path

import coverage_baselines
import coverage_checkout
import coverage_comparison
import coverage_model
import coverage_provenance
import coverage_reports


def run_update(args: argparse.Namespace, out) -> int:
    """Write the baseline from a report; a baseline that names a CI host changes only from verified provenance."""
    reason = (args.reason or "").strip()
    if not reason:
        raise coverage_model.InputError("--update-baseline requires a non-empty --reason stating why the floor changes")
    stated = (args.runner or "").strip() or None
    path = Path(args.baseline)
    existing = coverage_baselines.load_baseline(str(path)) if path.exists() else None
    report = coverage_reports.load_report(args.report)
    inventory = coverage_reports.load_inventory(args.metadata)
    record = None
    if args.provenance:
        record = coverage_provenance.verify_provenance(args.provenance, args.report, args.metadata, report, args.target, stated, path)
        runner = record.runner
    elif stated is None:
        raise coverage_model.InputError("--update-baseline requires --provenance FILE, the record of the CI run that measured the "
                         f"report, or --runner {coverage_model.LOCAL_RUNNER} for a baseline CI never enforces")
    else:
        runner = stated
        # CI enforces only a baseline that names a CI host, so changing one needs verified evidence.
        hosts = ([(existing.target, existing.runner)] if existing else []) + [(args.target, runner)]
        named = list(dict.fromkeys(f"{host_target} on {host_runner}" for host_target, host_runner in hosts
                                   if host_runner != coverage_model.LOCAL_RUNNER))
        if named:
            raise coverage_model.InputError("--update-baseline requires --provenance FILE whenever the baseline names a CI host "
                             f"({'; '.join(named)}); retrieve and verify the run's evidence as "
                             f"{coverage_model.REBASELINE_PROCEDURE} describes")
    crates, _ = coverage_reports.aggregate(report, inventory, args.source_root)
    if record is not None:
        reason = f"{reason.rstrip('.')}. {record.sentence()}"
    if args.crate:
        updated = coverage_comparison.partial_update(existing, crates, inventory, args.target, runner, reason, args.crate)
    else:
        # A full update may move the baseline to the record's host; its reason names both hosts.
        if existing is not None and (existing.target, existing.runner) != (args.target, runner):
            reason = (f"Host migration from {existing.target} on {existing.runner} to {args.target} on "
                      f"{runner}. {reason}")
        updated = coverage_comparison.full_update(existing, crates, inventory, args.target, runner, reason)
    text = coverage_comparison.render_document(coverage_comparison.baseline_document(updated))
    # Never write a file the check would refuse to load.
    coverage_baselines.parse_baseline(json.loads(text), str(path))
    coverage_checkout._write_atomic(path, text)
    for line in coverage_comparison.describe_update(existing, updated):
        print(line, file=out)
    source = "the verified provenance record" if record is not None else "--target and --runner as stated"
    print(f"Wrote {path} for {args.target} on {runner}; the host identity comes from {source}. Review and "
          "commit the diff.", file=out)
    return 0
