"""Gate docs."""

from __future__ import annotations
import difflib
from typing import Iterable, Mapping, Sequence, TextIO

import gate_catalog
import gate_harness
import gate_model


GATE_BEGIN = "<!-- local-gate:begin -->"
GATE_END = "<!-- local-gate:end -->"
INVOCATION = "python3 scripts/local-gate.py"
DOCUMENTS = (
    ("CLAUDE.md", "en"),
    ("wiki/Development-and-Release.md", "en"),
    ("wiki/Development-and-Release-zh-CN.md", "zh-CN"),
)

_RENDER_TEXT = {
    "en": {
        "header": ("Step", "Command", "Local hosts", "Class", "Needs", "CI jobs"),
        "joiner": ", ",
        "classes": "Classes: `local` steps run by default; `release` steps run with "
        "`--with-release`; `optional` steps run with `--with-optional` and never run in CI.",
        "hosts": "Local hosts are the hosts where the runner selects a step. CI jobs are where CI "
        "runs it, which can cover fewer hosts, or none.",
        "needs": "Needs:",
        "always": "Every step also needs Git and Python 3 on `PATH`.",
        "colon": ": ",
        "index": 0,
    },
    "zh-CN": {
        "header": ("步骤", "命令", "本地主机", "类别", "前置条件", "CI job"),
        "joiner": "、",
        "classes": "类别：`local` 步骤默认运行；`release` 步骤需加 `--with-release`；"
        "`optional` 步骤需加 `--with-optional`，且从不在 CI 中运行。",
        "hosts": "本地主机是 runner 会选择该步骤的主机。CI job 是 CI 运行它的位置，可能覆盖更少的主机，"
        "或一个也没有。",
        "needs": "前置条件：",
        "always": "每个步骤还需要 `PATH` 上的 Git 与 Python 3。",
        "colon": "：",
        "index": 1,
    },
}


def render_gate_block(language: str, steps: Sequence[gate_model.Step] = gate_catalog.STEPS) -> str:
    """Render the table block that CLAUDE.md and the wiki embed between the gate markers."""
    text = _RENDER_TEXT[language]
    joiner = text["joiner"]
    header = text["header"]
    lines = [GATE_BEGIN, "", "| " + " | ".join(header) + " |", "|" + " --- |" * len(header)]
    for step in steps:
        hosts = joiner.join(gate_model.HOST_LABELS[host] for host in step.hosts)
        needs = joiner.join(f"`{key}`" for key in step.prerequisites)
        jobs = joiner.join(f"`{job}`" for job in step.ci_jobs) or "—"
        lines.append(
            f"| `{step.id}` | `{gate_harness.command_text(step)}` | {hosts} | `{step.evidence}` | {needs} | {jobs} |"
        )
    lines.extend(["", text["classes"], "", text["hosts"], "", text["needs"], ""])
    used = {key for step in steps for key in step.prerequisites}
    for key in gate_model.PREREQUISITES:
        if key in used:
            lines.append(f"- `{key}`{text['colon']}{gate_model.PREREQUISITES[key][text['index']]}")
    lines.extend([f"- {text['always']}", "", GATE_END])
    return "\n".join(lines)


def doc_parity_problems(
    name: str, text: str, language: str, steps: Sequence[gate_model.Step] = gate_catalog.STEPS
) -> list[str]:
    """Compare one document's gate block with the table's rendered form."""
    if text.count(GATE_BEGIN) != 1 or text.count(GATE_END) != 1:
        return [f"{name}: expected exactly one {GATE_BEGIN} ... {GATE_END} block"]
    start = text.index(GATE_BEGIN)
    end = text.index(GATE_END) + len(GATE_END)
    if end < start:
        return [f"{name}: {GATE_END} precedes {GATE_BEGIN}"]
    problems: list[str] = []
    actual = text[start:end]
    expected = render_gate_block(language, steps)
    if actual != expected:
        diff = difflib.unified_diff(
            actual.splitlines(), expected.splitlines(), f"{name} (documented)",
            f"local-gate.py --render {language}", lineterm="",
        )
        problems.append(f"{name}: gate block differs from the table:\n" + "\n".join(diff))
    if INVOCATION not in text:
        problems.append(f"{name}: does not show the `{INVOCATION}` invocation")
    return problems
