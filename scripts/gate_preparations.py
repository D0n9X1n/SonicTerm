"""Gate preparations."""

from __future__ import annotations
import hashlib
from pathlib import Path
from typing import Iterable, Mapping, Sequence, TextIO

import gate_model


def _perf_build(step_id: str, example: str, *, release: bool, timeout_s: int, counters: bool = False,
                features: tuple[str, ...] = (), harness_cfgs: tuple[str, ...] | None = None) -> gate_model.Step:
    """One build of a perf harness example: Cargo's JSON messages name the binary perf-compare.py runs.

    `features` are the perf features a tree declares; `counters` is perf-counters alone. `harness_cfgs` are the
    harness API cfgs both refs build with, composed at launch; None composes no harness flags.
    """
    profile = ("--release",) if release else ()
    chosen = features or (("perf-counters",) if counters else ())
    features = ("--features", ",".join(chosen)) if chosen else ()
    return gate_model.Step(step_id, ("cargo", "build", "--locked", *profile, "-p", "sonicterm-app", "--example", example,
                          "--message-format=json-render-diagnostics", *features),
                gate_model.HOSTS, timeout_s, "local", ("rust", "native"), (), windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY,
                harness_cfgs=harness_cfgs)


def _cfg_subsets() -> list[tuple[str, ...]]:
    """Every subset of the harness API cfg table, each in table order, smallest first."""
    # Resolve this runtime-only dependency after module initialization.
    import gate_harness
    return _feature_subsets(gate_harness.HARNESS_API_CFG_NAMES)


# perf-compare.py's builds, outside the table: each only compiles, and on Windows MSVC's linker can leave
# its `vctip.exe` helper alive after Cargo exits, so only these reviewed steps may have it cleaned.
PERF_BUILDS = {step.id: step for step in (
    _perf_build("build-perf_scenarios", "perf_scenarios", release=False, timeout_s=1500),
    *(_perf_build(f"build-{side}-{example}", example, release=True, timeout_s=3600)
      for side in ("head", "base") for example in ("perf_scenarios", "perf_scenarios_alloc")),
)}
# The same comparison builds for a tree that declares perf-counters, under the same ids.
PERF_COUNTER_BUILDS = {step.id: step for step in (
    _perf_build(f"build-{side}-{example}", example, release=True, timeout_s=3600, counters=True)
    for side in ("head", "base") for example in ("perf_scenarios", "perf_scenarios_alloc"))}
# Every perf feature a tree may declare, in the canonical order a build passes them; later hooks append.
# perf-compare.py's PERF_FEATURES must equal this.
PERF_FEATURES = ("perf-counters", "perf-frame-texture", "perf-hook-checkpoint-memory", "perf-echo-trace",
                 "perf-hook-trim")


def _feature_subsets(features: tuple[str, ...]) -> list[tuple[str, ...]]:
    """Every subset of `features`, each kept in `features` order, smallest first."""
    subsets: list[tuple[str, ...]] = [()]
    for feature in features:
        subsets += [subset + (feature,) for subset in subsets]
    return sorted(subsets, key=lambda subset: (len(subset), [features.index(name) for name in subset]))


# Every comparison build, keyed by the perf features a tree declares. The plain and counters sets are the
# two above; every other ordered subset of PERF_FEATURES has its own reviewed steps.
PERF_FEATURE_BUILDS = {
    (): {step_id: built for step_id, built in PERF_BUILDS.items() if step_id != "build-perf_scenarios"},
    ("perf-counters",): PERF_COUNTER_BUILDS,
    **{features: {step.id: step for step in (
        _perf_build(f"build-{side}-{example}", example, release=True, timeout_s=3600, features=features)
        for side in ("head", "base") for example in ("perf_scenarios", "perf_scenarios_alloc"))}
       for features in _feature_subsets(PERF_FEATURES) if features not in ((), ("perf-counters",))},
}
# The comparison builds perf-compare runs, keyed by `(enabled harness API cfgs, features)`: each perf feature
# subset once per subset of the cfg table. Each cfg is an explicit reviewed configuration, not a feature.
PERF_CFG_BUILDS = {
    (cfgs, features): {step.id: step for step in (
        _perf_build(f"build-{side}-{example}", example, release=True, timeout_s=3600, features=features,
                    harness_cfgs=cfgs)
        for side in ("head", "base") for example in ("perf_scenarios", "perf_scenarios_alloc"))}
    for cfgs in _cfg_subsets() for features in _feature_subsets(PERF_FEATURES)
}


def _previous_release_check(cfgs: tuple[str, ...], features: tuple[str, ...]) -> gate_model.Step:
    """The compile-only check of both harness examples on the previous release tag's tree."""
    chosen = ("--features", ",".join(features)) if features else ()
    return gate_model.Step("check-previous-release", ("cargo", "check", "--locked", "-p", "sonicterm-app", "--example",
                                           "perf_scenarios", "--example", "perf_scenarios_alloc", *chosen),
                gate_model.HOSTS, 3600, "local", ("rust", "native"), (), windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY,
                harness_cfgs=cfgs)


# The previous-release check, keyed like PERF_CFG_BUILDS.
PREVIOUS_RELEASE_CHECKS = {(cfgs, features): _previous_release_check(cfgs, features)
                           for cfgs in _cfg_subsets() for features in _feature_subsets(PERF_FEATURES)}
# The same catalog flattened to `(step id, features)`, for a lookup by both halves of a build's identity.
PERF_BUILD_CATALOG = {(step_id, features): built
                      for features, catalog in PERF_FEATURE_BUILDS.items() for step_id, built in catalog.items()}


