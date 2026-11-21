"""Perf signals: a cancelled comparison still writes its partial report."""

from __future__ import annotations
import signal
import threading
from types import FrameType


# A cancelled GitHub job gets SIGINT, then SIGTERM, then SIGKILL; a desk sends SIGINT with Ctrl-C.
STOP_SIGNALS = tuple(getattr(signal, name) for name in ("SIGINT", "SIGTERM") if hasattr(signal, name))


class Terminated(KeyboardInterrupt):
    """The first stop signal of a comparison.

    It is a KeyboardInterrupt, so run_step kills and reaps the harness in flight and reports it INTERRUPTED,
    which ends the comparison as unresolved cleanup, exactly as Ctrl-C does.
    """


class StopSignals:
    """While measuring, the first SIGINT or SIGTERM raises Terminated; every signal is recorded.

    `hold()` makes later signals only recorded, so a second signal cannot cut the partial report short. Handlers
    are installed only on the main thread, the one thread Python delivers signals to; `__exit__` restores them.
    """

    def __init__(self, signals: tuple[int, ...] = STOP_SIGNALS) -> None:
        self.signals = signals
        self.received: list[int] = []
        self.holding = False
        self._previous: dict[int, object] = {}

    def handle(self, signal_number: int, _frame: FrameType | None) -> None:
        """Record the signal; raise Terminated for the first one unless the report is being written."""
        self.received.append(signal_number)
        # When: an earlier signal already ended the measuring, or the report is being written, this one is noted only.
        if len(self.received) == 1 and not self.holding:
            raise Terminated(self.describe())

    def hold(self) -> None:
        """Record every later signal without raising, while the partial report is written."""
        self.holding = True

    def check(self) -> None:
        """Raise Terminated when a signal arrived that something swallowed, so no further run starts."""
        if self.received:
            raise Terminated(self.describe())

    def describe(self) -> str:
        """The received signals by name, such as `SIGINT, then SIGTERM`."""
        return ", then ".join(signal.Signals(number).name for number in self.received)

    def __enter__(self) -> StopSignals:
        # When: this is not the main thread (a test runner's worker), signal.signal would raise, so nothing is installed.
        if threading.current_thread() is threading.main_thread():
            for signal_number in self.signals:
                self._previous[signal_number] = signal.signal(signal_number, self.handle)
        return self

    def __exit__(self, *_exception: object) -> None:
        for signal_number, previous in self._previous.items():
            # A handler installed outside Python reads None and cannot be reinstalled; the default stands in for it.
            signal.signal(signal_number, previous if previous is not None else signal.SIG_DFL)
        self._previous.clear()
