"""Gate harness."""

from __future__ import annotations
from dataclasses import dataclass, replace
import re
import sys
from typing import Iterable, Mapping, Sequence, TextIO

import gate_model


@dataclass(frozen=True)
class HarnessApiCfg:
    """One harness-only cfg: on for both refs of a comparison only when both trees define every method."""

    name: str
    # Each method as (owning crate source directory, owner type, method name): it must be a `pub fn` in an
    # inherent `impl <owner>` block of that crate, outside test-only code.
    methods: tuple[tuple[str, str, str], ...]


# The canonical harness API cfg table. perf-compare decides each entry from both trees' code and builds both
# refs with the same decision; every comparison build declares every entry through the compiler flags, so a
# tree whose manifest predates an entry builds clean. None is a Cargo feature.
HARNESS_API_CFGS = (
    # S1/atlas-retry's driver: the renderer's atlas change and the scene it reads, absent before v1.3.9.
    HarnessApiCfg("perf_atlas_retry_api", (
        ("crates/sonicterm-gpu/src", "GpuRenderer", "__change_glyph_atlas_during_next_assembly"),
        ("crates/sonicterm-app/src", "App", "__test_window_active_tab_title"),
        ("crates/sonicterm-gpu/src", "GpuRenderer", "last_missing_chrome"),
        ("crates/sonicterm-gpu/src", "GpuRenderer", "font_fallback_notice_id"),
    )),
    # S10's per-present attribution watch: the App's arm and disarm methods, absent before the prerequisite.
    HarnessApiCfg("perf_s10_attribution_api", (
        ("crates/sonicterm-app/src", "App", "arm_s10_attribution"),
        ("crates/sonicterm-app/src", "App", "disarm_s10_attribution"),
    )),
    # S9's and S12's perf-end glyph completeness reading: the renderer's checkpoint, absent before v1.3.9.
    HarnessApiCfg("perf_completeness_api", (
        ("crates/sonicterm-gpu/src", "GpuRenderer", "completeness_checkpoint"),
    )),
    # The per-sample dispatch timeline: the App's arm and take methods, absent before the prerequisite.
    HarnessApiCfg("perf_dispatch_timeline_api", (
        ("crates/sonicterm-app/src", "App", "arm_dispatch_timeline_v1"),
        ("crates/sonicterm-app/src", "App", "take_dispatch_timeline_v1"),
    )),
    # S2's echo timeline: the App's one-shot V1 transfer of a taken watch's timeline, absent before v1.3.9.
    # The harness names it only with perf-echo-trace as well, so the combined steps compile its cfg-off side.
    HarnessApiCfg("perf_echo_timeline_api", (
        ("crates/sonicterm-app/src", "App", "take_echo_timeline_v1"),
    )),
    # The guard-correlation transport: the App's raw record take, absent before v1.3.9.
    HarnessApiCfg("perf_guard_spans_api", (
        ("crates/sonicterm-app/src", "App", "take_guard_correlation_v1"),
    )),
    # S12/status-line's taskbar checks: the App's counting progress surface and its delivery record,
    # absent before v1.3.11.
    HarnessApiCfg("perf_taskbar_progress_api", (
        ("crates/sonicterm-app/src", "App", "__install_counting_taskbar_progress"),
        ("crates/sonicterm-app/src", "App", "__take_taskbar_progress_deliveries"),
    )),
)
HARNESS_API_CFG_NAMES = tuple(entry.name for entry in HARNESS_API_CFGS)
# The combined harness-API steps enable every entry at once, so one rebuild covers them all and a new entry
# cannot skip its cfg-on lint and tests.
_HARNESS_API_CFG_FLAGS = tuple(token for name in HARNESS_API_CFG_NAMES for token in ("--cfg", name))


class HarnessFlagConflict(ValueError):
    """An inherited compiler flag sets a harness API cfg against the comparison's decision."""


