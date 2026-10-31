"""Gate ci catalog."""

from __future__ import annotations
from dataclasses import dataclass, replace
import re

import gate_catalog


@dataclass(frozen=True)
class CiOnly:
    """A ci.yml gate invocation that deliberately has no table step, and why."""

    kind: str
    command: str
    jobs: tuple[str, ...]
    reason: str


# `setup` installs dependencies and gates nothing. `evidence-rerun` reruns an
# integration test that the same job's workspace step, or the reviewed shard in
# `RERUN_BACKING_JOBS`, already runs, only to print its report. `runtime-evidence` and `package-evidence` need hosted
# runners, release binaries, or built packages; their scripts' own tests are
# table steps. A self-test can never be listed here.
CI_ONLY_KINDS = ("setup", "evidence-rerun", "runtime-evidence", "package-evidence")

CI_ONLY = (
    CiOnly("setup", "bash scripts/install-pinned-tool.sh actionlint", ("tooling",),
           "installs the pinned workflow syntax checker required by workflow-syntax"),
    CiOnly("runtime-evidence", "python3 scripts/glyph-oracle.py prepare", ("windows-glyph-oracle",),
           "binds the exact source/font/toolchain inputs before the canonical helper gate"),
    CiOnly("runtime-evidence", "python3 scripts/glyph-oracle.py seal", ("windows-glyph-oracle",),
           "publishes only a successful canonical helper gate's four rows"),
    CiOnly("runtime-evidence", "python3 scripts/glyph-oracle.py verify",
           (gate_catalog._WINDOWS_WORKSPACE, gate_catalog._WINDOWS_HARNESS, gate_catalog._WINDOWS_HARNESS_FEATURES, gate_catalog._WINDOWS_HARNESS_API),
           "verifies the exact producer artifact before each real-renderer consumer suite"),
    CiOnly("setup", ".\\scripts\\setup-windows-cairo.ps1",
           ("windows-native", "windows-checks", gate_catalog._WINDOWS_WORKSPACE, gate_catalog._WINDOWS_HARNESS, gate_catalog._WINDOWS_HARNESS_FEATURES,
            gate_catalog._WINDOWS_HARNESS_API, gate_catalog._WINDOWS_RUNTIME, "windows-smoke", "windows-glyph-oracle"),
           "installs the vcpkg Cairo that the native prerequisite names; it checks nothing"),
    CiOnly("setup", "python scripts/setup-windows-conpty.py --destination target/release", ("windows-smoke",),
           "stages the pinned Windows console runtime before native smoke; its offline tests run in workflow-supply-chain"),
    CiOnly("runtime-evidence", "python scripts/verify-windows-conpty.py --runtime-dir target/release/conpty "
           "--logs \"$env:RUNNER_TEMP\\sonicterm-windows-smoke\\logs\"", ("windows-smoke",),
           "requires pinned runtime bytes and positive packaged selection from the preceding smoke"),
    CiOnly("setup", "python3 scripts/setup-macos-native.py --work-dir \"$RUNNER_TEMP\"", ("macos-core",),
           "installs the pinned macOS 14 Cairo bottles the native prerequisite names on Apple Silicon; its offline tests "
           "run in workflow-supply-chain"),
    CiOnly("setup", "bash scripts/install-pinned-tool.sh cargo-llvm-cov", ("macos-core", "linux-coverage"),
           "installs the pinned cargo-llvm-cov that the llvm-cov prerequisite names; it checks nothing"),
    CiOnly("setup", "bash scripts/install-pinned-tool.sh cargo-deny", ("linux-core",),
           "installs the pinned cargo-deny that the cargo-deny prerequisite names; it checks nothing"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-gpu --test ci_host_capability_probe -- --nocapture",
           ("macos-core", gate_catalog._WINDOWS_RUNTIME),
           "prints the hosted window-capability report; workspace-crates already runs the test"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-gpu --test ci_adapter_classification_probe -- --nocapture",
           ("macos-core", gate_catalog._WINDOWS_RUNTIME),
           "prints the hosted adapter classification; workspace-crates already runs the test"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-gpu --test renderer_churn_baseline -- --nocapture",
           ("macos-core", gate_catalog._WINDOWS_RUNTIME),
           "prints the renderer churn baseline on Windows; the target compiles only for Windows, "
           "so on macOS this step and workspace-crates both run no tests"),
    CiOnly("evidence-rerun",
           "cargo test -p sonicterm-app --test windows_software_selection_present -- --nocapture",
           (gate_catalog._WINDOWS_RUNTIME,),
           "prints the selection-presentation report; workspace-crates already runs the test"),
    CiOnly("runtime-evidence",
           "python scripts/native-smoke-runner.py --timeout-seconds 120 "
           "--log-file \"$env:RUNNER_TEMP\\sonicterm-windows-gdi.log\" --require-capability EXERCISED "
           "-- cargo test -p sonicterm-gpu --test windows_software_present_capability -- --nocapture",
           (gate_catalog._WINDOWS_RUNTIME,),
           "requires the hosted runner's unique capability=EXERCISED verdict; workspace-crates "
           "runs the same test, which accepts HOST_INCAPABLE"),
    CiOnly("runtime-evidence",
           "python3 scripts/resource-baseline-evidence.py --runner-label macos-15 "
           "--output-dir target/v1.2.0-baseline/evidence-macos-15",
           ("macos-core",),
           "captures real resource evidence under the hosted runner label for upload; "
           "resource-baseline-tests covers the collector"),
    CiOnly("runtime-evidence",
           "\"$python_cmd\" scripts/resource-baseline-evidence.py --runner-label windows-latest "
           "--output-dir target/v1.2.0-baseline/evidence-windows-latest",
           (gate_catalog._WINDOWS_WORKSPACE,),
           "captures real resource evidence under the hosted runner label for upload; "
           "resource-baseline-tests covers the collector"),
    CiOnly("runtime-evidence",
           "python3 scripts/native-smoke-runner.py --timeout-seconds 45 "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-smoke-${{ matrix.arch }}\" "
           "--log-file \"$RUNNER_TEMP/sonicterm-macos-smoke-${{ matrix.arch }}.log\" "
           "-- target/release/sonicterm-mac --runtime-smoke",
           ("macos-smoke",),
           "runs the release-macos binary's native runtime smoke on the Intel image"),
    CiOnly("runtime-evidence",
           "python3 scripts/native-smoke-runner.py --timeout-seconds 45 "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-smoke-aarch64\" "
           "--log-file \"$RUNNER_TEMP/sonicterm-macos-smoke-aarch64.log\" "
           "-- target/release/sonicterm-mac --runtime-smoke",
           ("macos-core",),
           "runs the release-macos binary's native runtime smoke on Apple Silicon in the core gates"),
    CiOnly("runtime-evidence",
           "python scripts/native-smoke-runner.py --timeout-seconds 45 "
           "--state-dir \"$env:RUNNER_TEMP\\sonicterm-windows-smoke\" "
           "--log-file \"$env:RUNNER_TEMP\\sonicterm-windows-smoke.log\" "
           "-- ./target/release/sonicterm-windows.exe --runtime-smoke",
           ("windows-smoke",),
           "runs the release-windows binary's native runtime smoke on the hosted runner"),
    CiOnly("runtime-evidence",
           "python3 scripts/native-smoke-runner.py --timeout-seconds 45 --scenario frame-validation "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-frame-validation-smoke-${{ matrix.arch }}\" "
           "--log-file \"$RUNNER_TEMP/sonicterm-macos-frame-validation-smoke-${{ matrix.arch }}.log\" "
           "-- target/release/sonicterm-mac --runtime-smoke",
           ("macos-smoke",),
           "runs the release-macos binary's persistent frame-validation smoke on the Intel image"),
    CiOnly("runtime-evidence",
           "python3 scripts/native-smoke-runner.py --timeout-seconds 45 --scenario frame-validation "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-frame-validation-smoke-aarch64\" "
           "--log-file \"$RUNNER_TEMP/sonicterm-macos-frame-validation-smoke-aarch64.log\" "
           "-- target/release/sonicterm-mac --runtime-smoke",
           ("macos-core",),
           "runs the release-macos binary's persistent frame-validation smoke on Apple Silicon in the core gates"),
    CiOnly("runtime-evidence",
           "python scripts/native-smoke-runner.py --timeout-seconds 45 --scenario frame-validation "
           "--state-dir \"$env:RUNNER_TEMP\\sonicterm-windows-frame-validation-smoke\" "
           "--log-file \"$env:RUNNER_TEMP\\sonicterm-windows-frame-validation-smoke.log\" "
           "-- ./target/release/sonicterm-windows.exe --runtime-smoke",
           ("windows-smoke",),
           "runs the release-windows binary's persistent frame-validation smoke on the hosted runner"),
    CiOnly("runtime-evidence",
           "python3 scripts/native-smoke-runner.py --timeout-seconds 45 --scenario device-recovery "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-device-recovery-smoke-${{ matrix.arch }}\" "
           "--log-file \"$RUNNER_TEMP/sonicterm-macos-device-recovery-smoke-${{ matrix.arch }}.log\" "
           "-- target/release/sonicterm-mac --runtime-smoke",
           ("macos-smoke",),
           "runs shared-device recovery in the release-macos binary on the Intel image"),
    CiOnly("runtime-evidence",
           "python3 scripts/native-smoke-runner.py --timeout-seconds 45 --scenario device-recovery "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-device-recovery-smoke-aarch64\" "
           "--log-file \"$RUNNER_TEMP/sonicterm-macos-device-recovery-smoke-aarch64.log\" "
           "-- target/release/sonicterm-mac --runtime-smoke",
           ("macos-core",),
           "runs shared-device recovery in the release-macos binary on Apple Silicon in the core gates"),
    CiOnly("runtime-evidence",
           "python scripts/native-smoke-runner.py --timeout-seconds 45 --scenario device-recovery "
           "--state-dir \"$env:RUNNER_TEMP\\sonicterm-windows-device-recovery-smoke\" "
           "--log-file \"$env:RUNNER_TEMP\\sonicterm-windows-device-recovery-smoke.log\" "
           "-- ./target/release/sonicterm-windows.exe --runtime-smoke",
           ("windows-smoke",),
           "runs shared-device recovery in the release-windows binary on the hosted runner"),
    CiOnly("package-evidence",
           "bash scripts/make-macos-dmg.sh target/release/sonicterm-mac ci mac-${{ matrix.arch }}",
           ("macos-smoke",),
           "packages the release-macos binary; workspace-crates runs the bundle tool tests"),
    CiOnly("package-evidence",
           "bash scripts/make-macos-dmg.sh target/release/sonicterm-mac ci mac-aarch64",
           ("macos-core",),
           "packages the Apple Silicon release-macos binary; workspace-crates runs the bundle tool tests"),
    CiOnly("package-evidence",
           "python3 scripts/test-macos-package.py "
           "--dmg \"dist/SonicTerm-ci-mac-${{ matrix.arch }}.dmg\" "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-package-${{ matrix.arch }}\" "
           "--max-minimum-macos \"${{ matrix.minimum }}\"",
           ("macos-smoke",),
           "validates the packaged DMG; workspace-crates runs test-macos-package_tests.py"),
    CiOnly("package-evidence",
           "python3 scripts/test-macos-package.py "
           "--dmg \"dist/SonicTerm-ci-mac-aarch64.dmg\" "
           "--state-dir \"$RUNNER_TEMP/sonicterm-macos-package-aarch64\" "
           "--max-minimum-macos \"14.0\"",
           ("macos-core",),
           "validates the Apple Silicon packaged DMG; workspace-crates runs test-macos-package_tests.py"),
    CiOnly("package-evidence",
           "python3 scripts/prepare-release-assets.py check-version --tag \"$tag\"",
           ("linux-packages",),
           "checks the package tag before packaging; release-assets-tests covers the tool"),
    CiOnly("package-evidence",
           "SOURCE_DATE_EPOCH=\"$(git show -s --format=%ct HEAD)\" "
           "bash scripts/make-linux-packages.sh target/release/sonicterm \"$tag\" dist",
           ("linux-packages",),
           "builds the .tar.gz and .deb from the release-linux binary"),
    CiOnly("package-evidence", "bash scripts/test-linux-packages.sh \"$tarball\" \"$deb\"",
           ("linux-packages",),
           "validates the built packages; linux-packages-tests runs the script's own tests"),
    CiOnly("runtime-evidence",
           "bash scripts/smoke-linux-packages.sh \"${{ steps.packages.outputs.tarball }}\" "
           "\"${{ steps.packages.outputs.deb }}\"",
           ("linux-packages",),
           "runs both package layouts on X11/Xvfb and Wayland/Weston with lavapipe"),
    CiOnly("runtime-evidence",
           "bash scripts/smoke-linux-packages.sh \"${{ steps.packages.outputs.tarball }}\" "
           "\"${{ steps.packages.outputs.deb }}\" frame-validation",
           ("linux-packages",),
           "runs persistent frame-validation in both package layouts on X11 and Wayland"),
    CiOnly("runtime-evidence",
           "bash scripts/smoke-linux-packages.sh \"${{ steps.packages.outputs.tarball }}\" "
           "\"${{ steps.packages.outputs.deb }}\" device-recovery",
           ("linux-packages",),
           "runs shared-device recovery in both package layouts on X11 and Wayland"),
)

