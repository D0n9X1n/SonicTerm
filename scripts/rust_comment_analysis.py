"""Rust comment analysis."""

from __future__ import annotations
from pathlib import Path

import rust_comment_branches
import rust_comment_contracts
import rust_comment_control_flow
import rust_comment_functions
import rust_comment_markers
import rust_comment_model
import rust_comment_modules
import rust_comment_sources


def _analyze_unit(unit: rust_comment_model.SourceUnit, selected: bool) -> tuple[list[rust_comment_model.Diagnostic], list[rust_comment_model.Diagnostic], dict]:
    diagnostics: list[rust_comment_model.Diagnostic] = []
    semantic: list[rust_comment_model.Diagnostic] = []
    used_markers: set[int] = set()
    traits, impls, public_types = rust_comment_functions._find_traits_and_impls(unit)
    functions = rust_comment_functions._functions(unit)
    counts = {
        "public_candidates": 0,
        "branches_mandatory": 0,
        "branches_exempt": 0,
        "branches_advisory": 0,
        "branches_value_selectors": 0,
        "unsafe_required": 0,
        "lock_required": 0,
        "ordering_required": 0,
        "lifecycle_required": 0,
    }

    for function in functions:
        effective, trait_member = rust_comment_functions._function_effective_visibility(unit, function, traits, impls, public_types)
        function.effective_public = effective
        function.public_trait_member = trait_member
        if effective and not unit.excluded and not function.test_context and not unit.binary_context:
            counts["public_candidates"] += 1
            comments, attributes, _ = rust_comment_sources._prelude(unit, function.start_token.start)
            docs = rust_comment_sources._doc_text(unit, comments, attributes)
            if not docs and selected:
                message = "public trait function needs purpose rustdoc" if trait_member else "effectively public function needs purpose rustdoc"
                diagnostics.append(rust_comment_model.Diagnostic(unit.path, function.start_token.line, function.start_token.column, "public-doc", message))
            if function.unsafe and "# Safety" not in docs and selected:
                diagnostics.append(rust_comment_model.Diagnostic(unit.path, function.start_token.line, function.start_token.column, "safety-doc", "effectively public unsafe function rustdoc needs a # Safety section"))

        if function.unsafe:
            counts["unsafe_required"] += 1
            if selected:
                rust_comment_contracts._attach_marker(unit, function.start_token, "SAFETY", used_markers, diagnostics, "unsafe function needs one substantive // SAFETY: marker immediately above")

        if not (function.test_context or unit.file_test_context or unit.binary_context):
            nested_ranges = rust_comment_contracts._nested_function_ranges(function, functions)
            body_tokens = rust_comment_contracts._function_body_tokens(function)
            if body_tokens:
                lock_ids = rust_comment_contracts._lock_identifiers(body_tokens, nested_ranges)
                if len(lock_ids) >= 2:
                    counts["lock_required"] += 1
                    if selected:
                        rust_comment_contracts._attach_marker(
                            unit,
                            function.start_token,
                            "Lock order",
                            used_markers,
                            diagnostics,
                            "function with multiple lock identifiers needs // Lock order: immediately above",
                            rust_comment_contracts._lock_validator(lock_ids),
                        )
                atomic_sites = rust_comment_contracts._atomic_sites(body_tokens, nested_ranges)
                non_seqcst = any(orderings - {"SeqCst"} for _, orderings in atomic_sites)
                ordering_markers = rust_comment_markers._strict_prelude_instances(
                    unit,
                    function.start_token,
                    "Ordering",
                )
                if non_seqcst:
                    counts["ordering_required"] += 1
                if selected and (non_seqcst or ordering_markers):
                    rust_comment_contracts._attach_marker(
                        unit,
                        function.start_token,
                        "Ordering",
                        used_markers,
                        diagnostics,
                        "function using non-SeqCst atomic ordering needs // Ordering: immediately above",
                        rust_comment_contracts._ordering_validator(atomic_sites),
                    )

            arms, suppressed = rust_comment_control_flow._match_arms(unit, function, nested_ranges)
            branch_rows = (
                arms
                + rust_comment_control_flow._let_else_branches(unit, function, nested_ranges)
                + rust_comment_control_flow._if_branches(unit, function, suppressed, nested_ranges)
            )
            local_offsets = {
                comment.start
                for branch in branch_rows
                if branch.mandatory
                for instance in rust_comment_markers._first_body_instances(
                    unit,
                    branch.body_start,
                    branch.body_end,
                    "When",
                )
                for comment in instance.comments
            }
            shared_by_construct: dict[int, list[MarkerInstance]] = {}
            for branch in branch_rows:
                shared: list[MarkerInstance] = []
                if branch.construct is not None:
                    key = branch.construct.start
                    if key not in shared_by_construct:
                        shared_by_construct[key] = rust_comment_branches._shared_when_instances(
                            unit,
                            branch.construct,
                            local_offsets,
                        )
                    shared = shared_by_construct[key]
                if branch.mandatory:
                    counts["branches_mandatory"] += 1
                    if selected:
                        rust_comment_branches._branch_marker(unit, branch, shared, used_markers, diagnostics)
                else:
                    counts["branches_exempt"] += 1
                    counts["branches_advisory"] += 1
                    if branch.value_selector:
                        counts["branches_value_selectors"] += 1
                    if selected:
                        message = (
                            "value-selection branch has no mandatory // When: requirement"
                            if branch.value_selector
                            else "ordinary branch has no mandatory // When: requirement"
                        )
                        semantic.append(
                            rust_comment_model.Diagnostic(
                                unit.path,
                                branch.anchor.line,
                                branch.anchor.column,
                                "when-advisory",
                                message,
                            )
                        )

    for index, token in enumerate(unit.tokens):
        if token.text != "unsafe":
            continue
        next_text = unit.tokens[index + 1].text if index + 1 < len(unit.tokens) else ""
        if next_text == "fn":
            continue
        kind = None
        if next_text == "{":
            kind = "block"
        elif next_text == "impl":
            kind = "impl"
        elif next_text == "extern":
            cursor = index + 2
            if cursor < len(unit.tokens) and unit.tokens[cursor].kind == "STRING":
                cursor += 1
            if cursor >= len(unit.tokens) or unit.tokens[cursor].text != "fn":
                kind = "extern block"
        if kind:
            counts["unsafe_required"] += 1
            if selected:
                rust_comment_contracts._attach_marker(unit, token, "SAFETY", used_markers, diagnostics, f"unsafe {kind} needs one substantive // SAFETY: marker immediately above")

    for start, end, target, is_drop, impl_index in impls:
        impl_token = unit.tokens[impl_index]
        if not is_drop or rust_comment_functions._non_safety_exempt_at(unit, impl_token.start, functions):
            continue
        counts["lifecycle_required"] += 1
        if selected:
            body_tokens = [token for token in unit.tokens if start <= token.start < end]
            rust_comment_contracts._attach_marker(
                unit,
                impl_token,
                "Lifecycle",
                used_markers,
                diagnostics,
                "Drop impl needs // Lifecycle: immediately above",
                rust_comment_contracts._lifecycle_validator(target, rust_comment_contracts._release_tokens(body_tokens)),
            )

    if selected:
        detached_messages = {
            "When": "// When: marker is not attached to a mandatory branch",
            "SAFETY": "// SAFETY: marker is not attached to an unsafe construct",
            "Lock order": "// Lock order: marker is not attached to a qualifying function",
            "Ordering": "// Ordering: marker is not attached to a qualifying function",
            "Lifecycle": "// Lifecycle: marker is not attached to a Drop impl",
        }
        for comment in unit.lexed.comments:
            marker = rust_comment_markers._marker_name(comment)
            if not marker or comment.start in used_markers:
                continue
            if marker != "SAFETY" and rust_comment_functions._non_safety_exempt_at(unit, comment.start, functions):
                continue
            diagnostics.append(
                rust_comment_model.Diagnostic(
                    unit.path,
                    comment.line,
                    comment.column,
                    rust_comment_contracts._rule_for_marker(marker),
                    detached_messages[marker],
                )
            )

    return diagnostics, semantic, counts


