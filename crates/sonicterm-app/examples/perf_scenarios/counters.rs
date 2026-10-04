//! Per-phase frame and lock counters in `result.json`'s contract shape.
//!
//! The contract types, the delta and the serializer compile in every build. Only the code
//! that reads the App's counter API sits behind `perf-counters`, so this harness also builds
//! against a tree whose App has no counters; such a run reports `"unsupported"`.
#![cfg_attr(not(feature = "perf-counters"), allow(dead_code))]

use serde::{Serialize, Serializer};
use serde_json::{json, Map, Value};

/// Whether a run's phases carry `frame_counters`: the top-level `"frame_counters"` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CountersMode {
    /// Built without `perf-counters`; the App has no counter API.
    Unsupported,
    /// Built with it, run without `--counters`.
    Off,
    /// `--counters`: every phase carries its counter delta.
    On,
}

impl CountersMode {
    /// The mode for a run that did or did not pass `--counters`, in this build.
    pub(crate) fn for_run(requested: bool) -> Self {
        match (cfg!(feature = "perf-counters"), requested) {
            (false, _) => Self::Unsupported,
            (true, false) => Self::Off,
            (true, true) => Self::On,
        }
    }

    /// The mode's name in `result.json` and `progress.json`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

impl Serialize for CountersMode {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A contract section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    Window,
    App,
    VtParser,
    Renderer,
}

impl Section {
    /// Every section, in the order the object lists them.
    pub(crate) const ALL: [Self; 4] = [Self::Window, Self::App, Self::VtParser, Self::Renderer];

    /// The section's key in a phase's `frame_counters` object.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::App => "app",
            Self::VtParser => "vt",
            Self::Renderer => "renderer",
        }
    }
}

/// A histogram's unit in the contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Millis,
    Micros,
}

impl Unit {
    /// The unit's name, as the counter API and the contract both spell it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Millis => "ms",
            Self::Micros => "us",
        }
    }

    /// The bucket upper bounds; one more bucket holds the overflow.
    pub(crate) fn bounds(self) -> &'static [u64] {
        match self {
            Self::Millis => &[4, 7, 9, 12, 17, 25, 34, 50, 100],
            Self::Micros => &[10, 50, 100, 500, 1_000, 5_000],
        }
    }
}

/// What a contract field holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Count,
    Histogram(Unit),
}

/// One contract field: its section, contract name, kind and the counter API's name for it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FieldSpec {
    pub(crate) section: Section,
    pub(crate) name: &'static str,
    pub(crate) kind: FieldKind,
    pub(crate) source: &'static str,
}

const fn count(section: Section, name: &'static str) -> FieldSpec {
    FieldSpec { section, name, kind: FieldKind::Count, source: name }
}

const fn histogram(
    section: Section,
    name: &'static str,
    unit: Unit,
    source: &'static str,
) -> FieldSpec {
    FieldSpec { section, name, kind: FieldKind::Histogram(unit), source }
}

/// Every contract field. Window and renderer fields come from window records, app and vt
/// fields from the App's record; a histogram's API name lacks the contract's unit suffix.
pub(crate) const FIELDS: &[FieldSpec] = &[
    count(Section::Window, "attempts"),
    count(Section::Window, "presented"),
    count(Section::Window, "cached"),
    count(Section::Window, "settled"),
    count(Section::Window, "retry"),
    count(Section::Window, "surface_retry"),
    count(Section::Window, "stopped"),
    count(Section::Window, "failed"),
    count(Section::Window, "contention_parser"),
    count(Section::Window, "contention_images"),
    count(Section::Window, "defer_timeout"),
    count(Section::Window, "defer_contention"),
    count(Section::Window, "defer_streaming"),
    count(Section::Window, "contention_retry_armed"),
    count(Section::Window, "dirt_ack_dropped"),
    count(Section::Window, "native_request_redraw"),
    count(Section::Window, "user_request_redraw"),
    count(Section::Window, "redraw_requested"),
    histogram(Section::Window, "present_interval_ms", Unit::Millis, "present_interval"),
    histogram(Section::Window, "handler_ms", Unit::Millis, "handler"),
    histogram(Section::Window, "flush_to_redraw_ms", Unit::Millis, "flush_to_redraw"),
    count(Section::App, "wake_init"),
    count(Section::App, "wake_poll"),
    count(Section::App, "wake_wait_cancelled"),
    count(Section::App, "wake_resume_time"),
    count(Section::App, "wake_user"),
    count(Section::App, "ui_parser_locks"),
    count(Section::App, "fg_probe_calls"),
    count(Section::App, "fg_probe_panes"),
    count(Section::App, "fg_worker_probes"),
    count(Section::App, "fg_worker_panes"),
    count(Section::App, "fg_results_stale"),
    count(Section::App, "native_request_redraw_unregistered"),
    histogram(Section::App, "about_to_wait_ms", Unit::Millis, "about_to_wait"),
    histogram(Section::App, "user_event_ms", Unit::Millis, "user_event"),
    histogram(Section::App, "new_events_ms", Unit::Millis, "new_events"),
    histogram(Section::App, "ui_parser_wait_us", Unit::Micros, "ui_parser_wait"),
    histogram(Section::App, "fg_probe_us", Unit::Micros, "fg_probe"),
    histogram(Section::App, "fg_worker_probe_us", Unit::Micros, "fg_worker_probe"),
    count(Section::VtParser, "parse_bytes"),
    count(Section::VtParser, "batches"),
    count(Section::VtParser, "flushes"),
    count(Section::VtParser, "flushes_untargeted"),
    count(Section::VtParser, "flushes_coalesced"),
    count(Section::VtParser, "flushes_suppressed"),
    histogram(Section::VtParser, "parser_lock_wait_us", Unit::Micros, "parser_lock_wait"),
    histogram(Section::VtParser, "parser_lock_hold_us", Unit::Micros, "parser_lock_hold"),
    histogram(Section::VtParser, "parse_us", Unit::Micros, "parse"),
    count(Section::Renderer, "vertex_bytes"),
    count(Section::Renderer, "index_bytes"),
    count(Section::Renderer, "damage_permille_sum"),
    count(Section::Renderer, "damaged_frames"),
    count(Section::Renderer, "software_frames"),
    count(Section::Renderer, "gpu_frames"),
    count(Section::Renderer, "row_cache_hits"),
    count(Section::Renderer, "row_cache_misses"),
    count(Section::Renderer, "shape_requests"),
    count(Section::Renderer, "full_frames"),
    count(Section::Renderer, "row_cache_invalidate_visits"),
    count(Section::Renderer, "row_cache_invalidate_us"),
    count(Section::Renderer, "recolor_glyphs_visited"),
    count(Section::Renderer, "font_fallback_applies"),
    histogram(Section::Renderer, "assembly_us", Unit::Micros, "assembly"),
];

