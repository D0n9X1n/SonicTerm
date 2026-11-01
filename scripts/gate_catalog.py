"""Gate catalog."""

from __future__ import annotations

import gate_harness
import gate_model


_RUSTDOC_WARNINGS = (("RUSTDOCFLAGS", "-D warnings"),)
# The pinned winit's doc comments are upstream text under the native-source digest; rustdoc from
# Rust 1.99 reads one `<kbd>*</kbd>` list as nested emphasis, so its doc phase allows only that lint.
_WINIT_RUSTDOC_FLAGS = (("RUSTDOCFLAGS", "-D warnings -A rustdoc::invalid_html_tags"),)
_CORE_CHECKS = ("macos-core", "windows-checks", "linux-core")
_SCRIPT_CHECKS = ("tooling", "windows-checks", "linux-core")
_SCRIPT_TESTS = ("tooling", "windows-tests-workspace", "linux-core")
# Windows splits its tests across parallel shards, so each test step names the shard that runs it there.
_WINDOWS_WORKSPACE = "windows-tests-workspace"
_WINDOWS_HARNESS = "windows-tests-harness"
_WINDOWS_HARNESS_FEATURES = "windows-tests-harness-features"
_WINDOWS_HARNESS_API = "windows-tests-harness-api"
_WINDOWS_RUNTIME = "windows-tests-runtime"
_WORKSPACE_TESTS = ("macos-core", _WINDOWS_WORKSPACE, "linux-core")
_HARNESS_TESTS = ("macos-core", _WINDOWS_HARNESS, "linux-core")
_HARNESS_FEATURE_TESTS = ("macos-core", _WINDOWS_HARNESS_FEATURES, "linux-core")
_HARNESS_API_TESTS = ("macos-core", _WINDOWS_HARNESS_API, "linux-core")

