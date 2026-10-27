"""Coverage provenance."""

from __future__ import annotations
from dataclasses import dataclass, field
import json
from pathlib import Path
from typing import Dict, List, Mapping, Optional, Tuple

import coverage_model
import coverage_records
import coverage_reports


def _text(value: object) -> bool:
    """Whether `value` is a non-empty string."""
    return isinstance(value, str) and bool(value.strip())


def _listed(paths: List[str]) -> str:
    """Join at most TREE_DIFF_LIMIT paths and count the rest."""
    shown = ", ".join(str(path) for path in paths[:coverage_model.TREE_DIFF_LIMIT])
    return shown + (f" and {len(paths) - coverage_model.TREE_DIFF_LIMIT} more" if len(paths) > coverage_model.TREE_DIFF_LIMIT else "")


def checkout_root(baseline_path: Path) -> Optional[str]:
    """Return the top level of the Git checkout that holds `baseline_path`, or None."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    return coverage_checkout._git_text(str(baseline_path.resolve().parent), "rev-parse", "--show-toplevel")


def measured_tree_problems(measured: dict, root: str, baseline_path: Path) -> List[str]:
    """Compare the measured tree's coverage-relevant files with the checkout that holds the baseline."""
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    if not all(isinstance(key, str) and isinstance(value, str) for key, value in measured.items()):
        return ["the record's tree_entries must map each path to its mode and object id"]
    current = coverage_checkout.tree_entries(root)
    changes = coverage_checkout.worktree_changes(root)
    if current is None or changes is None:
        return [f"{baseline_path} is not in a Git checkout with a commit, so the measured tree cannot be compared"]
    differing = sorted(path for path in set(measured) | set(current)
                       if coverage_checkout.coverage_relevant(path) and measured.get(path) != current.get(path))
    uncommitted = [path for path in changes if coverage_checkout.coverage_relevant(path)]
    problems = []
    if differing:
        problems.append("the measured tree's coverage-relevant source or policy differs from this checkout's HEAD "
                        f"in {_listed(differing)}; only the baseline and documentation may differ")
    if uncommitted:
        problems.append(f"this checkout has uncommitted coverage-relevant changes in {_listed(uncommitted)}")
    return problems


@dataclass(frozen=True)
class EvidenceKind:
    """Which coverage job a record must come from, its artifact, and whether it binds a workspace inventory."""

    job: str
    artifact_prefix: str
    inventory: bool


# The crate floors' run binds its report, inventory and subset-gate and floor checks; the macOS-only run its report.
CRATE_EVIDENCE = EvidenceKind(coverage_model.COVERAGE_JOB, coverage_model.EVIDENCE_ARTIFACT_PREFIX, True)
MACOS_EVIDENCE = EvidenceKind(coverage_model.MACOS_JOB, coverage_model.MACOS_ARTIFACT_PREFIX, False)


def pinned_tool_problems(tool: str, root: str) -> List[str]:
    """Refuse a record whose cargo-llvm-cov is not the release this checkout's manifest pins."""
    try:
        entry = json.loads((Path(root) / coverage_model.PINNED_TOOLS_PATH).read_text(encoding="utf-8"))["cargo-llvm-cov"]
        expected = entry["version_output"].replace("{version}", entry["version"])
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        return [f"{coverage_model.PINNED_TOOLS_PATH} in this checkout does not pin cargo-llvm-cov ({error!r})"]
    if tool != expected:
        return [f"the record names {tool}, not {expected}, the version {coverage_model.PINNED_TOOLS_PATH} pins"]
    return []


