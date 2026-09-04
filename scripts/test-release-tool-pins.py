#!/usr/bin/env python3
"""Keep release-tool pins, MSI validation, and Packaging instructions aligned."""

from __future__ import annotations

from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent
CI = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
RELEASE = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
PACKAGING = (ROOT / "wiki/Packaging.md").read_text(encoding="utf-8")

PINS = {
    "CARGO_WIX_VERSION": "0.3.9",
    "WIX_TOOLSET_VERSION": "3.14.1.20250415",
    "CARGO_LLVM_COV_VERSION": "0.9.0",
}


def require(text: str, needle: str, subject: str) -> None:
    if needle not in text:
        raise AssertionError(f"{subject} is missing {needle!r}")


def step_block(workflow: str, name: str) -> str:
    matches = list(
        re.finditer(
            rf"(?ms)^\s*- name: {re.escape(name)}\s*$.*?(?=^\s*- (?:name:|uses:)|^\s{{2}}[A-Za-z0-9_-]+:|\Z)",
            workflow,
        )
    )
    if len(matches) != 1:
        raise AssertionError(f"workflow step {name!r} occurs {len(matches)} times")
    return matches[0].group(0)


def forbid(text: str, needle: str, subject: str) -> None:
    if needle in text:
        raise AssertionError(f"{subject} still contains floating command {needle!r}")


def require_in_step(workflow: str, name: str, needle: str) -> None:
    require(step_block(workflow, name), needle, f"workflow step {name}")


def require_top_level_env(workflow: str, key: str, value: str) -> None:
    preamble = workflow.split("\njobs:\n", 1)[0]
    env = re.search(r"(?ms)^env:\n(?P<body>(?:(?:^  [^\n]*\n)|(?:^\s*$))*)", preamble)
    if env is None:
        raise AssertionError("workflow has no top-level env block")
    matches = re.findall(rf'(?m)^  {re.escape(key)}: "([^"]+)"$', env.group("body"))
    occurrence_matches = re.findall(
        rf'(?m)^\s+{re.escape(key)}:\s*(?:"([^"]+)"|([^"\n]+?))\s*$',
        workflow,
    )
    all_occurrences = [quoted or unquoted for quoted, unquoted in occurrence_matches]
    if matches != [value] or all_occurrences != [value]:
        raise AssertionError(
            f"top-level env {key!r} has {matches!r} with all occurrences "
            f"{all_occurrences!r}, expected exactly {[value]!r}"
        )


def require_timeout(workflow: str, name: str, expected: int) -> None:
    require_in_step(workflow, name, f"timeout-minutes: {expected}")


def main() -> None:
    require_top_level_env(RELEASE, "CARGO_WIX_VERSION", PINS["CARGO_WIX_VERSION"])
    require_top_level_env(RELEASE, "WIX_TOOLSET_VERSION", PINS["WIX_TOOLSET_VERSION"])
    require_top_level_env(CI, "CARGO_LLVM_COV_VERSION", PINS["CARGO_LLVM_COV_VERSION"])

    shadowed_release = RELEASE.replace(
        "  build-windows:\n",
        "  build-windows:\n    env:\n      CARGO_WIX_VERSION: 0.3.8\n",
        1,
    )
    if shadowed_release == RELEASE:
        raise AssertionError("release workflow has no build-windows job for shadow mutation")
    try:
        require_top_level_env(
            shadowed_release,
            "CARGO_WIX_VERSION",
            PINS["CARGO_WIX_VERSION"],
        )
    except AssertionError:
        pass
    else:
        raise AssertionError("unquoted job-local cargo-wix shadow was accepted")

    forbid(RELEASE, "cargo install cargo-wix --locked", "release workflow")
    forbid(RELEASE, "choco install wixtoolset --no-progress -y", "release workflow")
    forbid(CI, "cargo install cargo-llvm-cov --locked", "CI workflow")
    require_in_step(
        RELEASE,
        "Install cargo-wix",
        'cargo install cargo-wix --version "${{ env.CARGO_WIX_VERSION }}" --locked',
    )
    require_in_step(
        RELEASE,
        "Install WiX Toolset",
        'choco install wixtoolset --version "${{ env.WIX_TOOLSET_VERSION }}" --no-progress -y',
    )
    require_in_step(
        CI,
        "Install cargo-llvm-cov",
        'cargo install cargo-llvm-cov --version "${{ env.CARGO_LLVM_COV_VERSION }}" --locked',
    )
    require_in_step(RELEASE, "Build and register msi asset", "--target x86_64-pc-windows-msvc")
    require_in_step(RELEASE, "Build and register msi asset", "--install-version $numericVersion")
    require_in_step(RELEASE, "Validate MSI metadata", "scripts\\validate-windows-msi.ps1")
    require_timeout(RELEASE, "Install cargo-wix", 10)
    require_timeout(RELEASE, "Install WiX Toolset", 10)
    require_timeout(RELEASE, "Validate MSI metadata", 5)
    require_in_step(CI, "Test MSI validator", "scripts\\validate-windows-msi_tests.ps1")
    require_timeout(CI, "Test MSI validator", 5)
    require_timeout(CI, "Install cargo-llvm-cov", 10)

    english, chinese = PACKAGING.split("## 中文", 1)
    for language, half in (("English", english), ("中文", chinese)):
        for version in PINS.values():
            require(half, version, f"{language} Packaging")
        require(half, "--target x86_64-pc-windows-msvc", f"{language} Packaging")
        require(half, "validate-windows-msi.ps1", f"{language} Packaging")
        require(half, "tooling", f"{language} tooling-update procedure")

    print("release tool pin tests: ok")


if __name__ == "__main__":
    main()
