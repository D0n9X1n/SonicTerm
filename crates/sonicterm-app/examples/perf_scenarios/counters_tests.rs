//! Pins the counter contract: the delta across a closing window, the serialized shape, the
//! modes, shape refusal, the real snapshot API and the feature gate around every API call.

pub(super) use std::collections::HashMap;

use super::*;

/// A reader over `fields`, standing in for one `CounterRecord`.
pub(super) fn reader<'map>(
    fields: &'map HashMap<&'static str, SourceValue>,
) -> impl Fn(&str) -> SourceValue + 'map {
    |name| fields.get(name).cloned().unwrap_or(SourceValue::Absent)
}

/// A window record with `attempts` attempts and one handler time of `handler_us`.
pub(super) fn window_record(attempts: u64, handler_us: u64) -> HashMap<&'static str, SourceValue> {
    let mut counts = vec![0; Unit::Millis.bounds().len() + 1];
    counts[0] = 1;
    HashMap::from([
        ("attempts", SourceValue::Count(attempts)),
        ("shape_requests", SourceValue::Count(attempts * 2)),
        (
            "handler",
            SourceValue::Histogram {
                unit: "ms",
                bounds: Unit::Millis.bounds().to_vec(),
                counts,
                sum_us: handler_us,
            },
        ),
    ])
}

