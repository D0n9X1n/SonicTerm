"""Perf signals tests."""

from __future__ import annotations
import signal
import threading
import unittest

import perf_signals


class StopSignalTests(unittest.TestCase):
    """A cancelled comparison: the first signal ends the measuring, later ones cannot cut the report short."""

    def test_the_first_signal_raises_and_later_ones_are_only_recorded(self):
        # The first SIGTERM ends the run in flight as an interrupt; a second one, sent while the report is being
        # written, is recorded and never raised, and the description names both in order.
        stop = perf_signals.StopSignals()
        with self.assertRaises(perf_signals.Terminated) as raised:
            stop.handle(signal.SIGTERM, None)
        self.assertIsInstance(raised.exception, KeyboardInterrupt)
        stop.handle(signal.SIGINT, None)
        self.assertEqual(stop.describe(), "SIGTERM, then SIGINT")

    def test_a_held_handler_records_without_raising(self):
        # Once the report is being written, even the first signal is only noted.
        stop = perf_signals.StopSignals()
        stop.hold()
        stop.handle(signal.SIGTERM, None)
        self.assertEqual(stop.received, [signal.SIGTERM])

    def test_check_raises_only_after_a_signal(self):
        # A signal something swallowed still stops the next run from starting.
        stop = perf_signals.StopSignals()
        stop.check()
        stop.hold()
        stop.handle(signal.SIGINT, None)
        with self.assertRaises(perf_signals.Terminated):
            stop.check()

    def test_handlers_are_installed_and_restored_on_the_main_thread(self):
        # Inside the block each stop signal reaches the handler; afterwards the previous handlers are back.
        before = {number: signal.getsignal(number) for number in perf_signals.STOP_SIGNALS}
        stop = perf_signals.StopSignals()
        with stop:
            for number in perf_signals.STOP_SIGNALS:
                self.assertEqual(signal.getsignal(number), stop.handle)
        self.assertEqual({number: signal.getsignal(number) for number in perf_signals.STOP_SIGNALS}, before)

    def test_nothing_is_installed_off_the_main_thread(self):
        # signal.signal raises off the main thread, so a comparison driven from a worker thread installs nothing.
        before = {number: signal.getsignal(number) for number in perf_signals.STOP_SIGNALS}
        seen = []

        def enter_and_read():
            with perf_signals.StopSignals():
                seen.append({number: signal.getsignal(number) for number in perf_signals.STOP_SIGNALS})
        worker = threading.Thread(target=enter_and_read)
        worker.start()
        worker.join()
        self.assertEqual(seen, [before])


if __name__ == "__main__":
    unittest.main(verbosity=2)
