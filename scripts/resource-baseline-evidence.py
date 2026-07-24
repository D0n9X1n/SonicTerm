#!/usr/bin/env python3
"""Capture real cross-platform resource baseline evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time

SCHEMA_VERSION = "resource-baseline-evidence/1"
SUBJECT = "harness-process"
CLAIM_SCOPE = "baseline-capture-capability"
RUNNER_LABELS = ("macos-14", "windows-2022", "windows-latest")
_CURRENT_WINDOWS_BUILD = 26100


class EvidenceError(ValueError):
    """Raised when runner provenance cannot support the requested evidence lane."""


class CommandSpec:
    """A named command whose complete output belongs in the evidence bundle."""

    def __init__(self, name: str, argv: tuple[str, ...]) -> None:
        self.name = name
        self.argv = argv


def classify_platform(runner_label: str, facts: dict) -> dict:
    """Validate runtime facts against a configured evidence runner lane."""
    if runner_label not in RUNNER_LABELS:
        raise EvidenceError("unsupported runner label: {}".format(runner_label))

    profile = dict(facts)
    system = facts.get("system")
    if runner_label == "macos-14":
        if system != "Darwin":
            raise EvidenceError("macos-14 requires a Darwin runtime")
        profile.update({"family": "macos", "lane": "macos", "windows_build": None})
        return profile

    if system != "Windows":
        raise EvidenceError("{} requires a Windows runtime".format(runner_label))
    build = facts.get("windows_build")
    if not isinstance(build, int) or build <= 0:
        raise EvidenceError("{} requires a positive Windows build number".format(runner_label))
    if runner_label == "windows-2022":
        if build >= _CURRENT_WINDOWS_BUILD:
            raise EvidenceError(
                "windows-2022 requires Windows build older than {}".format(
                    _CURRENT_WINDOWS_BUILD
                )
            )
        lane = "old-windows"
    else:
        if build < _CURRENT_WINDOWS_BUILD:
            raise EvidenceError(
                "windows-latest requires Windows build {} or newer".format(
                    _CURRENT_WINDOWS_BUILD
                )
            )
        lane = "current-windows"
    profile.update({"family": "windows", "lane": lane, "windows_build": build})
    return profile


def command_specs(profile: dict, python_bin: str) -> list[CommandSpec]:
    """Return the exact real-process commands for a validated platform profile."""
    cargo_prefix = ("cargo", "test", "-p", "sonicterm-io", "--lib")
    child_exit = CommandSpec(
        "pty-child-exit",
        cargo_prefix
        + ("child_exit_probe_observes_short_lived_process", "--", "--nocapture"),
    )
    soak_live = CommandSpec(
        "soak-live",
        (
            python_bin,
            "scripts/soak-harness.py",
            "--scenario",
            "noop",
            "--live",
            "--duration",
            "64",
            "--warmup",
            "8",
            "--out",
            "-",
        ),
    )
    if profile.get("family") == "macos":
        return [
            child_exit,
            CommandSpec(
                "pty-descendant-cleanup",
                cargo_prefix
                + (
                    "observed_shell_exit_still_kills_background_process_group",
                    "--",
                    "--nocapture",
                ),
            ),
            soak_live,
        ]
    if profile.get("family") == "windows":
        return [
            child_exit,
            CommandSpec(
                "pty-thread-cleanup",
                cargo_prefix
                + (
                    "dropping_live_windows_pty_terminates_native_io_threads",
                    "--",
                    "--nocapture",
                ),
            ),
            CommandSpec(
                "conpty-close-drain",
                cargo_prefix
                + (
                    "conpty_close_runs_while_output_reader_is_draining",
                    "--",
                    "--nocapture",
                ),
            ),
            soak_live,
        ]
    raise EvidenceError("platform profile has no supported family")


def validate_live_result(profile: dict, result: dict) -> list[str]:
    """Return every reason a soak result cannot support the baseline claim."""
    errors = []
    if result.get("schema_version") != "soak-harness/1":
        errors.append("soak schema must be soak-harness/1")
    if result.get("status") != "ok":
        errors.append("soak result status must be ok")
    if result.get("scenario") != "noop":
        errors.append("soak result scenario must be noop")
    if result.get("data_source") != "live":
        errors.append("soak result data_source must be live")

    capabilities = result.get("capabilities")
    if not isinstance(capabilities, dict):
        errors.append("soak result capabilities must be an object")
        return errors

    required = ["rss_bytes", "thread_count", "handle_or_fd_count"]
    if profile.get("family") == "windows":
        required.append("private_bytes")
    for field in required:
        if capabilities.get(field) is not True:
            errors.append("required live capability is unavailable: {}".format(field))
    if profile.get("family") == "macos" and capabilities.get("private_bytes") is not False:
        errors.append("macOS private_bytes capability must remain unavailable")

    samples = result.get("samples")
    if not isinstance(samples, list) or len(samples) < 2:
        errors.append("soak result must contain at least two live samples")
        return errors
    for field in required:
        for index, sample in enumerate(samples):
            value = sample.get(field) if isinstance(sample, dict) else None
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                errors.append(
                    "live sample {} has invalid {} value".format(index, field)
                )
                break
    if result.get("sample_count") != len(samples):
        errors.append("soak sample_count must match samples")
    return errors


def _render_bytes(payload: dict) -> bytes:
    return (
        json.dumps(payload, ensure_ascii=True, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")


def _as_bytes(value) -> bytes:
    if value is None:
        return b""
    if isinstance(value, bytes):
        return value
    return str(value).encode("utf-8", errors="replace")


def _duration_ms(started: float, finished: float) -> int:
    return max(0, int(round((finished - started) * 1000)))


def _default_executor(spec: CommandSpec, cwd: Path):
    return subprocess.run(
        list(spec.argv),
        cwd=str(cwd),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def _write_checksums(output_dir: Path) -> None:
    lines = []
    for path in sorted(output_dir.iterdir(), key=lambda item: item.name):
        if not path.is_file() or path.name == "SHA256SUMS":
            continue
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        lines.append("{}  {}".format(digest, path.name))
    (output_dir / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")


def _failure_platform(facts: dict) -> dict:
    profile = dict(facts)
    profile.update({"family": None, "lane": None})
    return profile


def collect_evidence(
    *,
    repo_root: Path,
    output_dir: Path,
    runner_label: str,
    source_sha: str,
    environ: dict,
    platform_facts: dict,
    executor=None,
    clock=None,
) -> dict:
    """Capture a complete evidence bundle and return its summary document."""
    executor = executor or _default_executor
    clock = clock or time.time
    repo_root = Path(repo_root)
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    checksum_path = output_dir / "SHA256SUMS"
    if checksum_path.exists():
        checksum_path.unlink()

    started = clock()
    errors = []
    try:
        profile = classify_platform(runner_label, platform_facts)
    except EvidenceError as error:
        profile = _failure_platform(platform_facts)
        errors.append(str(error))

    python_bin = "python3" if profile.get("family") == "macos" else "python"
    specs = command_specs(profile, python_bin) if not errors else []
    commands = [{"name": spec.name, "argv": list(spec.argv)} for spec in specs]
    command_results = []
    soak_result = None

    for spec in specs:
        command_started = clock()
        try:
            completed = executor(spec, repo_root)
            returncode = int(completed.returncode)
            stdout = _as_bytes(completed.stdout)
            stderr = _as_bytes(completed.stderr)
        except Exception as error:  # Preserve failures as evidence instead of aborting.
            returncode = 126
            stdout = b""
            stderr = "{}: {}\n".format(type(error).__name__, error).encode("utf-8")
        command_finished = clock()

        stdout_name = "{}.stdout.log".format(spec.name)
        stderr_name = "{}.stderr.log".format(spec.name)
        (output_dir / stdout_name).write_bytes(stdout)
        (output_dir / stderr_name).write_bytes(stderr)
        command_results.append(
            {
                "name": spec.name,
                "argv": list(spec.argv),
                "started_at_unix": int(command_started),
                "finished_at_unix": int(command_finished),
                "duration_ms": _duration_ms(command_started, command_finished),
                "returncode": returncode,
                "stdout_log": stdout_name,
                "stderr_log": stderr_name,
            }
        )
        if returncode != 0:
            errors.append("{} exited with {}".format(spec.name, returncode))

        if spec.name == "soak-live":
            (output_dir / "soak-live.json").write_bytes(stdout)
            try:
                parsed = json.loads(stdout.decode("utf-8"))
                if not isinstance(parsed, dict):
                    raise ValueError("top-level value is not an object")
                soak_result = parsed
            except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
                errors.append("soak-live output is not valid JSON: {}".format(error))

    if soak_result is not None:
        errors.extend(validate_live_result(profile, soak_result))
    elif specs and not any(error.startswith("soak-live output") for error in errors):
        errors.append("soak-live result is missing")

    if not re.fullmatch(r"[0-9a-fA-F]{40}", source_sha or ""):
        errors.append("source_sha must be a 40-character hexadecimal commit")

    finished = clock()
    provenance = {
        "source_sha": source_sha,
        "runner_label": runner_label,
        "github_run_id": environ.get("GITHUB_RUN_ID"),
        "github_run_attempt": environ.get("GITHUB_RUN_ATTEMPT"),
        "github_job": environ.get("GITHUB_JOB"),
        "runner_arch": environ.get("RUNNER_ARCH") or platform_facts.get("machine"),
        "image_os": platform_facts.get("image_os"),
        "image_version": platform_facts.get("image_version"),
        "adapter": "not-applicable",
        "started_at_unix": int(started),
        "finished_at_unix": int(finished),
        "duration_ms": _duration_ms(started, finished),
    }
    github_context = environ.get("GITHUB_ACTIONS") == "true" or any(
        provenance[field]
        for field in ("github_run_id", "github_run_attempt", "github_job")
    )
    if github_context:
        for field in (
            "github_run_id",
            "github_run_attempt",
            "github_job",
            "runner_arch",
            "image_os",
            "image_version",
        ):
            if not provenance[field]:
                errors.append("GitHub Actions provenance is missing: {}".format(field))
    document = {
        "schema_version": SCHEMA_VERSION,
        "status": "fail" if errors else "pass",
        "errors": errors,
        "subject": SUBJECT,
        "claim_scope": CLAIM_SCOPE,
        "provenance": provenance,
        "platform": profile,
        "commands": commands,
        "command_results": command_results,
        "soak_result": soak_result,
    }
    (output_dir / "evidence.json").write_bytes(_render_bytes(document))
    _write_checksums(output_dir)
    return document


def _windows_build() -> int | None:
    getter = getattr(sys, "getwindowsversion", None)
    if getter is not None:
        try:
            return int(getter().build)
        except (AttributeError, TypeError, ValueError):
            pass
    numbers = [int(value) for value in re.findall(r"\d+", platform.version())]
    return numbers[-1] if numbers else None


def platform_facts(environ: dict) -> dict:
    """Collect OS facts without trusting the configured runner label."""
    system = platform.system()
    return {
        "system": system,
        "release": platform.release(),
        "version": platform.version(),
        "machine": platform.machine(),
        "macos_version": platform.mac_ver()[0] or None if system == "Darwin" else None,
        "windows_build": _windows_build() if system == "Windows" else None,
        "image_os": environ.get("ImageOS"),
        "image_version": environ.get("ImageVersion"),
    }


def _source_sha(repo_root: Path, environ: dict) -> str:
    github_sha = environ.get("GITHUB_SHA")
    if github_sha:
        return github_sha.strip()
    try:
        return (
            subprocess.check_output(
                ["git", "rev-parse", "HEAD"], cwd=str(repo_root), stderr=subprocess.DEVNULL
            )
            .decode("ascii")
            .strip()
        )
    except (OSError, subprocess.CalledProcessError, UnicodeDecodeError):
        return ""


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Capture resource baseline evidence.")
    parser.add_argument("--runner-label", choices=RUNNER_LABELS, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--source-sha", default=None)
    return parser


def main(
    argv=None,
    *,
    repo_root=None,
    source_sha=None,
    environ=None,
    platform_facts=None,
    executor=None,
    clock=None,
) -> int:
    """Run the collector CLI; failed evidence maps to a blocking exit code."""
    args = _parser().parse_args(argv)
    root = Path(repo_root) if repo_root is not None else Path(__file__).resolve().parent.parent
    environment = dict(os.environ if environ is None else environ)
    facts = platform_facts if platform_facts is not None else globals()["platform_facts"](environment)
    sha = source_sha or args.source_sha or _source_sha(root, environment)
    document = collect_evidence(
        repo_root=root,
        output_dir=args.output_dir,
        runner_label=args.runner_label,
        source_sha=sha,
        environ=environment,
        platform_facts=facts,
        executor=executor,
        clock=clock,
    )
    return 0 if document["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
