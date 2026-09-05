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

    def test_every_repository_workflow_is_scanned(self):
        # Pins the discovery itself: were the glob to miss a workflow, every
        # rule above would still pass while that file went unchecked.
        found = {path.name for path in checker.workflow_paths(_HERE.parent)}
        self.assertEqual(found, {"ci.yml", "publish-wiki.yml", "release.yml"})

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
                ("macos-core", "macos-features", "macos-coverage", "macos-smoke"),
            ),
            "windows": (
                "windows-latest / unit tests",
                (
                    "windows-native",
                    "windows-checks",
                    "windows-features",
                    "windows-tests",
                    "windows-smoke",
                ),
            ),
            "linux": (
                "ubuntu 22.04 / workspace, packages, X11, Wayland",
                ("linux-core", "linux-features", "linux-packages"),
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

    def test_ci_caches_are_bounded_and_cairo_is_published_immediately(self):
        text = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("CI_CACHE_NAMESPACE: ci-v3", text)
        self.assertEqual(text.count("uses: Swatinem/rust-cache@"), 11)
        self.assertEqual(text.count("shared-key: ${{ env.CI_CACHE_NAMESPACE }}-"), 11)
        self.assertEqual(text.count("add-job-id-key: false"), 11)
        self.assertEqual(text.count("cache-workspace-crates: false"), 11)
        self.assertEqual(
            text.count(
                "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
            ),
            3,
        )
        self.assertEqual(text.count("save-if: false"), 8)

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
            ("ci.yml", "linux-features", "Install runner and native dependencies"),
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

    def test_ci_verifies_every_declared_optional_feature_on_its_minimum_native_matrix(self):
        # Cargo metadata is the source of truth: adding a feature-bearing package
        # fails this test until the dedicated restore-only shards select it for
        # lint, docs, and tests on every host its platform-conditioned code needs.
        packages = optional_feature_packages()
        self.assertEqual(
            packages,
            {
                "sonicterm-app": ("ssh",),
                "sonicterm-font-config": ("distro-defaults",),
                "sonicterm-io": ("ssh",),
                "sonicterm-resource": ("test-util",),
            },
        )
        native = "-p sonicterm-app -p sonicterm-io"
        with_neutral = native + " -p sonicterm-font-config -p sonicterm-resource"
        selections = {
            "macos-features": native,
            "windows-features": native,
            "linux-features": with_neutral,
        }
        workflow = (_HERE.parent / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        for job_name, selected in selections.items():
            with self.subTest(job=job_name):
                job = workflow.split(f"  {job_name}:\n", 1)[1]
                job = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", job, maxsplit=1)[0]
                self.assertIn("timeout-minutes: 45", job.split("    steps:\n", 1)[0])
                self.assertIn("cache-workspace-crates: false", job)
                self.assertIn("save-if: false", job)
                if job_name == "windows-features":
                    self.assertIn('AWS_LC_SYS_PREBUILT_NASM: "1"', job)
                else:
                    self.assertNotIn("AWS_LC_SYS_PREBUILT_NASM", job)
                expected = (
                    "      - name: Verify optional feature surfaces\n"
                    "        timeout-minutes: 25\n"
                )
                self.assertIn(expected, job)
                commands = (
                    f"cargo clippy {selected} --all-features --all-targets -- -D warnings",
                    f'RUSTDOCFLAGS="-D warnings" cargo doc {selected} --all-features --no-deps',
                    f"cargo test {selected} --all-features --lib --bins --tests --no-fail-fast",
                )
                for command in commands:
                    self.assertEqual(job.count(command), 1)

        for core_job in ("macos-core", "windows-checks", "linux-core"):
            core = workflow.split(f"  {core_job}:\n", 1)[1]
            core = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", core, maxsplit=1)[0]
            self.assertNotIn("optional feature", core.lower())
            self.assertNotIn("optional ssh", core.lower())
            self.assertNotIn("--all-features", core)

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
