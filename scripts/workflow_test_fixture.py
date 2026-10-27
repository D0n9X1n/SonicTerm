"""Workflow test fixture."""

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


_HERE = Path(__file__).resolve().parent


def load_local_gate():
    """Use the same gate catalog and Step class as every production caller."""
    import local_gate
    return local_gate
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


# The parallel Windows jobs that replaced the single Windows test job, in ci.yml order.
WINDOWS_TEST_SHARDS = (
    "windows-tests-workspace",
    "windows-tests-harness",
    "windows-tests-harness-features",
    "windows-tests-harness-api",
    "windows-tests-runtime",
)


# The non-setup steps of the single Windows test job at cac36ca4, main's tip when the split was rebased,
# verbatim, except that the harness API step carries every local-gate.py HARNESS_API_CFGS entry and is
# followed by the same tests with the echo trace. Each must appear exactly once across the parallel Windows
# test shards, unchanged, in this relative order within its shard.
WINDOWS_TEST_WORK = (
    '      - name: Measure PTY close baseline\n        # Includes the cold test-binary build; the baseline-only observation envelope is 640 seconds.\n        run: cargo test -p sonicterm-app --lib pty_close_baseline -- --ignored --nocapture\n',
    '      - name: Run workspace unit and integration tests\n        shell: bash\n        run: bash scripts/check-workspace-crates.sh\n',
    '      - name: Run workspace documentation tests\n        run: cargo test --workspace --doc --no-fail-fast\n',
    '      - name: Run scenario harness unit tests\n        run: cargo test --locked -p sonicterm-app --example perf_scenarios\n',
    '      - name: Run scenario harness unit tests with frame counters\n        run: cargo test --locked -p sonicterm-app --example perf_scenarios --features perf-counters,perf-hook-checkpoint-memory,perf-hook-trim\n',
    '      - name: Measure the glyph atlas working set\n        run: cargo test --locked -p sonicterm-app --example perf_scenarios glyph_atlas_working_set -- --ignored --nocapture\n',
    '      - name: Run scenario harness unit tests with the frame texture reading\n        run: cargo test --locked -p sonicterm-app --example perf_scenarios --features perf-frame-texture\n',
    '      - name: Run scenario harness unit tests with the echo trace\n        run: cargo test --locked -p sonicterm-app --example perf_scenarios --features perf-echo-trace\n',
    '      - name: Run scenario harness unit tests with the harness API cfgs\n        shell: bash\n        run: python3 scripts/harness-target.py --cfgs perf_atlas_retry_api,perf_s10_attribution_api,perf_completeness_api,perf_dispatch_timeline_api,perf_echo_timeline_api,perf_guard_spans_api,perf_taskbar_progress_api -- cargo test --locked -p sonicterm-app --example perf_scenarios --features perf-counters,perf-hook-checkpoint-memory,perf-hook-trim\n',
    '      - name: Run scenario harness unit tests with the harness API cfgs and the echo trace\n        shell: bash\n        run: python3 scripts/harness-target.py --cfgs perf_atlas_retry_api,perf_s10_attribution_api,perf_completeness_api,perf_dispatch_timeline_api,perf_echo_timeline_api,perf_guard_spans_api,perf_taskbar_progress_api -- cargo test --locked -p sonicterm-app --example perf_scenarios --features perf-counters,perf-hook-checkpoint-memory,perf-hook-trim,perf-echo-trace\n',
    '      - name: Report host window capability\n        run: cargo test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture\n',
    '      - name: Report host adapter classification\n        run: cargo test -p sonicterm-gpu --test ci_adapter_classification_probe -- --nocapture\n',
    '      - name: Report renderer churn baseline\n        run: cargo test -p sonicterm-gpu --test renderer_churn_baseline -- --nocapture\n',
    '      - name: Require Windows GDI capability=EXERCISED\n        shell: pwsh\n        run: |\n          python scripts/native-smoke-runner.py `\n            --timeout-seconds 120 `\n            --log-file "$env:RUNNER_TEMP\\sonicterm-windows-gdi.log" `\n            --require-capability EXERCISED `\n            -- cargo test -p sonicterm-gpu --test windows_software_present_capability -- --nocapture\n',
    '      - name: Upload Windows GDI probe log\n        if: ${{ failure() }}\n        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: sonicterm-windows-gdi-log-${{ github.sha }}\n          path: ${{ runner.temp }}/sonicterm-windows-gdi.log\n          if-no-files-found: ignore\n',
    '      - name: Verify Windows WARP allocator baseline\n        run: cargo test -p sonicterm-gpu --test windows_warp_allocator_baseline -- --nocapture\n',
    '      - name: Verify Windows selection presentation\n        run: cargo test -p sonicterm-app --test windows_software_selection_present -- --nocapture\n',
    '      - name: Build Windows perf scenario harness\n        run: cargo build --locked -p sonicterm-app --example perf_scenarios\n',
    '      - name: Require Windows perf scenario smoke\n        run: python scripts/perf-compare.py --smoke\n',
    "      - name: Upload Windows perf scenario smoke evidence\n        if: ${{ (failure() || env.SONICTERM_PERF_REPLAY_RETRIED == '1') && env.SONICTERM_PERF_EVIDENCE_DIR != '' }}\n        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: sonicterm-windows-perf-smoke-${{ github.sha }}\n          path: ${{ env.SONICTERM_PERF_EVIDENCE_DIR }}\n          if-no-files-found: error\n",
    '      - name: Test MSI validator\n        shell: pwsh\n        run: .\\scripts\\validate-windows-msi_tests.ps1\n',
    '      - name: Run release-note unit test\n        shell: bash\n        run: bash scripts/test-release-notes.sh\n',
    '      - name: Test wiki publisher\n        shell: bash\n        run: bash scripts/test-wiki-publish.sh\n',
    '      - name: Verify frozen PTY feasibility evidence\n        shell: bash\n        run: bash scripts/pty-backend-feasibility.sh --check\n',
    '      - name: Verify resource inventory\n        shell: bash\n        run: bash scripts/test-resource-inventory.sh\n',
    '      - name: Run deterministic soak control gate\n        shell: bash\n        run: bash scripts/test-soak-harness.sh\n',
    '      - name: Test resource baseline evidence collector\n        shell: bash\n        run: bash scripts/test-resource-baseline-evidence.sh\n',
    '      - name: Capture real resource baseline evidence\n        id: capture_resource_baseline\n        shell: bash\n        run: |\n          python_cmd=python3\n          if ! command -v "$python_cmd" >/dev/null 2>&1; then\n            python_cmd=python\n          fi\n          "$python_cmd" scripts/resource-baseline-evidence.py \\\n            --runner-label windows-latest \\\n            --output-dir target/v1.2.0-baseline/evidence-windows-latest\n',
    "      - name: Upload resource baseline evidence\n        if: ${{ !cancelled() && steps.capture_resource_baseline.conclusion != 'skipped' }}\n        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: resource-baseline-evidence-windows-latest\n          path: target/v1.2.0-baseline/evidence-windows-latest\n          if-no-files-found: error\n",
)

