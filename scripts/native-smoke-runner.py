#!/usr/bin/env python3
"""Run one native smoke under a hard deadline and optional verdict contract."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
from typing import Mapping, Sequence

TIMEOUT_EXIT_CODE = 124
VERDICT_EXIT_CODE = 90
LAUNCH_EXIT_CODE = 91


def smoke_environment(state_dir: Path, base: Mapping[str, str]) -> dict[str, str]:
    """Add the explicit scratch root without replacing user-home variables."""
    environment = dict(base)
    environment.pop("NO_COLOR", None)
    environment["SONICTERM_RUNTIME_SMOKE_DIR"] = str(state_dir)
    return environment


def has_required_capability(output: bytes, required: str) -> bool:
    """Accept exactly one capability verdict and require its value to match."""
    verdicts = []
    for line in output.decode("utf-8", errors="replace").splitlines():
        for token in line.split():
            if token.startswith("capability="):
                verdicts.append(token.partition("=")[2])
    return verdicts == [required]


def _terminate_process_tree(process: subprocess.Popen[bytes]) -> None:
    if os.name == "nt":
        try:
            subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
                timeout=10,
            )
        except (OSError, subprocess.TimeoutExpired):
            pass
    else:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    try:
        process.kill()
    except OSError:
        pass


def _close_pipe(pipe) -> None:
    if pipe is not None:
        try:
            pipe.close()
        except OSError:
            pass


def run_command(
    command: Sequence[str],
    cwd: Path,
    timeout_seconds: int,
    environment: Mapping[str, str],
) -> subprocess.CompletedProcess[bytes]:
    """Capture output while bounding and reaping the complete child tree."""
    try:
        process = subprocess.Popen(
            list(command),
            cwd=str(cwd),
            env=dict(environment),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=os.name != "nt",
            creationflags=(
                subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
            ),
        )
    except OSError as error:
        return subprocess.CompletedProcess(
            command,
            LAUNCH_EXIT_CODE,
            b"",
            ("native smoke launch failed: {}\n".format(error)).encode(
                "utf-8", errors="replace"
            ),
        )
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    except subprocess.TimeoutExpired as timeout:
        _terminate_process_tree(process)
        try:
            stdout, stderr = process.communicate(timeout=10)
        except subprocess.TimeoutExpired as cleanup_timeout:
            _terminate_process_tree(process)
            stdout = cleanup_timeout.output or timeout.output or b""
            stderr = cleanup_timeout.stderr or timeout.stderr or b""
            _close_pipe(process.stdout)
            _close_pipe(process.stderr)
        message = "native smoke timed out after {} seconds\n".format(
            timeout_seconds
        ).encode("utf-8")
        return subprocess.CompletedProcess(
            command,
            TIMEOUT_EXIT_CODE,
            stdout or b"",
            (stderr or b"") + message,
        )


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--timeout-seconds", type=int, required=True)
    parser.add_argument("--state-dir", type=Path)
    parser.add_argument("--log-file", type=Path)
    parser.add_argument("--require-capability")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be positive")
    if args.command and args.command[0] == "--":
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    with tempfile.TemporaryDirectory(prefix="sonicterm-native-smoke-") as temporary:
        state_dir = args.state_dir or Path(temporary)
        state_dir.mkdir(parents=True, exist_ok=True)
        environment = smoke_environment(state_dir, os.environ)
        completed = run_command(
            args.command, Path.cwd(), args.timeout_seconds, environment
        )
        combined = completed.stdout + completed.stderr
        sys.stdout.buffer.write(completed.stdout)
        sys.stderr.buffer.write(completed.stderr)
        wrapper_diagnostic = b""
        return_code = completed.returncode
        if completed.returncode == 0 and args.require_capability and not has_required_capability(
            combined, args.require_capability
        ):
            wrapper_diagnostic = (
                "required capability verdict missing or not uniquely {}\n".format(
                    args.require_capability
                )
            ).encode("utf-8")
            sys.stderr.buffer.write(wrapper_diagnostic)
            return_code = VERDICT_EXIT_CODE
        if args.log_file is not None:
            args.log_file.parent.mkdir(parents=True, exist_ok=True)
            args.log_file.write_bytes(combined + wrapper_diagnostic)
        return return_code


if __name__ == "__main__":
    raise SystemExit(main())
