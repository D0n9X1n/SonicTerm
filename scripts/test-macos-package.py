#!/usr/bin/env python3
"""Exercise a relocated macOS package without reading Homebrew runtime libraries."""

from __future__ import annotations

import argparse
import importlib.util
import io
import json
import os
import re
import stat
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import time
import uuid
from xml.parsers.expat import ExpatError

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("smoke_runner", ROOT / "scripts/native-smoke-runner.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)
GATE_SPEC = importlib.util.spec_from_file_location("package_attachment_gate", ROOT / "scripts/local-gate.py")
GATE = importlib.util.module_from_spec(GATE_SPEC)
sys.modules[GATE_SPEC.name] = GATE
GATE_SPEC.loader.exec_module(GATE)
FACES = {
    "Regular": "RecMonoSt.Helens",
    "Italic": "RecMonoSt.Helens-Italic",
    "Bold": "RecMonoSt.Helens-Bold",
    "BoldItalic": "RecMonoSt.Helens-BoldItalic",
}
DENY_BREW = '(version 1) (allow default) (deny file-read* (subpath "/opt/homebrew") (subpath "/usr/local"))'

# The CI step allows 480 s. Commands share a 420 s deadline from entry; the rest covers interpreter
# start, the runner's post-kill waits, and file work outside commands.
STEP_BUDGET_SECONDS = 420
# Attachment cleanup reserves a census, owned detach, and confirming census before new work.
CLEANUP_RESERVE_SECONDS = 75
VALIDATION_SUPERVISION_SECONDS = 10
ATTACH_SUPERVISION_SECONDS = 15
ATTACH_SETTLEMENT_SECONDS = 55
ATTACH_QUERY_SECONDS = 3
ATTACH_DETACH_SECONDS = 20
ATTACH_OUTPUT_LIMIT = 1 << 20
# `hdiutil create` can fail transiently with `Resource busy`; only that failure is retried.
BUSY_RETRY_ATTEMPTS = 3
BUSY_RETRY_WAIT_SECONDS = 10
BUSY_RETRY_MINIMUM_SECONDS = 30
# Set by `main()` at entry. `None` leaves every timeout unchanged, as helpers called directly expect.
DEADLINE: float | None = None
clock = time.monotonic
sleep = time.sleep


def capped_timeout(label: str, timeout: int, cleanup: bool = False) -> int:
    """Cap `timeout` at the time left before the unmount reserve, or before the deadline for cleanup."""
    if DEADLINE is None:
        return timeout
    limit = DEADLINE if cleanup else DEADLINE - CLEANUP_RESERVE_SECONDS - VALIDATION_SUPERVISION_SECONDS
    remaining = int(limit - clock())
    if cleanup:
        # The unmount runs even after the other commands' budget is spent.
        return max(1, min(timeout, remaining))
    if remaining < 1:
        raise RuntimeError(f"{label}: validator time budget exhausted")
    return min(timeout, remaining)


def run_capture(command: list[str], state: Path, label: str, timeout: int = 60, env=None,
                cleanup: bool = False) -> subprocess.CompletedProcess[bytes]:
    """Run one bounded command, keep its output under `label`, and return the result without raising."""
    timeout = capped_timeout(label, timeout, cleanup)
    print(f"[package-check] start {label} timeout={timeout}s", file=sys.stderr, flush=True)
    result = RUNNER.run_command(command, ROOT, timeout, env or clean_environment())
    (state / (label + ".log")).write_bytes(result.stdout + result.stderr)
    print(f"[package-check] finish {label} exit={result.returncode} timeout={timeout}s", file=sys.stderr, flush=True)
    return result


def command_failure(label: str, result: subprocess.CompletedProcess[bytes]) -> RuntimeError:
    output = result.stdout + result.stderr
    return RuntimeError(f"{label} exited {result.returncode}: {output.decode(errors='replace')[-4000:]}")


def run(command: list[str], state: Path, label: str, timeout: int = 60, env=None, cleanup: bool = False) -> bytes:
    result = run_capture(command, state, label, timeout, env, cleanup)
    if result.returncode:
        raise command_failure(label, result)
    return result.stdout + result.stderr


def clean_environment() -> dict[str, str]:
    return {key: value for key, value in os.environ.items()
            if key != "NO_COLOR" and not key.startswith("DYLD_")}


def attachment_census(payload: bytes) -> list[dict]:
    """Validate the complete image/device inventory without equating an unmounted device with absence."""
    if len(payload) > ATTACH_OUTPUT_LIMIT:
        raise RuntimeError("attachment census exceeds its output bound")
    try:
        value = plistlib.loads(payload)
    except (ValueError, TypeError, plistlib.InvalidFileException, ExpatError) as error:
        raise RuntimeError("attachment census is not a complete plist") from error
    if not isinstance(value, dict) or not isinstance(value.get("images"), list):
        raise RuntimeError("attachment census has no images array")
    images, devices = [], set()
    for image in value["images"]:
        if not isinstance(image, dict):
            raise RuntimeError("attachment census image is not a dictionary")
        path, entities = image.get("image-path"), image.get("system-entities")
        if not isinstance(path, str) or not Path(path).is_absolute() or not isinstance(entities, list):
            raise RuntimeError("attachment census image identity is incomplete")
        current = {"image": str(Path(path).resolve()), "entities": []}
        for entity in entities:
            if not isinstance(entity, dict):
                raise RuntimeError("attachment census device is not a dictionary")
            device, mount = entity.get("dev-entry"), entity.get("mount-point")
            if not isinstance(device, str) or not re.fullmatch(r"/dev/disk[0-9]+(?:s[0-9]+)*", device):
                raise RuntimeError("attachment census device identity is invalid")
            if device in devices:
                raise RuntimeError("attachment census repeats a device identity")
            devices.add(device)
            if "mount-point" in entity and (not isinstance(mount, str) or not Path(mount).is_absolute()):
                raise RuntimeError("attachment census mount identity is invalid")
            current["entities"].append({"device": device, "mount": str(Path(mount).resolve()) if mount else None})
        images.append(current)
    return images


def attachment_identity(path: Path) -> tuple[int, int, int]:
    info = path.lstat()
    if stat.S_ISLNK(info.st_mode):
        raise RuntimeError(f"attachment path is a symlink: {path}")
    return info.st_dev, info.st_ino, stat.S_IFMT(info.st_mode)


class DmgAttachment:
    """Own only an attachment proved new at this invocation's private mountpoint."""

    def __init__(self, image: Path, state: Path):
        self.image, self.state = image.resolve(), state
        self.mount = state / "mounted"
        self.mount.mkdir()
        self.image_identity = attachment_identity(self.image)
        self.directory_identity = attachment_identity(self.mount)
        self.parent_identity = attachment_identity(state)
        if self.image_identity[2] != stat.S_IFREG or self.directory_identity[2] != stat.S_IFDIR:
            raise RuntimeError("attachment requires a regular image and private directory")
        self.before_devices: set[str] = set()
        self.attempted = False
        self.report = {"status": "FAIL", "image": str(self.image), "mount": str(self.mount),
                       "commands": [], "censuses": [], "observation_errors": [],
                       "cleanup_errors": [], "attach_attempts": 0}

    def remaining(self, cleanup: bool) -> float:
        if DEADLINE is None:
            return float("inf")
        return DEADLINE - (0 if cleanup else CLEANUP_RESERVE_SECONDS) - clock()

    def command(self, label: str, argv: list[str], timeout: int, *, cleanup=False, keep=0) -> bytes:
        if attachment_identity(self.state) != self.parent_identity:
            raise RuntimeError("attachment command directory changed")
        if self.remaining(cleanup) < timeout + ATTACH_SUPERVISION_SECONDS + keep:
            self.report["commands"].append({"label": label, "status": "SKIPPED_BUDGET"})
            raise RuntimeError(f"{label}: attachment admission budget exhausted")
        step = GATE.Step(label, tuple(argv), ("macos",), timeout, "local", (), ())
        environment = clean_environment()
        environment["LC_ALL"] = "C"
        print(f"[package-check] start {label} timeout={timeout}s", file=sys.stderr, flush=True)
        result = GATE.run_step(step, len(self.report["commands"]) + 1, ROOT, self.state,
                               environment, output_limit_bytes=ATTACH_OUTPUT_LIMIT)
        self.report["commands"].append({"label": label, "argv": argv, "status": result.status,
                                       "exit_code": result.exit_code, "elapsed_seconds": result.elapsed_s,
                                       "leftover_processes": result.leftover_processes,
                                       "detail": result.detail, "log": result.log_path.name})
        print(f"[package-check] finish {label} status={result.status} exit={result.exit_code}",
              file=sys.stderr, flush=True)
        if result.status != GATE.PASS or result.exit_code != 0 or result.leftover_processes != 0:
            tail = result.log_path.read_bytes()[-4000:].decode(errors="replace")
            raise RuntimeError(f"{label}: {result.status} exit={result.exit_code}: {tail}")
        header = io.BytesIO()
        GATE._write_header(header, step, GATE.launch_argv(step), ROOT)
        data = result.log_path.read_bytes()
        footer = f"\n[local-gate] result=PASS exit=0 elapsed={result.elapsed_s:.1f}s\n".encode()
        if not data.startswith(header.getvalue()) or not data.endswith(footer):
            raise RuntimeError(f"{label}: supervisor output framing changed")
        return data[len(header.getvalue()):-len(footer)]

    def census(self, label: str, *, cleanup=False, keep=0) -> list[dict]:
        payload = self.command(label, ["/usr/bin/hdiutil", "info", "-plist"],
                               ATTACH_QUERY_SECONDS, cleanup=cleanup, keep=keep)
        inventory = attachment_census(payload)
        self.report["censuses"].append({"label": label, "images": inventory})
        return inventory

    def check_paths(self) -> None:
        if attachment_identity(self.state) != self.parent_identity:
            raise RuntimeError("attachment state directory changed")
        if attachment_identity(self.image) != self.image_identity:
            raise RuntimeError("attachment image identity changed")
        if self.mount.is_symlink():
            raise RuntimeError("attachment mountpoint became a symlink")
        if not self.mount.is_mount() and attachment_identity(self.mount) != self.directory_identity:
            raise RuntimeError("attachment private mount directory changed")

    def matches_image(self, path: str) -> bool:
        candidate = Path(path)
        if candidate == self.image:
            return True
        try:
            info = candidate.stat()
            return (info.st_dev, info.st_ino) == self.image_identity[:2]
        except FileNotFoundError:
            return False

    def owned_device(self, inventory: list[dict]) -> str | None:
        self.check_paths()
        images = [image for image in inventory if self.matches_image(image["image"])]
        mounted = [(image, entity) for image in inventory for entity in image["entities"]
                   if entity["mount"] == str(self.mount.resolve())]
        if not images and not mounted and not self.mount.is_mount():
            if any(self.mount.iterdir()):
                raise RuntimeError("unmounted private directory is not empty")
            return None
        if len(images) != 1 or len(mounted) != 1 or mounted[0][0] is not images[0]:
            raise RuntimeError("attachment ownership is partial or ambiguous")
        entities = images[0]["entities"]
        if any(entity["device"] in self.before_devices for entity in entities):
            raise RuntimeError("attachment reuses a pre-existing device")
        if any(entity["mount"] not in (None, str(self.mount.resolve())) for entity in entities):
            raise RuntimeError("target image is mounted outside its private directory")
        if not self.mount.is_mount():
            raise RuntimeError("census mount is not present at its private directory")
        return mounted[0][1]["device"]

    def attach(self) -> Path:
        problem = GATE.sigchld_problem()
        if problem:
            raise RuntimeError(problem)
        self.check_paths()
        if self.mount.is_mount() or any(self.mount.iterdir()):
            raise RuntimeError("private mount directory is already occupied")
        before = self.census("mount-before")
        if any(self.matches_image(image["image"]) for image in before) or any(
                entity["mount"] == str(self.mount.resolve()) for image in before for entity in image["entities"]):
            raise RuntimeError("image or mountpoint was already attached")
        self.before_devices = {entity["device"] for image in before for entity in image["entities"]}
        self.check_paths()
        if self.mount.is_mount() or any(self.mount.iterdir()):
            raise RuntimeError("private mount directory changed before attachment")
        if self.remaining(False) < 60 + ATTACH_SUPERVISION_SECONDS + ATTACH_QUERY_SECONDS + ATTACH_SUPERVISION_SECONDS:
            raise RuntimeError("mount: insufficient budget for attachment and ownership proof")
        self.attempted = True
        self.report["attach_attempts"] = 1
        self.command("mount", ["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse",
                               "-mountpoint", str(self.mount), str(self.image)], 60)
        device = self.owned_device(self.census("mount-after"))
        if device is None:
            raise RuntimeError("successful attach produced no owned mount")
        self.report["owned_device"] = device
        return self.mount

    def settle(self, failed: bool) -> None:
        if not self.attempted:
            return
        failure_started = clock()
        errors = []
        device = None
        for index, delay in enumerate((0, 2, 5) if failed else (0,)):
            try:
                wait = max(0, failure_started + delay - clock())
                keep = ATTACH_DETACH_SECONDS + ATTACH_SUPERVISION_SECONDS if index == 0 else ATTACH_SETTLEMENT_SECONDS
                if self.remaining(True) < wait + ATTACH_QUERY_SECONDS + ATTACH_SUPERVISION_SECONDS + keep:
                    self.report["commands"].append({"label": f"mount-cleanup-state-{index}", "status": "SKIPPED_BUDGET"})
                    raise RuntimeError("attachment cleanup evidence budget exhausted")
                if wait:
                    sleep(wait)
                inventory = self.census(f"mount-cleanup-state-{index}", cleanup=True, keep=keep)
                device = self.owned_device(inventory)
                if device:
                    break
            except Exception as error:
                errors.append(str(error))
                self.report["observation_errors"].append({"label": f"mount-cleanup-state-{index}", "error": str(error)})
        if device:
            try:
                self.command("unmount", ["/usr/bin/hdiutil", "detach", device], ATTACH_DETACH_SECONDS,
                             cleanup=True)
                after = self.census("unmount-after", cleanup=True)
                if self.owned_device(after) is not None:
                    raise RuntimeError("owned attachment remains after detach")
                self.report["detached_device"] = device
                errors.clear()
            except Exception as error:
                errors.append(str(error))
        self.report["cleanup_errors"].extend(errors)
        if errors:
            raise RuntimeError("attachment cleanup unconfirmed: " + "; ".join(errors))

    def finish(self, original: BaseException | None) -> None:
        cleanup_error = None
        try:
            self.settle(original is not None)
        except BaseException as error:
            cleanup_error = error
        self.report["original_error"] = str(original) if original is not None else None
        self.report["status"] = "PASS" if original is None and cleanup_error is None else "FAIL"
        try:
            if attachment_identity(self.state) != self.parent_identity:
                raise RuntimeError("attachment evidence directory changed")
            destination = self.state / "attachment-result.json"
            with destination.open("x", encoding="utf-8") as output:
                output.write(json.dumps(self.report, indent=2) + "\n")
        except Exception as error:
            print(f"[package-check] attachment evidence write failed: {error}", file=sys.stderr, flush=True)
            if cleanup_error is None:
                cleanup_error = error
        if cleanup_error is not None:
            if original is None:
                raise cleanup_error
            print(f"[package-check] secondary cleanup failure: {cleanup_error}", file=sys.stderr, flush=True)


def size(path: Path) -> int:
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file() and not p.is_symlink())