/// Totals over `live` windows and the `closed` record, as `snapshot_totals` builds them.
pub(super) fn totals(
    live: &[HashMap<&'static str, SourceValue>],
    closed: &HashMap<&'static str, SourceValue>,
) -> CounterTotals {
    let mut totals = CounterTotals::zero();
    for record in live.iter().chain(std::iter::once(closed)) {
        totals.add_record(&[Section::Window, Section::Renderer], reader(record)).unwrap();
    }
    totals
}

#[test]
pub(super) fn a_window_closing_between_snapshots_moves_through_closed_windows_and_nothing_goes_negative(
) {
    // At the start window A is live; by the end A has closed into closed_windows with more
    // counts and B has opened. The delta is the growth across both, never negative.
    let start = totals(&[window_record(5, 1_000)], &HashMap::new());
    let end = totals(&[window_record(2, 400)], &window_record(7, 3_000));
    let delta = end.delta_since(&start);
    assert_eq!(delta.get("attempts"), Some(&FieldValue::Count(4)));
    assert_eq!(delta.get("shape_requests"), Some(&FieldValue::Count(8)));
    let Some(FieldValue::Histogram { counts, sum_us }) = delta.get("handler_ms") else {
        panic!("handler_ms is a histogram");
    };
    assert_eq!((counts[0], counts.iter().sum::<u64>(), *sum_us), (1, 1, 2_400));
    // A field that fell would read zero, not wrap.
    assert_eq!(start.delta_since(&end).get("attempts"), Some(&FieldValue::Count(0)));
    assert_eq!(end.delta_since(&end), CounterTotals::zero());
}

/// Each contract section's fields, as developer B's comparison reads them.
pub(super) const CONTRACT: &[(&str, &[&str])] = &[
    (
        "window",
        &[
            "attempts",
            "presented",
            "cached",
            "settled",
            "retry",
            "surface_retry",
            "stopped",
            "failed",
            "contention_parser",
            "contention_images",
            "defer_timeout",
            "defer_contention",
            "defer_sync",
            "defer_streaming",
            "stream_clock_exempt",
            "display_link_ticks",
            "display_link_admissions",
            "display_link_fallbacks",
            "contention_retry_armed",
            "dirt_ack_dropped",
            "native_request_redraw",
            "user_request_redraw",
            "redraw_requested",
            "present_interval_ms",
            "handler_ms",
            "flush_to_redraw_ms",
        ],
    ),
    (
        "app",
        &[
            "wake_init",
            "wake_poll",
            "wake_wait_cancelled",
            "wake_resume_time",
            "wake_user",
            "ui_parser_locks",
            "fg_probe_calls",
            "fg_probe_panes",
            "fg_worker_probes",
            "fg_worker_panes",
            "fg_results_stale",
            "native_request_redraw_unregistered",
            "ui_guard_custody_ns",
            "ui_guard_custodies",
            "frame_dispatch_ns",
            "frame_dispatches",
            "frame_dispatch_failed_ns",
            "frame_dispatches_failed",
            "hover_lookups",
            "hover_detections",
            "hover_cache_hits",
            "hover_rows_read",
            "path_probes_submitted",
            "quick_select_rows_scanned",
            "status_line_samples",
            "status_line_wakes",
            "status_line_redraws",
            "status_line_sel_counts",
            "status_line_sel_cells",
            "status_line_history_shows",
            "status_line_held_shows",
            "status_line_glyph_shows",
            "program_progress_events",
            "program_notify_events",
            "program_toast_redraws",
            "taskbar_progress_deliveries",
            "status_line_tree_builds",
            "status_line_full_passes",
            "startup_window_ns",
            "startup_adapter_install_ns",
            "startup_renderer_ns",
            "startup_load_checks_ns",
            "startup_pane_spawn_ns",
            "about_to_wait_ms",
            "user_event_ms",
            "new_events_ms",
            "ui_parser_wait_us",
            "fg_probe_us",
            "fg_worker_probe_us",
        ],
    ),
    (
        "vt",
        &[
            "parse_bytes",
            "batches",
            "flushes",
            "flushes_untargeted",
            "flushes_coalesced",
            "flushes_suppressed",
            "sync_timeouts",
            "parser_lock_wait_ns",
            "parser_sections",
            "parser_lock_wait_us",
            "parser_lock_hold_us",
            "parse_us",
        ],
    ),
    (
        "renderer",
        &[
            "vertex_bytes",
            "index_bytes",
            "damage_permille_sum",
            "damaged_frames",
            "damage_waste_permille_sum",
            "software_frames",
            "gpu_frames",
            "row_cache_hits",
            "row_cache_misses",
            "shape_requests",
            "full_frames",
            "partial_frames",
            "partial_fallbacks",
            "row_cells_hashed",
            "row_cache_invalidate_visits",
            "row_cache_invalidate_us",
            "recolor_glyphs_visited",
            "font_fallback_applies",
            "shape_ns",
            "raster_ns",
            "raster_calls",
            "raster_tiles",
            "font_generation_applies",
            "font_prepare_ns",
            "font_generation_prepare_ns",
            "tab_title_reuses",
            "tab_title_prepares",
            "chrome_run_reuses",
            "chrome_run_prepares",
            "render_attempts",
            "render_attempts_presented",
            "render_attempt_ns",
            "render_attempt_shape_ns",
            "render_attempt_raster_ns",
            "render_attempt_shape_requests",
            "render_attempt_raster_calls",
            "render_attempt_raster_tiles",
            "apply_attempts",
            "apply_attempts_presented",
            "apply_attempt_ns",
            "apply_attempt_shape_ns",
            "apply_attempt_raster_ns",
            "apply_attempt_shape_requests",
            "apply_attempt_raster_calls",
            "apply_attempt_raster_tiles",
            "assembly_us",
            "glyph_atlas_growths",
            "atlas_growth_abandoned",
            "atlas_growth_to_present_ms",
        ],
    ),
];

#[test]
pub(super) fn every_contract_field_serializes_present_and_zero_with_the_right_histogram_shape() {
    // A field with no events is 0, never absent; a histogram is unit, bounds, counts one
    // longer than bounds (overflow last) and the exact integer sum_us, with no other key.
    let document = CounterTotals::zero().to_json();
    let sections = document.as_object().expect("an object");
    assert_eq!(sections.len(), CONTRACT.len());
    for (section, names) in CONTRACT {
        let fields = document[*section].as_object().unwrap_or_else(|| panic!("{section}"));
        let mut listed: Vec<&str> = fields.keys().map(String::as_str).collect();
        let mut expected = names.to_vec();
        listed.sort_unstable();
        expected.sort_unstable();
        assert_eq!(listed, expected, "{section}");
        for name in *names {
            let value = &fields[*name];
            let unit = name.rsplit('_').next().filter(|suffix| ["ms", "us"].contains(suffix));
            let is_histogram = unit.is_some() && value.is_object();
            if !is_histogram {
                assert_eq!(value, &json!(0), "{section}.{name}");
                continue;
            }
            let histogram = value.as_object().unwrap();
            let mut keys: Vec<&str> = histogram.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["bounds", "counts", "sum_us", "unit"], "{name}");
            let bounds: &[u64] = if unit == Some("ms") {
                &[4, 7, 9, 12, 17, 25, 34, 50, 100]
            } else {
                &[10, 50, 100, 500, 1_000, 5_000]
            };
            assert_eq!(histogram["unit"], json!(unit), "{name}");
            assert_eq!(histogram["bounds"], json!(bounds), "{name}");
            assert_eq!(histogram["counts"], json!(vec![0; bounds.len() + 1]), "{name}");
            assert_eq!(histogram["sum_us"], json!(0), "{name}");
        }
    }
    // Every field ending in _ms or _us is a histogram; the names above with those suffixes
    // are exactly the fourteen contract histograms (the retired event-loop fg_probe_us beside the
    // worker's fg_worker_probe_us, and the renderer's atlas_growth_to_present_ms);
    // row_cache_invalidate_us is a plain sum.
    let histograms = FIELDS.iter().filter(|field| matches!(field.kind, FieldKind::Histogram(_)));
    assert_eq!(histograms.count(), 14);
}

