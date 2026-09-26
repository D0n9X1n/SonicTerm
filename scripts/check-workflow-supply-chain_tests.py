#!/usr/bin/env python3
"""Contract tests for scripts/check-workflow-supply-chain.py.

The checker's failure mode that matters is the quiet one: a parser change that
stops *finding* `uses:` lines reports a clean scan, and a clean scan is
indistinguishable from a compliant repository. So every rule here is asserted
in both directions — a compliant fixture passes, and a fixture violating
exactly that rule fails — and one test runs the checker against the real
workflows so the gate cannot pass by scanning nothing.
"""

from __future__ import annotations

from contextlib import contextmanager
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest

_HERE = Path(__file__).resolve().parent
_CHECKER_PATH = _HERE / "check-workflow-supply-chain.py"

_spec = importlib.util.spec_from_file_location("check_workflow_supply_chain", _CHECKER_PATH)
checker = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = checker
_spec.loader.exec_module(checker)

# A real pin, so the compliant fixture exercises the same shape the repository
# ships rather than a synthetic one the checker might treat differently.
PINNED_CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1"


def workflow(body: str) -> str:
    """Return a dedented workflow document."""
    return textwrap.dedent(body).lstrip("\n")


@contextmanager
def repository(workflows: dict[str, str]):
    """Materialize a throwaway checkout containing only `.github/workflows`."""
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        target = root / ".github" / "workflows"
        target.mkdir(parents=True)
        for name, content in workflows.items():
            (target / name).write_text(content, encoding="utf-8")
        yield root


def compliant(uses: str = PINNED_CHECKOUT, permissions: str = "contents: read") -> str:
    """A minimal workflow that satisfies every rule, for one-rule mutation."""
    return workflow(
        f"""
        name: Fixture

        on:
          push:
            branches: [main]

        permissions:
          {permissions}

        jobs:
          build:
            runs-on: ubuntu-latest
            timeout-minutes: 5
            steps:
              - uses: {uses}
                timeout-minutes: 5
        """
    )


def messages(findings) -> str:
    """Flatten findings so a test can assert on the diagnostic a human reads."""
    return "\n".join(finding.render() for finding in findings)


def findings_for(document: str, name: str = "ci.yml"):
    """Check one materialized workflow and return its findings."""
    with repository({name: document}) as root:
        return checker.check(root)


def job_block(workflow_name: str, job_name: str) -> str:
    """Return one repository job's text, bounded by the next job key."""
    text = (_HERE.parent / ".github" / "workflows" / workflow_name).read_text(
        encoding="utf-8"
    )
    block = text.split(f"  {job_name}:\n", 1)[1]
    return re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", block, maxsplit=1)[0]


def shell_commands(block: str) -> str:
    """Join shell line-continuations so one invocation reads as one line.

    `run:` blocks wrap long commands with a trailing backslash for legibility.
    The contract under test is the command a runner executes, not where the
    author happened to break the line, so reflowing must not turn a gate red.
    """
    return re.sub(r"[ \t]+", " ", re.sub(r"\\\n\s*", " ", block))


def optional_feature_packages() -> dict[str, tuple[str, ...]]:
    """Return every workspace package that declares a non-default feature."""
    completed = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=_HERE.parent,
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(completed.stdout)
    workspace = set(metadata["workspace_members"])
    return {
        package["name"]: tuple(
            sorted(feature for feature in package["features"] if feature != "default")
        )
        for package in metadata["packages"]
        if package["id"] in workspace
        and any(feature != "default" for feature in package["features"])
    }


class CompliantFixtureTests(unittest.TestCase):
    """The baseline: the checker must accept correct input, or nothing below means anything."""

    def test_pinned_ref_with_version_comment_passes(self):
        with repository({"ci.yml": compliant()}) as root:
            self.assertEqual(checker.check(root), [])

    def test_local_action_needs_no_pin(self):
        # A `./`-relative action is reviewed in the same pull request that
        # changes it, so there is no external revision to pin.
        with repository({"ci.yml": compliant(uses="./.github/actions/setup")}) as root:
            self.assertEqual(checker.check(root), [])

    def test_digest_pinned_docker_reference_passes(self):
        digest = "docker://alpine@sha256:" + "a" * 64
        with repository({"ci.yml": compliant(uses=digest)}) as root:
            self.assertEqual(checker.check(root), [])