# Common Windows setup; workspace is the sole test-codegen cache writer on main.
# Oracle consumers insert verified artifact setup; the producer brackets its measurement with publication.
WINDOWS_SHARD_SETUP = (
    '      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n',
    '      - name: Install Rust\n        uses: dtolnay/rust-toolchain@7e38f4b43b4db5c8dd498af069a4f6196df1d067 # v1\n        with:\n          toolchain: stable\n',
    '      - name: Resolve vcpkg commit\n        id: vcpkg\n        shell: pwsh\n        run: |\n          $root = if ($env:VCPKG_INSTALLATION_ROOT) { $env:VCPKG_INSTALLATION_ROOT } else { "C:\\vcpkg" }\n          "sha=$((git -C $root rev-parse HEAD).Trim())" | Out-File $env:GITHUB_OUTPUT -Append -Encoding utf8\n          "image=$env:ImageVersion" | Out-File $env:GITHUB_OUTPUT -Append -Encoding utf8\n',
    "      - name: Restore vcpkg binaries (Cairo)\n        uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0\n        with:\n          path: ${{ env.VCPKG_DEFAULT_BINARY_CACHE }}\n          key: ${{ env.CI_CACHE_NAMESPACE }}-vcpkg-cairo-${{ runner.os }}-${{ steps.vcpkg.outputs.image }}-${{ steps.vcpkg.outputs.sha }}-${{ hashFiles('scripts/setup-windows-cairo.ps1') }}\n          restore-keys: |\n            ${{ env.CI_CACHE_NAMESPACE }}-vcpkg-cairo-${{ runner.os }}-\n",
    '      - name: Install Cairo for Windows\n        shell: pwsh\n        run: .\\scripts\\setup-windows-cairo.ps1\n',
    "      - name: Restore Cargo dependencies\n        uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2\n        with:\n          shared-key: ${{ env.CI_CACHE_NAMESPACE }}-test-windows-latest\n          add-job-id-key: false\n          cache-workspace-crates: false\n          save-if: false\n",
)

