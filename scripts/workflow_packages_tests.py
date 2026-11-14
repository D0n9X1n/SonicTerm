"""Workflow packages tests."""

from __future__ import annotations
from pathlib import Path
import re
import subprocess
import sys
import unittest

import workflow_test_fixture


class MacPackageGateTests(unittest.TestCase):
    """The macOS shipping artifact is the .dmg, so every gate must exercise that file."""

    def test_ci_smoke_covers_both_mac_architectures_with_isolated_caches(self):
        # One architecture's bundle cannot stand in for the other's: Intel and
        # Apple Silicon resolve different Cairo and font binaries. Apple Silicon
        # runs in macos-core on macos-15 and Intel in macos-smoke's one leg, and
        # their distinct cache keys keep either from restoring the other's target.
        job = workflow_test_fixture.job_block("ci.yml", "macos-smoke")
        self.assertIn("name: macOS native runtime smoke (${{ matrix.arch }})", job)
        self.assertIn("runs-on: ${{ matrix.runner }}", job)
        self.assertIn("fail-fast: false", job)
        self.assertEqual(re.findall(r"(?m)^          - runner: (\S+)$", job), ["macos-15-intel"])
        self.assertIn("- runner: macos-15-intel\n            arch: x86_64", job)
        self.assertIn(
            "shared-key: ${{ env.CI_CACHE_NAMESPACE }}-unit-${{ matrix.runner }}", job
        )
        self.assertNotIn("shared-key: ${{ env.CI_CACHE_NAMESPACE }}-unit-macos-15", job)
        core = workflow_test_fixture.job_block("ci.yml", "macos-core")
        self.assertIn("runs-on: macos-15", core)
        self.assertIn("shared-key: ${{ env.CI_CACHE_NAMESPACE }}-unit-macos-15", core)
        # Each job's smoke evidence names its architecture, so the two jobs'
        # artifacts never collide and each says which bundle it describes.
        for job_name, arch in (("macos-core", "aarch64"), ("macos-smoke", "${{ matrix.arch }}")):
            names = [
                name
                for name in re.findall(r"(?m)^          name: (.+)$", workflow_test_fixture.job_block("ci.yml", job_name))
                if name.startswith("sonicterm-macos-")
            ]
            self.assertEqual(len(names), 4, f"smoke artifact names in {job_name}")
            for name in names:
                with self.subTest(job=job_name, artifact=name):
                    self.assertIn(f"-{arch}-", name)

    def test_ci_smoke_validates_the_packaged_dmg_after_the_raw_binary_smoke(self):
        # The raw smoke proves the build tree runs where Homebrew's Cairo is on
        # the loader path; only the relocated, signed .dmg proves what ships.
        # Each architecture's job packages and validates its own bundle.
        # Apple Silicon installs the pinned macOS 14 bottles, which stay within the package's 15.0
        # floor whatever Homebrew's current bottles declare; the Intel job keeps Homebrew's Cairo.
        installs = {"macos-core": "python3 scripts/setup-macos-native.py", "macos-smoke": "brew install cairo pkg-config"}
        for job_name, arch in (("macos-core", "aarch64"), ("macos-smoke", "${{ matrix.arch }}")):
            with self.subTest(job=job_name):
                job = workflow_test_fixture.job_block("ci.yml", job_name)
                self.assertIn(installs[job_name], job)
                self.assertLess(job.index(installs[job_name]), job.index("cargo build --release -p sonicterm-mac"))
                self.assertIn("-- target/release/sonicterm-mac --runtime-smoke", job)
                self.assertIn(
                    f"bash scripts/make-macos-dmg.sh target/release/sonicterm-mac ci mac-{arch}",
                    job,
                )
                self.assertIn(f'--dmg "dist/SonicTerm-ci-mac-{arch}.dmg"', job)
                self.assertIn(
                    f'--state-dir "$RUNNER_TEMP/sonicterm-macos-package-{arch}"', job
                )
                self.assertLess(job.index("--runtime-smoke"), job.index("make-macos-dmg.sh"))
                self.assertLess(job.index("make-macos-dmg.sh"), job.index("test-macos-package.py"))

    def test_ci_package_validation_has_no_timeout_override(self):
        # The validator retains its own process deadlines without a CI step override, in both macOS jobs.
        for job_name in ("macos-core", "macos-smoke"):
            with self.subTest(job=job_name):
                job = workflow_test_fixture.job_block("ci.yml", job_name)
                step = job.split("- name: Validate packaged macOS dmg", 1)[1]
                step = re.split(r"(?m)^      - ", step, maxsplit=1)[0]
                self.assertNotIn("timeout-minutes:", step)

    def test_release_builds_and_validates_each_dmg_on_its_own_architecture(self):
        # Packaging Intel bytes on an Apple Silicon host cannot run the bundle
        # it produced, so the load closure it signs off on is never executed.
        contracts = (
            ("build-mac-aarch64", "aarch64", "arm64", "aarch64-apple-darwin"),
            ("build-mac-x86_64", "x86_64", "x86_64", "x86_64-apple-darwin"),
        )
        for job_name, arch, lipo_arch, target in contracts:
            with self.subTest(job=job_name):
                job = workflow_test_fixture.job_block("release.yml", job_name)
                commands = workflow_test_fixture.shell_commands(job)
                self.assertIn("brew install create-dmg", commands)
                # The DMG ships the committed icon exports, not a runner-local bake.
                self.assertNotIn("bake-icons.sh", commands)
                self.assertNotIn("imagemagick", commands)
                binary = f"target/{target}/release/sonicterm-mac"
                self.assertIn(f'test "$(lipo -archs {binary})" = "{lipo_arch}"', commands)
                self.assertIn(
                    f'bash scripts/make-macos-dmg.sh {binary} '
                    f'"${{{{ github.ref_name }}}}" mac-{arch}',
                    commands,
                )
                dmg = f"dist/SonicTerm-${{{{ github.ref_name }}}}-mac-{arch}.dmg"
                self.assertIn(f'--dmg "{dmg}"', commands)
                self.assertIn(f"--arch {arch}", commands)
                self.assertIn(f"--output dist/macos-{arch}-dmg.asset.json", commands)
                # Named outside `release-assets-*` on purpose: `publish` globs
                # that prefix with merge-multiple, and a per-arch upload sharing
                # it would land the same filenames twice in one directory.
                self.assertIn(f"name: macos-packaged-{arch}", job)
                self.assertNotIn(f"name: release-assets-macos-{arch}", job)
                self.assertLess(job.index("--runtime-smoke"), job.index("make-macos-dmg.sh"))

    def test_release_package_mac_consolidates_prebuilt_dmgs_without_repackaging(self):
        # The aggregator keeps its job id so `publish` needs no change, but it
        # must no longer package: re-running the packager here would rebuild the
        # Intel bundle on an Apple Silicon host and discard the tested bytes.
        job = workflow_test_fixture.job_block("release.yml", "package-mac")
        self.assertIn("needs: [build-mac-x86_64, build-mac-aarch64]", job)
        for arch in ("aarch64", "x86_64"):
            with self.subTest(arch=arch):
                self.assertIn(f"name: macos-packaged-{arch}", job)
                self.assertIn(
                    f"dist/SonicTerm-${{{{ github.ref_name }}}}-mac-{arch}.dmg", job
                )
                self.assertIn(f"dist/macos-{arch}-dmg.asset.json", job)
        self.assertIn("name: release-assets-macos\n", job)
        for removed in (
            "brew install",
            "make-macos-dmg.sh",
            "bake-icons.sh",
            "sonicterm-mac-x86_64",
            "sonicterm-mac-aarch64",
        ):
            with self.subTest(removed=removed):
                self.assertNotIn(removed, job)

    def test_publish_glob_collects_one_upload_per_platform(self):
        # `publish` downloads `release-assets-*` with merge-multiple into one
        # directory. An intermediate upload matching that prefix would deliver
        # the same filenames twice, so which bytes land becomes order-dependent.
        text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        uploads = re.findall(r"(?m)^          name: (release-assets-\S*)$", text)
        self.assertEqual(
            sorted(uploads),
            ["release-assets-linux", "release-assets-macos", "release-assets-windows"],
        )
        publish = workflow_test_fixture.job_block("release.yml", "publish")
        self.assertIn("pattern: release-assets-*", publish)
        self.assertIn("merge-multiple: true", publish)
        for intermediate in ("macos-packaged-aarch64", "macos-packaged-x86_64"):
            with self.subTest(artifact=intermediate):
                self.assertFalse(intermediate.startswith("release-assets-"))
                self.assertIn(f"name: {intermediate}", text)

    def test_every_mac_package_gate_pins_an_explicit_deployment_ceiling(self):
        # The floor is policy, not a reading. Asserting it as an explicit
        # ceiling on both the packager env and the validator means a newer SDK
        # or Homebrew bottle that raises LC_BUILD_VERSION fails the gate rather
        # than silently shipping a build that excludes supported hosts.
        # Both architectures ship a 15.0 floor, the deployment target FreeType's weak HVF load needs.
        # macos-core packages the Apple Silicon bundle with a literal ceiling;
        # macos-smoke's Intel leg reads its ceiling from the matrix.
        core = workflow_test_fixture.job_block("ci.yml", "macos-core")
        self.assertIn('SONICTERM_MAX_MACOS_MINIMUM: "15.0"', core)
        self.assertNotIn('"14.0"', core)
        self.assertIn('--max-minimum-macos "15.0"', workflow_test_fixture.shell_commands(core))
        smoke = workflow_test_fixture.job_block("ci.yml", "macos-smoke")
        self.assertIn('minimum: "15.0"', smoke)
        self.assertNotIn('minimum: "14.0"', smoke)
        self.assertIn(
            "SONICTERM_MAX_MACOS_MINIMUM: ${{ matrix.minimum }}", smoke
        )
        self.assertIn(
            '--max-minimum-macos "${{ matrix.minimum }}"', workflow_test_fixture.shell_commands(smoke)
        )

        for job_name, ceiling in (
            ("build-mac-aarch64", "15.0"),
            ("build-mac-x86_64", "15.0"),
        ):
            with self.subTest(job=job_name):
                job = workflow_test_fixture.job_block("release.yml", job_name)
                self.assertIn(f'SONICTERM_MAX_MACOS_MINIMUM: "{ceiling}"', job)
                self.assertIn(
                    f'--max-minimum-macos "{ceiling}"', workflow_test_fixture.shell_commands(job)
                )
        # The Apple Silicon release links the pinned macOS 14 bottles, installed before it builds, so
        # its 15.0 ceiling holds whatever floor Homebrew's current bottles declare.
        release = workflow_test_fixture.job_block("release.yml", "build-mac-aarch64")
        self.assertIn("python3 scripts/setup-macos-native.py", release)
        self.assertNotIn("brew install cairo", release)
        self.assertLess(release.index("setup-macos-native.py"), release.index("cargo build --release"))
        # The pins live in a private prefix, so the setup never runs Homebrew, and packaging tools
        # Homebrew installs afterwards cannot replace a pinned library.
        setup_source = (workflow_test_fixture._HERE / "setup-macos-native.py").read_text(encoding="utf-8")
        self.assertNotIn('"brew"', setup_source)

    def test_every_mac_build_requires_hvf_and_ships_a_weak_libhvf_load(self):
        # A runner whose SDK lacks hvf/Scaler.h must fail rather than ship a FreeType that cannot open
        # PingFang, and each packaged executable must load libhvf weakly so macOS 15.0-15.3 still launch.
        gates = (
            ("ci.yml", "macos-core", "dist/SonicTerm.app/Contents/MacOS/sonicterm-mac"),
            ("ci.yml", "macos-smoke", "dist/SonicTerm.app/Contents/MacOS/sonicterm-mac"),
            ("release.yml", "build-mac-aarch64", "dist/SonicTerm.app/Contents/MacOS/sonicterm-mac"),
            ("release.yml", "build-mac-x86_64", "dist/SonicTerm.app/Contents/MacOS/sonicterm-mac"),
        )
        for workflow_name, job_name, executable in gates:
            with self.subTest(workflow=workflow_name, job=job_name):
                job = workflow_test_fixture.job_block(workflow_name, job_name)
                header = job.split("    steps:\n", 1)[0]
                self.assertIn('    env:\n', header)
                self.assertIn('      SONICTERM_REQUIRE_HVF: "1"\n', header)
                commands = workflow_test_fixture.shell_commands(job)
                check = f"python3 scripts/check-macos-weak-hvf.py {executable}"
                self.assertIn(check, commands)
                self.assertLess(commands.index("make-macos-dmg.sh"), commands.index(check))

    def test_every_shipping_mac_gate_checks_the_package_not_only_the_binary(self):
        # The regression this pins: a green `--runtime-smoke` on the build tree
        # said nothing about the bundle users open, which is where the missing
        # self-contained Cairo and font payload actually failed.
        gates = (
            ("ci.yml", "macos-core"),
            ("ci.yml", "macos-smoke"),
            ("release.yml", "build-mac-aarch64"),
            ("release.yml", "build-mac-x86_64"),
        )
        for workflow_name, job_name in gates:
            with self.subTest(workflow=workflow_name, job=job_name):
                job = workflow_test_fixture.job_block(workflow_name, job_name)
                self.assertIn("--runtime-smoke", job)
                self.assertIn("python3 scripts/test-macos-package.py", job)
                self.assertIn("--dmg ", job)
                self.assertIn("--state-dir ", job)