def retry_fits() -> bool:
    """Whether a busy retry would still keep its minimum time before the unmount reserve after the wait."""
    if DEADLINE is None:
        return True
    remaining = (DEADLINE - CLEANUP_RESERVE_SECONDS - VALIDATION_SUPERVISION_SECONDS
                 - clock() - BUSY_RETRY_WAIT_SECONDS)
    return remaining >= BUSY_RETRY_MINIMUM_SECONDS


def create_measurement_image(source: Path, image: Path, state: Path, label: str) -> None:
    """Create one measurement image, retrying only `hdiutil create`'s transient `Resource busy`."""
    command = ["/usr/bin/hdiutil", "create", "-volname", "SonicTerm", "-srcfolder", str(source),
               "-ov", "-format", "UDZO", str(image)]
    for attempt in range(1, BUSY_RETRY_ATTEMPTS + 1):
        # The first attempt keeps the phase's own label; later attempts keep their output separately.
        attempt_label = label if attempt == 1 else f"{label}-attempt{attempt}"
        result = run_capture(command, state, attempt_label, 120)
        if not result.returncode:
            return
        output = result.stdout + result.stderr
        # A timed-out create is not transient, whatever it printed before the runner killed it.
        busy = result.returncode != RUNNER.TIMEOUT_EXIT_CODE and b"Resource busy" in output
        if not busy or attempt == BUSY_RETRY_ATTEMPTS or not retry_fits():
            raise command_failure(attempt_label, result)
        lines = output.decode(errors="replace").strip().splitlines()
        detail = lines[-1] if lines else f"exit {result.returncode}"
        print(f"[package-check] retry {label} attempt={attempt + 1}/{BUSY_RETRY_ATTEMPTS} after: {detail}",
              file=sys.stderr, flush=True)
        sleep(BUSY_RETRY_WAIT_SECONDS)