def _inherited_rustflags(environ: Mapping[str, str]) -> list[str]:
    """The compiler flags a Cargo build would take from `environ`: CARGO_ENCODED_RUSTFLAGS split on 0x1f when
    set (an empty value means none), else RUSTFLAGS split on spaces with empty tokens dropped, as Cargo does.
    The two sources are never combined: Cargo reads only the first."""
    encoded = environ.get("CARGO_ENCODED_RUSTFLAGS")
    if encoded is not None:
        return encoded.split("\x1f") if encoded else []
    return [token for token in (part.strip() for part in environ.get("RUSTFLAGS", "").split(" ")) if token]


def _set_cfgs(tokens: Sequence[str]) -> list[str]:
    """Each cfg spec the tokens set, as written: `--cfg X`, `--cfg=X`, and, conservatively, a single token that
    joins `--cfg` and its spec with whitespace."""
    found = []
    for index, token in enumerate(tokens):
        if token == "--cfg" and index + 1 < len(tokens):
            found.append(tokens[index + 1])
        elif token.startswith("--cfg="):
            found.append(token[len("--cfg="):])
        elif token.startswith("--cfg") and token[len("--cfg"):][:1].isspace():
            found.append(token[len("--cfg"):])
    return found


# A cfg spec as rustc reads it: surrounding whitespace ignored, an optional raw-identifier prefix, the name, and
# an optional `= value`.
_CFG_SPEC = re.compile(r"\s*(?:r#)?([A-Za-z_][A-Za-z0-9_]*)\s*(=.*)?\s*", re.S)


def _owned_cfg(spec: str) -> tuple[str, bool] | None:
    """`(owned name, has a value)` when `spec` sets a table entry; None when it sets another cfg. A spec that does
    not parse but names an entry is read as setting it with a value, so it is always refused."""
    match = _CFG_SPEC.fullmatch(spec)
    if match is not None:
        return (match[1], match[2] is not None) if match[1] in HARNESS_API_CFG_NAMES else None
    named = [name for name in HARNESS_API_CFG_NAMES if re.search(rf"(?<![A-Za-z0-9_]){name}(?![A-Za-z0-9_])", spec)]
    return (named[0], True) if named else None


def compose_harness_rustflags(environ: Mapping[str, str], enabled: Sequence[str]) -> list[str]:
    """The compiler flags a harness build runs with: the inherited flags, then `--check-cfg` for every table
    entry and `--cfg` for each one in `enabled`. Appending cannot unset a cfg, so an inherited flag that sets a
    table entry `enabled` leaves off, or sets one to a value, is refused."""
    unknown = [name for name in enabled if name not in HARNESS_API_CFG_NAMES]
    if unknown:
        raise HarnessFlagConflict(f"unknown harness API cfg {unknown}")
    inherited = _inherited_rustflags(environ)
    for written in _set_cfgs(inherited):
        owned = _owned_cfg(written)
        if owned is not None and (owned[1] or owned[0] not in enabled):
            name = owned[0]
            raise HarnessFlagConflict(f"the inherited compiler flags set `{written}`, but the derived decision "
                                      f"is {name}={'on' if name in enabled else 'off'}")
    declared = [token for name in HARNESS_API_CFG_NAMES for token in ("--check-cfg", f"cfg({name})")]
    return inherited + declared + [token for name in HARNESS_API_CFG_NAMES if name in enabled
                                   for token in ("--cfg", name)]


def harness_dependency_environment(environ: Mapping[str, str], enabled: Sequence[str]) -> dict[str, str]:
    """Validate inherited cfgs, then move agreeing owned cfgs out of dependency-wide flags."""
    compose_harness_rustflags(environ, enabled)
    inherited = _inherited_rustflags(environ)
    kept = []
    index = 0
    while index < len(inherited):
        token = inherited[index]
        width = 2 if token == "--cfg" and index + 1 < len(inherited) else 1
        specs = _set_cfgs(inherited[index:index + width])
        if not any(_owned_cfg(spec) is not None for spec in specs):
            kept.extend(inherited[index:index + width])
        index += width
    environment = dict(environ)
    if kept != inherited:
        environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(kept)
    return environment


_PLAIN_WORD = re.compile(r"^[A-Za-z0-9_@%+=:,./\\-]+$")


def _display_word(word: str) -> str:
    """Quote a word only when a shell would otherwise split or expand it."""
    if _PLAIN_WORD.match(word):
        return word
    return '"' + word.replace('"', '\\"') + '"'