/// One field's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FieldValue {
    Count(u64),
    /// Per-bucket counts (overflow last) and the exact sum in microseconds.
    Histogram {
        counts: Vec<u64>,
        sum_us: u64,
    },
}

impl FieldValue {
    /// The zero value of a field of `kind`.
    fn zero(kind: FieldKind) -> Self {
        match kind {
            FieldKind::Count => Self::Count(0),
            FieldKind::Histogram(unit) => {
                Self::Histogram { counts: vec![0; unit.bounds().len() + 1], sum_us: 0 }
            }
        }
    }
}

/// What a source record holds for one field; a record without the field reads `Absent`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceValue {
    Absent,
    Count(u64),
    Histogram { unit: &'static str, bounds: Vec<u64>, counts: Vec<u64>, sum_us: u64 },
}

/// Every contract field's value, in [`FIELDS`] order. A supported field with no events is
/// zero; `None` is a field no record supplied, which an older base's App cannot read, and
/// its key is left out so a comparison shows it as n/a rather than as a measured zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CounterTotals {
    pub(crate) values: Vec<Option<FieldValue>>,
}

impl CounterTotals {
    /// Every field supported and zero.
    #[cfg(test)]
    pub(crate) fn zero() -> Self {
        Self { values: FIELDS.iter().map(|field| Some(FieldValue::zero(field.kind))).collect() }
    }

    /// Every field unsupported until a record supplies it.
    pub(crate) fn unsupported() -> Self {
        Self { values: vec![None; FIELDS.len()] }
    }

    /// The value of contract field `name`.
    #[cfg(test)]
    pub(crate) fn get(&self, name: &str) -> Option<&FieldValue> {
        let index = FIELDS.iter().position(|field| field.name == name)?;
        self.values[index].as_ref()
    }

    /// Add one record's fields in `sections`; `read` looks a field up by its API name. A field
    /// of the wrong kind, unit or bounds is refused rather than coerced.
    pub(crate) fn add_record(
        &mut self,
        sections: &[Section],
        read: impl Fn(&str) -> SourceValue,
    ) -> Result<(), String> {
        for (field, total) in FIELDS.iter().zip(&mut self.values) {
            if !sections.contains(&field.section) {
                continue;
            }
            let source = read(field.source);
            if source == SourceValue::Absent {
                // the record lacks the field, so it adds nothing and supports nothing.
                continue;
            }
            // A record that supplies the field makes it supported, starting from zero.
            let total = total.get_or_insert_with(|| FieldValue::zero(field.kind));
            match (field.kind, source, total) {
                (FieldKind::Count, SourceValue::Count(value), FieldValue::Count(sum)) => {
                    *sum += value;
                }
                (
                    FieldKind::Histogram(unit),
                    SourceValue::Histogram { unit: source_unit, bounds, counts, sum_us },
                    FieldValue::Histogram { counts: total_counts, sum_us: total_sum },
                ) if source_unit == unit.name()
                    && bounds == unit.bounds()
                    && counts.len() == total_counts.len() =>
                {
                    for (slot, value) in total_counts.iter_mut().zip(counts) {
                        *slot += value;
                    }
                    *total_sum += sum_us;
                }
                (_, found, _) => {
                    // When: the API's field does not match the contract's kind, unit or bounds.
                    return Err(format!(
                        "{} reads {found:?}, not the contract's shape",
                        field.name
                    ));
                }
            }
        }
        Ok(())
    }