def _reviewed_step(step: gate_model.Step) -> bool:
    """Whether `step` is one of the gate's own step objects, not a copy that could claim their authority."""
    # Resolve this runtime-only dependency after module initialization.
    import gate_catalog
    import gate_harness
    reviewed = (*gate_catalog.STEPS, *PERF_BUILDS.values(), *PERF_BUILD_CATALOG.values(), *PREVIOUS_RELEASE_CHECKS.values(),
                *(step for catalog in PERF_CFG_BUILDS.values() for step in catalog.values()))
    return any(step is canonical for canonical in (*reviewed, *gate_harness._TARGET_COMPILE_STEPS))


_PREPARATION_SCRIPTS = {
    "workspace-crates": ("check-workspace-crates.sh", "01ea549949d24c6205cc5e72f9c5ac0f3aed20baac3873cb04681fa280a4dece"),
    "pty-feasibility": ("pty-backend-feasibility.sh", "46c57646081f8faa6b61f85572b49d5ea88e1e5e9ce1aae7596ffef705200256"),
}


def _no_run(argv: tuple[str, ...]) -> tuple[str, ...]:
    boundary = argv.index("--") if "--" in argv else len(argv)
    return argv[:boundary] + ("--no-run",) + argv[boundary:]


def _preparation_script_lines(text: str) -> list[str]:
    lines, pending = [], ""
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            if pending:
                raise ValueError("preparation parity: interrupted shell continuation")
            continue
        if line.endswith(chr(92)):
            if not raw.endswith(chr(92)):
                raise ValueError("preparation parity: whitespace after shell continuation")
            pending += line[:-1].strip() + " "
        else:
            lines.append(pending + line)
            pending = ""
    if pending:
        raise ValueError("preparation parity: unterminated continuation")
    return lines


def _script_preparations(step: gate_model.Step, root: Path) -> tuple[gate_model.Preparation, ...]:
    # Resolve this runtime-only dependency after module initialization.
    import gate_catalog
    name, expected_shape = _PREPARATION_SCRIPTS[step.id]
    lines = _preparation_script_lines((root / "scripts" / name).read_text(encoding="utf-8"))
    calls = [line for line in lines if line.startswith(("cargo ", "RUSTDOCFLAGS="))]
    # Pin the surrounding reviewed shell layout; new wrappers, scopes and hidden calls fail closed.
    shape = "\n".join("@cargo" if line in calls else line for line in lines)
    if hashlib.sha256(shape.encode()).hexdigest() != expected_shape:
        raise ValueError(f"preparation parity: unsupported shell layout in {name}")
    if step.id == "workspace-crates":
        if lines.count('winit_target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"') != 1:
            raise ValueError("preparation parity: winit target directory fallback changed")
        doc_prefix = f'RUSTDOCFLAGS="{gate_catalog._WINIT_RUSTDOC_FLAGS[0][1]}" '
        records = []
        for index, line in enumerate(calls):
            env = ()
            if line.startswith(doc_prefix):
                env = gate_catalog._WINIT_RUSTDOC_FLAGS
                line = line[len(doc_prefix):]
            if index < 2:
                if not line.endswith(" || status=1"):
                    raise ValueError("preparation parity: unsupported Cargo failure handling")
                line = line[:-len(" || status=1")]
            if "$winit_target_dir" in line and line.count('"$winit_target_dir"') != 1:
                raise ValueError("preparation parity: winit target directory must remain quoted")
            line = line.replace('"$winit_target_dir"', "$winit_target_dir")
            if any(char in line for char in "\"';&|`<>()"):
                raise ValueError("preparation parity: unsupported Cargo invocation spelling")
            argv = tuple(line.split())
            if argv[:2] == ("cargo", "test"):
                argv = _no_run(argv)
            elif argv[:2] != ("cargo", "doc"):
                raise ValueError("preparation parity: unsupported Cargo subcommand")
            records.append(gate_model.Preparation(argv, env))
    else:
        records = []
        for line in calls:
            if not line.startswith("cargo run ") or not line.endswith(" 2>/dev/null"):
                raise ValueError("preparation parity: unsupported feasibility invocation")
            argv = tuple(line[:-len(" 2>/dev/null")].split())
            records.append(gate_model.Preparation(("cargo", "build", *argv[2:])))
    if tuple(records) != step.windows_preparations:
        raise ValueError(f"preparation parity: ordered Cargo calls differ from records in {name}")
    return tuple(records)


def windows_preparations(step: gate_model.Step, root: Path, env: Mapping[str, str]) -> tuple[gate_model.Preparation, ...]:
    """Verify reviewed sources and resolve compilation phases before any Windows launch."""
    # Resolve this runtime-only dependency after module initialization.
    import gate_catalog
    if not step.windows_preparations and step.id not in (*_PREPARATION_SCRIPTS, "pty-close-baseline", "windows-warp-allocator"):
        return ()
    if not any(step is canonical for canonical in gate_catalog.STEPS):
        raise ValueError("preparation parity: synthetic step cannot authorize compilation cleanup")
    if step.id in ("pty-close-baseline", "windows-warp-allocator"):
        if step.windows_preparations != (gate_model.Preparation(),):
            raise ValueError("preparation parity: derived test record changed")
        return (gate_model.Preparation(_no_run(step.argv)),)
    if step.id not in _PREPARATION_SCRIPTS:
        raise ValueError("preparation parity: unreviewed step")
    records = _script_preparations(step, root)
    target = env.get("CARGO_TARGET_DIR") or str(root / "target")
    return tuple(gate_model.Preparation(tuple(target if word == "$winit_target_dir" else word for word in item.argv), item.env)
                 for item in records)