# Each shard's preserved work, in order, by step name; together they are the former job's work exactly once.
WINDOWS_SHARD_WORK = {
    "windows-glyph-oracle": ("Measure the glyph atlas working set",),
    "windows-tests-workspace": (
        "Run workspace unit and integration tests", "Run workspace documentation tests",
        "Test MSI validator", "Run release-note unit test", "Test wiki publisher",
        "Verify frozen PTY feasibility evidence", "Verify resource inventory", "Run deterministic soak control gate",
        "Test resource baseline evidence collector", "Capture real resource baseline evidence",
        "Upload resource baseline evidence",
    ),
    "windows-tests-harness": ("Run scenario harness unit tests", "Run scenario harness unit tests with frame counters"),
    "windows-tests-harness-features": (
        "Run scenario harness unit tests with the frame texture reading",
        "Run scenario harness unit tests with the echo trace",
    ),
    "windows-tests-harness-api": (
        "Run scenario harness unit tests with the harness API cfgs",
        "Run scenario harness unit tests with the harness API cfgs and the echo trace",
    ),
    "windows-tests-runtime": (
        "Measure PTY close baseline", "Report host window capability", "Report host adapter classification", "Report renderer churn baseline",
        "Require Windows GDI capability=EXERCISED", "Upload Windows GDI probe log",
        "Verify Windows WARP allocator baseline", "Verify Windows selection presentation",
        "Build Windows perf scenario harness", "Require Windows perf scenario smoke",
        "Upload Windows perf scenario smoke evidence",
    ),
}


def windows_work_step(name: str) -> str:
    """Return one inventoried step's verbatim text by its name."""
    return next(step for step in WINDOWS_TEST_WORK if step.startswith(f"      - name: {name}\n"))


ORACLE_CONSUMER_SETUP = ('      - name: Download the exact-run helper oracle\n        uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1\n        with:\n          name: glyph-oracle-${{ github.run_id }}-${{ needs.windows-glyph-oracle.outputs.attempt }}\n          path: ${{ env.SONICTERM_GLYPH_ORACLE_DIR }}\n', '      - name: Verify helper sources fonts and provenance\n        env:\n          ORACLE_ATTEMPT: ${{ needs.windows-glyph-oracle.outputs.attempt }}\n          ORACLE_DIGEST: ${{ needs.windows-glyph-oracle.outputs.digest }}\n        run: python3 scripts/glyph-oracle.py verify\n')
ORACLE_PRODUCER_BEFORE = ('      - name: Bind glyph helper inputs\n        run: python3 scripts/glyph-oracle.py prepare\n',)
ORACLE_PRODUCER_AFTER = ('      - name: Seal the verified helper oracle\n        id: oracle\n        run: python3 scripts/glyph-oracle.py seal\n', '      - name: Publish the helper oracle\n        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1\n        with:\n          name: glyph-oracle-${{ github.run_id }}-${{ github.run_attempt }}\n          path: ${{ env.SONICTERM_GLYPH_ORACLE_EXPORT }}\n          if-no-files-found: error\n')


def windows_shard_problems(workflow: str) -> list[str]:
    """Check each Windows test shard runs its pinned setup, then exactly its pinned work, unchanged and in order."""
    problems: list[str] = []
    for shard, names in WINDOWS_SHARD_WORK.items():
        if f"\n  {shard}:\n" not in workflow:
            problems.append(f"ci.yml has no {shard} job")
            continue
        block = re.split(r"\n  (?=[a-z][a-z0-9_-]*:\n)", workflow.split(f"\n  {shard}:\n", 1)[1], maxsplit=1)[0]
        head, _, body = block.partition("    steps:\n")
        consumer = shard not in ("windows-tests-runtime", "windows-glyph-oracle")
        dependencies = "windows-native, windows-glyph-oracle" if consumer else "windows-native"
        if "    runs-on: windows-latest\n" not in head or f"    needs: [{dependencies}]\n" not in head:
            problems.append(f"{shard} must run on windows-latest after windows-native")
        if re.search(r"(?m)^    (?:if|continue-on-error):", head):
            problems.append(f"{shard} must not be conditional or advisory")
        body = re.sub(r"(?m)^      #.*\n", "", body)
        steps = [step.rstrip("\n") + "\n" for step in re.split(r"(?m)^(?=      - )", body) if step.startswith("      - ")]
        setup = list(WINDOWS_SHARD_SETUP)
        if shard == "windows-tests-workspace":
            setup[-1] = setup[-1].replace("save-if: false", "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}")
        if consumer:
            setup[5:5] = ORACLE_CONSUMER_SETUP
        if shard == "windows-glyph-oracle":
            setup.extend(ORACLE_PRODUCER_BEFORE)
        # When: setup differs from the pinned sequence, a step was dropped, edited or disguised as setup.
        if steps[:len(setup)] != setup:
            problems.append(f"{shard} does not begin with the pinned setup steps")
        expected = [windows_work_step(name) for name in names]
        if shard == "windows-glyph-oracle":
            expected.extend(ORACLE_PRODUCER_AFTER)
        if steps[len(setup):] != expected:
            problems.append(f"{shard} does not run exactly its pinned work steps in order")
    return problems


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