def measure_font_savings(app: Path, state: Path) -> dict[str, int]:
    measured = state / "measurement/SonicTerm.app"
    shutil.copytree(app, measured, symlinks=True)
    result = {}
    for label in ("single", "duplicated"):
        if label == "duplicated":
            shutil.copytree(measured / "Contents/Resources/assets/fonts", measured / "Contents/Resources/Fonts")
            plist = measured / "Contents/Info.plist"
            info = plistlib.loads(plist.read_bytes())
            info["ATSApplicationFontsPath"] = "Fonts"
            plist.write_bytes(plistlib.dumps(info))
            run(["/usr/bin/codesign", "--force", "--sign", "-", str(measured)], state, "measurement-sign")
        image = state / (label + "-fonts.dmg")
        create_measurement_image(measured, image, state, label + "-measurement")
        result[label + "_dmg_bytes"] = image.stat().st_size
    result["font_dmg_saved_bytes"] = result["duplicated_dmg_bytes"] - result["single_dmg_bytes"]
    if result["font_dmg_saved_bytes"] <= 0:
        raise RuntimeError("controlled font deduplication did not reduce the DMG")
    shutil.rmtree(measured.parent)
    return result


def run_font_probe(probe: Path, state: Path, cairo: Path) -> None:
    timeout = capped_timeout("probe-launch", 25)
    print(f"[package-check] start probe-launch timeout={timeout}s", file=sys.stderr, flush=True)
    try:
        report = state / "native-fonts-cairo.log"
        if report.exists():
            raise RuntimeError(f"native probe report already exists: {report}")
        result = RUNNER.run_command(
            ["/usr/bin/open", "-n", "-g", "-W", str(probe), "--args", str(report), str(cairo)],
            ROOT, timeout, clean_environment())
        output = result.stdout + result.stderr
        (state / "probe-launch.log").write_bytes(output)
        wait_race = result.returncode == 1 and not result.stdout and result.stderr.strip() == (
            b"Unable to block on applications (initial call to kevent() failed: No such process)")
        if result.returncode and not wait_race:
            raise RuntimeError(f"probe-launch exited {result.returncode}: {output.decode(errors='replace')[-4000:]}")
        if not report.is_file():
            raise RuntimeError(f"native probe report missing: {report}")
        text = report.read_text()
        lines = text.splitlines()
        verdicts = [line for line in lines if line.startswith("RESULT ")]
        passed = "RESULT fonts=4/4 cairo=PASS verdict=PASS"
        if verdicts != [passed] or lines[-1] != passed or not text.endswith("\n"):
            raise RuntimeError("native registration or Cairo gradient report has no unique final passing verdict: " + text)
        if wait_race:
            # open can lose its kevent target after the short-lived probe has flushed a complete verdict and exited.
            print("probe-launch: completed report confirms success after open -W exit-before-wait race", file=sys.stderr)
    except Exception:
        print(f"[package-check] finish probe-launch result=FAIL timeout={timeout}s", file=sys.stderr, flush=True)
        raise
    print(f"[package-check] finish probe-launch result=PASS timeout={timeout}s", file=sys.stderr, flush=True)


