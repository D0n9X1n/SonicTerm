#!/usr/bin/env python3
"""Record the bytes tmux and rmux write to their outer terminal for a long, wrapping URL.

Each case starts the multiplexer in a pseudo-terminal of fixed size, with a private `-L`
socket and `-f /dev/null`, lets a program inside print a long URL, and saves exactly what
the multiplexer drew. The path-detection unit tests in sonicterm-app replay these
recordings, so the layout constants here are mirrored in `path_target_tests.rs`.

Cases, for each multiplexer:
  plain-full           a shell prints the bare URL; the full-width pane wraps it;
  plain-split          the same in the right pane of a vertical split;
  claude-edge-full     Claude Code layout: "⏺ " then the URL broken by the program exactly
                       at the pane's right edge, continuation rows indented two spaces;
  claude-edge-split    the same in the right pane of a vertical split;
  claude-margin1-full  the same, broken 1 column before the edge;
  claude-margin2-full  the same, broken 2 columns before the edge;
  claude-margin3-full  the same, broken 3 columns before the edge;
  paren-full           control: "See (" + URL + ") for details" broken at the pane edge.

The Claude Code layout reproduces what the real CLI (2.1.306) was measured to draw for a
long URL reply at 64, 80, 103 and 120 columns: every full row ends at the pane's right edge
and continuation rows start at the URL's column. The real recordings are not committed
because they carry the account's status line, model and paths.

Every recording is checked to contain each visible fragment of the URL; a recording that
does not is reported and the script exits 1. Every subprocess is bounded, and the private
server is killed on every exit path.
"""
from __future__ import annotations

import argparse
import fcntl
import os
from pathlib import Path
import select
import shlex
import shutil
import struct
import subprocess
import sys
import termios
import time

URL = (
    "https://github.com/D0n9X1n/SonicTerm/issues/1669#hard-wrapped-bare-urls-in-tmux-and-rmux-"
    "are-joined-across-rows-when-a-program-breaks-them?from=claude-code&case=multiplexer-panes"
)
SCREEN_ROWS = 12
FULL_COLUMNS = 60
SPLIT_COLUMNS = 81
RIGHT_PANE_COLUMNS = 40
CLAUDE_BULLET = "⏺ "
CLAUDE_INDENT = "  "
PAREN_PREFIX = "See ("
PAREN_SUFFIX = ") for details"
MULTIPLEXERS = ("tmux", "rmux")
# Case name -> (layout, drawn in the right pane of a split, columns left blank before the edge).
CASES = {
    "plain-full": ("plain", False, 0),
    "plain-split": ("plain", True, 0),
    "claude-edge-full": ("claude", False, 0),
    "claude-edge-split": ("claude", True, 0),
    "claude-margin1-full": ("claude", False, 1),
    "claude-margin2-full": ("claude", False, 2),
    "claude-margin3-full": ("claude", False, 3),
    "paren-full": ("paren", False, 0),
}
# How long a case may take before it is reported incomplete, and the quiet period after the
# last expected fragment appears.
CAPTURE_DEADLINE_S = 8.0
REFRESH_AFTER_S = 4.0
QUIET_AFTER_S = 0.5
# Upper bound for every multiplexer command and for reaping the recorded client.
COMMAND_TIMEOUT_S = 10.0
DEFAULT_OUTPUT = Path(__file__).resolve().parent.parent / "crates/sonicterm-app/test-fixtures/url-wrap"


def chunks(text: str, width: int) -> list[str]:
    """`text` cut into pieces of at most `width` characters."""
    return [text[start : start + width] for start in range(0, len(text), width)]


def pane_columns(case: str) -> int:
    """Width of the pane the program prints into."""
    _, split, _ = CASES[case]
    return RIGHT_PANE_COLUMNS if split else FULL_COLUMNS


def program_lines(case: str) -> list[str]:
    """The lines the program inside the pane prints, already hard-wrapped where the case says."""
    layout, _, margin = CASES[case]
    columns = pane_columns(case)
    if layout == "plain":
        return [URL]
    if layout == "claude":
        pieces = chunks(URL, columns - margin - len(CLAUDE_INDENT))
        return [CLAUDE_BULLET + pieces[0]] + [CLAUDE_INDENT + piece for piece in pieces[1:]]
    return chunks(PAREN_PREFIX + URL + PAREN_SUFFIX, columns)


def visible_fragments(case: str) -> list[str]:
    """The URL text each screen row must show, which the recording must contain."""
    layout, _, _ = CASES[case]
    if layout == "plain":
        return chunks(URL, pane_columns(case))
    if layout == "claude":
        return [line[len(CLAUDE_INDENT) :] for line in program_lines(case)]
    return program_lines(case)


