#!/usr/bin/env python3
"""Contracts of the performance comparison script, driven entirely by fakes.

No test builds, launches or signals a real process: the process table, the
`lsappinfo` and `footprint` commands, the gate's `run_step` and the clock are
replaced, so the suite runs unchanged on macOS, Windows and Linux.
"""

from __future__ import annotations
import contextlib
import copy
import dataclasses
import hashlib
import importlib.util
import inspect
import io
import json
import os
import re
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest import mock

# Keep the existing script import surface; implementation modules own their state.
from perf_test_fixture import (
    SPEC,
    perf,
)
from perf_statistics_tests import (
    StatisticsTests,
    AbbaScheduleTests,
)
from perf_focus_tests import (
    command,
    FRONT_ARGV,
    FrontApplicationTests,
    reading,
    FocusTheftTests,
)
from perf_processes_tests import (
    FakeProcess,
    FakeTable,
    FakeClock,
    LAUNCH_UNIX_S,
    RECORD_TEXT,
    leader,
    anchor,
    ACKED,
    SessionRecordTests,
    SessionValidationTests,
    SessionCleanupTests,
    ProcessTableDecodingTests,
    LiveProcessTableTests,
    STAMP,
    STAMP_UNIX_S,
)
from perf_memory_tests import (
    memory_line,
    GRID_FIELDS,
    MemoryLineTests,
    RenderTimingTests,
    HARNESS_HASH,
)
from perf_result_tests import (
    without_presenter,
    MACOS_PRESENTER,
    valid_result,
    ResultSchemaTests,
)
from perf_home_tests import (
    HomeWriteTests,
    SELECTION_TABLE,
    HARNESS_TABLES,
    PACKAGE,
    DEPENDENCIES,
    HEAD_MANIFEST,
    BASE_MANIFEST,
)
from perf_build_tests import (
    CargoOverlayTests,
    HarnessHashTests,
    LIST_JSON,
    BuildAndListTests,
    REAL_GATE,
)
from perf_watcher_tests import (
    FakeGate,
    HARNESS_PID,
    write_atlas_retry_api,
    harness_process,
    RunWatcherTests,
    FrontSamplerTests,
    IDLE_SCENARIO,
    STARTUP_OCCLUSION_REASON,
    UNCOUNTED_STOP,
)
from perf_outcomes_tests import (
    WINDOWS_ADAPTER_LINE,
    make_outcome,
    ClassificationTests,
)
from perf_execution_tests import (
    wait_for,
    ExecuteRunTests,
)
from perf_comparison_tests import (
    timed_outcome,
    row_for,
    ComparisonTableTests,
)
from perf_smoke_tests import (
    outcome_of,
    SmokeTests,
)
from perf_sets_tests import (
    RunSetTests,
)
from perf_host_tests import (
    FakeGit,
    WorktreeTests,
    HostBlockTests,
    CliTests,
    RUSTC_VV,
)
from perf_driver_tests import (
    CompareHarness,
    CompareDriverTests,
    StrictBaseTests,
)
from perf_prebuilt_tests import (
    PRODUCER_ENV,
    PrebuiltHarness,
    BuildOnlyTests,
    PrebuiltManifestTests,
    PrebuiltRefusalTests,
    PrebuiltRerunTests,
    PrebuiltCompareTests,
)
from perf_display_tests import (
    display_run,
    no_display,
    DisplayCheckTests,
    AssetCheckTests,
    SmokeSetupTests,
    HomeSymlinkTests,
)
from perf_workflow_fixture import (
    PERF_WORKFLOW,
    ELIGIBILITY,
    PERF_JOBS,
    COMPARISON_JOBS,
    REF_STEP,
    PROFILE_STEP,
    BUILD_STEP,
    PACKAGE_STEP,
    UPLOAD_BINARIES_STEP,
    BUILD_EVIDENCE_STEP,
    PLAN_STEP,
    DOWNLOAD_STEP,
    UNPACK_STEP,
    COMPARE_STEP,
    SUMMARY_STEP,
    EVIDENCE_STEP,
    RESULT_STEP,
    ALL_SCENARIOS,
    PLATFORM_SCENARIOS,
    JOB_RESULTS,
    _YAML_ENTRY,
    _indent,
    _strip_yaml_comment,
    _flow_items,
    _yaml_scalar,
    WorkflowYaml,
    load_perf_workflow,
)
from perf_expression_fixture import (
    job_step,
    UnsupportedExpression,
    _EXPRESSION_TOKEN,
    STATUS_FUNCTIONS,
    IF_REFERENCES,
    VALUE_REFERENCES,
    _ExpressionParser,
    _truthy,
    _uses_status,
    evaluate_expression,
    _lookup,
    condition_holds,
    substitute,
    read_key_values,
)
from perf_workflow_tests import (
    RECORD_ARGV,
    UNEXPECTED_COMMAND,
    run_bash_step,
    PushSimulator,
    WorkflowModelTests,
    FirstReleaseWorkflowTests,
    PlanStepTests,
    PerfResultJobTests,
    EligibilityTests,
)
from perf_workflow_identity_tests import (
    pull_request_github,
    push_github,
    RESULT_NAME,
    RESULT_NOT_RUN,
    ELIGIBILITY_CASES,
    ResultIdentityTests,
    ProfileStepTests,
    comparison_context,
    compare_argv,
    CountersWorkflowTests,
    PrebuiltDownloadTests,
    FAKE_PRODUCER,
    ProducerBuildStepTests,
    EvidenceArtifactTests,
    FAKE_GIT,
    ReleaseRefSelectionTests,
    WindowsComparisonLegTests,
)
from perf_counter_schema_tests import (
    COUNTER_CONTRACT,
    CONTRACT_FIELD_COUNT,
    MILLISECOND_BOUNDS,
    MICROSECOND_BOUNDS,
    frame_counters,
    counters_result,
    FEATURES_TABLE,
    COUNTERS_MANIFEST,
    BASE_COUNTERS_MANIFEST,
    LOGGING_LIB,
    LOGGING_WITH_FILTER,
    LOGGING_WITHOUT_FILTER,
    MISSING,
    FrameCounterSchemaTests,
    GUARD_COUNTERS,
    GuardCounterTests,
    CounterBuildTests,
    CounterCliTests,
)
from perf_counter_rows_tests import (
    counters_side,
    CounterTableTests,
)
from perf_windows_tests import (
    PROGRAM_PID,
    PROGRAM_TEXT,
    FILETIME_EPOCH_OFFSET_S,
    filetime,
    program_table,
    custody,
    member,
    FakeKernel,
    KERNEL_ROWS,
    KERNEL_CREATED,
    WindowsProcessTableTests,
    ProgramRecordTests,
    CustodyCleanupTests,
    LiveWindowsTests,
    PANE_EXIT_REASON,
)
from perf_foreground_tests import (
    HARDWARE_RENDERER,
    WGPU_PRESENTER,
    macos_run,
    windows_run,
    foreground,
    ForegroundTests,
    checkpoint_outcome,
    WindowsTableTests,
)
from perf_delivery_tests import (
    delivery_record,
    unseen_check,
    unseen_record,
    DeliveryResultTests,
    VERIFIED,
    DeliveryReplayTests,
)
from perf_variants_tests import (
    FRAME_TEXTURE_TABLE,
    BOTH_FEATURES_TABLE,
    FRAME_MANIFEST,
    BOTH_MANIFEST,
    CAPPED_LISTING,
    RunCapListTests,
    RunCapCompareTests,
    memory_sample,
    MemoryFreshnessTests,
    GridBytesPerPaneRowTests,
    FrameTextureRowTests,
    FrameTextureFeatureTests,
)
from perf_fallback_tests import (
    FALLBACK_EPOCH_S,
    fallback_stamp,
    font_line,
    receive_enter,
    receive_return,
    dispatch_phase,
    COUNTEREXAMPLE,
    counterexample_lines,
    counterexample_result,
    LapsSelectionTests,
    FallbackLogTests,
    FallbackVerdictTests,
    HOOK_SOURCE,
    HOOK_TABLE,
    HOOK_MANIFEST,
)
from perf_atlas_tests import (
    TRIM_SOURCE,
    TRIM_MANIFEST,
    ALL_FEATURES_TABLE,
    FOUR_FEATURES_MANIFEST,
    FIVE_FEATURES_MANIFEST,
    COUNTERS_HOOK_MANIFEST,
    CHECKPOINT_FIXTURE,
    ATTEMPT_FIXTURE,
    fixture_run,
    fixture_outcome,
    tagged_sample,
    CheckpointMemoryTests,
    atlas_facts,
    atlas_outcome,
    GlyphAtlasReportTests,
    CheckpointFixtureTests,
    CheckpointConflictTests,
)
from perf_hook_tests import (
    HookSourceScanTests,
    CheckpointMemoryFeatureTests,
    CELL_MIB,
)
from perf_cell_layout_tests import (
    cell_layout_run,
    cell_layout_runs,
    PASSING,
    FAILING,
    CellLayoutBoundaryTests,
    CellLayoutAggregationTests,
    CellLayoutOutcomeTests,
    CellLayoutHeadOnlyTests,
    cell_line,
    CellLayoutParsedLineTests,
    CELL_HEAD,
    write_cell_artifact,
    HEAD_OUTCOME,
    CellLayoutArtifactTests,
    CellLayoutDamagedEvidenceTests,
    CellLayoutUndecodableEvidenceTests,
)
from perf_row_cache_tests import (
    RowGlyphCacheReportTests,
)
from perf_latency_tests import (
    split_sample,
    reason_sample,
    split_latency,
    split_problems,
    LIST_WITH_SPLIT,
    split_side,
    LIST_WITH_KINDS,
    DROP,
    kind_phase,
    PhaseKindTests,
    LatencySplitTests,
)
from perf_trim_tests import (
    TrimHookFeatureTests,
    TrimHookResultTests,
    trim_run,
    covered_sample,
    TRIMMED_RENDERER_ENTRY,
    renderer_entries,
    formatted_covered_line,
    TrimExperimentTests,
    ATLAS_RETRY_LIST,
)
from perf_atlas_retry_tests import (
    recovery_records,
    recovery_result,
    AtlasRetryVariantTests,
    PhaseDurationTests,
    CountersOnlyKindTests,
    EntryPointTests,
)
from perf_kind_tests import (
    kind_outcome,
    transition_phase,
    side_with,
    require,
    PhaseKindReportTests,
    flag_result,
    sustained_with,
    CandidateFlagTests,
    RunIdentityTests,
    PhaseKindCoverageTests,
)
from perf_cfg_tests import (
    HarnessCfgDetectionTests,
    TaskbarProgressCfgTests,
    AtlasRetryAvailabilityTests,
    HarnessCfgPrebuiltTests,
    PreviousReleaseCheckTests,
)
from perf_attribution_tests import (
    present_line,
    armed_outcome,
    complete_lines,
    S10AttributionCfgTests,
    attributed_result,
    ARMED_RECORD,
    S10AttributionResultTests,
    S10AttributionLineTests,
    S10AttributionAnalysisTests,
    PRESENT_TEXT,
    S10AttributionConsistencyTests,
    S10AttributionPrebuiltTests,
    CERTIFIED,
)
from perf_completeness_tests import (
    end_point,
    CompletenessCheckpointTests,
)
from perf_timeline_tests import (
    TIMELINE_PARTS,
    TIMELINE_NULLS,
    TIMELINE_READINESS,
    TIMELINE_FALLBACK_PAIR,
    OWN_REASON,
    timeline,
    timeline_sample,
    LAST_FIVE_SPLIT,
    timeline_latency,
    timeline_problems,
    timeline_side,
    producer_split_reason,
    TIMELINE_RULE_CASES,
    population,
    EchoTimelineTests,
    GUARD_NONCE,
    GUARD_HASH,
)
from perf_guard_tests import (
    guard_pane,
    guard_batch,
    guard_run,
    guard_phases,
    guard_rewrite,
    guard_status,
    GuardCorrelationTests,
)
from perf_echo_tests import (
    ECHO_BASE_PARTS,
    ECHO_BASE_SHA,
    ECHO_HEAD_SHA,
    ECHO_SETTINGS,
    ECHO_CAPABILITIES,
    echo_identity,
    control_head,
    echo_entry,
    echo_sample,
    echo_run,
    echo_run_id,
    echo_execution,
    echo_replay,
    echo_control,
    ECHO_BASE,
    ECHO_RENDER,
    EchoAttributionTests,
)
from perf_echo_loader_tests import (
    write_echo_artifact,
    write_json,
    ECHO_ATTEMPTS,
    EchoAttributionLoaderTests,
    ScratchKeepingTests,
    OutputDiagnosticTests,
)

if __name__ == "__main__":
    unittest.main(verbosity=2)