_ICONS = Path(__file__).resolve().parent.parent / "assets" / "icons"
_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def png_header(path: Path) -> tuple[int, int, int]:
    """Width, height and colour type from a PNG's IHDR chunk."""
    data = path.read_bytes()[:26]
    if data[:8] != _PNG_SIGNATURE or data[12:16] != b"IHDR":
        raise AssertionError(f"{path} is not a PNG")
    return int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big"), data[25]


class IconExportTests(unittest.TestCase):
    """Packaging reads the committed icon exports as they are, so each must exist at its size."""

    def test_every_png_is_square_at_its_named_size_with_alpha(self):
        # The Linux hicolor install, the runtime window icon and the .ico all pick files by name.
        sizes_px = (16, 22, 24, 32, 48, 64, 128, 256, 512, 1024)
        retina_pt = (16, 32, 64, 128, 256, 512)
        expected = {f"sonic-{size}.png": size for size in sizes_px}
        expected.update({f"sonic-{size}@2x.png": size * 2 for size in retina_pt})
        present = {path.name for path in (_ICONS / "exports" / "png").glob("*.png")}
        self.assertEqual(present, set(expected))
        for name, size_px in expected.items():
            with self.subTest(png=name):
                width_px, height_px, colour_type = png_header(_ICONS / "exports" / "png" / name)
                self.assertEqual((width_px, height_px), (size_px, size_px))
                # Colour type 6 is RGBA: the squircle's corners must stay transparent.
                self.assertEqual(colour_type, 6)

    def test_the_ico_holds_every_windows_scale_step(self):
        # Windows looks for an exact size per DPI scale, else shrinks a larger entry and draws it soft.
        data = (_ICONS / "exports" / "sonic.ico").read_bytes()
        self.assertEqual(data[:4], b"\x00\x00\x01\x00")
        count = int.from_bytes(data[4:6], "little")
        # A directory entry stores 0 for a 256 px side.
        sizes_px = sorted((data[6 + 16 * index] or 256) for index in range(count))
        self.assertEqual(sizes_px, [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256])

    def test_the_icns_holds_every_iconset_slot(self):
        # iconutil's ten slots, 16 pt to 512 pt at 1x and 2x; it stores 16 and 32 px as ic04 and ic05.
        data = (_ICONS / "exports" / "sonic.icns").read_bytes()
        self.assertEqual(data[:4], b"icns")
        self.assertEqual(int.from_bytes(data[4:8], "big"), len(data))
        types, offset = set(), 8
        while offset < len(data):
            types.add(data[offset:offset + 4].decode("ascii"))
            offset += int.from_bytes(data[offset + 4:offset + 8], "big")
        self.assertLessEqual({"ic04", "ic05", "ic07", "ic08", "ic09", "ic10", "ic11", "ic12", "ic13", "ic14"},
                             types)

    def test_the_scalable_export_is_the_master(self):
        # Linux installs the exported SVG as the scalable icon, so it must not drift from the master.
        self.assertEqual((_ICONS / "exports" / "sonic.svg").read_bytes(),
                         (_ICONS / "source" / "sonic.svg").read_bytes())
        self.assertFalse((_ICONS / "source" / "sonic.png").exists(), "the vector is the only master")


class CommandLineTests(unittest.TestCase):
    """The gate is invoked as a process, so its exit codes are part of the contract."""

    def run_checker(self, root: Path) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(workflow_test_fixture._CHECKER_PATH), "--root", str(root)],
            capture_output=True,
            check=False,
        )

    def test_compliant_tree_exits_zero(self):
        with workflow_test_fixture.repository({"ci.yml": workflow_test_fixture.compliant()}) as root:
            completed = self.run_checker(root)
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())

    def test_violating_tree_exits_nonzero_and_names_the_file(self):
        with workflow_test_fixture.repository({"ci.yml": workflow_test_fixture.compliant(uses="actions/checkout@v7")}) as root:
            completed = self.run_checker(root)
        self.assertEqual(completed.returncode, 1)
        self.assertIn("ci.yml", completed.stderr.decode())