class MutableRefTests(unittest.TestCase):
    """Each mutable spelling a `uses:` ref can take must be rejected."""

    def assert_rejected(self, uses: str, expected: str):
        with repository({"ci.yml": compliant(uses=uses)}) as root:
            findings = checker.check(root)
        self.assertTrue(findings, f"checker accepted {uses!r}")
        self.assertIn(expected, messages(findings))

    def test_version_tag_is_rejected(self):
        self.assert_rejected("actions/checkout@v7", "mutable ref 'v7'")

    def test_branch_name_is_rejected(self):
        # The failure that motivates the gate: `@stable` and `@main` are
        # branches, and a force-push changes what executes with no diff here.
        self.assert_rejected("dtolnay/rust-toolchain@stable", "mutable ref 'stable'")

    def test_abbreviated_sha_is_rejected(self):
        # An abbreviated SHA is a prefix, and a prefix can gain a second match
        # as the upstream repository grows.
        self.assert_rejected("actions/checkout@3d3c42e", "mutable ref '3d3c42e'")

    def test_uppercase_sha_is_rejected(self):
        # Uppercase does not resolve the same way and defeats the pin-equality
        # comparison that catches half-applied Dependabot bumps.
        self.assert_rejected(
            "actions/checkout@" + "3D3C42E5AAC5BA805825DA76410C181273BA90B1",
            "mutable ref",
        )

    def test_missing_revision_is_rejected(self):
        self.assert_rejected("actions/checkout", "declares no revision")

    def test_tag_pinned_docker_reference_is_rejected(self):
        self.assert_rejected("docker://alpine:3.19", "not digest-pinned")

    def test_flow_mapping_uses_is_rejected(self):
        document = compliant().replace(
            f"- uses: {PINNED_CHECKOUT}",
            "- { uses: actions/checkout@v7, timeout-minutes: 5 }",
        )
        self.assertIn("flow-style sequence mappings", messages(findings_for(document)))

    def test_quoted_uses_key_is_still_checked(self):
        document = compliant().replace(
            f"uses: {PINNED_CHECKOUT}", '"uses": actions/checkout@v7'
        )
        self.assertIn("mutable ref 'v7'", messages(findings_for(document)))

    def test_single_quoted_uses_value_is_checked(self):
        document = compliant().replace(
            PINNED_CHECKOUT, "'actions/checkout@v7' # v7.0.1"
        )
        self.assertIn("mutable ref 'v7'", messages(findings_for(document)))

    def test_yaml_alias_uses_is_rejected(self):
        document = compliant().replace(
            f"uses: {PINNED_CHECKOUT}", "uses: *checkout"
        )
        self.assertIn("YAML anchors and aliases", messages(findings_for(document)))

    def test_yaml_type_tag_uses_is_rejected(self):
        for tag in ("!!str", "!<tag:yaml.org,2002:str>", "!custom"):
            with self.subTest(tag=tag):
                document = compliant().replace(
                    f"uses: {PINNED_CHECKOUT}", f"uses: {tag} actions/checkout@v7"
                )
                self.assertIn("explicit YAML type tags", messages(findings_for(document)))

    def test_explicit_uses_key_is_rejected(self):
        document = compliant().replace(
            f"      - uses: {PINNED_CHECKOUT}",
            "      - ? uses\n        : actions/checkout@v7",
        )
        self.assertIn("explicit YAML mapping keys", messages(findings_for(document)))

    def test_single_quoted_hash_stays_inside_uses_value(self):
        document = compliant().replace(
            PINNED_CHECKOUT,
            "'actions/check#out@" + "a" * 40 + "' # v1",
        )
        self.assertEqual(findings_for(document), [])


class VersionCommentTests(unittest.TestCase):
    """A bare SHA is immutable but unreadable, and Dependabot needs the token."""

    def test_pin_without_comment_is_rejected(self):
        with repository(
            {"ci.yml": compliant(uses="actions/checkout@" + "a" * 40)}
        ) as root:
            findings = checker.check(root)
        self.assertIn("no trailing version comment", messages(findings))

    def test_pin_with_non_version_comment_is_rejected(self):
        with repository(
            {"ci.yml": compliant(uses="actions/checkout@" + "a" * 40 + " # pinned")}
        ) as root:
            findings = checker.check(root)
        self.assertIn("no trailing version comment", messages(findings))


