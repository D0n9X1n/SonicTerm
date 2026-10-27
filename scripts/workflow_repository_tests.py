"""Workflow repository tests."""

from __future__ import annotations
import re
import shutil
import subprocess
import unittest

import workflow_test_fixture


class RepositoryTests(unittest.TestCase):
    """The rules are asserted against the workflows this repository ships."""

    def test_repository_workflows_satisfy_the_contract(self):
        self.assertEqual(workflow_test_fixture.checker.check(workflow_test_fixture._HERE.parent), [])

    def test_pty_close_baseline_follows_cargo_restore_on_every_desktop(self):
        # Capture the before/after measurement before later gates, including compilation in the same step.
        command = "cargo test -p sonicterm-app --lib pty_close_baseline -- --ignored --nocapture"
        for job in ("macos-core", "windows-tests-runtime", "linux-core"):
            with self.subTest(job=job):
                block = workflow_test_fixture.job_block("ci.yml", job)
                steps = re.split(r"(?m)^      - ", block)[1:]
                restore = next(index for index, step in enumerate(steps)
                               if step.startswith("name: Restore Cargo dependencies\n"))
                baseline = steps[restore + 1]
                self.assertTrue(baseline.startswith("name: Measure PTY close baseline\n"))
                self.assertNotIn("timeout-minutes:", baseline)
                self.assertIn(f"run: {command}", baseline)
                self.assertEqual(block.count(command), 1)

    def test_every_repository_workflow_is_scanned(self):
        # Pins the discovery itself: were the glob to miss a workflow, every
        # rule above would still pass while that file went unchecked.
        found = {path.name for path in workflow_test_fixture.checker.workflow_paths(workflow_test_fixture._HERE.parent)}
        self.assertEqual(found, {"ci.yml", "perf.yml", "publish-wiki.yml", "release.yml"})

    def test_every_repository_uses_is_pinned_to_a_sha(self):
        # Independent of the checker's own parser: re-derives the refs with a
        # separate scan, so a parser bug cannot make this assertion vacuous.
        unpinned = []
        for path in workflow_test_fixture.checker.workflow_paths(workflow_test_fixture._HERE.parent):
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                stripped = line.strip()
                if not stripped.startswith(("uses:", "- uses:")):
                    continue
                ref = stripped.split("uses:", 1)[1].split("#")[0].strip()
                revision = ref.rpartition("@")[2]
                if not workflow_test_fixture.checker.SHA_PIN.match(revision):
                    unpinned.append(f"{path.name}:{number}: {ref}")
        self.assertEqual(unpinned, [])

    def test_write_boundary_names_only_existing_publish_jobs(self):
        # A boundary entry for a renamed or deleted job silently permits
        # nothing, and hides that the real job now runs unchecked.
        for name, job in workflow_test_fixture.checker.WRITE_BOUNDARY:
            with self.subTest(workflow=name, job=job):
                path = workflow_test_fixture._HERE.parent / ".github" / "workflows" / name
                self.assertTrue(path.exists(), f"{name} does not exist")
                self.assertIn(f"\n  {job}:\n", path.read_text(encoding="utf-8"))

    def test_main_push_runs_are_unique_and_only_pr_updates_cancel(self):
        text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn(
            "group: ${{ github.workflow }}-${{ github.event_name }}-"
            "${{ github.event_name == 'pull_request' && github.ref || github.sha }}",
            text,
        )
        self.assertIn(
            "cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
            text,
        )

    def test_ci_aggregates_fail_closed_over_every_platform_shard(self):
        text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        contracts = {
            "macos": (
                "macos-14 / unit tests",
                ("macos-core", "macos-smoke", "tooling"),
            ),
            "windows": (
                "windows-latest / unit tests",
                ("windows-native", "windows-glyph-oracle", "windows-checks", *workflow_test_fixture.WINDOWS_TEST_SHARDS, "windows-smoke"),
            ),
            "linux": (
                "ubuntu 22.04 / workspace, packages, X11, Wayland",
                ("linux-core", "linux-coverage", "linux-packages"),
            ),
        }

        def assert_contract(job: str, block: str, name: str, shards: tuple[str, ...]):
            self.assertIn(f"name: {name}", block)
            self.assertIn(f"needs: [{', '.join(shards)}]", block)
            self.assertIn("if: always()", block)
            loop = re.search(r"(?m)^          for result in (.*); do$", block)
            self.assertIsNotNone(loop)
            for shard in shards:
                # Each shard's result is bound to a variable, and the loop tests that variable.
                variable = re.search(rf"(?m)^          ([A-Z_]+): \$\{{{{ needs\.{re.escape(shard)}\.result \}}}}$",
                                     block)
                self.assertIsNotNone(variable, shard)
                self.assertIn(f'"${variable[1]}"', loop[1].split(), shard)
            self.assertIn('test "$result" = "success"', block)
            # The one verification step must run, fail on any non-success, and stay mandatory.
            steps = re.split(r"(?m)^      - ", block.split("    steps:\n", 1)[1])[1:]
            self.assertEqual(len(steps), 1)
            self.assertNotRegex(steps[0], r"(?m)^        (?:if|continue-on-error):")
            self.assertNotRegex(block, r"(?m)^    continue-on-error:")
            self.assertIn("\n        shell: bash\n", "\n" + steps[0])
            # The step has one block `run:` key whose script is exactly the loop, line for line, so a comment, an
            # early exit or a second loop cannot hide a fail-open script; it tests exactly the bound shard results.
            self.assertEqual(re.findall(r"(?m)^        run:(.*)$", steps[0]), [" |"])
            script = [line[10:] for line in steps[0].split("\n        run: |\n", 1)[1].split("\n") if line.strip()]
            self.assertEqual(script[1:], ['  test "$result" = "success"', "done"])
            header = re.fullmatch(r'for result in ((?:"\$[A-Z_]+")(?: "\$[A-Z_]+")*); do', script[0])
            self.assertIsNotNone(header)
            bindings = re.findall(r"(?m)^          ([A-Z_]+): \$\{\{ needs\.[A-Za-z0-9_-]+\.result \}\}$", steps[0])
            self.assertEqual(sorted(operand[2:-1] for operand in header[1].split(" ")), sorted(bindings))

        for job, (name, shards) in contracts.items():
            with self.subTest(job=job):
                block = text.split(f"  {job}:\n", 1)[1]
                block = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", block, maxsplit=1)[0]
                assert_contract(job, block, name, shards)
                with self.assertRaises(AssertionError):
                    assert_contract(job, block.replace("if: always()", "if: success()"), name, shards)
                # Dropping one operand from the result loop leaves a shard unchecked even though it is needed.
                operand = re.search(r'for result in ("\$[A-Z_]+") ', block)[1]
                with self.assertRaises(AssertionError):
                    assert_contract(job, block.replace(operand + " ", "", 1), name, shards)
                # A skipped, advisory or bypassed verification step would let a failed shard pass.
                verify = re.search(r"(?m)^      - name: .+\n        shell: bash\n", block)[0]
                for bypass in (verify + "        if: false\n", verify + "        continue-on-error: true\n"):
                    with self.assertRaises(AssertionError):
                        assert_contract(job, block.replace(verify, bypass, 1), name, shards)
                with self.assertRaises(AssertionError):
                    assert_contract(job, block.replace('= "success"', '= "success" || true', 1), name, shards)
                # A header that consumes the real results in a no-op loop and then tests a literal is fail-open.
                header = re.search(r"(?m)^          for result in (.+); do$", block)
                compound = header[0].replace("; do", ' ; do :; done; for result in "success"; do', 1)
                with self.assertRaises(AssertionError):
                    assert_contract(job, block.replace(header[0], compound, 1), name, shards)
                # An early exit behind a commented copy of the run key never reaches the loop.
                decoy = "          exit 0\n          #        run: |\n" + header[0]
                with self.assertRaises(AssertionError):
                    assert_contract(job, block.replace(header[0], decoy, 1), name, shards)
                with self.assertRaises(AssertionError):
                    assert_contract(
                        job,
                        block.replace(f"${{{{ needs.{shards[0]}.result }}}}", "missing", 1),
                        name,
                        shards,
                    )

    def test_frame_validation_runs_after_default_with_own_deadline_and_state(self):
        # Every native smoke shard and all three release jobs need a fresh process and isolated evidence.
        for workflow, job_name, platform in (
            ("ci.yml", "macos-core", "macOS"), ("ci.yml", "macos-smoke", "macOS"),
            ("ci.yml", "windows-smoke", "Windows"),
            ("release.yml", "build-mac-x86_64", "macOS"),
            ("release.yml", "build-mac-aarch64", "macOS"),
            ("release.yml", "build-windows", "Windows"),
        ):
            with self.subTest(workflow=workflow, job=job_name):
                job = workflow_test_fixture.job_block(workflow, job_name)
                default = f"Require {platform} native runtime smoke"
                scenario = f"Require {platform} GPU frame-validation smoke"
                self.assertLess(job.index(default), job.index(scenario))
                block = job.split(f"- name: {scenario}\n", 1)[1].split("\n      - name:", 1)[0]
                self.assertNotIn("timeout-minutes:", block)
                self.assertIn("--timeout-seconds 45 --scenario frame-validation", block)
                self.assertIn("frame-validation-smoke", block)
                self.assertIn("--state-dir", block)
                self.assertIn("--log-file", block)
                self.assertIn("--runtime-smoke", block)

    def test_windows_native_smokes_use_explicit_relative_executable_paths(self):
        # The ./ prefix makes each checkout-relative binary unambiguous to Windows CreateProcess.
        contracts = (
            ("ci.yml", "./target/release/sonicterm-windows.exe"),
            (
                "release.yml",
                "./target/x86_64-pc-windows-msvc/release/sonicterm-windows.exe",
            ),
        )
        for workflow_name, executable in contracts:
            with self.subTest(workflow=workflow_name):
                text = (
                    workflow_test_fixture._HERE.parent / ".github" / "workflows" / workflow_name
                ).read_text(encoding="utf-8")
                self.assertIn(f"-- {executable} --runtime-smoke", text)

    def test_windows_shards_keep_every_test_job_step_exactly_once(self):
        # The split must not drop, duplicate, edit, move or reorder any step of the former single Windows test job,
        # including its uploads beside the steps that produce their files, nor any shard's setup.
        assigned = [name for names in workflow_test_fixture.WINDOWS_SHARD_WORK.values() for name in names]
        self.assertEqual(sorted(workflow_test_fixture.windows_work_step(name) for name in assigned), sorted(workflow_test_fixture.WINDOWS_TEST_WORK))
        self.assertEqual(len(assigned), len(set(assigned)))
        workflow = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        self.assertEqual(workflow_test_fixture.windows_shard_problems(workflow), [])

        def in_shard(shard: str, old: str, new: str) -> str:
            """Mutate one shard's body only."""
            head, tail = workflow.split(f"  {shard}:\n", 1)
            body, rest = re.split(r"(?=\n  [a-z][a-z0-9_-]*:\n)", tail, maxsplit=1)
            self.assertEqual(body.count(old), 1, old)
            return head + f"  {shard}:\n" + body.replace(old, new, 1) + rest

        gdi_upload = workflow_test_fixture.windows_work_step("Upload Windows GDI probe log")
        smoke_upload = workflow_test_fixture.windows_work_step("Upload Windows perf scenario smoke evidence")
        build = workflow_test_fixture.windows_work_step("Build Windows perf scenario harness")
        smoke = workflow_test_fixture.windows_work_step("Require Windows perf scenario smoke")
        msi = workflow_test_fixture.windows_work_step("Test MSI validator")
        baseline = workflow_test_fixture.windows_work_step("Measure PTY close baseline")
        probe = workflow_test_fixture.windows_work_step("Report host window capability")
        checkout = workflow_test_fixture.WINDOWS_SHARD_SETUP[0]
        rust = workflow_test_fixture.WINDOWS_SHARD_SETUP[1]
        disguised = ("      - name: Install Rust evidence copy\n"
                     "        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n"
                     "        with:\n          name: copy\n          path: target\n")
        shared_key = "shared-key: ${{ env.CI_CACHE_NAMESPACE }}-test-windows-latest\n"
        restore_only = "save-if: false\n"
        relocated = in_shard("windows-tests-runtime", gdi_upload + "\n", "")
        relocated_head, relocated_tail = relocated.split("  windows-tests-workspace:\n", 1)
        workspace_tests = workflow_test_fixture.windows_work_step("Run workspace unit and integration tests")
        baseline_moved = in_shard("windows-tests-runtime", baseline + "\n", "")
        baseline_head, baseline_tail = baseline_moved.split("  windows-tests-workspace:\n", 1)
        mutations = {
            "dropped step": in_shard("windows-tests-runtime", gdi_upload, ""),
            "duplicated step": in_shard("windows-tests-runtime", build, build + "\n" + build),
            "lost upload condition": in_shard("windows-tests-runtime", smoke_upload,
                                              smoke_upload.replace("failure() || ", "", 1)),
            # Steps are separated by one blank line in the file; the inventory strips it.
            "reordered steps": in_shard("windows-tests-runtime", build + "\n" + smoke, smoke + "\n" + build),
            "upload moved away from its producer": relocated_head + "  windows-tests-workspace:\n"
                + relocated_tail.replace(msi, gdi_upload + "\n" + msi, 1),
            # The baseline is runtime's first work step, straight after the Cargo restore, so its build stays cold.
            "baseline missing": in_shard("windows-tests-runtime", baseline + "\n", ""),
            "baseline duplicated": in_shard("windows-tests-runtime", baseline, baseline + "\n" + baseline),
            "a probe built before the baseline": in_shard(
                "windows-tests-runtime", baseline + "\n" + probe, probe + "\n" + baseline),
            # A true relocation keeps the inventory exactly-once, so only the shard assignment can catch it.
            "baseline returned to the workspace shard": baseline_head + "  windows-tests-workspace:\n"
                + baseline_tail.replace(workspace_tests, baseline + "\n" + workspace_tests, 1),
            "work disguised as setup": in_shard("windows-tests-harness", rust, rust + "\n" + disguised),
            "missing checkout": in_shard("windows-tests-harness", checkout + "\n", ""),
            "altered setup": in_shard("windows-tests-harness", "toolchain: stable", "toolchain: nightly"),
            "advisory shard": in_shard("windows-tests-harness", "    runs-on: windows-latest\n",
                                       "    runs-on: windows-latest\n    continue-on-error: true\n"),
            # Each shard restores the main-written family and never writes, so no new family or writer appears.
            "an unintended new cache family": in_shard(
                "windows-tests-harness", shared_key, shared_key.replace("windows-latest", "windows-latest-harness")),
            "a test shard becoming a main writer": in_shard(
                "windows-tests-harness", restore_only,
                "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}\n"),
            "a pull-request writer": in_shard("windows-tests-harness", restore_only, "save-if: true\n"),
            "cache namespace removed": in_shard(
                "windows-tests-harness", shared_key, "shared-key: test-windows-latest\n"),
            # The action hashes the Rust toolchain and environment into the key by default; disabling it would let
            # a shard restore artifacts built under other compiler settings.
            "environment hashing disabled": in_shard(
                "windows-tests-harness", restore_only, restore_only + "          add-rust-environment-hash-key: false\n"),
        }
        for label, mutated in mutations.items():
            with self.subTest(mutation=label):
                self.assertTrue(mutated != workflow, "the mutation did not apply")
                self.assertTrue(workflow_test_fixture.windows_shard_problems(mutated))

    @unittest.skipUnless(shutil.which("actionlint"), "actionlint runs in the mandatory tooling job")
    def test_actionlint_rejects_job_level_runner_context(self):
        # The old source checker accepted a workflow GitHub could not schedule. Exercise the real
        # expression validator: workspace is valid at job scope, runner.temp is not.
        source = """name: fixture
on: push
jobs:
  example:
    runs-on: ubuntu-latest
    env:
      ORACLE: ${{ github.workspace }}/oracle
    steps:
      - run: true
"""
        for invalid in (False, True):
            text = source.replace("github.workspace", "runner.temp") if invalid else source
            completed = subprocess.run(["actionlint", "-shellcheck=", "-pyflakes=", "-"], input=text,
                                       capture_output=True, text=True, timeout=30)
            if invalid:
                self.assertNotEqual(completed.returncode, 0)
                self.assertIn('context "runner" is not allowed here', completed.stdout + completed.stderr)
            else:
                self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)

    def test_tooling_job_preserves_each_relocated_gate_and_requires_actionlint(self):
        # These eleven commands leave the macOS critical chain, not CI: the aggregate contract
        # above requires this job's success, and none of its commands is conditional or advisory.
        block = workflow_test_fixture.job_block("ci.yml", "tooling")
        self.assertIn("runs-on: ubuntu-latest", block)
        self.assertNotRegex(block, r"(?m)^\s+(?:if|continue-on-error):")
        for script in ("check-rust-version.sh", "check-no-raw-process-exit.sh", "check-window-owner-registration.sh",
                       "check-workflow-supply-chain.sh", "check-authored-rust-comments.sh", "check-script-identifiers.sh",
                       "test-release-notes.sh", "test-wiki-publish.sh", "test-resource-inventory.sh",
                       "test-soak-harness.sh", "test-resource-baseline-evidence.sh"):
            command = f"run: bash scripts/{script}"
            self.assertEqual(block.count(command), 1, script)
            self.assertNotIn(command, workflow_test_fixture.job_block("ci.yml", "macos-core"), script)
        self.assertIn("run: bash scripts/install-pinned-tool.sh actionlint", block)
        self.assertIn("run: actionlint -shellcheck= -pyflakes= .github/workflows/ci.yml .github/workflows/perf.yml", block)
        for job in ("windows-glyph-oracle", "windows-tests-workspace", "windows-tests-harness",
                    "windows-tests-harness-features", "windows-tests-harness-api"):
            header = workflow_test_fixture.job_block("ci.yml", job).split("    steps:", 1)[0]
            self.assertNotIn("runner.temp", header)
            self.assertIn("${{ github.workspace }}/ci-glyph-oracle", header)

    def test_ci_caches_are_bounded_and_cairo_is_published_immediately(self):
        text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("CI_CACHE_NAMESPACE: ci-v3", text)
        # Thirteen restores, six main-only writers plus Intel smoke; check/test/release/instrumented are distinct.
        self.assertEqual(text.count("uses: Swatinem/rust-cache@"), 13)
        self.assertEqual(text.count("shared-key: ${{ env.CI_CACHE_NAMESPACE }}-"), 13)
        self.assertEqual(text.count("add-job-id-key: false"), 13)
        self.assertEqual(text.count("cache-workspace-crates: false"), 13)
        self.assertEqual(
            text.count(
                "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
            ),
            6,
        )
        self.assertEqual(text.count("save-if: false"), 6)

        native = text.split("  windows-native:\n", 1)[1]
        native = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", native, maxsplit=1)[0]
        restore = native.index("uses: actions/cache/restore@")
        install = native.index("- name: Install Cairo for Windows")
        save = native.index("uses: actions/cache/save@")
        self.assertLess(restore, install)
        self.assertLess(install, save)
        self.assertIn("if: steps.vcpkg-cache.outputs.cache-hit != 'true'", native)
        self.assertIn("key: ${{ env.CI_CACHE_NAMESPACE }}-vcpkg-cairo-", native)

        release = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("CI_CACHE_NAMESPACE: ci-v3", release)
        self.assertIn("key: ${{ env.CI_CACHE_NAMESPACE }}-vcpkg-cairo-", release)

    def test_each_rust_cache_has_one_architecture_specific_main_writer(self):
        # Every restored key needs one main-only producer; PRs and Apple Silicon smoke cannot become extra writers.
        main = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
        intel = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' && matrix.runner == 'macos-15-intel' }}"
        contracts = {
            "macos-core": ("unit-macos-14", main),
            "linux-coverage": ("instrumented-ubuntu-22.04", main),
            "macos-smoke": ("unit-${{ matrix.runner }}", intel),
            "windows-checks": ("check-windows-latest", main),
            "windows-tests-workspace": ("test-windows-latest", main),
            "windows-tests-harness": ("test-windows-latest", "false"),
            "windows-tests-harness-features": ("test-windows-latest", "false"),
            "windows-tests-harness-api": ("test-windows-latest", "false"),
            "windows-tests-runtime": ("test-windows-latest", "false"),
            "windows-smoke": ("release-windows-latest", main),
            "windows-glyph-oracle": ("test-windows-latest", "false"),
            "linux-core": ("linux-ubuntu-22.04", main),
            "linux-packages": ("linux-ubuntu-22.04", "false"),
        }
        def verify(block, key, policy):
            caches = block.split("uses: Swatinem/rust-cache@")
            self.assertEqual(len(caches), 2)
            cache = re.split(r"\n      - ", caches[1], maxsplit=1)[0]
            self.assertEqual(re.findall(r"(?m)^          shared-key: (.+)$", cache),
                             ["${{ env.CI_CACHE_NAMESPACE }}-" + key])
            self.assertEqual(re.findall(r"(?m)^          save-if: (.+)$", cache), [policy])
            self.assertIn("          add-job-id-key: false\n", cache)
            self.assertIn("          cache-workspace-crates: false\n", cache)
        for job, (key, policy) in contracts.items():
            with self.subTest(job=job):
                block = workflow_test_fixture.job_block("ci.yml", job)
                verify(block, key, policy)
                with self.assertRaises(AssertionError):
                    verify(block.replace(f"save-if: {policy}", "save-if: true"), key, policy)
                with self.assertRaises(AssertionError):
                    verify(block.replace(f"shared-key: ${{{{ env.CI_CACHE_NAMESPACE }}}}-{key}",
                                         "shared-key: shared-across-architectures"), key, policy)
        block = workflow_test_fixture.job_block("ci.yml", "macos-smoke")
        for bad in ("false", main, intel.replace("&&", "||", 1),
                    intel.replace("'push'", "'pull_request'"),
                    intel.replace("'refs/heads/main'", "'refs/heads/other'"),
                    intel.replace("'macos-15-intel'", "'macos-14'")):
            with self.subTest(invalid_policy=bad), self.assertRaises(AssertionError):
                verify(block.replace(intel, bad), contracts["macos-smoke"][0], intel)

    def test_windows_cairo_installs_remain_without_ci_timeout_overrides(self):
        # A restored archive does not replace installation after image or package ABI changes.
        text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        for job_name in (
            "windows-native", "windows-glyph-oracle", "windows-checks", *workflow_test_fixture.WINDOWS_TEST_SHARDS, "windows-smoke",
        ):
            with self.subTest(job=job_name):
                job = text.split(f"  {job_name}:\n", 1)[1]
                job = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", job, maxsplit=1)[0]
                step = job.split("- name: Install Cairo for Windows\n", 1)[1]
                step = re.split(r"(?m)^      - ", step, maxsplit=1)[0]
                self.assertNotIn("timeout-minutes:", step)
                self.assertIn("shell: pwsh", step)
                self.assertIn(r"run: .\scripts\setup-windows-cairo.ps1", step)

    def test_linux_core_installs_gpu_runtime_dependencies(self):
        text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        core = text.split("  linux-core:\n", 1)[1]
        core = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", core, maxsplit=1)[0]
        for dependency in ("mesa-vulkan-drivers", "libvulkan1"):
            with self.subTest(dependency=dependency):
                self.assertIn(dependency, core)

    def test_ubuntu_installs_keep_commands_without_timeout_overrides(self):
        installs = (
            ("ci.yml", "linux-core", "Install runner and native dependencies"),
            ("ci.yml", "linux-packages", "Install runner, package, and runtime dependencies"),
            ("release.yml", "package-linux", "Install runner, package, and runtime dependencies"),
        )
        for workflow_name, job_name, step_name in installs:
            with self.subTest(workflow=workflow_name, job=job_name):
                text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / workflow_name).read_text(
                    encoding="utf-8"
                )
                job = text.split(f"  {job_name}:\n", 1)[1]
                job = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", job, maxsplit=1)[0]
                step = job.split(f"- name: {step_name}\n", 1)[1]
                step = re.split(r"(?m)^      - ", step, maxsplit=1)[0]
                self.assertIn("apt-get update", step)
                self.assertIn("apt-get install -y --no-install-recommends", step)
                self.assertNotIn("timeout-minutes:", step)

    def test_ci_verifies_every_declared_optional_feature(self):
        # Cargo metadata is the source of truth: a new feature-bearing package
        # fails this test until CI compiles, lints, documents, and tests it.
        # `sonicterm-logging` dev-depends on `test-util`, so workspace Clippy and
        # tests already build it, but `cargo doc` builds no dev-dependencies, so
        # `linux-core` documents it. The perf features gate only perf_scenarios
        # example code, which `cargo doc` never documents; each host's core jobs
        # test and lint that example with each feature set below, and every
        # declared perf feature is in exactly one set.
        self.assertEqual(
            workflow_test_fixture.optional_feature_packages(),
            {"sonicterm-resource": ("test-util",),
             "sonicterm-app": ("perf-counters", "perf-echo-trace", "perf-frame-texture",
                               "perf-hook-checkpoint-memory", "perf-hook-trim")},
        )
        feature_sets = ("perf-counters,perf-hook-checkpoint-memory,perf-hook-trim", "perf-frame-texture",
                        "perf-echo-trace")
        covered = [feature for feature_set in feature_sets for feature in feature_set.split(",")]
        self.assertEqual(sorted(covered), sorted(workflow_test_fixture.optional_feature_packages()["sonicterm-app"]))
        manifest = (workflow_test_fixture._HERE.parent / "crates" / "sonicterm-logging" / "Cargo.toml").read_text(
            encoding="utf-8"
        )
        dev_dependencies = manifest.split("\n[dev-dependencies]\n", 1)[1].split("\n[", 1)[0]
        self.assertIn(
            'sonicterm-resource = { workspace = true, features = ["test-util"] }',
            dev_dependencies,
        )
        workflow = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        for retired in ("macos-features", "windows-features", "linux-features"):
            self.assertNotIn(f"  {retired}:\n", workflow)
        self.assertNotIn("AWS_LC_SYS_PREBUILT_NASM", workflow)
        command = 'RUSTDOCFLAGS="-D warnings" cargo doc -p sonicterm-resource --all-features --no-deps'
        core = workflow.split("  linux-core:\n", 1)[1]
        core = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", core, maxsplit=1)[0]
        self.assertEqual(core.count(command), 1)
        self.assertEqual(workflow.count("--all-features"), 1)
        def job_body(name):
            body = workflow.split(f"  {name}:\n", 1)[1]
            return re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", body, maxsplit=1)[0]

        def run_lines(body, command):
            """How many steps of `body` run exactly `command`, so a longer command never counts as it."""
            return len(re.findall(rf"(?m)^ +run: {re.escape(command)}$", body))

        # Each harness API cfg configuration is a run of its own: the gate renders it, and CI runs it once in
        # each of its jobs and nowhere else; the counters set is the one it compiles with, and the echo-trace
        # pair adds perf-echo-trace so the echo timeline's adapter, which needs the feature and its cfg, compiles.
        gate = workflow_test_fixture.load_local_gate()
        echo_cfg_ids = ("perf-scenarios-harness-api-echo-trace-clippy", "perf-scenarios-harness-api-echo-trace-tests")
        cfg_steps = [step for step in gate.STEPS if step.id in ("perf-scenarios-harness-api-clippy",
                                                                 "perf-scenarios-harness-api-tests", *echo_cfg_ids)]
        self.assertEqual(len(cfg_steps), 4)
        for step in cfg_steps:
            command = gate.command_text(step)
            self.assertIn(f"--features {feature_sets[0]}", command)
            self.assertEqual(f"--features {feature_sets[0]},perf-echo-trace" in command, step.id in echo_cfg_ids)
            for job in step.ci_jobs:
                with self.subTest(command=command, job=job):
                    self.assertEqual(run_lines(job_body(job), command), 1)
            self.assertEqual(run_lines(workflow, command), len(step.ci_jobs))
            self.assertNotIn(command, job_body("macos-smoke"))

        # Each feature's tests run exactly once on each platform that runs the harness's plain tests, and its
        # lint wherever the workspace lint runs; no job runs either twice, and macos-smoke runs neither. On
        # Windows the plain and feature tests sit in different parallel shards, so they are counted per platform.
        plain_test = "cargo test --locked -p sonicterm-app --example perf_scenarios"
        test_platforms = (("macos-core",), workflow_test_fixture.WINDOWS_TEST_SHARDS, ("linux-core",))
        for feature in feature_sets:
            feature_test = f"{plain_test} --features {feature}"
            feature_lint = (f"cargo clippy --locked -p sonicterm-app --example perf_scenarios --features {feature}"
                            " -- -D warnings")
            for platform_jobs in test_platforms:
                with self.subTest(command=feature_test, platform=platform_jobs[0]):
                    self.assertEqual(sum(run_lines(job_body(job), feature_test) for job in platform_jobs), 1)
                    self.assertEqual(sum(run_lines(job_body(job), plain_test) for job in platform_jobs), 1)
            self.assertEqual(run_lines(workflow, feature_test), len(test_platforms))
            lint_jobs = ("macos-core", "windows-checks", "linux-core")
            for job in lint_jobs:
                with self.subTest(command=feature_lint, job=job):
                    body = job_body(job)
                    self.assertEqual(run_lines(body, feature_lint), 1)
                    self.assertIn("        run: cargo clippy --workspace --all-targets -- -D warnings\n", body)
            self.assertEqual(run_lines(workflow, feature_lint), len(lint_jobs))
            self.assertNotIn(feature, job_body("macos-smoke"))
            # The plain lint and test in their three jobs each, plus the cfg runs that compile this set.
            cfg_runs = sum(len(step.ci_jobs) for step in cfg_steps
                           if f"--features {feature}" in gate.command_text(step))
            self.assertEqual(workflow.count(f"--features {feature}"), 6 + cfg_runs)

    def test_workspace_tests_cover_unit_and_integration_targets_once(self):
        script = (workflow_test_fixture._HERE.parent / "scripts" / "check-workspace-crates.sh").read_text(
            encoding="utf-8"
        )
        command = r"(?m)^cargo test --workspace --lib --bins --tests --no-fail-fast$"
        self.assertEqual(len(re.findall(command, script)), 1)
        self.assertNotIn("while IFS=", script)

        workflow = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertEqual(workflow.count("bash scripts/check-workspace-crates.sh"), 3)
        self.assertNotIn("Run per-crate unit/build gate", workflow)
        self.assertNotIn("cargo test --workspace --lib --bins\n", workflow)

    def test_release_reuses_exact_main_ci_and_runs_packaging_directly(self):
        text = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        validation = text.split("  validate-release-tag:\n", 1)[1].split(
            "\n  build-mac-x86_64:\n", 1
        )[0]
        required = [
            "actions: read",
            "fetch-depth: 0",
            'resolve-commit --revision "$GITHUB_SHA"',
            'echo "sha=${release_sha}" >> "$GITHUB_OUTPUT"',
            "RELEASE_SHA: ${{ steps.release-commit.outputs.sha }}",
            'git fetch --no-tags origin "+refs/heads/main:refs/remotes/origin/main"',
            'git merge-base --is-ancestor "$RELEASE_SHA" refs/remotes/origin/main',
            "actions/workflows/ci.yml/runs?branch=main&event=push&status=success&",
            'head_sha=${RELEASE_SHA}&per_page=100',
            'check-main-ci --sha "$RELEASE_SHA"',
            'check-version --tag "${{ github.ref_name }}"',
        ]
        for contract in required:
            with self.subTest(contract=contract):
                self.assertIn(contract, validation)
        for removed in (
            "cargo fmt --all --check",
            "cargo clippy --workspace",
            "cargo doc --workspace",
            "unit-tests-mac:",
            "unit-tests-windows:",
            "unit-tests-linux:",
            "Swatinem/rust-cache@",
        ):
            with self.subTest(removed=removed):
                self.assertNotIn(removed, text)
        for job in ("build-mac-x86_64", "build-mac-aarch64", "build-windows", "package-linux"):
            block = text.split(f"  {job}:\n", 1)[1]
            block = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", block, maxsplit=1)[0]
            self.assertIn("needs: [validate-release-tag]", block)
        self.assertIn("uses: actions/cache/restore@", text)
        self.assertNotIn("uses: actions/cache/save@", text)
