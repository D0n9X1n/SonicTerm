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
        # Apple Silicon installs the pinned macOS 14 bottles, since the runner's current Cairo bottles
        # declare a newer floor than the package's 14.0; the Intel job keeps Homebrew's Cairo.
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
                self.assertIn("brew install create-dmg imagemagick", commands)
                self.assertIn("bash scripts/bake-icons.sh", commands)
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
        # Apple Silicon ships a 14.0 floor, Intel 15.0, matching each runner.
        # macos-core packages the Apple Silicon bundle with a literal ceiling;
        # macos-smoke's Intel leg reads its ceiling from the matrix.
        core = workflow_test_fixture.job_block("ci.yml", "macos-core")
        self.assertIn('SONICTERM_MAX_MACOS_MINIMUM: "14.0"', core)
        self.assertIn('--max-minimum-macos "14.0"', workflow_test_fixture.shell_commands(core))
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
            ("build-mac-aarch64", "14.0"),
            ("build-mac-x86_64", "15.0"),
        ):
            with self.subTest(job=job_name):
                job = workflow_test_fixture.job_block("release.yml", job_name)
                self.assertIn(f'SONICTERM_MAX_MACOS_MINIMUM: "{ceiling}"', job)
                self.assertIn(
                    f'--max-minimum-macos "{ceiling}"', workflow_test_fixture.shell_commands(job)
                )
        # The Apple Silicon release links the pinned macOS 14 bottles, installed before it builds, so
        # its 14.0 ceiling holds on a macOS 15 runner.
        release = workflow_test_fixture.job_block("release.yml", "build-mac-aarch64")
        self.assertIn("python3 scripts/setup-macos-native.py", release)
        self.assertNotIn("brew install cairo", release)
        self.assertLess(release.index("setup-macos-native.py"), release.index("cargo build --release"))
        # The pins live in a private prefix, so the setup never runs Homebrew, and packaging tools
        # Homebrew installs afterwards cannot replace a pinned library.
        setup_source = (workflow_test_fixture._HERE / "setup-macos-native.py").read_text(encoding="utf-8")
        self.assertNotIn('"brew"', setup_source)

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