#[test]
pub(super) fn the_mode_names_the_build_and_the_request() {
    // Built without the API a run is "unsupported"; with it, --counters chooses on or off.
    let expected = if cfg!(feature = "perf-counters") {
        ("off", "on")
    } else {
        ("unsupported", "unsupported")
    };
    assert_eq!(
        (CountersMode::for_run(false).as_str(), CountersMode::for_run(true).as_str()),
        expected
    );
    assert_eq!(serde_json::to_value(CountersMode::On).unwrap(), json!("on"));
}

#[test]
pub(super) fn a_source_field_of_another_shape_is_refused() {
    // The harness never coerces a field: a count where a histogram belongs, or a histogram in
    // another unit, is an error naming the field.
    let mut totals = CounterTotals::zero();
    let wrong_kind = HashMap::from([("handler", SourceValue::Count(3))]);
    let error = totals.add_record(&[Section::Window], reader(&wrong_kind)).unwrap_err();
    assert!(error.contains("handler_ms"), "{error}");
    let wrong_unit = HashMap::from([(
        "parse",
        SourceValue::Histogram {
            unit: "ms",
            bounds: Unit::Millis.bounds().to_vec(),
            counts: vec![0; 10],
            sum_us: 0,
        },
    )]);
    assert!(totals.add_record(&[Section::VtParser], reader(&wrong_unit)).is_err());
}

#[cfg(feature = "perf-counters")]
#[test]
pub(super) fn the_real_snapshot_api_supplies_every_window_app_and_vt_field() {
    // Every contract field outside the renderer section has a source in a live window's or the
    // App's record; renderer fields appear only with a renderer, which a unit test lacks. A
    // child reaped between two snapshots leaves a delta of zeros, never a negative.
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut app =
        sonicterm_app::app::App::new(Theme::default(), Config::default(), Keymap::default());
    enable(&mut app).expect("no window yet");
    let start = snapshot_totals(&app).expect("counting").expect("contract shape");
    let child = app.__test_seed_child_window(&[]);
    let snapshot = app.frame_counters_snapshot().expect("counting");
    let (_, window) = snapshot.windows.iter().find(|(id, _)| *id == child).expect("child");
    for field in FIELDS {
        let record = match field.section {
            Section::Window => window,
            Section::App | Section::VtParser => &snapshot.app,
            Section::Renderer => continue,
        };
        assert_ne!(
            read_field(record, field.source),
            SourceValue::Absent,
            "{} has no source",
            field.name
        );
    }
    app.__test_invoke_reap_empty_child(child);
    let end = snapshot_totals(&app).expect("counting").expect("contract shape");
    // Window fields count from zero (no window existed at the start); renderer fields stay
    // unsupported, since no window here has a renderer.
    let delta = end.delta_since(&start);
    for (field, value) in FIELDS.iter().zip(&delta.values) {
        match field.section {
            Section::Renderer => assert_eq!(value, &None, "{}", field.name),
            _ => assert_eq!(value, &Some(FieldValue::zero(field.kind)), "{}", field.name),
        }
    }
    assert!(enable(&mut app).is_err(), "a window exists, so forcing the gate is refused");
}

/// The counter API calls the harness may make only behind the feature.
pub(super) const GATED_CALLS: &[&str] = &[
    "force_frame_counters_on",
    "frame_counters_snapshot",
    "histogram_buckets",
    "CounterRecord",
    "counters::enable",
    "snapshot_totals(",
    "init_in_with_filter",
    "gate_on(",
];

/// The counter API's gate.
pub(super) const COUNTERS_GATE: &str = "#[cfg(feature = \"perf-counters\")]";

