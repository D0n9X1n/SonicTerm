"""Run SonicTerm's local verification gate from one declarative, host-aware step table.

STEPS is the only definition of the local gate. CLAUDE.md and both
Development-and-Release wiki files embed its rendered form, and ci.yml keeps
explicit steps that local-gate_tests.py checks against it, so a gate command
edited in one place alone fails a test.

The runner executes the steps selected for the current host in table order. POSIX
steps reuse the native smoke runner's process-group launch and tree kill. Windows
steps use owned Job Objects and an assigned-before-launch bootstrap. Later steps
still run after a failure, a timeout, or a launch error. On POSIX a step also
fails when members of its process group outlive the leader by LEFTOVER_GRACE_S;
they are killed and counted while the unreaped leader still pins the group id.
A child that leaves the group, such as through setsid, is outside both bounds.
Tracked and untracked Git state, including file modes, is recorded before and
after the run; changes are reported, never cleaned.
"""

from __future__ import annotations
import argparse
from dataclasses import dataclass, replace
from enum import Enum
import difflib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import select
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from typing import Iterable, Mapping, Sequence, TextIO

# Keep the existing script import surface; implementation modules own their state.
from gate_model import (
    _HERE,
    _load_smoke_runner,
    SMOKE_RUNNER,
    WINDOWS_JOB,
    HOSTS,
    HOST_LABELS,
    EVIDENCE_CLASSES,
    PASS,
    CLEANED_NOT_NATURAL,
    FAIL,
    TIMEOUT,
    LAUNCH,
    INTERRUPTED,
    NOT_RUN,
    GIT_TIMEOUT_S,
    LEFTOVER_GRACE_S,
    PS_TIMEOUT_S,
    TAIL_LINES,
    PRE_EXISTING_LIMIT,
    PREREQUISITES,
    WindowsPolicy,
    _COMPILE_ONLY_STEPS,
    Preparation,
    _WINIT_TEST,
    _WINIT_DOC,
    _WORKSPACE_TEST,
    _FEASIBILITY_BUILD,
    Step,
)
from gate_harness import (
    HarnessApiCfg,
    HARNESS_API_CFGS,
    HARNESS_API_CFG_NAMES,
    _HARNESS_API_CFG_FLAGS,
    HarnessFlagConflict,
    _inherited_rustflags,
    _set_cfgs,
    _CFG_SPEC,
    _owned_cfg,
    compose_harness_rustflags,
    harness_dependency_environment,
    _PLAIN_WORD,
    _display_word,
    command_text,
    launch_argv,
    target_launch_argv,
    _TARGET_COMPILE_STEPS,
    HARNESS_LINT_TARGETS,
    target_compile_steps,
)
from gate_catalog import (
    _RUSTDOC_WARNINGS,
    _WINIT_RUSTDOC_FLAGS,
    _CORE_CHECKS,
    _SCRIPT_CHECKS,
    _SCRIPT_TESTS,
    _WINDOWS_WORKSPACE,
    _WINDOWS_HARNESS,
    _WINDOWS_HARNESS_FEATURES,
    _WINDOWS_HARNESS_API,
    _WINDOWS_RUNTIME,
    _WORKSPACE_TESTS,
    _HARNESS_TESTS,
    _HARNESS_FEATURE_TESTS,
    _HARNESS_API_TESTS,
    STEPS,
)
from gate_preparations import (
    _perf_build,
    _cfg_subsets,
    PERF_BUILDS,
    PERF_COUNTER_BUILDS,
    PERF_FEATURES,
    _feature_subsets,
    PERF_FEATURE_BUILDS,
    PERF_CFG_BUILDS,
    _previous_release_check,
    PREVIOUS_RELEASE_CHECKS,
    PERF_BUILD_CATALOG,
    _reviewed_step,
    _PREPARATION_SCRIPTS,
    _no_run,
    _preparation_script_lines,
    _script_preparations,
    windows_preparations,
)
from gate_ci_catalog import (
    CiOnly,
    CI_ONLY_KINDS,
    CI_ONLY,
    _CARGO_ALIASES,
    _CARGO_GATES,
    _INTERPRETERS,
    _ASSIGNMENT,
    _VARIABLE_WORD,
    _SCRIPT_PATH,
    _SCRIPT_MENTION,
    _GATE_MENTION,
    _EXPRESSION,
    _EXPRESSION_WORD,
    _UNMODELED_OPERATORS,
    _SELF_TEST_NAME,
    _BARE_TEST_NAME,
    _CARGO_NAME,
    WORKSPACE_TEST_COMMAND,
)
from gate_commands import (
    Invocation,
    _Unsupported,
    _WORD_END,
    _AMBIGUOUS_ESCAPE,
    _substitution_span,
    _read_word,
    _tokens,
    _simple_commands,
    _script_name,
    _program_name,
    _gate_word,
    _classify_cargo,
    _classify_script,
    _classify_words,
    classify_command,
    is_self_test,
    _KEY,
    _JOB_ID,
    _JOBS_KEY,
    _BLOCK_HEADER,
)
from gate_workflow import (
    _NESTED_RUN,
    _SCALAR_INDICATORS,
    _STEP_INDENT,
    _KEY_INDENT,
    _STEP_SHELLS,
    _indent,
    _unfamiliar,
    job_host,
    _logical_lines,
    _block_body,
    _step_commands,
    ci_job_commands,
    _MACOS_SMOKE_GATES,
    _MACOS_RELEASE_BUILD,
    _MACOS_SMOKE_JOBS,
    macos_smoke_ci_problems,
    _macos_smoke_job_problems,
    _WINDOWS_PERF_GATES,
    _job_body,
    _mandatory_step_index,
    windows_runtime_ci_problems,
    ci_parity_problems,
)
from gate_ci_checks import (
    _test_section_disables,
    RERUN_BACKING_JOBS,
    _WINDOWS_AGGREGATE,
    _AGGREGATE_HEADER,
    aggregate_loop_operands,
    aggregate_checks,
    rerun_backing_problems,
    _rerun_problems,
    ci_only_problems,
)
from gate_docs import (
    GATE_BEGIN,
    GATE_END,
    INVOCATION,
    DOCUMENTS,
    _RENDER_TEXT,
    render_gate_block,
    doc_parity_problems,
)
from gate_process import (
    resolve_program,
    _verified_custody,
    PhaseResult,
    StepResult,
    _step_log_path,
    _copy_output,
    _copy_limited_output,
    _write_header,
    _finish,
    _reap_leader,
    _kill_tree,
    _reap,
    _close,
    _exit_text,
    _WAITID_NAMES,
    _KQUEUE_NAMES,
)
from gate_custody import (
    leader_watches,
    _LEADER_WATCH,
    _LeaderLost,
    _await_leader,
    _group_members,
    _settle_group,
)
from gate_execution import (
    _PhaseSink,
    _phase_status,
    _run_windows_step,
    harness_test_executable,
    harness_test_environment,
    _run_harness_step,
    run_step,
)
from gate_files import (
    GitSnapshot,
    _git,
    _first_line,
    _porcelain_entries,
    _fingerprint,
    _real,
    log_dir_problem,
    sigchld_problem,
    runner_outputs,
    runner_output_problem,
    _output_alias,
    _alias_problem,
    _create_output,
    _write_output,
    _dir_identity,
    _log_dir_replaced,
    snapshot_git,
    _display_path,
    describe_changes,
)
from gate_report import (
    GateReport,
    summary_lines,
    summary_json,
    _say,
    _print_tail,
)
from gate_cli import (
    run_gate,
    detect_host,
    select_steps,
    format_listing,
    main,
)