def _crate_key(path: str) -> str:
    parts = path.split("/")
    return parts[1] if len(parts) > 2 and parts[0] == "crates" else "(root)"


def analyze_repository(root: Path, paths: list[str] | None = None, caller_cwd: Path | None = None) -> rust_comment_model.Report:
    """Analyze one tracked repository with optional diagnostic path filtering."""
    root = root.resolve()
    caller_cwd = (caller_cwd or Path.cwd()).resolve()
    tracked_paths = rust_comment_sources._tracked_rust(root)
    if not tracked_paths:
        raise rust_comment_model.AnalysisError("no tracked Rust files")
    units = {path: rust_comment_sources._source_unit(root, path) for path in tracked_paths}
    resolution_diagnostics: list[Diagnostic] = []
    rust_comment_modules._build_modules(units, set(tracked_paths), resolution_diagnostics)
    check_paths = [path for path, unit in units.items() if not unit.excluded and not unit.file_test_context]
    analysis_paths = [path for path, unit in units.items() if not unit.excluded]
    if not analysis_paths:
        raise rust_comment_model.AnalysisError("no in-scope tracked Rust files")
    selected_paths: set[str] | None = None
    if paths:
        selected_paths = {rust_comment_sources._normalize_path(root, value, caller_cwd) for value in paths}
        expanded: set[str] = set()
        for selected in selected_paths:
            selected_path = root / selected
            if selected_path.is_dir():
                expanded.update(path for path in tracked_paths if path.startswith(selected.rstrip("/") + "/"))
            else:
                expanded.add(selected)
        selected_paths = expanded

    diagnostics = [item for item in resolution_diagnostics if selected_paths is None or item.path in selected_paths]
    semantic: list[Diagnostic] = []
    aggregate = {
        "public_candidates": 0,
        "branches_mandatory": 0,
        "branches_exempt": 0,
        "branches_advisory": 0,
        "branches_value_selectors": 0,
        "unsafe_required": 0,
        "lock_required": 0,
        "ordering_required": 0,
        "lifecycle_required": 0,
    }
    for path in sorted(analysis_paths):
        unit = units[path]
        selected = selected_paths is None or path in selected_paths
        unit_diagnostics, unit_semantic, counts = _analyze_unit(unit, selected)
        diagnostics.extend(unit_diagnostics)
        semantic.extend(unit_semantic)
        if selected:
            for key, value in counts.items():
                aggregate[key] += value
    diagnostics.sort()
    semantic.sort()
    excluded = sorted(path for path, unit in units.items() if unit.excluded)
    test_context = sorted(path for path, unit in units.items() if unit.file_test_context)
    value_selectors_by_crate: dict[str, int] = {}
    for candidate in semantic:
        if "value-selection branch" not in candidate.message:
            continue
        crate = _crate_key(candidate.path)
        value_selectors_by_crate[crate] = value_selectors_by_crate.get(crate, 0) + 1
    counts = {
        "files": {"resolution": len(tracked_paths), "check": len(check_paths), "excluded": len(excluded), "test_context": len(test_context)},
        "public_docs": {"candidates": aggregate["public_candidates"], "missing": sum(item.rule == "public-doc" for item in diagnostics)},
        "branches": {
            "mandatory": aggregate["branches_mandatory"],
            "exempt": aggregate["branches_exempt"],
            "advisory": aggregate["branches_advisory"],
            "value_selectors": aggregate["branches_value_selectors"],
            "value_selectors_by_crate": dict(sorted(value_selectors_by_crate.items())),
        },
        "unsafe": {"required": aggregate["unsafe_required"]},
        "lock_order": {"required": aggregate["lock_required"]},
        "ordering": {"required": aggregate["ordering_required"]},
        "lifecycle": {"required": aggregate["lifecycle_required"]},
    }
    return rust_comment_model.Report(
        diagnostics,
        semantic,
        counts,
        {"resolution": sorted(tracked_paths), "check": sorted(check_paths), "excluded": excluded, "test_context": test_context},
    )