class PermissionTests(unittest.TestCase):
    """Write must exist only on the enumerated publish jobs."""

    def test_missing_top_level_permissions_is_rejected(self):
        document = compliant().replace("permissions:\n  contents: read\n\n", "")
        with repository({"ci.yml": document}) as root:
            findings = checker.check(root)
        self.assertIn("no top-level permissions block", messages(findings))

    def test_workflow_level_write_is_rejected(self):
        with repository({"ci.yml": compliant(permissions="contents: write")}) as root:
            findings = checker.check(root)
        self.assertIn("workflow-level permissions grant write", messages(findings))

    def test_inline_write_all_is_rejected(self):
        # `permissions: write-all` carries its value on the key's own line, a
        # different shape from the nested block and an easy one to miss.
        document = compliant().replace("permissions:\n  contents: read", "permissions: write-all")
        with repository({"ci.yml": document}) as root:
            findings = checker.check(root)
        self.assertIn("workflow-level permissions grant write", messages(findings))

    def test_job_level_write_outside_the_boundary_is_rejected(self):
        document = compliant().replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: write\n    steps:",
        )
        with repository({"ci.yml": document}) as root:
            findings = checker.check(root)
        self.assertIn("outside the documented publish boundary", messages(findings))

    def test_job_level_write_inside_the_boundary_passes(self):
        # The same grant on an enumerated publish job is the shape the release
        # and wiki workflows actually ship.
        document = compliant().replace(
            "  build:", "  publish:"
        ).replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: write\n    steps:",
        )
        with repository({"release.yml": document}) as root:
            self.assertEqual(checker.check(root), [])

    def test_job_level_read_outside_the_boundary_passes(self):
        document = compliant().replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: read\n    steps:",
        )
        with repository({"ci.yml": document}) as root:
            self.assertEqual(checker.check(root), [])

    def test_flow_mapping_workflow_write_is_rejected(self):
        document = compliant().replace(
            "permissions:\n  contents: read", "permissions: { contents: write }"
        )
        self.assertIn("flow-style mappings", messages(findings_for(document)))

    def test_quoted_workflow_write_is_rejected(self):
        document = compliant().replace("contents: read", 'contents: "write"')
        self.assertIn("workflow-level permissions grant write", messages(findings_for(document)))

    def test_alternate_indent_job_write_is_rejected(self):
        document = compliant().replace(
            "    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:",
            "      runs-on: ubuntu-latest\n      timeout-minutes: 5\n      permissions:\n        contents: write\n      steps:",
        ).replace("      - uses:", "        - uses:")
        self.assertIn("outside the documented publish boundary", messages(findings_for(document)))

    def test_publish_job_cannot_write_an_extra_scope(self):
        document = compliant().replace("  build:", "  publish:").replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: write\n      packages: write\n    steps:",
        )
        self.assertIn("write scopes", messages(findings_for(document, "release.yml")))

    def test_duplicate_permissions_are_rejected(self):
        document = compliant().replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: read\n    permissions:\n      contents: write\n    steps:",
        )
        self.assertIn("permissions more than once", messages(findings_for(document)))

    def test_permission_merge_key_is_rejected(self):
        document = compliant().replace(
            "  contents: read", "  <<: *defaults\n  contents: read"
        )
        findings = messages(findings_for(document))
        self.assertIn("YAML merge keys", findings)
        self.assertIn("YAML anchors and aliases", findings)

    def test_explicit_permissions_key_is_rejected(self):
        document = compliant().replace(
            "permissions:\n  contents: read",
            "? permissions\n: { contents: write }",
        )
        self.assertIn("explicit YAML mapping keys", messages(findings_for(document)))


class ConsistencyTests(unittest.TestCase):
    """A half-applied bump leaves some call sites on the abandoned revision."""

    def test_same_action_on_two_shas_is_rejected(self):
        other = "actions/checkout@" + "b" * 40 + " # v7.0.1"
        with repository(
            {"ci.yml": compliant(), "release.yml": compliant(uses=other)}
        ) as root:
            findings = checker.check(root)
        self.assertIn("different commits", messages(findings))

    def test_same_action_on_one_sha_across_workflows_passes(self):
        with repository(
            {"ci.yml": compliant(), "release.yml": compliant()}
        ) as root:
            self.assertEqual(checker.check(root), [])