    /// What grew since `start`. Totals sum live and closed windows, so they never fall; a
    /// field that did would read zero rather than wrap. A field `start` lacked counts from zero,
    /// as when no window existed yet; a field the end lacks stays unsupported.
    pub(crate) fn delta_since(&self, start: &Self) -> Self {
        let values = self
            .values
            .iter()
            .zip(&start.values)
            .map(|(end, begin)| match (end.as_ref()?, begin.as_ref()) {
                (end, None) => Some(end.clone()),
                (FieldValue::Count(end), Some(FieldValue::Count(begin))) => {
                    Some(FieldValue::Count(end.saturating_sub(*begin)))
                }
                (
                    FieldValue::Histogram { counts, sum_us },
                    Some(FieldValue::Histogram { counts: begin_counts, sum_us: begin_sum }),
                ) => Some(FieldValue::Histogram {
                    counts: counts
                        .iter()
                        .zip(begin_counts)
                        .map(|(end, begin)| end.saturating_sub(*begin))
                        .collect(),
                    sum_us: sum_us.saturating_sub(*begin_sum),
                }),
                // Both totals are built from FIELDS, so kinds always match.
                (end, Some(_)) => Some(end.clone()),
            })
            .collect();
        Self { values }
    }

    /// The phase's `frame_counters` object: each section maps a field to a count or to
    /// `{"unit", "bounds", "counts", "sum_us"}`.
    pub(crate) fn to_json(&self) -> Value {
        let mut document = Map::new();
        for section in Section::ALL {
            let mut fields = Map::new();
            for (field, value) in FIELDS.iter().zip(&self.values) {
                let Some(value) = value.as_ref().filter(|_| field.section == section) else {
                    // When: another section's field, or an unsupported one, whose key is left out.
                    continue;
                };
                let entry = match (field.kind, value) {
                    (FieldKind::Histogram(unit), FieldValue::Histogram { counts, sum_us }) => {
                        json!({
                            "unit": unit.name(),
                            "bounds": unit.bounds(),
                            "counts": counts,
                            "sum_us": sum_us,
                        })
                    }
                    (_, FieldValue::Count(value)) => json!(value),
                    (_, FieldValue::Histogram { sum_us, .. }) => json!(sum_us),
                };
                fields.insert(field.name.to_owned(), entry);
            }
            document.insert(section.key().to_owned(), Value::Object(fields));
        }
        Value::Object(document)
    }
}

impl Serialize for CounterTotals {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        self.to_json().serialize(serializer)
    }
}

/// Why the App's counter gate disagrees with the mode the run reports, if it does.
pub(crate) fn gate_mismatch(mode: CountersMode, gate_on: bool) -> Option<String> {
    match (mode, gate_on) {
        (CountersMode::Off, true) => {
            Some("the App's counter gate is on in a run that reports off".to_owned())
        }
        (CountersMode::On, false) => {
            Some("the App's counter gate is off in a run that reports on".to_owned())
        }
        _ => None,
    }
}

/// Whether `app`'s counter gate is on.
#[cfg(feature = "perf-counters")]
pub(crate) fn gate_on(app: &sonicterm_app::app::App) -> bool {
    app.frame_counters_snapshot().is_some()
}

/// Turn `app`'s frame counters on before it creates any window or pane.
#[cfg(feature = "perf-counters")]
pub(crate) fn enable(app: &mut sonicterm_app::app::App) -> Result<(), String> {
    app.force_frame_counters_on().map_err(|error| error.to_string())
}

/// `app`'s totals now: every window, live and closed, with its renderer, plus the App-global
/// and VT counts; `None` when its counters are off.
#[cfg(feature = "perf-counters")]
pub(crate) fn snapshot_totals(
    app: &sonicterm_app::app::App,
) -> Option<Result<CounterTotals, String>> {
    let snapshot = app.frame_counters_snapshot()?;
    let mut totals = CounterTotals::unsupported();
    let windows = snapshot.windows.iter().map(|(_, record)| record);
    let result = windows
        .chain(std::iter::once(&snapshot.closed_windows))
        .try_for_each(|record| {
            totals
                .add_record(&[Section::Window, Section::Renderer], |name| read_field(record, name))
        })
        .and_then(|()| {
            totals.add_record(&[Section::App, Section::VtParser], |name| {
                read_field(&snapshot.app, name)
            })
        });
    Some(result.map(|()| totals))
}

/// Field `name` of `record`.
#[cfg(feature = "perf-counters")]
pub(crate) fn read_field(record: &sonicterm_app::app::CounterRecord, name: &str) -> SourceValue {
    if let Some(buckets) = record.histogram_buckets(name) {
        return SourceValue::Histogram {
            unit: buckets.unit,
            bounds: buckets.bounds.to_vec(),
            counts: buckets.counts.to_vec(),
            sum_us: buckets.sum_us,
        };
    }
    record.count(name).map_or(SourceValue::Absent, SourceValue::Count)
}

#[cfg(test)]
#[path = "counters_tests.rs"]
mod counters_tests;