def validate(app: Path, state: Path, dmg: Path | None, max_minimum: str) -> None:
    executable = app / "Contents/MacOS/sonicterm-mac"
    run([sys.executable, str(ROOT / "scripts/macos-bundle.py"), "verify", str(app), "--max-minimum-macos", max_minimum], state, "closure")
    run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], state, "signature")
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    if info.get("ATSApplicationFontsPath") != "assets/fonts":
        raise RuntimeError("native registration does not use the canonical font directory")
    fonts = app / "Contents/Resources/assets/fonts"
    expected = {fonts / f"RecMonoSt.Helens-{face}.ttf" for face in FACES}
    if set(app.rglob("*.ttf")) != expected:
        raise RuntimeError("package must contain exactly one copy of each required font")
    for font in expected:
        if font.read_bytes() != (ROOT / "assets/fonts" / font.name).read_bytes():
            raise RuntimeError(f"packaged font changed: {font.name}")
    # A denied read must fail before trusting a sandboxed successful launch as isolation evidence.
    protected = [str(path) for path in (Path("/opt/homebrew"), Path("/usr/local")) if path.is_dir()]
    if not protected:
        raise RuntimeError("Homebrew-denial control requires a present package-manager prefix")
    for index, prefix in enumerate(protected):
        control = RUNNER.run_command(["/bin/ls", prefix], ROOT, capped_timeout(f"sandbox-control-{index}", 10),
                                     clean_environment())
        denied = RUNNER.run_command(["/usr/bin/sandbox-exec", "-p", DENY_BREW, "/bin/ls", prefix],
                                    ROOT, capped_timeout(f"sandbox-canary-{index}", 10), clean_environment())
        (state / f"sandbox-canary-{index}.log").write_bytes(denied.stdout + denied.stderr)
        if control.returncode != 0 or denied.returncode == 0:
            raise RuntimeError(f"Homebrew-denial control did not distinguish access to {prefix}")
    environment = RUNNER.smoke_environment(state / "runtime", clean_environment())
    run(["/usr/bin/sandbox-exec", "-p", DENY_BREW, str(executable), "--runtime-smoke"],
        state, "isolated-runtime", 45, environment)

    # LaunchServices registration is tested in a copy; the shipping seal stays untouched.
    probe = state / "FontRegistration.app"
    shutil.copytree(app, probe, symlinks=True)
    probe_bin = probe / "Contents/MacOS/package-probe"
    run(["/usr/bin/xcrun", "clang", "-fobjc-arc", "-Wall", "-Wextra", "-Werror",
         "-framework", "AppKit", "-framework", "CoreText",
         str(ROOT / "scripts/macos-package-probe.m"), "-o", str(probe_bin)], state, "probe-build")
    info["CFBundleExecutable"] = "package-probe"
    info["CFBundleIdentifier"] = "org.sonicterm.packageprobe." + uuid.uuid4().hex
    info["LSUIElement"] = True
    info["ProbeExpectedFontsPath"] = "assets/fonts"
    info["ProbeFonts"] = [{"File": f"RecMonoSt.Helens-{face}.ttf", "PostScriptName": name}
                          for face, name in FACES.items()]
    (probe / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
    run(["/usr/bin/codesign", "--force", "--sign", "-", str(probe)], state, "probe-sign")
    cairo = probe / "Contents/Frameworks/libcairo.2.dylib"
    run(["/usr/bin/sandbox-exec", "-p", DENY_BREW, str(probe_bin), "--cairo-only", str(cairo)],
        state, "isolated-cairo", 20)
    run_font_probe(probe, state, cairo)
    result = {"app_bytes": size(app), "font_bytes": sum(p.stat().st_size for p in expected),
              "font_files": 4, "framework_bytes": size(app / "Contents/Frameworks"),
              "dmg_bytes": dmg.stat().st_size if dmg else None,
              "architecture": run(["/usr/bin/lipo", "-archs", str(executable)], state, "architecture").decode().strip(),
              "runtime_homebrew_denied": True, "cairo_homebrew_denied": True,
              "minimum_macos": info["LSMinimumSystemVersion"],
              "native_fonts": "4/4", "cairo_gradient": "PASS", "colr_glyph": "not exercised by probe"}
    result.update(measure_font_savings(app, state))
    (state / "package-evidence.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


def main() -> None:
    global DEADLINE
    # The budget counts from entry, so every later command, including a retry, shares one bound.
    DEADLINE = clock() + STEP_BUDGET_SECONDS
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--app", type=Path)
    source.add_argument("--dmg", type=Path)
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--max-minimum-macos", default="14.0")
    args = parser.parse_args()
    state = args.state_dir.resolve()
    state.mkdir(parents=True, exist_ok=False)
    attachment = DmgAttachment(args.dmg, state) if args.dmg else None
    original = None
    try:
        if attachment:
            mount = attachment.attach()
            candidates = list(mount.glob("*.app"))
            if len(candidates) != 1:
                raise RuntimeError("DMG must contain exactly one application")
            app = state / "installed/SonicTerm.app"
            shutil.copytree(candidates[0], app, symlinks=True)
        else:
            app = args.app.resolve()
        validate(app, state, args.dmg, args.max_minimum_macos)
    except BaseException as error:
        original = error
        raise
    finally:
        if attachment:
            attachment.finish(original)


if __name__ == "__main__":
    main()