/// The echo-watch API's gate.
pub(super) const ECHO_TRACE_GATE: &str = "#[cfg(feature = \"perf-echo-trace\")]";

/// The echo-watch API the harness may name only behind `perf-echo-trace`: its calls and every type
/// a base without the watch lacks.
pub(super) const ECHO_TRACE_CALLS: &[&str] = &[
    "arm_echo_watch",
    "take_echo_watch",
    "ArmToken",
    "EchoTrace",
    "ArmOutcome",
    "TakeOutcome",
    "EchoWatchTarget",
    "EchoRowIdentity",
    "EchoDeliveryOutcome",
];

/// The gate of S1/atlas-retry's App and renderer API: the harness-only cfg perf-compare sets on both refs or neither.
pub(super) const ATLAS_RETRY_GATE: &str = "#[cfg(perf_atlas_retry_api)]";

/// The App and renderer API S1/atlas-retry's driver may name only behind its cfg; none exists in v1.3.8.
pub(super) const ATLAS_RETRY_CALLS: &[&str] = &[
    "__change_glyph_atlas_during_next_assembly",
    "__test_window_active_tab_title",
    "last_missing_chrome",
    "font_fallback_notice_id",
];

/// The gate of the App's S10 attribution API: the harness-only cfg perf-compare sets on both sides or neither.
pub(super) const ATTRIBUTION_GATE: &str =
    "#[cfg(all(perf_s10_attribution_api, any(target_os = \"macos\", windows)))]";

/// The App API the harness may name only behind the attribution cfg: the watch methods and the parser
/// read the baseline depends on, which a tree before the API does not have.
pub(super) const ATTRIBUTION_CALLS: &[&str] =
    &["arm_s10_attribution", "disarm_s10_attribution", "synchronized_output", "SyncState"];
/// The gate of the renderer's perf-end completeness checkpoint.
pub(super) const COMPLETENESS_GATE: &str = "#[cfg(perf_completeness_api)]";

/// The renderer and harness API the completeness reading may name only behind its cfg; v1.3.8's
/// renderer has no checkpoint.
pub(super) const COMPLETENESS_CALLS: &[&str] =
    &["completeness_checkpoint", "completeness_from_checkpoint"];

/// The gate of the App's dispatch-timeline API: the harness-only cfg perf-compare sets on both sides or neither.
pub(super) const DISPATCH_TIMELINE_GATE: &str = "#[cfg(perf_dispatch_timeline_api)]";

/// The App API the harness may name only behind the dispatch-timeline cfg: the arm and take methods and
/// their types, none of which exists in a tree before the prerequisite.
pub(super) const DISPATCH_TIMELINE_CALLS: &[&str] = &[
    "arm_dispatch_timeline_v1",
    "take_dispatch_timeline_v1",
    "DispatchTimelineToken",
    "DispatchTimelineV1",
    "DispatchTimelineArmV1",
    "DispatchTimelineTakeV1",
    "AppearanceTargetV1",
];

/// The gate of the App's V1 echo-timeline accessor: the echo trace feature and the harness-only cfg.
pub(super) const TIMELINE_GATE: &str =
    "#[cfg(all(feature = \"perf-echo-trace\", perf_echo_timeline_api))]";

/// The App API the harness may name only behind the timeline gate; v1.3.8 has none of it.
pub(super) const TIMELINE_CALLS: &[&str] = &[
    "take_echo_timeline_v1",
    "EchoTimelineTakeV1",
    "EchoTimelineKindV1",
    "EchoTimelineV1",
    "AdmissionDecisionV1",
    "OutputCheckOutcomeV1",
    "TickIdentityV1",
];

/// The gate of the App's guard-correlation take: the harness-only cfg perf-compare sets on both sides or neither.
pub(super) const GUARD_SPANS_GATE: &str = "#[cfg(perf_guard_spans_api)]";

/// The App API the harness may name only behind the guard-correlation cfg: the take, the shared clock epoch
/// and every V1 type, none of which a tree before the prerequisite has.
pub(super) const GUARD_SPANS_CALLS: &[&str] = &[
    "take_guard_correlation_v1",
    "clock_epoch",
    "GuardCorrelationTakeV1",
    "GuardCorrelationV1",
    "PaneSectionsV1",
    "SpanBatchV1",
    "SectionRecordV1",
    "SpanRecordV1",
    "PendingSectionV1",
    "AbandonedSectionV1",
    "LossIntervalV1",
];