# Local timeouts are explicit command budgets independent of CI timeout policy.
# They allow cold-cache runtime; commands grouped in CI retain their local
# budgets, capped at five minutes for the shell tooling tests. A step that no
# CI job runs is sized well above its measured runtime.
# Prerequisites include what a command reaches indirectly, such as a test suite
# that reads `cargo metadata`.
STEPS = (
    # CI runs this immediately after the dependency restore, so its budget includes the test binary's cold build.
    gate_model.Step("pty-close-baseline",
         ("cargo", "test", "-p", "sonicterm-app", "--lib", "pty_close_baseline", "--", "--ignored", "--nocapture"),
         gate_model.HOSTS, 1200, "local", ("rust", "native"), ("macos-core", _WINDOWS_RUNTIME, "linux-core"),
         windows_preparations=(gate_model.Preparation(),)),
    # Physical source size is platform independent; contract tests run before the tracked-tree scan.
    gate_model.Step("source-size", ("python3", "scripts/check-source-size.py"), gate_model.HOSTS, 120,
         "local", ("python",), ("tooling",)),
    gate_model.Step("fmt", ("cargo", "fmt", "--all", "--check"), gate_model.HOSTS, 300, "local",
         ("rust",), _CORE_CHECKS),
    gate_model.Step("clippy", ("cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _CORE_CHECKS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    # perf-counters marks only that the App has the counter API, and perf-hook-checkpoint-memory and
    # perf-hook-trim that it has the checkpoint memory and covered-window trim hooks; the perf_scenarios
    # example is their sole reader, so every host lints that example with them as well as without them.
    gate_model.Step("perf-scenarios-counters-clippy",
         ("cargo", "clippy", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-counters,perf-hook-checkpoint-memory,perf-hook-trim", "--", "-D", "warnings"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _CORE_CHECKS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    # perf-frame-texture marks only that the renderer reports its frame texture's extent; the same example
    # is its sole reader, so every host lints it with that feature too.
    gate_model.Step("perf-scenarios-frame-texture-clippy",
         ("cargo", "clippy", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-frame-texture", "--", "-D", "warnings"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _CORE_CHECKS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    # perf-echo-trace marks only that the App has the S2 echo watch (it implies perf-counters); the same example
    # is its sole caller, so every host lints it with that feature too.
    gate_model.Step("perf-scenarios-echo-trace-clippy",
         ("cargo", "clippy", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-echo-trace", "--", "-D", "warnings"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _CORE_CHECKS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    # Every harness API cfg's code compiles only with its cfg, which perf-compare sets when both refs have the
    # entry's methods; every host lints all of them at once, with the counters they need.
    gate_model.Step("perf-scenarios-harness-api-clippy",
         ("cargo", "clippy", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios", "--all-targets",
          "--features", "perf-counters,perf-hook-checkpoint-memory,perf-hook-trim", "--", "-D", "warnings"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _CORE_CHECKS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY,
         harness_cfgs=gate_harness.HARNESS_API_CFG_NAMES),
    # The same lint with perf-echo-trace as well: the S2 echo timeline's adapter needs both the feature and its
    # cfg, so only this step compiles it; the two steps above compile its cfg-off sides.
    gate_model.Step("perf-scenarios-harness-api-echo-trace-clippy",
         ("cargo", "clippy", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios", "--all-targets",
          "--features", "perf-counters,perf-hook-checkpoint-memory,perf-hook-trim,perf-echo-trace", "--", "-D",
          "warnings"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _CORE_CHECKS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY,
         harness_cfgs=gate_harness.HARNESS_API_CFG_NAMES),
    gate_model.Step("doc", ("cargo", "doc", "--workspace", "--no-deps"), gate_model.HOSTS, 600, "local",
         ("rust", "native"), _CORE_CHECKS, env=_RUSTDOC_WARNINGS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    gate_model.Step("doc-resource-features",
         ("cargo", "doc", "-p", "sonicterm-resource", "--all-features", "--no-deps"),
         gate_model.HOSTS, 600, "local", ("rust",), ("linux-core",), env=_RUSTDOC_WARNINGS, windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    gate_model.Step("authored-comments", ("bash", "scripts/check-authored-rust-comments.sh"), gate_model.HOSTS, 300,
         "local", ("bash",), _SCRIPT_CHECKS),
    gate_model.Step("script-identifiers", ("bash", "scripts/check-script-identifiers.sh"), gate_model.HOSTS, 300,
         "local", ("bash",), _SCRIPT_CHECKS),
    gate_model.Step("no-raw-exit", ("bash", "scripts/check-no-raw-process-exit.sh"), gate_model.HOSTS, 120, "local",
         ("bash",), _SCRIPT_CHECKS),
    gate_model.Step("rust-version", ("bash", "scripts/check-rust-version.sh"), gate_model.HOSTS, 300, "local",
         ("rust", "bash"), _SCRIPT_CHECKS),
    gate_model.Step("window-owner", ("bash", "scripts/check-window-owner-registration.sh"), gate_model.HOSTS, 120,
         "local", ("bash",), _SCRIPT_CHECKS),
    gate_model.Step("workflow-supply-chain", ("bash", "scripts/check-workflow-supply-chain.sh"), gate_model.HOSTS, 300,
         "local", ("rust", "bash"), _SCRIPT_CHECKS),
    # Keep the one macOS-only custody regression on macOS when the portable suite moves to Ubuntu.
    gate_model.Step("macos-gate-custody", ("python3", "scripts/local-gate_tests.py",
                              "RunnerTests.test_a_zombie_only_group_kill_is_refused_on_macos_and_the_step_is_still_reaped"),
         ("macos",), 60, "local", ("bash",), ("macos-core",)),
    gate_model.Step("workflow-syntax", ("actionlint", "-shellcheck=", "-pyflakes=", ".github/workflows/ci.yml",
                             ".github/workflows/perf.yml"), gate_model.HOSTS, 60, "local", ("actionlint",), ("tooling",)),
    # The dependency audit reads only Cargo.lock and deny.toml, so CI runs it once, in linux-core.
    gate_model.Step("cargo-deny", ("cargo", "deny", "--locked", "check"), gate_model.HOSTS, 300, "local",
         ("rust", "cargo-deny"), ("linux-core",)),
    gate_model.Step("workspace-crates", ("bash", "scripts/check-workspace-crates.sh"), gate_model.HOSTS, 2100, "local",
         ("rust", "native", "bash"), _WORKSPACE_TESTS,
         windows_preparations=(gate_model.Preparation(gate_model._WINIT_TEST), gate_model.Preparation(gate_model._WINIT_DOC, _WINIT_RUSTDOC_FLAGS),
                               gate_model.Preparation(gate_model._WORKSPACE_TEST))),
    # After workspace-crates, so the libraries the doctests link are already built.
    gate_model.Step("doctests", ("cargo", "test", "--workspace", "--doc", "--no-fail-fast"), gate_model.HOSTS, 900,
         "local", ("rust", "native"), _WORKSPACE_TESTS),
    # workspace-crates' `--lib --bins --tests` skips examples, so the scenario harness's unit tests run here.
    gate_model.Step("perf-scenarios-tests", ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _HARNESS_TESTS,
         windows_preparations=(gate_model.Preparation(),)),
    # The same unit tests with the counter API and the checkpoint memory and trim hooks compiled in, wherever
    # the plain ones run.
    gate_model.Step("perf-scenarios-counters-tests",
         ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-counters,perf-hook-checkpoint-memory,perf-hook-trim"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _HARNESS_TESTS,
         windows_preparations=(gate_model.Preparation(),)),
    # The ignored working-set check: each start constant covers this host's measured glyph atlas need.
    gate_model.Step("glyph-atlas-working-set",
         ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "glyph_atlas_working_set", "--", "--ignored", "--nocapture"),
         ("macos", "windows"), 900, "local", ("rust", "native"), ("macos-core", "windows-glyph-oracle")),
    # The same unit tests with the frame-texture reading compiled in, wherever the plain ones run.
    gate_model.Step("perf-scenarios-frame-texture-tests",
         ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-frame-texture"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _HARNESS_FEATURE_TESTS,
         windows_preparations=(gate_model.Preparation(),)),
    # The same unit tests with the echo watch's calls compiled in, wherever the plain ones run.
    gate_model.Step("perf-scenarios-echo-trace-tests",
         ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-echo-trace"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _HARNESS_FEATURE_TESTS,
         windows_preparations=(gate_model.Preparation(),)),
    # Every API cfg is passed only to the example test target; dependency fingerprints remain shared.
    gate_model.Step("perf-scenarios-harness-api-tests",
         ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-counters,perf-hook-checkpoint-memory,perf-hook-trim"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _HARNESS_API_TESTS, harness_cfgs=gate_harness.HARNESS_API_CFG_NAMES),
    # The same tests with perf-echo-trace build and test the echo timeline's real adapter;
    # only the example receives the API compiler flags.
    gate_model.Step("perf-scenarios-harness-api-echo-trace-tests",
         ("cargo", "test", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios",
          "--features", "perf-counters,perf-hook-checkpoint-memory,perf-hook-trim,perf-echo-trace"),
         gate_model.HOSTS, 900, "local", ("rust", "native"), _HARNESS_API_TESTS, harness_cfgs=gate_harness.HARNESS_API_CFG_NAMES),
    # The ignored test runs alone: the general macOS pasteboard needs a hosted runner or explicit local opt-in.
    gate_model.Step("macos-clipboard-paste",
         ("cargo", "test", "--locked", "-p", "sonicterm-app", "--lib",
          "app::misc::clipboard::misc_clipboard_native_macos_tests::native_pasteboard_paste_action_reaches_real_pty",
          "--", "--exact", "--ignored", "--nocapture"),
         ("macos",), 900, "local", ("rust", "native"), ("macos-core",)),
    gate_model.Step("pty-feasibility", ("bash", "scripts/pty-backend-feasibility.sh", "--check"), gate_model.HOSTS, 300,
         "local", ("rust", "bash"), ("macos-core", _WINDOWS_WORKSPACE),
         windows_preparations=(gate_model.Preparation(gate_model._FEASIBILITY_BUILD),)),
    gate_model.Step("resource-inventory", ("bash", "scripts/test-resource-inventory.sh"), gate_model.HOSTS, 300, "local",
         ("bash",), ("tooling", _WINDOWS_WORKSPACE)),
    gate_model.Step("resource-baseline-tests", ("bash", "scripts/test-resource-baseline-evidence.sh"), gate_model.HOSTS,
         300, "local", ("bash",), ("tooling", _WINDOWS_WORKSPACE)),
    gate_model.Step("soak-harness", ("bash", "scripts/test-soak-harness.sh"), gate_model.HOSTS, 300, "local",
         ("bash",), ("tooling", _WINDOWS_WORKSPACE)),
    gate_model.Step("linux-packages-tests", ("bash", "scripts/test-linux-packages.sh"), gate_model.HOSTS, 300, "local",
         ("bash",), ("linux-core",)),
    gate_model.Step("release-assets-tests", ("bash", "scripts/test-release-assets.sh"), gate_model.HOSTS, 300, "local",
         ("rust", "bash"), ("tooling", "linux-core")),
    gate_model.Step("release-notes-tests", ("bash", "scripts/test-release-notes.sh"), gate_model.HOSTS, 300, "local",
         ("bash",), _SCRIPT_TESTS),
    gate_model.Step("wiki-publish-tests", ("bash", "scripts/test-wiki-publish.sh"), gate_model.HOSTS, 300, "local",
         ("rust", "bash"), _SCRIPT_TESTS),
    # Executed directly, as ci.yml does, so it needs a POSIX host.
    gate_model.Step("logic-coverage", ("scripts/rust-logic-coverage.sh",), ("macos", "linux"), 1200,
         "local", ("rust", "native", "llvm-cov"), ("linux-coverage",)),
    # The macOS-only floor instruments one crate: a cold build plus its tests, well inside the workspace run's budget.
    gate_model.Step("macos-logic-coverage", ("scripts/macos-logic-coverage.sh",), ("macos",), 900,
         "local", ("rust", "llvm-cov"), ("macos-core",)),
    # The current Linux host's Cairo is newer than the workspace container's; this one-crate test stays bounded.
    gate_model.Step("colr-tests", ("cargo", "test", "--locked", "-p", "sonicterm-font", "--lib", "colr_"), ("linux",), 900,
         "local", ("rust", "native"), ("tooling",)),
    gate_model.Step("windows-warp-allocator",
         ("cargo", "test", "-p", "sonicterm-gpu", "--test", "windows_warp_allocator_baseline",
          "--", "--nocapture"),
         ("windows",), 300, "local", ("rust", "native", "warp"), (_WINDOWS_RUNTIME,),
         windows_preparations=(gate_model.Preparation(),)),
    gate_model.Step("msi-validator-tests", (".\\scripts\\validate-windows-msi_tests.ps1",), ("windows",), 300,
         "local", ("pwsh",), (_WINDOWS_WORKSPACE,), shell="pwsh"),
    # A compiler can need forced cleanup on Windows, which only a compile-only step may accept; the harness
    # builds here, and the smoke's own compile-only build of the same example finds it fresh.
    gate_model.Step("windows-perf-build",
         ("cargo", "build", "--locked", "-p", "sonicterm-app", "--example", "perf_scenarios"),
         ("windows",), 1500, "local", ("rust", "native"), (_WINDOWS_RUNTIME,),
         windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    # The smoke's cold-build allowance (1500 s), at most 4 bounded runs (100 s each) of each of the five
    # Windows cases (2000 s), and up to 3 attempts (100 s each) of its one delivery replay, S10/sync
    # (300 s): 3800 s worst case, plus 400 s of headroom.
    gate_model.Step("windows-perf-smoke", ("python", "scripts/perf-compare.py", "--smoke"),
         ("windows",), 4200, "local", ("rust", "native"), (_WINDOWS_RUNTIME,)),
    gate_model.Step("macos-selection-build",
         ("cargo", "build", "--locked", "-p", "sonicterm-app", "--example", "native_split_selection"),
         ("macos",), 1500, "local", ("rust", "native"), ("macos-core", "macos-smoke")),
    gate_model.Step("macos-selection-smoke", ("python3", "scripts/native-selection-smoke.py"),
         ("macos",), 300, "local", ("rust", "native"), ("macos-core", "macos-smoke")),
    # The smoke builds its own debug perf_scenarios example, then runs three short cases (S1, S3, and S1
    # ended as at its deadline), each retried up to 3 times: the selection build's 1500 s cold-build
    # allowance plus 100 s for each of up to 12 harness runs.
    gate_model.Step("macos-perf-smoke", ("python3", "scripts/perf-compare.py", "--smoke"),
         ("macos",), 2700, "local", ("rust", "native"), ("macos-core", "macos-smoke")),
    # The head harness overlaid on the previous release tag, checked (not built) in an isolated target with
    # the features and harness cfgs a comparison would derive: a cold check of the workspace (3600 s) plus
    # the worktree and overlay. Ubuntu's example roots exclude the probe, so only macOS and Windows run it.
    gate_model.Step("perf-previous-release", ("python3", "scripts/perf-compare.py", "--check-previous-release"),
         ("macos",), 4200, "local", ("rust", "native"), ("macos-core",)),
    gate_model.Step("windows-perf-previous-release", ("python", "scripts/perf-compare.py", "--check-previous-release"),
         ("windows",), 4200, "local", ("rust", "native"), ("windows-checks",)),
    gate_model.Step("release-macos", ("cargo", "build", "--release", "-p", "sonicterm-mac"), ("macos",), 1500,
         "release", ("rust", "native"), ("macos-core", "macos-smoke")),
    gate_model.Step("release-windows", ("cargo", "build", "--release", "-p", "sonicterm-windows"),
         ("windows",), 1500, "release", ("rust", "native"), ("windows-smoke",), windows_policy=gate_model.WindowsPolicy.COMPILE_ONLY),
    gate_model.Step("release-linux", ("cargo", "build", "--release", "-p", "sonicterm-linux"), ("linux",),
         1800, "release", ("rust", "native"), ("linux-packages",)),
    gate_model.Step("windows-target", ("bash", "scripts/check-windows-target.sh"), ("macos",), 600,
         "optional", ("rust", "win-target", "bash"), ()),
)