def verify_provenance(path: str, report_path: str, metadata_path: Optional[str], report: coverage_model.Report, target: str,
                      runner: Optional[str], baseline_path: Path, kind: EvidenceKind = CRATE_EVIDENCE) -> coverage_records.Provenance:
    """Refuse a record that is incomplete, disagrees with its files or the stated host, or measured other content.

    The checks are offline integrity checks: matching digests prove that the files agree with the
    record, not where the record came from. The documented retrieval through the GitHub API ties
    the record to its run.
    """
    # Resolve this runtime-only dependency after module initialization.
    import coverage_checkout
    import coverage_records
    document = coverage_reports._read_json(path, "provenance record")
    if not isinstance(document, dict) or document.get("schema") != coverage_model.PROVENANCE_SCHEMA:
        raise coverage_model.InputError(f"provenance record {path} is not a {coverage_model.PROVENANCE_SCHEMA} document")
    if document.get("measurement") != "complete":
        raise coverage_model.InputError(f"provenance record {path} is incomplete: the run stopped in phase "
                         f"{document.get('failed_phase')!r} ({document.get('failure')}); incomplete evidence never "
                         "initializes or changes a floor")
    if document.get("runner") == coverage_model.LOCAL_RUNNER:
        raise coverage_model.InputError(f"provenance record {path} was measured outside CI (runner {coverage_model.LOCAL_RUNNER!r}); only a CI "
                         "run's record can change a floor")
    # A macOS-only record binds no inventory and runs no subset gate, so it carries neither.
    missing = [name for name in coverage_model.REQUIRED_RECORD_FIELDS
               if (kind.inventory or name != "inventory_sha256") and not _text(document.get(name))]
    if document.get("event") == "pull_request" and not _text(document.get("pull_request_head")):
        missing.append("pull_request_head")
    if kind.inventory and not isinstance(document.get("checks"), dict):
        missing.append("checks")
    if not isinstance(document.get("tree_entries"), dict) or not document.get("tree_entries"):
        missing.append("tree_entries")
    if not isinstance(document.get("worktree_changes"), list):
        missing.append("worktree_changes")
    if missing:
        raise coverage_model.InputError(f"provenance record {path} is missing required field(s): {', '.join(missing)}")
    problems = coverage_records.checks_problems(document["checks"]) if kind.inventory else []
    if document.get("checkout_drift"):
        problems.append("the record names checkout drift: " + "; ".join(map(str, document["checkout_drift"])))
    digests = [("report", report_path, "report_sha256")]
    if kind.inventory:
        digests.append(("inventory", metadata_path, "inventory_sha256"))
    for label, file_path, key in digests:
        actual = coverage_checkout.file_sha256(file_path)
        if actual != document[key]:
            problems.append(f"the {label} {file_path} has SHA-256 {actual}, but the record's {key} is {document[key]}")
    if report.tool != document["cargo_llvm_cov"]:
        problems.append(f"the report names {report.tool}, but the record names {document['cargo_llvm_cov']}")
    verbose = document["rustc_verbose"].splitlines()
    host = next((line[len("host: "):].strip() for line in verbose if line.startswith("host: ")), None)
    if host != document["target"]:
        problems.append(f"the record's rustc -vV reports host {host}, but its target is {document['target']}")
    if verbose[0].strip() != document["rustc_version"]:
        problems.append("the record's rustc_version is not the first line of its rustc -vV output")
    if document["job"] != kind.job:
        problems.append(f"the record's job is {document['job']}, not {kind.job}")
    if document["workflow"] != coverage_model.COVERAGE_WORKFLOW:
        problems.append(f"the record's workflow is {document['workflow']}, not {coverage_model.COVERAGE_WORKFLOW}")
    artifact = f"{kind.artifact_prefix}-{document['run_id']}-{document['run_attempt']}"
    if document["artifact"] != artifact:
        problems.append(f"the record's artifact is {document['artifact']}, not {artifact}")
    if target != document["target"]:
        problems.append(f"--target {target} conflicts with the record's target {document['target']}")
    if runner and runner != document["runner"]:
        problems.append(f"--runner {runner} conflicts with the record's runner {document['runner']}")
    if document["worktree_changes"]:
        problems.append(f"the measured checkout had uncommitted changes in {_listed(document['worktree_changes'])}")
    root = checkout_root(baseline_path)
    if root is None:
        problems.append(f"{baseline_path} is not in a Git checkout with a commit, so the measured tree cannot be "
                        "compared")
    else:
        problems += coverage_checkout.identity_problems(document, root)
        problems += measured_tree_problems(document["tree_entries"], root, baseline_path)
        problems += pinned_tool_problems(document["cargo_llvm_cov"], root)
    if problems:
        raise coverage_model.InputError(f"provenance record {path} does not verify: " + "; ".join(problems))
    return coverage_records.Provenance(document["target"], document["runner"], document["image_version"], document["rustc_version"],
                      document["cargo_llvm_cov"], document["repository"], document["run_id"],
                      document["run_attempt"], document["job"], document["commit"], document["artifact"],
                      document.get("run_url") if _text(document.get("run_url")) else None,
                      document.get("pull_request_head") if _text(document.get("pull_request_head")) else None)