/// The trim hook's gate.
pub(super) const TRIM_HOOK_GATE: &str = "#[cfg(feature = \"perf-hook-trim\")]";

/// The trim hook API the harness may name only behind `perf-hook-trim`: the method and its result types.
pub(super) const TRIM_HOOK_CALLS: &[&str] = &["__trim_covered_now", "TrimDecision", "TrimSkip"];

/// `text` with every comment, and with `strings` every string, raw string and character literal, replaced by
/// spaces of the same byte length; line breaks are kept, so byte offsets and line numbers still match `text`.
pub(super) fn blank_non_code(text: &str, strings: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let identifier = |character: char| character.is_alphanumeric() || character == '_';
    let mut out = String::with_capacity(text.len());
    let blank = |character: char, out: &mut String| {
        if character == '\n' {
            out.push('\n');
        } else {
            out.extend(std::iter::repeat_n(' ', character.len_utf8()));
        }
    };
    let keep = |character: char, out: &mut String| {
        if strings {
            blank(character, out);
        } else {
            out.push(character);
        }
    };
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied();
        let raw_start = current == 'r'
            && matches!(next, Some('#' | '"'))
            && (index == 0 || !identifier(chars[index - 1]) || chars[index - 1] == 'b');
        if current == '/' && next == Some('/') {
            // When: a line comment starts, everything up to the line break is blanked.
            while index < chars.len() && chars[index] != '\n' {
                blank(chars[index], &mut out);
                index += 1;
            }
        } else if current == '/' && next == Some('*') {
            // When: a block comment starts, it is blanked through its matching close, nested ones included.
            let mut depth = 0_usize;
            while index < chars.len() {
                let pair = (chars[index], chars.get(index + 1).copied());
                if pair == ('/', Some('*')) || pair == ('*', Some('/')) {
                    depth = if pair.0 == '/' { depth + 1 } else { depth - 1 };
                    blank(chars[index], &mut out);
                    blank(chars[index + 1], &mut out);
                    index += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    blank(chars[index], &mut out);
                    index += 1;
                }
            }
        } else if raw_start {
            // When: a raw string starts, its contents run to the quote followed by as many hashes as opened it.
            out.push('r');
            index += 1;
            let hashes = chars[index..].iter().take_while(|character| **character == '#').count();
            out.extend(std::iter::repeat_n('#', hashes));
            index += hashes;
            if chars.get(index) == Some(&'"') {
                out.push('"');
                index += 1;
                while index < chars.len() {
                    let closes = chars[index] == '"'
                        && chars[index + 1..]
                            .iter()
                            .take(hashes)
                            .filter(|character| **character == '#')
                            .count()
                            == hashes;
                    if closes {
                        out.push('"');
                        out.extend(std::iter::repeat_n('#', hashes));
                        index += 1 + hashes;
                        break;
                    }
                    keep(chars[index], &mut out);
                    index += 1;
                }
            }
        } else if current == '"' {
            // When: a string starts, its contents run to the next unescaped quote.
            out.push('"');
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    keep(chars[index], &mut out);
                    if let Some(escaped) = chars.get(index + 1) {
                        keep(*escaped, &mut out);
                    }
                    index += 2;
                    continue;
                }
                if chars[index] == '"' {
                    out.push('"');
                    index += 1;
                    break;
                }
                keep(chars[index], &mut out);
                index += 1;
            }
        } else if current == '\'' && (next == Some('\\') || chars.get(index + 2) == Some(&'\'')) {
            // When: a character literal starts (not a lifetime), it runs to its closing quote.
            out.push('\'');
            index += 1;
            while index < chars.len() && chars[index] != '\'' {
                let escaped = chars[index] == '\\';
                keep(chars[index], &mut out);
                index += 1;
                if escaped && index < chars.len() {
                    keep(chars[index], &mut out);
                    index += 1;
                }
            }
            if index < chars.len() {
                out.push('\'');
                index += 1;
            }
        } else {
            out.push(current);
            index += 1;
        }
    }
    out
}