# Cargo's built-in aliases for gate subcommands, and the subcommands a gate runs.
_CARGO_ALIASES = {"t": "test", "d": "doc"}
_CARGO_GATES = ("fmt", "clippy", "doc", "test", "deny")
_INTERPRETERS = ("bash", "sh", "python", "python3", "py", "pwsh", "powershell")
_ASSIGNMENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*=.*", re.S)
_VARIABLE_WORD = re.compile(r"\$\{?[A-Za-z_][A-Za-z0-9_]*\}?")
_SCRIPT_PATH = re.compile(r"(?:\./)*scripts/([A-Za-z0-9_.-]+)")
_SCRIPT_MENTION = re.compile(r"(?<![\w-])scripts[/\\]")
# A loose scan for text the tokenizer cannot split or does not model: `cargo`
# followed by a gate subcommand or alias, or a first-party script path.
_GATE_MENTION = re.compile(
    r"(?<![\w.-])cargo(?![\w-]).*?(?<![\w-])(?:fmt|clippy|doc|test|deny|t|d)(?![\w-])"
    r"|(?<![\w-])scripts[/\\]",
    re.S,
)
# The runner substitutes GitHub expressions before the shell reads the line.
_EXPRESSION = re.compile(r"\$\{\{.*?\}\}", re.S)
_EXPRESSION_WORD = "\x00expression\x00"
# A gate beside one of these can run without its exit status failing the step.
_UNMODELED_OPERATORS = ("|", "|&", "||", "&", "(", ")")
# A first-party test entry point: these must be table steps, never CI-only.
_SELF_TEST_NAME = re.compile(r"[A-Za-z0-9_.-]+_tests\.(?:py|ps1|sh)")
_BARE_TEST_NAME = re.compile(r"test-[A-Za-z0-9_.-]+\.(?:py|ps1|sh)")
_CARGO_NAME = re.compile(r"[A-Za-z0-9_-]+")
WORKSPACE_TEST_COMMAND = re.compile(
    r"(?m)^cargo test --workspace --lib --bins --tests --no-fail-fast$"
)
