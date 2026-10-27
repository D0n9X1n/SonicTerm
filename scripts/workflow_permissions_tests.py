"""Workflow permissions tests."""

from __future__ import annotations
import unittest

import workflow_test_fixture


class CompliantFixtureTests(unittest.TestCase):
    """The baseline: the checker must accept correct input, or nothing below means anything."""

    def test_pinned_ref_with_version_comment_passes(self):
        with workflow_test_fixture.repository({"ci.yml": workflow_test_fixture.compliant()}) as root:
            self.assertEqual(workflow_test_fixture.checker.check(root), [])

    def test_local_action_needs_no_pin(self):
        # A `./`-relative action is reviewed in the same pull request that
        # changes it, so there is no external revision to pin.
        with workflow_test_fixture.repository({"ci.yml": workflow_test_fixture.compliant(uses="./.github/actions/setup")}) as root:
            self.assertEqual(workflow_test_fixture.checker.check(root), [])

    def test_digest_pinned_docker_reference_passes(self):
        digest = "docker://alpine@sha256:" + "a" * 64
        with workflow_test_fixture.repository({"ci.yml": workflow_test_fixture.compliant(uses=digest)}) as root:
            self.assertEqual(workflow_test_fixture.checker.check(root), [])


class MutableRefTests(unittest.TestCase):
    """Each mutable spelling a `uses:` ref can take must be rejected."""

    def assert_rejected(self, uses: str, expected: str):
        with workflow_test_fixture.repository({"ci.yml": workflow_test_fixture.compliant(uses=uses)}) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertTrue(findings, f"checker accepted {uses!r}")
        self.assertIn(expected, workflow_test_fixture.messages(findings))

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
        document = workflow_test_fixture.compliant().replace(
            f"- uses: {workflow_test_fixture.PINNED_CHECKOUT}",
            "- { uses: actions/checkout@v7, timeout-minutes: 5 }",
        )
        self.assertIn("flow-style sequence mappings", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_quoted_uses_key_is_still_checked(self):
        document = workflow_test_fixture.compliant().replace(
            f"uses: {workflow_test_fixture.PINNED_CHECKOUT}", '"uses": actions/checkout@v7'
        )
        self.assertIn("mutable ref 'v7'", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_single_quoted_uses_value_is_checked(self):
        document = workflow_test_fixture.compliant().replace(
            workflow_test_fixture.PINNED_CHECKOUT, "'actions/checkout@v7' # v7.0.1"
        )
        self.assertIn("mutable ref 'v7'", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_yaml_alias_uses_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            f"uses: {workflow_test_fixture.PINNED_CHECKOUT}", "uses: *checkout"
        )
        self.assertIn("YAML anchors and aliases", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_yaml_type_tag_uses_is_rejected(self):
        for tag in ("!!str", "!<tag:yaml.org,2002:str>", "!custom"):
            with self.subTest(tag=tag):
                document = workflow_test_fixture.compliant().replace(
                    f"uses: {workflow_test_fixture.PINNED_CHECKOUT}", f"uses: {tag} actions/checkout@v7"
                )
                self.assertIn("explicit YAML type tags", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_explicit_uses_key_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            f"      - uses: {workflow_test_fixture.PINNED_CHECKOUT}",
            "      - ? uses\n        : actions/checkout@v7",
        )
        self.assertIn("explicit YAML mapping keys", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_single_quoted_hash_stays_inside_uses_value(self):
        document = workflow_test_fixture.compliant().replace(
            workflow_test_fixture.PINNED_CHECKOUT,
            "'actions/check#out@" + "a" * 40 + "' # v1",
        )
        self.assertEqual(workflow_test_fixture.findings_for(document), [])


class VersionCommentTests(unittest.TestCase):
    """A bare SHA is immutable but unreadable, and Dependabot needs the token."""

    def test_pin_without_comment_is_rejected(self):
        with workflow_test_fixture.repository(
            {"ci.yml": workflow_test_fixture.compliant(uses="actions/checkout@" + "a" * 40)}
        ) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("no trailing version comment", workflow_test_fixture.messages(findings))

    def test_pin_with_non_version_comment_is_rejected(self):
        with workflow_test_fixture.repository(
            {"ci.yml": workflow_test_fixture.compliant(uses="actions/checkout@" + "a" * 40 + " # pinned")}
        ) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("no trailing version comment", workflow_test_fixture.messages(findings))


class PermissionTests(unittest.TestCase):
    """Write must exist only on the enumerated publish jobs."""

    def test_missing_top_level_permissions_is_rejected(self):
        document = workflow_test_fixture.compliant().replace("permissions:\n  contents: read\n\n", "")
        with workflow_test_fixture.repository({"ci.yml": document}) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("no top-level permissions block", workflow_test_fixture.messages(findings))

    def test_workflow_level_write_is_rejected(self):
        with workflow_test_fixture.repository({"ci.yml": workflow_test_fixture.compliant(permissions="contents: write")}) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("workflow-level permissions grant write", workflow_test_fixture.messages(findings))

    def test_inline_write_all_is_rejected(self):
        # `permissions: write-all` carries its value on the key's own line, a
        # different shape from the nested block and an easy one to miss.
        document = workflow_test_fixture.compliant().replace("permissions:\n  contents: read", "permissions: write-all")
        with workflow_test_fixture.repository({"ci.yml": document}) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("workflow-level permissions grant write", workflow_test_fixture.messages(findings))

    def test_job_level_write_outside_the_boundary_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: write\n    steps:",
        )
        with workflow_test_fixture.repository({"ci.yml": document}) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("outside the documented publish boundary", workflow_test_fixture.messages(findings))

    def test_job_level_write_inside_the_boundary_passes(self):
        # The same grant on an enumerated publish job is the shape the release
        # and wiki workflows actually ship.
        document = workflow_test_fixture.compliant().replace(
            "  build:", "  publish:"
        ).replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: write\n    steps:",
        )
        with workflow_test_fixture.repository({"release.yml": document}) as root:
            self.assertEqual(workflow_test_fixture.checker.check(root), [])

    def test_job_level_read_outside_the_boundary_passes(self):
        document = workflow_test_fixture.compliant().replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: read\n    steps:",
        )
        with workflow_test_fixture.repository({"ci.yml": document}) as root:
            self.assertEqual(workflow_test_fixture.checker.check(root), [])

    def test_flow_mapping_workflow_write_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            "permissions:\n  contents: read", "permissions: { contents: write }"
        )
        self.assertIn("flow-style mappings", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_quoted_workflow_write_is_rejected(self):
        document = workflow_test_fixture.compliant().replace("contents: read", 'contents: "write"')
        self.assertIn("workflow-level permissions grant write", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_alternate_indent_job_write_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            "    runs-on: ubuntu-latest\n    timeout-minutes: 5\n    steps:",
            "      runs-on: ubuntu-latest\n      timeout-minutes: 5\n      permissions:\n        contents: write\n      steps:",
        ).replace("      - uses:", "        - uses:")
        self.assertIn("outside the documented publish boundary", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_publish_job_cannot_write_an_extra_scope(self):
        document = workflow_test_fixture.compliant().replace("  build:", "  publish:").replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: write\n      packages: write\n    steps:",
        )
        self.assertIn("write scopes", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document, "release.yml")))

    def test_duplicate_permissions_are_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            "    timeout-minutes: 5\n    steps:",
            "    timeout-minutes: 5\n    permissions:\n      contents: read\n    permissions:\n      contents: write\n    steps:",
        )
        self.assertIn("permissions more than once", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_permission_merge_key_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            "  contents: read", "  <<: *defaults\n  contents: read"
        )
        findings = workflow_test_fixture.messages(workflow_test_fixture.findings_for(document))
        self.assertIn("YAML merge keys", findings)
        self.assertIn("YAML anchors and aliases", findings)

    def test_explicit_permissions_key_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            "permissions:\n  contents: read",
            "? permissions\n: { contents: write }",
        )
        self.assertIn("explicit YAML mapping keys", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))
