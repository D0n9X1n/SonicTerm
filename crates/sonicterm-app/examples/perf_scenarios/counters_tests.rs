//! Pins the counter contract: the delta across a closing window, the serialized shape, the
//! modes, shape refusal, the real snapshot API and the feature gate around every API call.

use std::collections::HashMap;

use super::*;

/// A reader over `fields`, standing in for one `CounterRecord`.
fn reader<'map>(
    fields: &'map HashMap<&'static str, SourceValue>,
) -> impl Fn(&str) -> SourceValue + 'map {
    |name| fields.get(name).cloned().unwrap_or(SourceValue::Absent)
}

/// A window record with `attempts` attempts and one handler time of `handler_us`.
fn window_record(attempts: u64, handler_us: u64) -> HashMap<&'static str, SourceValue> {
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
fn totals(
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
fn a_window_closing_between_snapshots_moves_through_closed_windows_and_nothing_goes_negative() {
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
const CONTRACT: &[(&str, &[&str])] = &[
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
fn every_contract_field_serializes_present_and_zero_with_the_right_histogram_shape() {
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
fn the_mode_names_the_build_and_the_request() {
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
fn a_source_field_of_another_shape_is_refused() {
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
fn the_real_snapshot_api_supplies_every_window_app_and_vt_field() {
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
const GATED_CALLS: &[&str] = &[
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
const COUNTERS_GATE: &str = "#[cfg(feature = \"perf-counters\")]";

/// The echo-watch API's gate.
const ECHO_TRACE_GATE: &str = "#[cfg(feature = \"perf-echo-trace\")]";

/// The echo-watch API the harness may name only behind `perf-echo-trace`: its calls and every type
/// a base without the watch lacks.
const ECHO_TRACE_CALLS: &[&str] = &[
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
const ATLAS_RETRY_GATE: &str = "#[cfg(perf_atlas_retry_api)]";

/// The App and renderer API S1/atlas-retry's driver may name only behind its cfg; none exists in v1.3.8.
const ATLAS_RETRY_CALLS: &[&str] = &[
    "__change_glyph_atlas_during_next_assembly",
    "__test_window_active_tab_title",
    "last_missing_chrome",
    "font_fallback_notice_id",
];

/// The gate of the App's S10 attribution API: the harness-only cfg perf-compare sets on both sides or neither.
const ATTRIBUTION_GATE: &str =
    "#[cfg(all(perf_s10_attribution_api, any(target_os = \"macos\", windows)))]";

/// The App API the harness may name only behind the attribution cfg: the watch methods and the parser
/// read the baseline depends on, which a tree before the API does not have.
const ATTRIBUTION_CALLS: &[&str] =
    &["arm_s10_attribution", "disarm_s10_attribution", "synchronized_output", "SyncState"];
/// The gate of the renderer's perf-end completeness checkpoint.
const COMPLETENESS_GATE: &str = "#[cfg(perf_completeness_api)]";

/// The renderer and harness API the completeness reading may name only behind its cfg; v1.3.8's
/// renderer has no checkpoint.
const COMPLETENESS_CALLS: &[&str] = &["completeness_checkpoint", "completeness_from_checkpoint"];

/// The gate of the App's dispatch-timeline API: the harness-only cfg perf-compare sets on both sides or neither.
const DISPATCH_TIMELINE_GATE: &str = "#[cfg(perf_dispatch_timeline_api)]";

/// The App API the harness may name only behind the dispatch-timeline cfg: the arm and take methods and
/// their types, none of which exists in a tree before the prerequisite.
const DISPATCH_TIMELINE_CALLS: &[&str] = &[
    "arm_dispatch_timeline_v1",
    "take_dispatch_timeline_v1",
    "DispatchTimelineToken",
    "DispatchTimelineV1",
    "DispatchTimelineArmV1",
    "DispatchTimelineTakeV1",
    "AppearanceTargetV1",
];

/// The gate of the App's V1 echo-timeline accessor: the echo trace feature and the harness-only cfg.
const TIMELINE_GATE: &str = "#[cfg(all(feature = \"perf-echo-trace\", perf_echo_timeline_api))]";

/// The App API the harness may name only behind the timeline gate; v1.3.8 has none of it.
const TIMELINE_CALLS: &[&str] = &[
    "take_echo_timeline_v1",
    "EchoTimelineTakeV1",
    "EchoTimelineKindV1",
    "EchoTimelineV1",
    "AdmissionDecisionV1",
    "OutputCheckOutcomeV1",
    "TickIdentityV1",
];

/// The trim hook's gate.
const TRIM_HOOK_GATE: &str = "#[cfg(feature = \"perf-hook-trim\")]";

/// The trim hook API the harness may name only behind `perf-hook-trim`: the method and its result types.
const TRIM_HOOK_CALLS: &[&str] = &["__trim_covered_now", "TrimDecision", "TrimSkip"];

/// `text` with every comment, and with `strings` every string, raw string and character literal, replaced by
/// spaces of the same byte length; line breaks are kept, so byte offsets and line numbers still match `text`.
fn blank_non_code(text: &str, strings: bool) -> String {
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
fn ungated_calls(text: &str, gate: &str, calls: &[&str]) -> Vec<String> {
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

#[test]
fn every_counter_api_call_in_the_harness_is_behind_the_feature() {
    // perf-compare overlays this harness onto a base whose App has no counter API, so an
    // ungated call would not compile there.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap().replace("\r\n", "\n");
        let found = ungated_calls(&source, COUNTERS_GATE, GATED_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    // Negative fixture: the same call outside a gated item is reported.
    let fixture = "#[cfg(feature = \"perf-counters\")]\nfn on(app: &mut App) {\n    app.force_frame_counters_on();\n}\n\nfn off(app: &mut App) {\n    let _ = app.frame_counters_snapshot();\n}\n";
    assert_eq!(
        ungated_calls(fixture, COUNTERS_GATE, GATED_CALLS),
        vec!["7: frame_counters_snapshot".to_owned()]
    );
}

#[test]
fn every_echo_trace_call_in_the_harness_is_behind_its_feature() {
    // perf-compare overlays this harness onto a base whose App has no echo watch, so an ungated
    // reference to the watch's API would not compile there.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap().replace("\r\n", "\n");
        let found = ungated_calls(&source, ECHO_TRACE_GATE, ECHO_TRACE_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    // Negative fixture: an arm outside the gated item is reported; the gated one is not.
    let fixture = "#[cfg(feature = \"perf-echo-trace\")]\nmod on {\n    fn arm(app: &mut App) {\n        app.take_echo_watch(1, token);\n    }\n}\n\nfn off(app: &mut App) {\n    app.arm_echo_watch(1, target);\n}\n";
    assert_eq!(
        ungated_calls(fixture, ECHO_TRACE_GATE, ECHO_TRACE_CALLS),
        vec!["9: arm_echo_watch".to_owned()]
    );
}

#[test]
fn every_trim_hook_call_in_the_harness_is_behind_its_feature() {
    // perf-compare overlays this harness onto a base whose App has no trim hook, so an ungated
    // reference to the hook or its result types would not compile there.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap().replace("\r\n", "\n");
        let found = ungated_calls(&source, TRIM_HOOK_GATE, TRIM_HOOK_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    // Negative fixture: a hook call outside the gated item is reported; the gated one is not.
    let fixture = "#[cfg(feature = \"perf-hook-trim\")]\nfn on(app: &mut App) {\n    app.__trim_covered_now(window);\n}\n\nfn off(app: &mut App) {\n    let _ = app.__trim_covered_now(window);\n}\n";
    assert_eq!(
        ungated_calls(fixture, TRIM_HOOK_GATE, TRIM_HOOK_CALLS),
        vec!["7: __trim_covered_now".to_owned()]
    );
}

/// A feature-on build whose App never turned the counter gate on arms nothing: every credited
/// sample reads `arm-gate-off`, through the same arm, take and split path the probe runs.
#[cfg(feature = "perf-echo-trace")]
#[test]
fn gate_off_run_reads_arm_gate_off_for_every_credited_sample() {
    use crate::record::{echo_api, echo_outcome, echo_target, LatencySample, RowIdentity};
    let mut app = sonicterm_app::app::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let pane = app.__test_seed_tab("s2");
    let injected = std::time::Instant::now();
    let samples: Vec<_> = (0..5_u32)
        .map(|index| {
            let target = echo_target((0, 6), 80, index);
            let arm = echo_api::arm(&mut app, pane, &target, RowIdentity::default());
            let outcome = echo_outcome(arm, |token| echo_api::take(&mut app, pane, token, None).0);
            let ended = injected + std::time::Duration::from_millis(u64::from(index) + 1);
            LatencySample::credited(f64::from(index), injected, ended, &outcome, false)
        })
        .collect();
    assert!(samples.iter().all(|sample| sample.split_reason == "arm-gate-off"), "{samples:?}");
    assert!(samples.iter().all(|sample| sample.split.is_none() && sample.latency_ms.is_some()));
}

/// The window, vt and renderer fields a base without the newest counters cannot read, by API name.
const NEWER_SOURCES: &[&str] = &[
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
];

#[test]
fn a_field_the_base_cannot_read_is_omitted_not_reported_as_zero() {
    // perf-compare overlays this harness onto an older base whose records lack the newer
    // window, vt and renderer counters. Those keys must be absent, so the comparison shows n/a, while every
    // field the base does report is still written, through the snapshot path and the delta.
    let mut record = HashMap::new();
    for field in FIELDS {
        if NEWER_SOURCES.contains(&field.source) {
            continue;
        }
        let value = match field.kind {
            FieldKind::Count => SourceValue::Count(1),
            FieldKind::Histogram(unit) => SourceValue::Histogram {
                unit: unit.name(),
                bounds: unit.bounds().to_vec(),
                counts: vec![0; unit.bounds().len() + 1],
                sum_us: 0,
            },
        };
        record.insert(field.source, value);
    }
    let mut start = CounterTotals::unsupported();
    let mut end = CounterTotals::unsupported();
    for totals in [&mut start, &mut end] {
        totals.add_record(&[Section::Window, Section::Renderer], reader(&record)).unwrap();
        totals.add_record(&[Section::App, Section::VtParser], reader(&record)).unwrap();
    }
    for document in [end.to_json(), end.delta_since(&start).to_json()] {
        for field in FIELDS {
            let present = document[field.section.key()].get(field.name).is_some();
            let expected = !NEWER_SOURCES.contains(&field.source);
            assert_eq!(present, expected, "{}.{}: {document}", field.section.key(), field.name);
        }
    }
}

#[test]
fn a_gate_that_disagrees_with_the_reported_mode_is_named() {
    // The report must describe the App that ran: a gate on in an "off" run, or off in an "on"
    // run, voids the run; agreement, and a build with no counter API, pass.
    assert!(gate_mismatch(CountersMode::Off, true).is_some_and(|reason| reason.contains("off")));
    assert!(gate_mismatch(CountersMode::On, false).is_some_and(|reason| reason.contains("on")));
    assert_eq!(gate_mismatch(CountersMode::On, true), None);
    assert_eq!(gate_mismatch(CountersMode::Off, false), None);
    assert_eq!(gate_mismatch(CountersMode::Unsupported, false), None);
}

#[cfg(feature = "perf-counters")]
#[test]
fn a_laps_run_without_counters_has_its_gate_off_and_reports_off() {
    // The App reads its gate from the log filter when it is built; under the harness's laps
    // filter it must count nothing, matching the "off" the run reports.
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let filter = crate::workload::logging_filter(true, false).unwrap_or_else(|| {
        sonicterm_logging::filter_for_level(sonicterm_logging::LogLevel::Debug).to_owned()
    });
    let subscriber = Registry::default().with(EnvFilter::new(filter));
    let app = sonicterm_logging::test_capture::with_default(subscriber, || {
        sonicterm_app::app::App::new(Theme::default(), Config::default(), Keymap::default())
    });
    let mode = CountersMode::for_run(false);
    assert_eq!(mode, CountersMode::Off);
    assert!(!gate_on(&app), "the laps run's App counts with --counters off");
    assert_eq!(gate_mismatch(mode, gate_on(&app)), None);
}

#[test]
fn the_feature_gate_scan_reads_a_crlf_checkout_as_it_reads_an_lf_one() {
    // Windows CI checks sources out with CRLF line ends; the gate scan must report the same
    // calls for each harness source either way.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        let lf_text = std::fs::read_to_string(&entry_path).unwrap().replace("\r\n", "\n");
        let crlf_text = lf_text.replace('\n', "\r\n");
        for (gate, calls) in [
            (COUNTERS_GATE, GATED_CALLS),
            (ECHO_TRACE_GATE, ECHO_TRACE_CALLS),
            (TRIM_HOOK_GATE, TRIM_HOOK_CALLS),
        ] {
            assert_eq!(
                ungated_calls(&crlf_text, gate, calls),
                ungated_calls(&lf_text, gate, calls),
                "{name}"
            );
        }
    }
}

/// The fixture perf-compare's attempt-split test reads, written by this harness.
const ATTEMPT_FIXTURE: &str = "../../scripts/perf-compare_attempt_fixture.json";

/// A renderer record whose two attempt classes are both at these cumulative totals: attempts,
/// presented, attempt ns, shape ns, raster ns, shape requests, raster calls and raster tiles.
fn attempt_record(totals: [u64; 8]) -> HashMap<&'static str, SourceValue> {
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

#[test]
fn attempt_nanoseconds_survive_the_phase_delta_and_reach_perf_compare_exactly() {
    // Two runs, each a phase's start and end cumulative totals. Run 1 moves from
    // (1999, 999, 999) to (2999, 1499, 1499) ns: whole-microsecond rounding before the delta
    // would leave a negative remainder, while nanoseconds give (1000, 500, 500) and none. The
    // deltas are written through the production serializer to the fixture perf-compare reads,
    // so the reader sees exactly what the harness writes. `SONICTERM_WRITE_ATTEMPT_FIXTURE=1`
    // rewrites it; otherwise the committed fixture must match.
    let phases = [
        ([1, 1, 1_999, 999, 999, 2, 2, 1], [2, 2, 2_999, 1_499, 1_499, 4, 4, 2]),
        ([2, 2, 5_000, 2_000, 2_000, 4, 4, 2], [5, 5, 8_000, 3_000, 3_500, 10, 8, 5]),
    ];
    let mut runs = Vec::new();
    for (start, end) in phases {
        let mut totals = [CounterTotals::unsupported(), CounterTotals::unsupported()];
        for (total, values) in totals.iter_mut().zip([start, end]) {
            total.add_record(&[Section::Renderer], reader(&attempt_record(values))).unwrap();
        }
        let delta = totals[1].delta_since(&totals[0]).to_json();
        runs.push(serde_json::json!({ "renderer": delta[Section::Renderer.key()] }));
    }
    let first = &runs[0]["renderer"];
    assert_eq!(
        (
            &first["apply_attempt_ns"],
            &first["apply_attempt_shape_ns"],
            &first["apply_attempt_raster_ns"]
        ),
        (&serde_json::json!(1_000), &serde_json::json!(500), &serde_json::json!(500))
    );
    let produced = serde_json::json!({ "runs": runs });
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ATTEMPT_FIXTURE);
    if std::env::var_os("SONICTERM_WRITE_ATTEMPT_FIXTURE").is_some() {
        let text = serde_json::to_string_pretty(&produced).expect("fixture serializes") + "\n";
        std::fs::write(&fixture_path, text).expect("fixture written");
        return;
    }
    let committed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&fixture_path).expect("the attempt fixture is committed"),
    )
    .expect("the attempt fixture is JSON");
    assert_eq!(committed, produced, "the fixture is what this harness writes");
}

#[test]
fn growth_counts_key_each_counted_window_by_the_memory_lines_native_label() {
    // perf-compare matches a window's counted growths to its renderer in the memory line by the native
    // label, so both must spell a window id the same way: the memory snapshot's `{window_id:?}`. A
    // window without a renderer has no count and is left out rather than reported as 0, and a run that
    // closed no counted window has a closed total of 0.
    let counted = winit::window::WindowId::from(7_u64);
    let unrendered = winit::window::WindowId::from(9_u64);
    let (live, closed) = growth_counts([(counted, Some(3)), (unrendered, None)], None);
    assert_eq!(live, std::collections::BTreeMap::from([(format!("{counted:?}"), 3)]));
    assert_eq!(closed, 0);
    assert_eq!(growth_counts([], Some(4)).1, 4);
    let memory_snapshot = include_str!("../../src/app/memory_snapshot.rs").replace("\r\n", "\n");
    assert!(
        memory_snapshot.contains("let label = format!(\"{window_id:?}\");"),
        "the memory line no longer labels a visible renderer with its window id's Debug form"
    );
}

#[cfg(feature = "perf-counters")]
#[test]
fn an_atlas_reading_counts_only_windows_with_a_renderer_while_the_app_counts() {
    // A counting App with no main window yet reports no main label, an empty per-window map (a seeded
    // window has no renderer, so no growth count) and a closed total of 0, at the attempt it was asked for.
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut app =
        sonicterm_app::app::App::new(Theme::default(), Config::default(), Keymap::default());
    enable(&mut app).expect("no window yet");
    let _child = app.__test_seed_child_window(&[]);
    assert_eq!(
        atlas_reading(&app, 3),
        AtlasReading {
            attempt: 3,
            main_window: None,
            counted_glyph_atlas_growths: Some(std::collections::BTreeMap::new()),
            closed_glyph_atlas_growths: Some(0),
        }
    );
}

#[cfg(not(feature = "perf-counters"))]
#[test]
fn an_atlas_reading_without_the_counter_feature_has_no_counts() {
    // A build without the counter API cannot count growths, so its reading says so with nulls rather
    // than zeros that perf-compare would compare as figures.
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let app = sonicterm_app::app::App::new(Theme::default(), Config::default(), Keymap::default());
    let reading = atlas_reading(&app, 1);
    assert_eq!(
        (reading.counted_glyph_atlas_growths, reading.closed_glyph_atlas_growths),
        (None, None)
    );
}

/// Every call of S1/atlas-retry's driver into the App and renderer API it needs sits behind
/// `perf_atlas_retry_api`, because perf-compare overlays this harness onto the previous release tag, which
/// has none of it, and builds it there with the cfg off. A call outside the gated item is reported.
#[test]
fn every_atlas_retry_api_call_in_the_harness_is_behind_its_cfg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap();
        let found = ungated_calls(&source, ATLAS_RETRY_GATE, ATLAS_RETRY_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    let fixture = format!(
        "{ATLAS_RETRY_GATE}\nfn parts(app: &App) {{\n    app.__test_window_active_tab_title(id);\n}}\n\nfn off(renderer: &GpuRenderer) {{\n    renderer.font_fallback_notice_id();\n}}\n"
    );
    assert_eq!(
        ungated_calls(&fixture, ATLAS_RETRY_GATE, ATLAS_RETRY_CALLS),
        vec!["7: font_fallback_notice_id".to_owned()]
    );
}

/// A real gate's own string argument is code, not a literal to skip: `#[cfg(feature = "...")]` still guards the
/// call in its body, even though the scan blanks string contents when it decides where a gate stands.
#[test]
fn a_gate_with_a_string_argument_still_guards_its_body() {
    let fixture = format!(
        "{COUNTERS_GATE}\nfn on(app: &mut App) {{\n    let _ = app.frame_counters_snapshot();\n}}\n"
    );
    assert_eq!(ungated_calls(&fixture, COUNTERS_GATE, GATED_CALLS), Vec::<String>::new());
}

/// A gate written in a comment or a string guards nothing, so the scan reads attributes and balances bodies in
/// code only: a commented-out gate or a gate's text inside a string literal above a call never guards it, and a
/// brace inside a comment or a string never stretches a gated body over the ungated call after it (a later
/// string's closing brace would otherwise close it there).
#[test]
fn a_gate_in_a_comment_or_a_string_guards_nothing() {
    let cases = [
        format!("// {ATLAS_RETRY_GATE}\nfn parts(app: &App) {{\n    app.__test_window_active_tab_title(id);\n}}\n"),
        format!("/* {ATLAS_RETRY_GATE} */\nfn parts(app: &App) {{\n    app.__test_window_active_tab_title(id);\n}}\n"),
        format!(
            "{ATLAS_RETRY_GATE}\nfn parts() {{\n    let brace = \"{{\";\n}}\nfn off(app: &App) {{\n    app.__test_window_active_tab_title(id);\n}}\nfn tail() {{\n    let close = \"}}\";\n}}\n"
        ),
        format!(
            "{ATLAS_RETRY_GATE}\nfn parts() {{\n    // {{\n}}\nfn off(app: &App) {{\n    app.__test_window_active_tab_title(id);\n}}\n"
        ),
        format!(
            "const NOTE: &str = \"{ATLAS_RETRY_GATE}\";\nfn off(app: &App) {{\n    app.__test_window_active_tab_title(id);\n}}\n"
        ),
        format!(
            "const NOTE: &str = r#\"{ATLAS_RETRY_GATE}\"#;\nfn off(app: &App) {{\n    app.__test_window_active_tab_title(id);\n}}\n"
        ),
    ];
    for fixture in cases {
        assert_eq!(
            ungated_calls(&fixture, ATLAS_RETRY_GATE, ATLAS_RETRY_CALLS).len(),
            1,
            "the call is ungated in {fixture:?}"
        );
    }
}

/// Every call into the App's attribution API, and every read of the parser state it depends on, sits
/// behind the attribution cfg, because perf-compare overlays this harness onto a tree that predates
/// them and builds it there with the cfg off. A call outside the gated item is reported.
#[test]
fn every_attribution_api_call_in_the_harness_is_behind_its_cfg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap();
        let found = ungated_calls(&source, ATTRIBUTION_GATE, ATTRIBUTION_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    let fixture = format!(
        "{ATTRIBUTION_GATE}\nmod api {{\n    fn arm(app: &mut App) {{\n        app.arm_s10_attribution(1, \"s\", \"p\", 1);\n    }}\n}}\n\nfn off(parser: &Parser) {{\n    parser.synchronized_output();\n}}\n"
    );
    // The scan matches names as substrings, so the ungated line names a call no other name contains.
    assert_eq!(
        ungated_calls(&fixture, ATTRIBUTION_GATE, ATTRIBUTION_CALLS),
        vec!["9: synchronized_output".to_owned()]
    );
}

/// Every call of the renderer's completeness checkpoint, and every use of the harness mapping that
/// names its types, sits behind `perf_completeness_api`: perf-compare overlays this harness onto the
/// previous release tag, whose renderer has no checkpoint, and builds it there with the cfg off. A
/// call outside the gated item is reported.
#[test]
fn every_completeness_api_call_in_the_harness_is_behind_its_cfg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap();
        let found = ungated_calls(&source, COMPLETENESS_GATE, COMPLETENESS_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    let fixture = format!(
        "{COMPLETENESS_GATE}\nfn on(renderer: &GpuRenderer) {{\n    renderer.completeness_checkpoint();\n}}\n\nfn off(renderer: &GpuRenderer) {{\n    renderer.completeness_checkpoint();\n}}\n"
    );
    assert_eq!(
        ungated_calls(&fixture, COMPLETENESS_GATE, COMPLETENESS_CALLS),
        vec!["7: completeness_checkpoint".to_owned()]
    );
}

/// Every call into the App's dispatch-timeline API, and every name of its types, sits behind
/// `perf_dispatch_timeline_api`: perf-compare overlays this harness onto trees that predate the
/// prerequisite and builds it there with the cfg off. A call outside the gated item is reported.
#[test]
fn every_dispatch_timeline_api_call_in_the_harness_is_behind_its_cfg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap();
        let found = ungated_calls(&source, DISPATCH_TIMELINE_GATE, DISPATCH_TIMELINE_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    let fixture = format!(
        "{DISPATCH_TIMELINE_GATE}\nmod api {{\n    fn arm(app: &mut App) {{\n        app.arm_dispatch_timeline_v1(1, None);\n    }}\n}}\n\nfn off(app: &mut App, token: DispatchTimelineToken) {{\n    app.take_dispatch_timeline_v1(token);\n}}\n"
    );
    assert_eq!(
        ungated_calls(&fixture, DISPATCH_TIMELINE_GATE, DISPATCH_TIMELINE_CALLS),
        vec!["9: take_dispatch_timeline_v1".to_owned(), "8: DispatchTimelineToken".to_owned()]
    );
}

/// Every name of the App's V1 echo-timeline accessor in the harness sits behind its gate:
/// perf-compare overlays this harness onto trees whose App has no such accessor and builds it there
/// with the cfg off. A name outside the gated item is reported.
#[test]
fn every_echo_timeline_api_call_in_the_harness_is_behind_its_cfg() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/perf_scenarios");
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry_path = entry.unwrap().path();
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        scanned += 1;
        let source = std::fs::read_to_string(&entry_path).unwrap();
        let found = ungated_calls(&source, TIMELINE_GATE, TIMELINE_CALLS);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    let fixture = format!(
        "{TIMELINE_GATE}\nmod on {{\n    fn take(app: &mut App) {{ app.take_echo_timeline_v1(1, token); }}\n}}\n\nfn off(app: &mut App) {{\n    app.take_echo_timeline_v1(1, token);\n}}\n"
    );
    assert_eq!(
        ungated_calls(&fixture, TIMELINE_GATE, TIMELINE_CALLS),
        vec!["7: take_echo_timeline_v1".to_owned()]
    );
}
