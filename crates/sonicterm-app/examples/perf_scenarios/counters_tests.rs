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
            "defer_streaming",
            "contention_retry_armed",
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
            "native_request_redraw_unregistered",
            "about_to_wait_ms",
            "user_event_ms",
            "new_events_ms",
            "ui_parser_wait_us",
            "fg_probe_us",
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
            "software_frames",
            "gpu_frames",
            "row_cache_hits",
            "row_cache_misses",
            "shape_requests",
            "full_frames",
            "row_cache_invalidate_visits",
            "row_cache_invalidate_us",
            "recolor_glyphs_visited",
            "assembly_us",
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
    // are exactly the twelve contract histograms; row_cache_invalidate_us is a plain sum.
    let histograms = FIELDS.iter().filter(|field| matches!(field.kind, FieldKind::Histogram(_)));
    assert_eq!(histograms.count(), 12);
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
    assert_eq!(end.delta_since(&start), CounterTotals::zero());
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
];

/// Each counter API call in `text` outside a `#[cfg(feature = "perf-counters")]` item, by line.
fn ungated_calls(text: &str) -> Vec<String> {
    const GATE: &str = "#[cfg(feature = \"perf-counters\")]";
    let mut gated = Vec::new();
    for (offset, _) in text.match_indices(GATE) {
        let after = offset + GATE.len();
        let open = after + text[after..].find('{').expect("a gated item has a body");
        let mut depth = 0_usize;
        for (index, character) in text[open..].char_indices() {
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
    for call in GATED_CALLS {
        for (offset, _) in text.match_indices(call) {
            let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
            let comment = text[line_start..offset].trim_start().starts_with("//");
            if !comment && !gated.iter().any(|range| range.contains(&offset)) {
                found.push(format!("{}: {call}", text[..offset].matches('\n').count() + 1));
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
        let found = ungated_calls(&std::fs::read_to_string(&entry_path).unwrap());
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
    assert!(scanned >= 10, "the harness sources were not found");
    // Negative fixture: the same call outside a gated item is reported.
    let fixture = "#[cfg(feature = \"perf-counters\")]\nfn on(app: &mut App) {\n    app.force_frame_counters_on();\n}\n\nfn off(app: &mut App) {\n    let _ = app.frame_counters_snapshot();\n}\n";
    assert_eq!(ungated_calls(fixture), vec!["7: frame_counters_snapshot".to_owned()]);
}