def multiplexer_commands(binary: str, socket_name: str, case: str) -> tuple[list[list[str]], list[str], int]:
    """Setup commands to run first, the client command line to record, and the outer width.

    tmux runs a `;`-chained command line, so one client both builds and shows the session.
    rmux runs only the first command of such a chain, so its session is built detached
    (created at the recording size, status line off, split) and the recorded client attaches.
    """
    _, split, _ = CASES[case]
    inner = "printf '%s\\n' " + " ".join(shlex.quote(line) for line in program_lines(case)) + "; sleep 60"
    base = [binary, "-L", socket_name, "-f", "/dev/null"]
    columns = SPLIT_COLUMNS if split else FULL_COLUMNS
    if binary == "tmux":
        if split:
            client = base + [
                "new-session", "/bin/sh", "-c", "sleep 60", ";",
                "set", "-g", "status", "off", ";",
                "split-window", "-h", "-l", str(RIGHT_PANE_COLUMNS), "/bin/sh", "-c", inner,
            ]
        else:
            client = base + ["new-session", "/bin/sh", "-c", inner, ";", "set", "-g", "status", "off"]
        return [], client, columns
    first_program = ["/bin/sh", "-c", "sleep 60"] if split else ["/bin/sh", "-c", inner]
    setup = [
        base + ["new-session", "-d", "-x", str(columns), "-y", str(SCREEN_ROWS)] + first_program,
        base + ["set", "-g", "status", "off"],
    ]
    if split:
        setup.append(base + ["split-window", "-h", "-l", str(RIGHT_PANE_COLUMNS), "/bin/sh", "-c", inner])
    return setup, base + ["attach-session"], columns


def clean_environment() -> dict[str, str]:
    """This process's environment without multiplexer variables, with a fixed terminal type and locale."""
    environment = {name: value for name, value in os.environ.items() if not name.startswith(("TMUX", "RMUX"))}
    environment.update(TERM="xterm-256color", LANG="en_US.UTF-8", LC_ALL="en_US.UTF-8")
    return environment


def run_bounded(command: list[str]) -> None:
    """Run one multiplexer command, killed if it outlives [`COMMAND_TIMEOUT_S`]."""
    try:
        subprocess.run(
            command, capture_output=True, check=False, env=clean_environment(), timeout=COMMAND_TIMEOUT_S
        )
    except subprocess.TimeoutExpired:
        print(f"timed out: {' '.join(command[:5])}", file=sys.stderr)


def reap(process: subprocess.Popen) -> None:
    """Stop the recorded client and wait for it, bounded."""
    process.kill()
    try:
        process.wait(timeout=COMMAND_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        print(f"client {process.pid} did not exit", file=sys.stderr)


def record(binary: str, case: str) -> tuple[bytes, list[str]]:
    """Run one case and return the recorded bytes and any fragments missing from them."""
    socket_name = f"sonic-url-wrap-{os.getpid()}-{case}"
    base = [binary, "-L", socket_name]
    setup, argv, columns = multiplexer_commands(binary, socket_name, case)
    expected = [fragment.encode() for fragment in visible_fragments(case)]
    recorded = bytearray()
    controller = None
    process = None
    try:
        for command in setup:
            run_bounded(command)
        if setup:
            # The program in the new pane prints before the client attaches; give it a moment.
            time.sleep(0.5)
        controller, terminal = os.openpty()
        fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", SCREEN_ROWS, columns, 0, 0))
        process = subprocess.Popen(
            argv, stdin=terminal, stdout=terminal, stderr=terminal, env=clean_environment(), start_new_session=True
        )
        os.close(terminal)
        started = time.monotonic()
        complete_at = None
        refreshed = False
        while time.monotonic() - started < CAPTURE_DEADLINE_S:
            readable, _, _ = select.select([controller], [], [], 0.05)
            if readable:
                try:
                    recorded += os.read(controller, 65536)
                except OSError:
                    break
            if complete_at is None and all(fragment in recorded for fragment in expected):
                complete_at = time.monotonic()
            if complete_at is not None and time.monotonic() - complete_at >= QUIET_AFTER_S:
                break
            if not refreshed and complete_at is None and time.monotonic() - started >= REFRESH_AFTER_S:
                # A slow first draw is retried once by asking the server to redraw the client.
                run_bounded(base + ["refresh-client"])
                refreshed = True
    finally:
        run_bounded(base + ["kill-server"])
        if process is not None:
            reap(process)
        if controller is not None:
            os.close(controller)
    missing = [fragment.decode() for fragment in expected if fragment not in recorded]
    return bytes(recorded), missing


def main() -> int:
    """Record the requested cases and report any recording that lacks the URL."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--multiplexer", choices=MULTIPLEXERS + ("both",), default="both")
    parser.add_argument("--case", choices=tuple(CASES) + ("all",), default="all")
    arguments = parser.parse_args()
    binaries = MULTIPLEXERS if arguments.multiplexer == "both" else (arguments.multiplexer,)
    cases = tuple(CASES) if arguments.case == "all" else (arguments.case,)
    arguments.output_dir.mkdir(parents=True, exist_ok=True)
    failures = 0
    for binary in binaries:
        if shutil.which(binary) is None:
            print(f"{binary}: not installed", file=sys.stderr)
            failures += 1
            continue
        for case in cases:
            recorded, missing = record(binary, case)
            destination = arguments.output_dir / f"{binary}-{case}.bin"
            destination.write_bytes(recorded)
            if missing:
                failures += 1
                print(f"{destination.name}: {len(recorded)} bytes, MISSING {missing}", file=sys.stderr)
            else:
                print(f"{destination.name}: {len(recorded)} bytes, URL present")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
