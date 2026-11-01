"""Workflow download tests."""

from __future__ import annotations
import json
import unittest

import workflow_test_fixture


RELEASE_WORKFLOW = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
CI_WORKFLOW = (workflow_test_fixture._HERE.parent / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
DENY_URL = "https://github.com/EmbarkStudios/cargo-deny/releases/download/0.19.7/cargo-deny.tar.gz"
DIGEST = "c59ad86eb8b545c4b7b9833c074094fce4a99886f741e4286adec15870c0a726"
PINNED_DOWNLOAD_ARTIFACT = "actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2"


def run_workflow(*commands: str, env: dict[str, str] | None = None) -> str:
    """A compliant workflow with one `run` step holding `commands`, and an optional step `env`."""
    lines = [
        "name: Fixture", "", "on:", "  push:", "    branches: [main]", "",
        "permissions:", "  contents: read", "", "jobs:", "  build:",
        "    runs-on: ubuntu-latest", "    timeout-minutes: 5", "    steps:",
        f"      - uses: {workflow_test_fixture.PINNED_CHECKOUT}", "        timeout-minutes: 5",
        "      - name: Step", "        timeout-minutes: 5",
    ]
    if env:
        lines.append("        env:")
        lines.extend(f'          {key}: "{value}"' for key, value in env.items())
    lines.append("        run: |")
    lines.extend(f"          {command}" for command in commands)
    return "\n".join(lines) + "\n"


def checked(workflows: dict[str, str], files: dict[str, str] | None = None):
    """Check a throwaway checkout holding `workflows` and the repository `files` given."""
    with workflow_test_fixture.repository(workflows) as root:
        for name, content in (files or {}).items():
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text(content, encoding="utf-8")
        return workflow_test_fixture.checker.check(root)


def manifest(drop: tuple[str, ...] = (), **changes) -> str:
    """A one-tool manifest for cargo-deny, with `changes` applied to its entry and `drop` removed."""
    entry = {
        "version": "0.19.7",
        "os": "linux",
        "arch": "x86_64",
        "url": "https://github.com/EmbarkStudios/cargo-deny/releases/download/{version}/"
               "cargo-deny-{version}-x86_64-unknown-linux-musl.tar.gz",
        "sha256": DIGEST,
        "member": "cargo-deny-{version}-x86_64-unknown-linux-musl/cargo-deny",
        "version_args": ["--version"],
        "version_output": "cargo-deny {version}",
    }
    entry.update(changes)
    for key in drop:
        del entry[key]
    return json.dumps({"cargo-deny": entry})


def artifact_workflow(**inputs: str) -> str:
    """A compliant workflow whose one step runs actions/download-artifact with `inputs`."""
    lines = [
        "name: Fixture", "", "on:", "  push:", "    branches: [main]", "",
        "permissions:", "  contents: read", "", "jobs:", "  build:",
        "    runs-on: ubuntu-latest", "    timeout-minutes: 5", "    steps:",
        "      - name: Download", "        timeout-minutes: 5",
        f"        uses: {PINNED_DOWNLOAD_ARTIFACT}", "        with:",
    ]
    lines.extend(f"          {key}: {value}" for key, value in inputs.items())
    return "\n".join(lines) + "\n"


INSTALL = "bash scripts/install-pinned-tool.sh cargo-deny"


class DownloadTests(unittest.TestCase):
    """A workflow downloads nothing itself: only the pinned-tool script fetches executables."""

    def test_each_inline_installer_that_qa_got_past_the_text_checks_is_rejected(self):
        # Protect against the four bypasses of the text-matching checker: any download in a
        # workflow fails, whatever the shell around it would or would not run.
        fetch = f'curl --fail --location --output "$RUNNER_TEMP/deny.tar.gz" "{DENY_URL}"'
        check = f'echo "{DIGEST}  $RUNNER_TEMP/deny.tar.gz" | sha256sum --check --strict'
        extract = 'tar -xzf "$RUNNER_TEMP/deny.tar.gz" -C "$HOME/.cargo/bin"'
        cases = {
            "check in a function never called": (("set -euo pipefail", fetch,
                                                  f"verify() {{ {check}; }}", extract), None),
            "check under if false": (("set -euo pipefail", fetch, f"if false; then {check}; fi", extract),
                                     None),
            "check result ignored": (("set -euo pipefail", fetch, f"if {check}; then :; fi", extract), None),
            "one file checked, another extracted": (
                ("set -euo pipefail", fetch, 'curl -o "$RUNNER_TEMP/other.tar.gz" "$OTHER_URL"', check,
                 'tar -xzf "$RUNNER_TEMP/other.tar.gz"'), None),
            "step env overrides the pin": (("set -euo pipefail", fetch, check, extract),
                                           {"CARGO_DENY_VERSION": "latest"}),
            "URL in a variable": (("set -euo pipefail", 'asset_url="$(cat url.txt)"',
                                   'curl --fail -o tool.tar.gz "$asset_url"', check, extract), None),
        }
        for case, (commands, env) in cases.items():
            with self.subTest(case=case):
                found = workflow_test_fixture.messages(checked({"ci.yml": run_workflow(*commands, env=env)}))
                self.assertIn("downloads with curl or wget", found)

    def test_every_other_spelling_of_a_download_is_rejected(self):
        # Protect the rule from spelling: a pipe, a substitution, wget and a download after a
        # package install all count, while installing the curl package itself does not.
        for command in (
            "wget https://example.com/tool.tar.gz",
            "curl -fsSL https://example.com/install.sh | sh",
            'echo "$(curl -fsSL https://example.com/version)"',
            "apt-get install -y curl && curl -O https://example.com/tool.tar.gz",
            "/usr/bin/curl -o tool https://example.com/tool",
        ):
            with self.subTest(command=command):
                self.assertIn("downloads with curl or wget",
                              workflow_test_fixture.messages(checked({"ci.yml": run_workflow(command)})))
        self.assertEqual(checked({"ci.yml": run_workflow("apt-get install -y --no-install-recommends curl git")}),
                         [])

    def test_the_release_workflow_api_read_is_allowed_only_exactly(self):
        # Protect the allowlist: the committed release.yml call passes, the same call writing a
        # file fails, and the call moved to another workflow fails.
        self.assertEqual(workflow_test_fixture.checker.API_READS and len(workflow_test_fixture.checker.API_READS), 1)
        allowed = workflow_test_fixture.messages(checked({"release.yml": RELEASE_WORKFLOW}))
        self.assertNotIn("downloads with curl or wget", allowed)
        (workflow, command), = workflow_test_fixture.checker.API_READS
        self.assertEqual(workflow, "release.yml")
        first_line = "curl --fail-with-body --silent --show-error \\\n"
        self.assertEqual(RELEASE_WORKFLOW.count(first_line), 1)
        writing = RELEASE_WORKFLOW.replace(first_line, "curl --fail-with-body -o out.json --silent --show-error \\\n")
        self.assertIn("downloads with curl or wget", workflow_test_fixture.messages(checked({"release.yml": writing})))
        moved = run_workflow(command)
        self.assertIn("downloads with curl or wget", workflow_test_fixture.messages(checked({"ci.yml": moved})))


class GuardrailTests(unittest.TestCase):
    """The download rule also names the other common ways a workflow fetches a tool."""

    def test_gh_release_download_then_tar_is_rejected(self):
        # Protect against QA's repro: fetching a release asset with gh and unpacking it into
        # the cargo bin directory replaces the verified tool without curl or wget.
        workflow = run_workflow(
            "set -euo pipefail",
            "gh release download 0.19.7 --repo EmbarkStudios/cargo-deny "
            "--pattern 'cargo-deny-*-x86_64-unknown-linux-musl.tar.gz' --dir \"$RUNNER_TEMP\"",
            'tar -xzf "$RUNNER_TEMP"/cargo-deny-*.tar.gz --strip-components=1 -C "$HOME/.cargo/bin"',
            env={"GH_TOKEN": "${{ github.token }}"},
        )
        self.assertIn("downloads with `gh release download`", workflow_test_fixture.messages(checked({"ci.yml": workflow})))

    def test_inline_python_downloads_are_rejected(self):
        # Protect against a download written in Python, inline with -c or in a heredoc, using
        # urllib, urlretrieve, requests or http.client.
        for case, commands in (
            ("heredoc urllib", ("python3 - <<'PY'", "import urllib.request",
                                f'urllib.request.urlretrieve("{DENY_URL}", "deny.tar.gz")', "PY",
                                'tar -xzf deny.tar.gz -C "$HOME/.cargo/bin"')),
            ("-c urlretrieve", (f"python -c 'from urllib.request import urlretrieve; "
                                f"urlretrieve(\"{DENY_URL}\", \"deny.tar.gz\")'",)),
            ("-c requests", ("python3 -c \"import requests; open('t', 'wb').write("
                             "requests.get('https://example.com/t').content)\"",)),
            ("heredoc http.client", ("python3 <<EOF", "import http.client",
                                     "http.client.HTTPSConnection('example.com')", "EOF")),
        ):
            with self.subTest(case=case):
                self.assertIn("downloads with inline Python",
                              workflow_test_fixture.messages(checked({"ci.yml": run_workflow(*commands)})))

    def test_python_without_a_network_module_passes(self):
        # Protect the committed inline readers: Python that only parses local JSON is not a download.
        commands = ("version=\"$(cargo metadata --no-deps --format-version 1 | python3 -c '",
                    "import json, sys", 'print(json.load(sys.stdin)["version"])', "')\"",
                    "python3 scripts/prepare-release-assets.py check-version --tag \"v$version\"")
        self.assertEqual(checked({"ci.yml": run_workflow(*commands)}), [])

    def test_download_artifact_from_another_repository_or_run_is_rejected(self):
        # Protect against artifacts pulled from outside this run; same-run artifacts pass.
        self.assertEqual(checked({"ci.yml": artifact_workflow(name="dist", path="dist")}), [])
        for case, inputs in (
            ("another repository", {"name": "cargo-deny", "repository": "someone/tools",
                                    "run-id": "12345", "github-token": "${{ secrets.TOKEN }}"}),
            ("another run", {"name": "cargo-deny", "run-id": "12345",
                             "github-token": "${{ github.token }}"}),
        ):
            with self.subTest(case=case):
                self.assertIn("downloads an artifact from another repository or run",
                              workflow_test_fixture.messages(checked({"ci.yml": artifact_workflow(**inputs)})))

    def test_cargo_binstall_of_any_tool_is_rejected(self):
        # Protect against binstall of a tool the manifest does not pin, which the manifest rule
        # alone would not name.
        for command in ("cargo binstall --no-confirm cargo-nextest",
                        "cargo +stable binstall -y cargo-nextest", "cargo-binstall -y cargo-nextest"):
            with self.subTest(command=command):
                self.assertIn("downloads with `cargo binstall`",
                              workflow_test_fixture.messages(checked({"ci.yml": run_workflow(command)})))

    def test_the_committed_workflows_still_pass(self):
        # Protect the committed workflows: their same-run artifact downloads and JSON-reading
        # Python must not trip the new rules.
        self.assertIn("actions/download-artifact@", RELEASE_WORKFLOW)
        self.assertIn("| python3 -c '", CI_WORKFLOW)
        self.assertEqual(workflow_test_fixture.checker.check(workflow_test_fixture._HERE.parent), [])


class PinnedToolTests(unittest.TestCase):
    """Each manifest tool is pinned exactly and installed only by the pinned-tool script."""

    def test_the_committed_manifest_and_workflows_pass(self):
        # Protect the committed pins: both tools are in the manifest and installed by the script.
        tools = json.loads((workflow_test_fixture._HERE / "pinned-tools.json").read_text(encoding="utf-8"))
        self.assertEqual(set(tools), {"cargo-deny", "cargo-llvm-cov", "actionlint"})
        for tool in tools:
            self.assertIn(f"run: bash scripts/install-pinned-tool.sh {tool}\n", CI_WORKFLOW)
        self.assertEqual(workflow_test_fixture.checker.check(workflow_test_fixture._HERE.parent), [])

    def test_a_malformed_manifest_entry_is_rejected(self):
        # Protect the pins the script reads: an exact version, a 64-hex digest, an https release URL.
        for case, entry, expected in (
            ("latest", {"version": "latest"}, "exact X.Y.Z"),
            ("short digest", {"sha256": "abc123"}, "64 lowercase hex"),
            ("plain HTTP", {"url": "http://github.com/EmbarkStudios/cargo-deny/releases/download/"
                                   "{version}/deny.tar.gz"}, "pinned https release URL"),
            ("not a release", {"url": "https://example.com/{version}/deny.tar.gz"}, "pinned https release URL"),
            ("latest release", {"url": "https://github.com/EmbarkStudios/cargo-deny/releases/latest/"
                                       "download/deny.tar.gz"}, "pinned https release URL"),
        ):
            with self.subTest(case=case):
                found = workflow_test_fixture.messages(checked({"ci.yml": run_workflow(INSTALL)},
                                         {"scripts/pinned-tools.json": manifest(**entry)}))
                self.assertIn(expected, found)

    def test_every_manifest_field_is_required(self):
        # Protect the installer's inputs statically: a missing or unusable os, arch, archive
        # member, version-check arguments or version output fails the checker, not CI.
        files = {"scripts/pinned-tools.json": manifest()}
        self.assertEqual(checked({"ci.yml": run_workflow(INSTALL)}, files), [])
        for case, drop, changes, expected in (
            ("version policy type", (), {"version_first_line": "true"}, "version_first_line must be boolean"),
            ("missing member", ("member",), {}, "cargo-deny: member"),
            ("missing os", ("os",), {}, "cargo-deny: os"),
            ("missing arch", ("arch",), {}, "cargo-deny: arch"),
            ("missing version_args", ("version_args",), {}, "cargo-deny: version_args"),
            ("missing version_output", ("version_output",), {}, "cargo-deny: version_output"),
            ("unknown os", (), {"os": "windows"}, "cargo-deny: os"),
            ("unknown arch", (), {"arch": "arm64"}, "cargo-deny: arch"),
            ("member escapes", (), {"member": "../cargo-deny"}, "cargo-deny: member"),
            ("absolute member", (), {"member": "/usr/bin/cargo-deny"}, "cargo-deny: member"),
            ("empty version_args", (), {"version_args": []}, "cargo-deny: version_args"),
            ("non-string version_args", (), {"version_args": ["--version", 1]}, "cargo-deny: version_args"),
        ):
            with self.subTest(case=case):
                found = workflow_test_fixture.messages(checked({"ci.yml": run_workflow(INSTALL)},
                                         {"scripts/pinned-tools.json": manifest(drop, **changes)}))
                self.assertIn(expected, found)

    def test_per_platform_pins_are_checked_one_by_one(self):
        # Protect the per-host form: each pin needs its own digest, host, member and release URL, no two
        # pins may name one host, and a per-host list may not sit beside a top-level pin.
        flat = json.loads(manifest())["cargo-deny"]
        pin = {key: flat.pop(key) for key in ("os", "arch", "url", "sha256", "member")}
        other = dict(pin, os="macos", arch="aarch64")

        def platform_manifest(pins, **changes) -> str:
            """A one-tool manifest whose cargo-deny entry pins `pins`, with `changes` applied."""
            return json.dumps({"cargo-deny": dict(flat, platforms=pins, **changes)})

        workflow = {"ci.yml": run_workflow(INSTALL)}
        self.assertEqual(checked(workflow, {"scripts/pinned-tools.json": platform_manifest([pin, other])}), [])
        for case, text, expected in (
            ("empty list", platform_manifest([]), "cargo-deny: platforms must be a non-empty list"),
            ("duplicate host", platform_manifest([pin, dict(pin)]), "one archive per os and arch"),
            ("beside a top-level pin", platform_manifest([pin], sha256=DIGEST), "cannot both pin an archive"),
            ("bad digest", platform_manifest([pin, dict(other, sha256="abc")]), "cargo-deny platforms[1]: sha256"),
            ("bad host", platform_manifest([dict(pin, os="windows")]), "cargo-deny platforms[0]: os"),
            ("bad url", platform_manifest([dict(pin, url="https://example.com/{version}/deny.tar.gz")]),
             "cargo-deny platforms[0]: url"),
        ):
            with self.subTest(case=case):
                self.assertIn(expected, workflow_test_fixture.messages(checked(workflow, {"scripts/pinned-tools.json": text})))

    def test_a_tool_is_installed_only_by_the_script_with_its_name_alone(self):
        # Protect the single install path: extra arguments, an unknown tool, another installer,
        # or a manifest tool no workflow installs are each rejected.
        files = {"scripts/pinned-tools.json": manifest()}
        self.assertEqual(checked({"ci.yml": run_workflow(INSTALL)}, files), [])
        for case, workflow, expected in (
            ("extra argument", run_workflow(f"{INSTALL} {DENY_URL}"), "only a manifest tool name"),
            ("unknown tool", run_workflow("bash scripts/install-pinned-tool.sh cargo-other"),
             "not in scripts/pinned-tools.json"),
            ("never installed", run_workflow("echo nothing"), "is never installed"),
            ("cargo install", run_workflow(INSTALL, "cargo install cargo-deny --locked"),
             "installed other than by scripts/install-pinned-tool.sh"),
        ):
            with self.subTest(case=case):
                self.assertIn(expected, workflow_test_fixture.messages(checked({"ci.yml": workflow}, files)))


class ConsistencyTests(unittest.TestCase):
    """A half-applied bump leaves some call sites on the abandoned revision."""

    def test_same_action_on_two_shas_is_rejected(self):
        other = "actions/checkout@" + "b" * 40 + " # v7.0.1"
        with workflow_test_fixture.repository(
            {"ci.yml": workflow_test_fixture.compliant(), "release.yml": workflow_test_fixture.compliant(uses=other)}
        ) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("different commits", workflow_test_fixture.messages(findings))

    def test_same_action_on_one_sha_across_workflows_passes(self):
        with workflow_test_fixture.repository(
            {"ci.yml": workflow_test_fixture.compliant(), "release.yml": workflow_test_fixture.compliant()}
        ) as root:
            self.assertEqual(workflow_test_fixture.checker.check(root), [])


class ScanCoverageTests(unittest.TestCase):
    """Guard the quiet failure: a scan that finds nothing is not a passing scan."""

    def test_empty_workflow_directory_is_rejected(self):
        with workflow_test_fixture.repository({}) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertIn("no workflow files found", workflow_test_fixture.messages(findings))

    def test_both_yaml_extensions_are_scanned(self):
        with workflow_test_fixture.repository({"ci.yaml": workflow_test_fixture.compliant(uses="actions/checkout@v7")}) as root:
            findings = workflow_test_fixture.checker.check(root)
        self.assertTrue(findings, "the .yaml extension was not scanned")

    def test_multiline_quoted_mapping_key_is_rejected(self):
        document = workflow_test_fixture.compliant().replace(
            f"      - uses: {workflow_test_fixture.PINNED_CHECKOUT}",
            '      - "us\n        es": actions/checkout@v7',
        )
        self.assertIn("outside the directly auditable grammar", workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)))

    def test_yaml_document_directive_is_rejected(self):
        for prefix in ("---\n", "%YAML 1.2\n---\n"):
            with self.subTest(prefix=prefix):
                document = prefix + workflow_test_fixture.compliant()
                self.assertIn(
                    "outside the directly auditable grammar",
                    workflow_test_fixture.messages(workflow_test_fixture.findings_for(document)),
                )
