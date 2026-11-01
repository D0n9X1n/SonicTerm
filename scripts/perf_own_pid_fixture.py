"""Pins the perf scripts' own pid for tests.

The kill and validation guards refuse `perf_sessions.own_pid()`. Tests feed those guards made-up
process ids (900, 950, 510 and others), and the OS may give the test process one of them, which
would make a fixture process look like the script itself. Importing this module pins own_pid to an
id that no fixture uses, so a guard refuses a fixture pid only when a test asks for it.
"""

from __future__ import annotations

import perf_sessions

# Positive, so the `pid <= 0` guards don't hide it, and unlike every pid a fixture uses.
PINNED_OWN_PID = 2_000_000_011


def _pinned_own_pid() -> int:
    """The pinned stand-in for this process's pid."""
    return PINNED_OWN_PID


perf_sessions.own_pid = _pinned_own_pid