class ScanCoverageTests(unittest.TestCase):
    """Guard the quiet failure: a scan that finds nothing is not a passing scan."""

    def test_empty_workflow_directory_is_rejected(self):
        with repository({}) as root:
            findings = checker.check(root)
        self.assertIn("no workflow files found", messages(findings))

    def test_both_yaml_extensions_are_scanned(self):
        with repository({"ci.yaml": compliant(uses="actions/checkout@v7")}) as root:
            findings = checker.check(root)
        self.assertTrue(findings, "the .yaml extension was not scanned")

    def test_multiline_quoted_mapping_key_is_rejected(self):
        document = compliant().replace(
            f"      - uses: {PINNED_CHECKOUT}",
            '      - "us\n        es": actions/checkout@v7',
        )
        self.assertIn("outside the directly auditable grammar", messages(findings_for(document)))

    def test_yaml_document_directive_is_rejected(self):
        for prefix in ("---\n", "%YAML 1.2\n---\n"):
            with self.subTest(prefix=prefix):
                document = prefix + compliant()
                self.assertIn(
                    "outside the directly auditable grammar",
                    messages(findings_for(document)),
                )


class RepositoryTests(unittest.TestCase):
    """The rules are asserted against the workflows this repository ships."""

    def test_repository_workflows_satisfy_the_contract(self):
        self.assertEqual(checker.check(_HERE.parent), [])

    def test_pty_close_baseline_follows_cargo_restore_on_every_desktop(self):
        # Capture the before/after measurement before later gates, with compilation inside its timed step.
        command = "cargo test -p sonicterm-app --lib pty_close_baseline -- --ignored --nocapture"
        for job in ("macos-core", "windows-tests", "linux-core"):
            with self.subTest(job=job):
                block = job_block("ci.yml", job)
                steps = re.split(r"(?m)^      - ", block)[1:]
                restore = next(index for index, step in enumerate(steps)
                               if step.startswith("name: Restore Cargo dependencies\n"))
                baseline = steps[restore + 1]
                self.assertTrue(baseline.startswith("name: Measure PTY close baseline\n"))
                self.assertIn("timeout-minutes: 20", baseline)
                self.assertIn(f"run: {command}", baseline)
                self.assertEqual(block.count(command), 1)

    def test_every_repository_workflow_is_scanned(self):
        # Pins the discovery itself: were the glob to miss a workflow, every
        # rule above would still pass while that file went unchecked.
        found = {path.name for path in checker.workflow_paths(_HERE.parent)}
        self.assertEqual(found, {"ci.yml", "publish-wiki.yml", "release.yml", "pty-diagnostic.yml"})

    def test_every_repository_uses_is_pinned_to_a_sha(self):
        # Independent of the checker's own parser: re-derives the refs with a
        # separate scan, so a parser bug cannot make this assertion vacuous.
        unpinned = []
        for path in checker.workflow_paths(_HERE.parent):
            for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                stripped = line.strip()
                if not stripped.startswith(("uses:", "- uses:")):
                    continue
                ref = stripped.split("uses:", 1)[1].split("#")[0].strip()
                revision = ref.rpartition("@")[2]
                if not checker.SHA_PIN.match(revision):
                    unpinned.append(f"{path.name}:{number}: {ref}")
        self.assertEqual(unpinned, [])

    def test_write_boundary_names_only_existing_publish_jobs(self):
        # A boundary entry for a renamed or deleted job silently permits
        # nothing, and hides that the real job now runs unchecked.
        for name, job in checker.WRITE_BOUNDARY:
            with self.subTest(workflow=name, job=job):
                path = _HERE.parent / ".github" / "workflows" / name
                self.assertTrue(path.exists(), f"{name} does not exist")
                self.assertIn(f"\n  {job}:\n", path.read_text(encoding="utf-8"))

    def test_main_push_runs_are_unique_and_only_pr_updates_cancel(self):
        text = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
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
        text = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        contracts = {
            "macos": (
                "macos-14 / unit tests",
                ("macos-core", "macos-coverage", "macos-smoke"),
            ),
            "windows": (
                "windows-latest / unit tests",
                (
                    "windows-native",
                    "windows-checks",
                    "windows-tests",
                    "windows-smoke",
                ),
            ),
            "linux": (
                "ubuntu 22.04 / workspace, packages, X11, Wayland",
                ("linux-core", "linux-packages"),
            ),
        }

        def assert_contract(job: str, block: str, name: str, shards: tuple[str, ...]):
            self.assertIn(f"name: {name}", block)
            self.assertIn(f"needs: [{', '.join(shards)}]", block)
            self.assertIn("if: always()", block)
            for shard in shards:
                self.assertIn(f"${{{{ needs.{shard}.result }}}}", block)
            self.assertIn('test "$result" = "success"', block)

        for job, (name, shards) in contracts.items():
            with self.subTest(job=job):
                block = text.split(f"  {job}:\n", 1)[1]
                block = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", block, maxsplit=1)[0]
                assert_contract(job, block, name, shards)
                with self.assertRaises(AssertionError):
                    assert_contract(job, block.replace("if: always()", "if: success()"), name, shards)
                with self.assertRaises(AssertionError):
                    assert_contract(
                        job,
                        block.replace(f"${{{{ needs.{shards[0]}.result }}}}", "missing", 1),
                        name,
                        shards,
                    )

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
                    _HERE.parent / ".github" / "workflows" / workflow_name
                ).read_text(encoding="utf-8")
                self.assertIn(f"-- {executable} --runtime-smoke", text)

    def test_ci_caches_are_bounded_and_cairo_is_published_immediately(self):
        text = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("CI_CACHE_NAMESPACE: ci-v3", text)
        self.assertEqual(text.count("uses: Swatinem/rust-cache@"), 8)
        self.assertEqual(text.count("shared-key: ${{ env.CI_CACHE_NAMESPACE }}-"), 8)
        self.assertEqual(text.count("add-job-id-key: false"), 8)
        self.assertEqual(text.count("cache-workspace-crates: false"), 8)
        self.assertEqual(
            text.count(
                "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
            ),
            3,
        )
        self.assertEqual(text.count("save-if: false"), 5)

        native = text.split("  windows-native:\n", 1)[1]
        native = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", native, maxsplit=1)[0]
        restore = native.index("uses: actions/cache/restore@")
        install = native.index("- name: Install Cairo for Windows")
        save = native.index("uses: actions/cache/save@")
        self.assertLess(restore, install)
        self.assertLess(install, save)
        self.assertIn("if: steps.vcpkg-cache.outputs.cache-hit != 'true'", native)
        self.assertIn("key: ${{ env.CI_CACHE_NAMESPACE }}-vcpkg-cairo-", native)

        release = (_HERE.parent / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("CI_CACHE_NAMESPACE: ci-v3", release)
        self.assertIn("key: ${{ env.CI_CACHE_NAMESPACE }}-vcpkg-cairo-", release)

    def test_windows_cairo_consumers_allow_cold_install_after_image_rollover(self):
        # Hosted image rollovers can invalidate every package in a successfully restored fallback cache.
        text = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        for job_name in (
            "windows-native", "windows-checks", "windows-tests", "windows-smoke",
        ):
            with self.subTest(job=job_name):
                job = text.split(f"  {job_name}:\n", 1)[1]
                job = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", job, maxsplit=1)[0]
                self.assertEqual(
                    re.findall(
                        r"(?m)^      - name: Install Cairo for Windows\n        timeout-minutes: (\d+)$",
                        job,
                    ),
                    ["30" if job_name == "windows-native" else "12"],
                    "A restored archive is not proof that vcpkg can reuse its package ABIs",
                )

    def test_linux_core_installs_gpu_runtime_dependencies(self):
        text = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        core = text.split("  linux-core:\n", 1)[1]
        core = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", core, maxsplit=1)[0]
        for dependency in ("mesa-vulkan-drivers", "libvulkan1"):
            with self.subTest(dependency=dependency):
                self.assertIn(dependency, core)

    def test_ubuntu_dependency_installs_allow_slow_cold_mirrors(self):
        installs = (
            ("ci.yml", "linux-core", "Install runner and native dependencies"),
            ("ci.yml", "linux-packages", "Install runner, package, and runtime dependencies"),
            ("release.yml", "package-linux", "Install runner, package, and runtime dependencies"),
        )
        for workflow_name, job_name, step_name in installs:
            with self.subTest(workflow=workflow_name, job=job_name):
                text = (_HERE.parent / ".github" / "workflows" / workflow_name).read_text(
                    encoding="utf-8"
                )
                job = text.split(f"  {job_name}:\n", 1)[1]
                job = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", job, maxsplit=1)[0]
                pattern = rf"(?m)^      - name: {re.escape(step_name)}\n        timeout-minutes: (\d+)$"
                self.assertEqual(
                    re.findall(pattern, job),
                    ["20"],
                    f"{workflow_name}:{job_name} must leave cold Ubuntu mirrors enough bounded install time",
                )

    def test_ci_verifies_every_declared_optional_feature(self):
        # Cargo metadata is the source of truth: a new feature-bearing package
        # fails this test until CI compiles, lints, documents, and tests it.
        # `test-util` is the only optional feature. `sonicterm-logging`
        # dev-depends on it, so workspace Clippy and tests already build it, but
        # `cargo doc` builds no dev-dependencies, so `linux-core` documents it.
        self.assertEqual(optional_feature_packages(), {"sonicterm-resource": ("test-util",)})
        manifest = (_HERE.parent / "crates" / "sonicterm-logging" / "Cargo.toml").read_text(
            encoding="utf-8"
        )
        dev_dependencies = manifest.split("\n[dev-dependencies]\n", 1)[1].split("\n[", 1)[0]
        self.assertIn(
            'sonicterm-resource = { workspace = true, features = ["test-util"] }',
            dev_dependencies,
        )
        workflow = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
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

    def test_workspace_tests_cover_unit_and_integration_targets_once(self):
        script = (_HERE.parent / "scripts" / "check-workspace-crates.sh").read_text(
            encoding="utf-8"
        )
        command = r"(?m)^cargo test --workspace --lib --bins --tests --no-fail-fast$"
        self.assertEqual(len(re.findall(command, script)), 1)
        self.assertNotIn("while IFS=", script)

        workflow = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertEqual(workflow.count("bash scripts/check-workspace-crates.sh"), 3)
        self.assertNotIn("Run per-crate unit/build gate", workflow)
        self.assertNotIn("cargo test --workspace --lib --bins\n", workflow)

    def test_release_reuses_exact_main_ci_and_runs_packaging_directly(self):
        text = (_HERE.parent / ".github" / "workflows" / "release.yml").read_text(
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


class MacPackageGateTests(unittest.TestCase):
    """The macOS shipping artifact is the .dmg, so every gate must exercise that file."""

    def test_ci_smoke_fans_out_over_both_mac_architectures_with_isolated_caches(self):
        # One architecture's bundle cannot stand in for the other's: Intel and
        # Apple Silicon resolve different Cairo and font binaries, and a shared
        # cache key would let one leg restore the other's target directory.
        job = job_block("ci.yml", "macos-smoke")
        self.assertIn("name: macOS native runtime smoke (${{ matrix.arch }})", job)
        self.assertIn("runs-on: ${{ matrix.runner }}", job)
        self.assertIn("fail-fast: false", job)
        self.assertIn("- runner: macos-14\n            arch: aarch64", job)
        self.assertIn("- runner: macos-15-intel\n            arch: x86_64", job)
        self.assertIn(
            "shared-key: ${{ env.CI_CACHE_NAMESPACE }}-unit-${{ matrix.runner }}", job
        )
        self.assertNotIn("shared-key: ${{ env.CI_CACHE_NAMESPACE }}-unit-macos-14", job)
        # Both legs upload from one job id, so an artifact name without the
        # architecture collides and the second leg's evidence is discarded.
        names = re.findall(r"(?m)^          name: (.+)$", job)
        self.assertTrue(names, "no upload-artifact names found in macos-smoke")
        for name in names:
            with self.subTest(artifact=name):
                self.assertIn("${{ matrix.arch }}", name)

    def test_ci_smoke_validates_the_packaged_dmg_after_the_raw_binary_smoke(self):
        # The raw smoke proves the build tree runs where Homebrew's Cairo is on
        # the loader path; only the relocated, signed .dmg proves what ships.
        job = job_block("ci.yml", "macos-smoke")
        self.assertIn("brew install cairo pkg-config", job)
        self.assertIn("-- target/release/sonicterm-mac --runtime-smoke", job)
        self.assertIn(
            "bash scripts/make-macos-dmg.sh target/release/sonicterm-mac ci "
            "mac-${{ matrix.arch }}",
            job,
        )
        self.assertIn(
            '--dmg "dist/SonicTerm-ci-mac-${{ matrix.arch }}.dmg"', job
        )
        self.assertIn(
            '--state-dir "$RUNNER_TEMP/sonicterm-macos-package-${{ matrix.arch }}"', job
        )
        self.assertLess(job.index("--runtime-smoke"), job.index("make-macos-dmg.sh"))
        self.assertLess(job.index("make-macos-dmg.sh"), job.index("test-macos-package.py"))

    def test_ci_package_validation_keeps_a_cold_run_bounded_budget(self):
        # The validator builds and mounts a controlled DMG pair, each hdiutil
        # pass bounded at 120s, so a 5-minute step truncates a cold CI run.
        job = job_block("ci.yml", "macos-smoke")
        step = job.split("- name: Validate packaged macOS dmg", 1)[1]
        step = re.split(r"(?m)^      - ", step, maxsplit=1)[0]
        self.assertIn("timeout-minutes: 8", step)

    def test_release_builds_and_validates_each_dmg_on_its_own_architecture(self):
        # Packaging Intel bytes on an Apple Silicon host cannot run the bundle
        # it produced, so the load closure it signs off on is never executed.
        contracts = (
            ("build-mac-aarch64", "aarch64", "arm64", "aarch64-apple-darwin"),
            ("build-mac-x86_64", "x86_64", "x86_64", "x86_64-apple-darwin"),
        )
        for job_name, arch, lipo_arch, target in contracts:
            with self.subTest(job=job_name):
                job = job_block("release.yml", job_name)
                commands = shell_commands(job)
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
        job = job_block("release.yml", "package-mac")
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
        text = (_HERE.parent / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        uploads = re.findall(r"(?m)^          name: (release-assets-\S*)$", text)
        self.assertEqual(
            sorted(uploads),
            ["release-assets-linux", "release-assets-macos", "release-assets-windows"],
        )
        publish = job_block("release.yml", "publish")
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
        self.assertIn(
            'minimum: "14.0"', job_block("ci.yml", "macos-smoke")
        )
        self.assertIn(
            'minimum: "15.0"', job_block("ci.yml", "macos-smoke")
        )
        smoke = job_block("ci.yml", "macos-smoke")
        self.assertIn(
            "SONICTERM_MAX_MACOS_MINIMUM: ${{ matrix.minimum }}", smoke
        )
        self.assertIn(
            '--max-minimum-macos "${{ matrix.minimum }}"', shell_commands(smoke)
        )

        for job_name, ceiling in (
            ("build-mac-aarch64", "14.0"),
            ("build-mac-x86_64", "15.0"),
        ):
            with self.subTest(job=job_name):
                job = job_block("release.yml", job_name)
                self.assertIn(f'SONICTERM_MAX_MACOS_MINIMUM: "{ceiling}"', job)
                self.assertIn(
                    f'--max-minimum-macos "{ceiling}"', shell_commands(job)
                )

    def test_every_shipping_mac_gate_checks_the_package_not_only_the_binary(self):
        # The regression this pins: a green `--runtime-smoke` on the build tree
        # said nothing about the bundle users open, which is where the missing
        # self-contained Cairo and font payload actually failed.
        gates = (
            ("ci.yml", "macos-smoke"),
            ("release.yml", "build-mac-aarch64"),
            ("release.yml", "build-mac-x86_64"),
        )
        for workflow_name, job_name in gates:
            with self.subTest(workflow=workflow_name, job=job_name):
                job = job_block(workflow_name, job_name)
                self.assertIn("--runtime-smoke", job)
                self.assertIn("python3 scripts/test-macos-package.py", job)
                self.assertIn("--dmg ", job)
                self.assertIn("--state-dir ", job)


class CommandLineTests(unittest.TestCase):
    """The gate is invoked as a process, so its exit codes are part of the contract."""

    def run_checker(self, root: Path) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(_CHECKER_PATH), "--root", str(root)],
            capture_output=True,
            check=False,
        )

    def test_compliant_tree_exits_zero(self):
        with repository({"ci.yml": compliant()}) as root:
            completed = self.run_checker(root)
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())

    def test_violating_tree_exits_nonzero_and_names_the_file(self):
        with repository({"ci.yml": compliant(uses="actions/checkout@v7")}) as root:
            completed = self.run_checker(root)
        self.assertEqual(completed.returncode, 1)
        self.assertIn("ci.yml", completed.stderr.decode())


if __name__ == "__main__":
    unittest.main(verbosity=2)
