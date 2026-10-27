"""Macos package attachment tests."""

import contextlib
import dataclasses
import io
import json
import os
from pathlib import Path
import plistlib
import re
import tempfile
import sys
import types
import unittest
from unittest.mock import patch

import macos_package_test_fixture
import gate_model


class AttachmentTests(unittest.TestCase):
    def test_census_accepts_empty_and_complete_multidevice_inventory(self):
        # Whole disks and slices remain distinct; normalized aliases preserve actual path identity.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            folder = fake.root / "alias"
            folder.mkdir()
            image = fake.owned(image=folder / ".." / fake.image.name)
            other = {"image-path": str(fake.root / "other.dmg"), "system-entities": [
                {"dev-entry": "/dev/disk3"}, {"dev-entry": "/dev/disk3s1"},
                {"dev-entry": "/dev/disk3s1s1", "mount-point": str(fake.root / "other mount")}]}
            self.assertEqual(macos_package_test_fixture.tool.attachment_census(plistlib.dumps({"images": []})), [])
            parsed = macos_package_test_fixture.tool.attachment_census(plistlib.dumps({"images": [image, other]}))
            self.assertEqual(parsed[0]["image"], str(fake.image))
            self.assertEqual(parsed[0]["entities"], [{"device": "/dev/disk2", "mount": None},
                {"device": "/dev/disk2s1", "mount": str(fake.mount)}])
            self.assertEqual(len(parsed[1]["entities"]), 3)

    def test_census_rejects_incomplete_ambiguous_and_unbounded_payloads(self):
        # Invalid census data cannot serve as absence or ownership evidence.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            invalid = [{}, {"images": {}}, {"images": [1]},
                {"images": [{"image-path": "relative.dmg", "system-entities": []}]},
                {"images": [{"image-path": str(fake.image)}]},
                {"images": [{"image-path": str(fake.image), "system-entities": {}}]},
                {"images": [{"image-path": str(fake.image), "system-entities": [1]}]},
                {"images": [{"image-path": str(fake.image), "system-entities": [{"dev-entry": "/dev/other"}]}]},
                {"images": [{"image-path": str(fake.image), "system-entities": [{"dev-entry": "/dev/disk2", "mount-point": "relative"}]}]},
                {"images": [fake.owned(), fake.owned()]}]
            payloads = [plistlib.dumps(value) for value in invalid]
            valid = plistlib.dumps({"images": []})
            payloads += [b"not plist", valid[:-12], valid + b"unparsed trailing bytes"]
            for payload in payloads:
                with self.subTest(payload=payload[:90]), self.assertRaises(RuntimeError):
                    macos_package_test_fixture.tool.attachment_census(payload)

    def test_census_rejects_oversized_otherwise_valid_plist(self):
        # Valid inventory syntax must not make the independent byte limit optional.
        payload = plistlib.dumps({"images": [], "padding": "x" * macos_package_test_fixture.tool.ATTACH_OUTPUT_LIMIT})
        self.assertGreater(len(payload), macos_package_test_fixture.tool.ATTACH_OUTPUT_LIMIT)
        with self.assertRaisesRegex(RuntimeError, "census exceeds its output bound"):
            macos_package_test_fixture.tool.attachment_census(payload)
        with patch.object(macos_package_test_fixture.tool, "ATTACH_OUTPUT_LIMIT", len(payload)):
            self.assertEqual(macos_package_test_fixture.tool.attachment_census(payload), [])

    def test_attach_success_owns_one_new_device_and_proves_detach(self):
        # Successful attachment is accepted only with matching private ownership and confirmed cleanup.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                self.assertEqual(attachment.attach(), fake.mount)
                attachment.finish(None)
            attach = next(step for step in fake.steps if step.argv[1] == "attach")
            self.assertEqual(attach.argv, ("/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse",
                "-mountpoint", str(fake.mount), str(fake.image)))
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
            report = fake.report()
            self.assertEqual((report["status"], report["attach_attempts"]), ("PASS", 1))
            self.assertEqual(report["owned_device"], "/dev/disk2s1")
            self.assertEqual(report["detached_device"], "/dev/disk2s1")
            self.assertEqual(report["censuses"][-1]["images"], [])
            self.assertTrue(all((fake.state / command["log"]).is_file() for command in report["commands"]))

    def test_refusal_is_never_retried_and_retains_three_late_censuses(self):
        # EAGAIN is one failed attempt, followed by evidence collection rather than another attach.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "Resource temporarily unavailable") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "info"])
            self.assertEqual(fake.waits, [2, 3])
            self.assertEqual(fake.report()["attach_attempts"], 1)
            self.assertEqual(fake.report()["status"], "FAIL")
            self.assertEqual([row["label"] for row in fake.report()["censuses"]],
                ["mount-before", "mount-cleanup-state-0", "mount-cleanup-state-1", "mount-cleanup-state-2"])
            self.assertTrue(all((fake.state / row["log"]).exists() for row in fake.report()["commands"]))

    def test_late_owned_mount_is_detached_without_erasing_attach_failure(self):
        # A later census can establish custody, but cannot retroactively make attach successful.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            fake.late_attach_query = 3
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations().count("attach"), 1)
            self.assertEqual(fake.operations().count("detach"), 1)
            self.assertEqual(fake.report()["status"], "FAIL")
            self.assertEqual(fake.report()["original_error"], str(caught.exception))
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")

    def test_partial_conflicting_preexisting_and_raced_mounts_are_not_detached(self):
        # Device names and mount paths alone never authorize teardown of an ambiguous attachment.
        for case in ("partial", "elsewhere", "conflict", "preexisting-device", "image-race", "directory-race"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
                if case == "preexisting-device":
                    fake.inventory = [{"image-path": str(fake.root / "other.dmg"),
                        "system-entities": [{"dev-entry": "/dev/disk2"}]}]
                def after(fake):
                    if case == "partial":
                        fake.clear()
                        fake.inventory = [{"image-path": str(fake.image), "system-entities": [{"dev-entry": "/dev/disk2"}]}]
                    elif case == "elsewhere":
                        fake.inventory = [fake.owned(mount=fake.root / "shared mount")]
                    elif case == "conflict":
                        fake.inventory = [fake.owned(image=fake.root / "other.dmg")]
                    elif case == "image-race":
                        fake.image.rename(fake.root / "original.dmg")
                        fake.image.write_bytes(b"replacement")
                    elif case == "directory-race":
                        fake.clear()
                        fake.mount.rename(fake.state / "original-mount")
                        fake.mount.mkdir()
                fake.after_attach = after
                with fake.active():
                    attachment = fake.attachment()
                    with self.assertRaises(RuntimeError) as caught:
                        attachment.attach()
                    attachment.finish(caught.exception)
                self.assertNotIn("detach", fake.operations())
                self.assertEqual(fake.report()["status"], "FAIL")

    def test_owned_census_requires_an_actual_mount_and_unique_mountpoint(self):
        # Census device membership alone cannot prove a live mount or choose between two mountpoint claimants.
        for case, reason in (("unmounted", "census mount is not present"),
                             ("duplicate-mount", "ownership is partial or ambiguous")):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
                def after(state):
                    if case == "unmounted":
                        state.mounted = False
                    else:
                        state.inventory[0]["system-entities"].append(
                            {"dev-entry": "/dev/disk3s1", "mount-point": str(state.mount)})
                fake.after_attach = after
                with fake.active():
                    attachment = fake.attachment()
                    with self.assertRaisesRegex(RuntimeError, reason) as caught:
                        attachment.attach()
                    attachment.finish(caught.exception)
                self.assertNotIn("detach", fake.operations())
                self.assertTrue(fake.report()["cleanup_errors"])

    def test_owned_image_accepts_physical_and_synthesized_devices(self):
        # APFS can expose physical and synthesized disk identities within one uniquely owned image.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            def after(state):
                state.inventory[0]["system-entities"].insert(0, {"dev-entry": "/dev/disk7"})
                state.inventory[0]["system-entities"].insert(1, {"dev-entry": "/dev/disk7s1"})
            fake.after_attach = after
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                attachment.finish(None)
            self.assertEqual(fake.report()["status"], "PASS")
            self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")
            self.assertEqual(len(fake.report()["censuses"][1]["images"][0]["entities"]), 4)

    def test_zero_exit_detach_requires_complete_absence_evidence(self):
        # A zero detach exit cannot replace the independent mount, device and complete-plist checks.
        cases = {"retained-mount": "owned attachment remains after detach",
                 "retained-device": "ownership is partial or ambiguous",
                 "invalid-plist": "census is not a complete plist"}
        for case, reason in cases.items():
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
                execute = fake.execute
                def contradict(step, *args, **kwargs):
                    result = execute(step, *args, **kwargs)
                    if step.id == "unmount":
                        if case == "retained-mount":
                            fake.install()
                        elif case == "retained-device":
                            fake.inventory = [{"image-path": str(fake.image),
                                               "system-entities": [{"dev-entry": "/dev/disk2"}]}]
                        else:
                            fake.census_payload = b"<plist><dict>"
                    return result
                with fake.active(), patch.object(macos_package_test_fixture.tool.GATE, "run_step", side_effect=contradict):
                    attachment = fake.attachment()
                    attachment.attach()
                    with self.assertRaisesRegex(RuntimeError, reason):
                        attachment.finish(None)
                self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
                self.assertNotIn("detached_device", fake.report())
                self.assertEqual(fake.report()["status"], "FAIL")
                self.assertIsNone(fake.report()["original_error"])

    def test_preexisting_image_alias_is_refused_before_attach(self):
        # Filesystem aliases to the same image cannot evade the pre-existing attachment census.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            alias = fake.root / "hardlink.dmg"
            os.link(fake.image, alias)
            fake.inventory = [fake.owned(image=alias, mount=fake.root / "shared")]
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "already attached") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])
            self.assertEqual(fake.report()["attach_attempts"], 0)

    def test_zero_exit_without_owned_mount_is_not_success(self):
        # A successful process exit without a census-backed mount cannot reach validation.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.after_attach = lambda fake: fake.clear()
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "no owned mount") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertNotIn("detach", fake.operations())
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_unsettled_or_fatal_supervisor_results_never_accept_attach(self):
        # Even owned-looking native side effects cannot turn an unsettled command into accepted attachment.
        outcomes = [("TIMEOUT", -9, 0), ("LAUNCH", None, 0), ("FAIL", None, 0),
            ("FAIL", -15, 0), ("FAIL", 0, 0), ("FAIL", 1, None),
            ("PASS", None, 0), ("PASS", -15, 0), ("PASS", 0, None), ("PASS", 0, 1)]
        for outcome in outcomes:
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as directory:
                fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
                fake.attach_result = outcome
                fake.after_attach = lambda state: state.install()
                with fake.active():
                    attachment = fake.attachment()
                    prefix = re.escape(f"mount: {outcome[0]} exit={outcome[1]}:")
                    with self.assertRaisesRegex(RuntimeError, "^" + prefix) as caught:
                        attachment.attach()
                    self.assertTrue(fake.mounted)
                    self.assertEqual(fake.operations(), ["info", "attach"])
                    self.assertNotIn("owned_device", attachment.report)
                    attachment.finish(caught.exception)
                self.assertEqual(fake.operations(), ["info", "attach", "info", "detach", "info"])
                self.assertFalse(fake.mounted)
                self.assertEqual(fake.report()["detached_device"], "/dev/disk2s1")
                self.assertEqual(fake.report()["original_error"], str(caught.exception))
                self.assertEqual(fake.report()["status"], "FAIL")

    def test_invalid_census_blocks_new_attachment(self):
        # Invalid successful query output is not treated as an empty inventory.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.census_payload = b"<plist><dict>"
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_admission_and_expired_deadline_never_launch_operations(self):
        # Admission includes command and supervision allowances, and expired cleanup cannot launch.
        for remaining in (17, -1):
            with self.subTest(remaining=remaining), tempfile.TemporaryDirectory() as directory:
                fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
                fake.now = fake.deadline - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS - remaining
                with fake.active():
                    attachment = fake.attachment()
                    with self.assertRaisesRegex(RuntimeError, "budget") as caught:
                        attachment.attach()
                    fake.now = fake.deadline + 1
                    attachment.attempted = True
                    attachment.finish(caught.exception)
                self.assertEqual(fake.steps, [])
                self.assertTrue(fake.report()["cleanup_errors"])

    def test_insufficient_attach_proof_budget_stops_after_initial_census(self):
        # The attach command must fit together with the required post-attach ownership census.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.now = fake.deadline - macos_package_test_fixture.tool.CLEANUP_RESERVE_SECONDS - 92
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaisesRegex(RuntimeError, "budget") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])

    def test_oversleep_cannot_spend_protected_settlement_budget(self):
        # A late scheduler wake is rechecked before launching any later query or detach.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            fake.oversleep = 500
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info", "attach", "info"])
            self.assertEqual(fake.waits, [2])
            self.assertTrue(fake.report()["cleanup_errors"])

    def test_initial_cleanup_census_can_use_confirmation_slack(self):
        # Current ownership and detach take priority when the full confirmation reserve no longer fits.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                fake.now = fake.deadline - 72
                attachment.finish(None)
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach", "info"])
            self.assertEqual(fake.report()["status"], "PASS")
            self.assertFalse(fake.mounted)

    def test_optional_late_census_preserves_detach_and_confirmation_time(self):
        # A failed attach may have escaped helpers; later observations cannot spend the settlement reserve.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            with fake.active():
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                fake.now = fake.deadline - 72
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info", "attach", "info"])
            self.assertEqual([row["label"] for row in fake.report()["commands"]
                              if row["status"] == "SKIPPED_BUDGET"],
                             ["mount-cleanup-state-1", "mount-cleanup-state-2"])
            self.assertTrue(fake.report()["cleanup_errors"])

    def test_slow_census_does_not_reserve_confirmation_instead_of_detaching(self):
        # Supervisor overrun may leave only enough time to detach; missing confirmation must still fail.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            execute = fake.execute
            def slow(step, *args, **kwargs):
                result = execute(step, *args, **kwargs)
                if step.id == "mount-cleanup-state-0":
                    fake.now = fake.deadline - 40
                elif step.id == "unmount":
                    fake.now = fake.deadline - 5
                return result
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                fake.now = fake.deadline - 75
                with patch.object(macos_package_test_fixture.tool.GATE, "run_step", side_effect=slow), \
                        self.assertRaisesRegex(RuntimeError, "cleanup unconfirmed"):
                    attachment.finish(None)
            self.assertEqual(fake.operations(), ["info", "attach", "info", "info", "detach"])
            self.assertFalse(fake.mounted)
            self.assertNotIn("detached_device", fake.report())
            self.assertEqual(fake.report()["commands"][-1],
                             {"label": "unmount-after", "status": "SKIPPED_BUDGET"})

    def test_later_owned_detach_resolves_earlier_observation_errors(self):
        # Retain a malformed early observation without misreporting a later confirmed owned cleanup.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.attach_result = ("FAIL", 1, 0)
            execute = fake.execute
            def observe(step, *args, **kwargs):
                fake.census_payload = b"invalid plist" if step.id == "mount-cleanup-state-0" else None
                if step.id == "mount-cleanup-state-1":
                    fake.install()
                return execute(step, *args, **kwargs)
            with fake.active(), patch.object(macos_package_test_fixture.tool.GATE, "run_step", side_effect=observe):
                attachment = fake.attachment()
                with self.assertRaises(RuntimeError) as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            report = fake.report()
            self.assertFalse(fake.mounted)
            self.assertEqual(report["detached_device"], "/dev/disk2s1")
            self.assertEqual(report["cleanup_errors"], [])
            self.assertTrue(report["observation_errors"])
            self.assertEqual(report["original_error"], str(caught.exception))
            self.assertEqual(report["status"], "FAIL")

    def test_unreadable_image_alias_remains_unknown(self):
        # A different unreadable pathname could be a hard link to the target, so it cannot prove absence.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            candidate = fake.root / "unreadable.dmg"
            fake.inventory = [fake.owned(image=candidate, mount=fake.root / "shared")]
            original_stat = type(candidate).stat
            def stat(path, *args, **kwargs):
                if path == candidate:
                    raise PermissionError("unreadable image identity")
                return original_stat(path, *args, **kwargs)
            with fake.active(), patch.object(type(candidate), "stat", stat):
                attachment = fake.attachment()
                with self.assertRaisesRegex(PermissionError, "unreadable image identity") as caught:
                    attachment.attach()
                attachment.finish(caught.exception)
            self.assertEqual(fake.operations(), ["info"])
            self.assertEqual(fake.report()["attach_attempts"], 0)

    def test_cleanup_failure_alone_fails_the_result(self):
        # A valid mount and validation cannot make an unconfirmed detach a passing result.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            fake.detach_result = ("FAIL", 1, 0)
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                with self.assertRaisesRegex(RuntimeError, "cleanup"):
                    attachment.finish(None)
            self.assertIsNone(fake.report()["original_error"])
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_evidence_write_failure_preserves_original_and_existing_bytes(self):
        # Exclusive evidence creation must preserve both the initial error and an existing receipt.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            original = RuntimeError("original validation failure")
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                receipt = fake.state / "attachment-result.json"
                receipt.write_bytes(b"earlier evidence")
                attachment.finish(original)
            self.assertEqual(receipt.read_bytes(), b"earlier evidence")
            self.assertEqual(attachment.report["original_error"], str(original))
            self.assertIn("detach", fake.operations())

    def test_cleanup_interrupt_does_not_replace_original(self):
        # A cleanup interrupt is secondary when an earlier validation error already owns the result.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            original = RuntimeError("original validation failure")
            with fake.active():
                attachment = fake.attachment()
                attachment.attach()
                with patch.object(macos_package_test_fixture.tool.GATE, "run_step", side_effect=KeyboardInterrupt):
                    attachment.finish(original)
            self.assertEqual(fake.report()["original_error"], str(original))
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_replaced_evidence_directory_prevents_commands_and_receipt_writes(self):
        # A renamed private directory cannot authorize commands or evidence writes into its replacement.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                fake.state.rename(fake.root / "original-state")
                fake.state.mkdir()
                with self.assertRaisesRegex(RuntimeError, "directory changed"):
                    attachment.command("must-not-launch", ["/usr/bin/hdiutil", "info", "-plist"], 3)
                original = RuntimeError("original validation failure")
                attachment.finish(original)
            self.assertEqual(fake.steps, [])
            self.assertFalse((fake.state / "attachment-result.json").exists())
            self.assertEqual(list(fake.state.iterdir()), [])

    def test_symlinked_mountpoint_is_rejected_without_native_work(self):
        # Simulate the filesystem symlink boundary without requiring Windows symlink privileges.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            with fake.active():
                attachment = fake.attachment()
                original_is_symlink = type(fake.mount).is_symlink
                with patch.object(type(fake.mount), "is_symlink",
                                  lambda path: True if path == fake.mount else original_is_symlink(path)):
                    with self.assertRaisesRegex(RuntimeError, "symlink") as caught:
                        attachment.attach()
                    attachment.finish(caught.exception)
            self.assertEqual(fake.steps, [])
            self.assertEqual(fake.report()["status"], "FAIL")

    def test_real_supervisor_overflow_is_not_accepted_as_successful_payload(self):
        # Truncated child output stays a custody failure even when the child itself exits zero.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            attachment = fake.attachment()
            code = f"import sys; sys.stdout.buffer.write(b'x' * {macos_package_test_fixture.tool.ATTACH_OUTPUT_LIMIT + 1})"
            with patch.object(macos_package_test_fixture.tool, "DEADLINE", None), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, r"^python-overflow: FAIL exit=0:") as caught:
                    attachment.command("python-overflow", [sys.executable, "-c", code], 5)
                attachment.finish(caught.exception)
            command = fake.report()["commands"][0]
            self.assertEqual((command["status"], command["exit_code"], command["leftover_processes"]), ("FAIL", 0, 0))
            self.assertIn(f"output limit of {macos_package_test_fixture.tool.ATTACH_OUTPUT_LIMIT} bytes exceeded", command["detail"])
            self.assertEqual(fake.report()["status"], "FAIL")
            self.assertEqual(fake.report()["attach_attempts"], 0)

    def test_windows_phase_framing_returns_exact_child_payload(self):
        # Actual runner framing must preserve child bytes, including lines that resemble supervisor records.
        payloads = (b"", b"no final newline", b"first\r\nsecond\x00\xff",
                    b"[local-gate] phase=execution result=PASS exit=0 custody={}\n",
                    plistlib.dumps({"images": []}))
        for payload in payloads:
            with self.subTest(payload=payload[:50]), tempfile.TemporaryDirectory() as directory:
                fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
                attachment = fake.attachment()
                custody = {"before_cleanup": {"active_processes": 0, "total_processes": 1},
                           "after_cleanup": {"active_processes": 0, "total_processes": 1},
                           "cleanup": "none", "empty": True, "bootstrap_reaped": True,
                           "protocol_complete": True, "capture_complete": True, "errors": []}
                def execute(command, **kwargs):
                    self.assertEqual(command, (sys.executable, "-c", "pass"))
                    kwargs["sink"].write(payload)
                    return {"exit_code": 0, "interrupted": False, "timed_out": False,
                            "launch_failed": False, "natural": True, "errors": [], "custody": custody}
                with patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)), \
                        patch.object(macos_package_test_fixture.tool, "DEADLINE", None), contextlib.redirect_stderr(io.StringIO()):
                    actual = attachment.command("windows-framing", [sys.executable, "-c", "pass"], 5)
                    attachment.finish(None)
                self.assertEqual(actual, payload)
                command = fake.report()["commands"][0]
                self.assertEqual((command["status"], command["detail"]), ("PASS", "execution=PASS"))

    def test_attachment_framing_rejects_phase_and_envelope_drift(self):
        # Unknown phases or changed envelope bytes cannot be silently trimmed into an accepted inventory.
        mutations = ("extra-phase", "wrong-phase", "cleaned-phase", "wrong-argv", "phase-environment",
                     "incomplete-custody", "header", "phase-header", "phase-footer", "footer", "trailing")
        for mutation in mutations:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
                attachment = fake.attachment()
                custody = {"before_cleanup": {"active_processes": 0, "total_processes": 1},
                           "after_cleanup": {"active_processes": 0, "total_processes": 1},
                           "cleanup": "none", "empty": True, "bootstrap_reaped": True,
                           "protocol_complete": True, "capture_complete": True, "errors": []}
                def execute(_command, **kwargs):
                    kwargs["sink"].write(plistlib.dumps({"images": []}))
                    return {"exit_code": 0, "interrupted": False, "timed_out": False,
                            "launch_failed": False, "natural": True, "errors": [], "custody": custody}
                run_step = macos_package_test_fixture.tool.GATE.run_step
                def corrupt(*args, **kwargs):
                    result = run_step(*args, **kwargs)
                    phase = result.phases[0]
                    if mutation == "extra-phase":
                        return dataclasses.replace(result, phases=(phase, phase))
                    changes = {
                        "wrong-phase": {"name": "preparation-1"},
                        "cleaned-phase": {"policy": macos_package_test_fixture.tool.GATE.WindowsPolicy.COMPILE_ONLY},
                        "wrong-argv": {"argv": (sys.executable, "-c", "other")},
                        "phase-environment": {"env": (("UNEXPECTED", "value"),)},
                        "incomplete-custody": {"custody": {**custody, "capture_complete": False}},
                    }
                    if mutation in changes:
                        return dataclasses.replace(result, phases=(dataclasses.replace(phase, **changes[mutation]),))
                    data = result.log_path.read_bytes()
                    changes = {
                        "header": (b"[local-gate] step=", b"[changed-gate] step="),
                        "phase-header": (b"phase=execution policy=strict", b"phase=execution policy=compile-only"),
                        "phase-footer": (b"phase=execution result=PASS", b"phase=execution result=FAIL"),
                        "footer": (b" detail=execution=PASS\n", b" detail=altered\n"),
                    }
                    if mutation == "trailing":
                        data += b"unattributed output"
                    else:
                        before, after = changes[mutation]
                        self.assertEqual(data.count(before), 1)
                        data = data.replace(before, after, 1)
                    result.log_path.write_bytes(data)
                    return result
                with patch.object(gate_model, "WINDOWS_JOB", types.SimpleNamespace(run=execute)), \
                        patch.object(macos_package_test_fixture.tool.GATE, "run_step", side_effect=corrupt), \
                        patch.object(macos_package_test_fixture.tool, "DEADLINE", None), contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaisesRegex(RuntimeError, "supervisor (phases|output framing)"):
                        attachment.command("windows-framing", [sys.executable, "-c", "pass"], 5)

    def test_real_supervisor_framing_preserves_home_and_removes_color(self):
        # A harmless real Python child pins the actual shared-runner framing and environment contract.
        with tempfile.TemporaryDirectory() as directory:
            fake = macos_package_test_fixture.FakeAttachmentGate(Path(directory))
            attachment = fake.attachment()
            home = str(fake.root / "real home")
            code = "import json,os; print(json.dumps([os.environ.get('HOME'),os.environ.get('NO_COLOR')]))"
            with patch.object(macos_package_test_fixture.tool, "DEADLINE", None), patch.dict(os.environ, {"HOME": home, "NO_COLOR": "1"}), \
                    contextlib.redirect_stderr(io.StringIO()):
                payload = attachment.command("python-framing", [sys.executable, "-c", code], 5)
                attachment.finish(None)
            self.assertEqual(json.loads(payload), [home, None])
            command = fake.report()["commands"][0]
            self.assertEqual((command["status"], command["exit_code"], command["leftover_processes"]), ("PASS", 0, 0))
            self.assertIn(b"[local-gate] result=PASS", (fake.state / command["log"]).read_bytes())