/// Each of `calls` in `text` outside an item gated by `gate`, by line. A gate counts only in code, never in a
/// comment or a string literal, while a string argument inside a real gate is kept; a gated body is balanced in
/// code with comments and literals blanked, so no gate's text and no brace in a comment or a string can hide a call.
pub(super) fn ungated_calls(text: &str, gate: &str, calls: &[&str]) -> Vec<String> {
    // A CRLF checkout is read as LF, so line numbers match either way.
    let text = &text.replace("\r\n", "\n");
    let code = blank_non_code(text, false);
    let structure = blank_non_code(text, true);
    let mut gated = Vec::new();
    for (offset, _) in code.match_indices(gate) {
        if structure.as_bytes()[offset] != b'#' {
            // When: the gate's `#` is blanked with the literals, its text sits inside a string and guards nothing.
            continue;
        }
        let after = offset + gate.len();
        let open = after + structure[after..].find('{').expect("a gated item has a body");
        let mut depth = 0_usize;
        for (index, character) in structure[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        gated.push(offset..open + index + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    for call in calls {
        for (offset, _) in code.match_indices(call) {
            if !gated.iter().any(|range| range.contains(&offset)) {
                found.push(format!("{}: {call}", code[..offset].matches('\n').count() + 1));
            }
        }
    }
    found
}

/// The window, app, vt and renderer fields a base without the newest counters cannot read, by API name.
pub(super) const NEWER_SOURCES: &[&str] = &[
    "sync_timeouts",
    "defer_sync",
    "stream_clock_exempt",
    "display_link_ticks",
    "display_link_admissions",
    "display_link_fallbacks",
    "dirt_ack_dropped",
    "damage_waste_permille_sum",
    "full_frames",
    "partial_frames",
    "partial_fallbacks",
    "row_cells_hashed",
    "row_cache_invalidate_visits",
    "row_cache_invalidate_us",
    "recolor_glyphs_visited",
    "font_fallback_applies",
    "shape_ns",
    "raster_ns",
    "raster_calls",
    "raster_tiles",
    "font_generation_applies",
    "font_prepare_ns",
    "font_generation_prepare_ns",
    "tab_title_reuses",
    "tab_title_prepares",
    "chrome_run_reuses",
    "chrome_run_prepares",
    "render_attempts",
    "render_attempts_presented",
    "render_attempt_ns",
    "render_attempt_shape_ns",
    "render_attempt_raster_ns",
    "render_attempt_shape_requests",
    "render_attempt_raster_calls",
    "render_attempt_raster_tiles",
    "apply_attempts",
    "apply_attempts_presented",
    "apply_attempt_ns",
    "apply_attempt_shape_ns",
    "apply_attempt_raster_ns",
    "apply_attempt_shape_requests",
    "apply_attempt_raster_calls",
    "apply_attempt_raster_tiles",
    "assembly",
    "glyph_atlas_growths",
    "atlas_growth_abandoned",
    "atlas_growth_to_present",
    "hover_lookups",
    "hover_detections",
    "hover_cache_hits",
    "hover_rows_read",
    "path_probes_submitted",
    "quick_select_rows_scanned",
    "status_line_samples",
    "status_line_wakes",
    "status_line_redraws",
    "status_line_sel_counts",
    "status_line_sel_cells",
    "status_line_history_shows",
    "status_line_held_shows",
    "status_line_glyph_shows",
    "program_progress_events",
    "program_notify_events",
    "program_toast_redraws",
    "taskbar_progress_deliveries",
    "status_line_tree_builds",
    "status_line_full_passes",
    "startup_window_ns",
    "startup_adapter_install_ns",
    "startup_renderer_ns",
    "startup_load_checks_ns",
    "startup_pane_spawn_ns",
];

/// The fixture perf-compare's attempt-split test reads, written by this harness.
pub(super) const ATTEMPT_FIXTURE: &str = "../../scripts/perf-compare_attempt_fixture.json";

/// A renderer record whose two attempt classes are both at these cumulative totals: attempts,
/// presented, attempt ns, shape ns, raster ns, shape requests, raster calls and raster tiles.
pub(super) fn attempt_record(totals: [u64; 8]) -> HashMap<&'static str, SourceValue> {
    let roles = [
        "attempts",
        "attempts_presented",
        "attempt_ns",
        "attempt_shape_ns",
        "attempt_raster_ns",
        "attempt_shape_requests",
        "attempt_raster_calls",
        "attempt_raster_tiles",
    ];
    let mut record = HashMap::new();
    for prefix in ["render_", "apply_"] {
        for (role, value) in roles.iter().zip(totals) {
            let name: &'static str = Box::leak(format!("{prefix}{role}").into_boxed_str());
            record.insert(name, SourceValue::Count(value));
        }
    }
    record
}