def command_text(step: gate_model.Step) -> str:
    """Render the canonical command, including the target-only launcher for API cfgs."""
    words = [f"{name}={_display_word(value)}" for name, value in step.env]
    words.extend(_display_word(word) for word in target_launch_argv(step, display=True))
    return " ".join(words)


def launch_argv(step: gate_model.Step) -> tuple[str, ...]:
    """Return the argv the runner starts, wrapping a PowerShell script in `pwsh -File`."""
    if step.shell == "pwsh":
        return ("pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-File", *step.argv)
    return target_launch_argv(step)


def target_launch_argv(step: gate_model.Step, *, display: bool = False) -> tuple[str, ...]:
    """Route cfg-bearing Cargo commands through the target-only compiler/test launcher."""
    if step.harness_cfgs is None:
        return step.argv
    script = "scripts/harness-target.py" if display else str(gate_model._HERE / "harness-target.py")
    python = "python3" if display else sys.executable
    return (python, script, "--cfgs", ",".join(step.harness_cfgs) or "none", "--", *step.argv)


# The target launcher registers only plans derived from this process's canonical catalog.
_TARGET_COMPILE_STEPS: list[gate_model.Step] = []
# Both roots include the API-gated modules; alloc_main has test=false in Cargo.toml.
HARNESS_LINT_TARGETS = (("perf_scenarios", False), ("perf_scenarios", True), ("perf_scenarios_alloc", False))


def target_compile_steps(original: gate_model.Step) -> tuple[gate_model.Step, ...]:
    """Plan cfg-scoped compilation; Cargo JSON identifies the test executable, never a glob."""
    # Resolve this runtime-only dependency after module initialization.
    import gate_preparations
    if not gate_preparations._reviewed_step(original) or original.harness_cfgs is None:
        raise ValueError("target compilation requires a canonical cfg-bearing step")
    words = list(original.argv)
    operation = words[1]
    extra = []
    if "--" in words:
        boundary = words.index("--")
        extra, words = words[boundary + 1:], words[:boundary]
    examples = [words[index + 1] for index, word in enumerate(words) if word == "--example"]
    if not examples or set(examples) - {"perf_scenarios", "perf_scenarios_alloc"}:
        raise ValueError("only the scenario harness targets accept API cfgs")
    cfg_flags = compose_harness_rustflags({}, original.harness_cfgs)
    plans = []
    if operation == "clippy":
        # Keep the original all-targets lint, then lint both cfg-on harness modes alone.
        plans.append(tuple([*words, "--", *extra]))
        selected = [word for word in words if word != "--all-targets"]
        selected[1] = "rustc"
        # cargo clippy forwards its arguments to the package library too. rustc's final-target
        # arguments plus the Clippy driver lint each API consumer without enabling dependency cfgs.
        for example, tests in HARNESS_LINT_TARGETS:
            target = list(selected)
            target[target.index("--example") + 1] = example
            plans.append(tuple([*target, "--profile", "check", "--", *extra, *cfg_flags,
                                *(["--test"] if tests else []), "--emit=metadata"]))
    else:
        words[1] = "rustc"
        selected = []
        skip = False
        for word in words:
            if skip:
                skip = False
            elif word == "--example":
                skip = True
            else:
                selected.append(word)
        if not any(word.startswith("--message-format") for word in selected):
            selected.append("--message-format=json-render-diagnostics")
        if operation in ("test", "check"):
            selected += ["--profile", operation]
        for example in examples:
            plans.append(tuple([*selected, "--example", example, "--", *cfg_flags,
                                *(["--emit=metadata"] if operation == "check" else [])]))
    steps = tuple(gate_model.Step(f"{original.id}-target-{number}", argv, original.hosts, original.timeout_s,
                       original.evidence, original.prerequisites, (),
                       env=(("RUSTC_WORKSPACE_WRAPPER", "clippy-driver"),) if operation == "clippy" and number else (),
                       windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY) for number, argv in enumerate(plans))
    _TARGET_COMPILE_STEPS.extend(steps)
    return steps
