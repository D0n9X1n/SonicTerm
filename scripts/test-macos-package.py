#!/usr/bin/env python3
"""Exercise a relocated macOS package without reading Homebrew runtime libraries."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("smoke_runner", ROOT / "scripts/native-smoke-runner.py")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)
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
# Held back for the final unmount: its 60 s timeout plus the runner's 10 s post-kill wait.
CLEANUP_RESERVE_SECONDS = 75
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
    limit = DEADLINE if cleanup else DEADLINE - CLEANUP_RESERVE_SECONDS
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


def size(path: Path) -> int:
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file() and not p.is_symlink())


def retry_fits() -> bool:
    """Whether a busy retry would still keep its minimum time before the unmount reserve after the wait."""
    if DEADLINE is None:
        return True
    remaining = DEADLINE - CLEANUP_RESERVE_SECONDS - clock() - BUSY_RETRY_WAIT_SECONDS
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
    mount = state / "mounted"
    try:
        if args.dmg:
            mount.mkdir()
            run(["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", str(mount),
                 str(args.dmg.resolve())], state, "mount")
            candidates = list(mount.glob("*.app"))
            if len(candidates) != 1:
                raise RuntimeError("DMG must contain exactly one application")
            app = state / "installed/SonicTerm.app"
            shutil.copytree(candidates[0], app, symlinks=True)
        else:
            app = args.app.resolve()
        validate(app, state, args.dmg, args.max_minimum_macos)
    finally:
        if mount.is_mount():
            run(["/usr/bin/hdiutil", "detach", str(mount)], state, "unmount", cleanup=True)


if __name__ == "__main__":
    main()
