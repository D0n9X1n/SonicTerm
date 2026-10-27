//! Feature-gated application counter snapshots and atlas readings.

use super::*;

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

/// Each counted window's `glyph_atlas_growths` keyed by its native label, and the closed windows'
/// total (0 when none was counted). A window whose count is `None` has no renderer and is left out,
/// never reported as 0. The label is `{window_id:?}`, the form the memory line gives its renderer.
pub(crate) fn growth_counts(
    windows: impl IntoIterator<Item = (winit::window::WindowId, Option<u64>)>,
    closed: Option<u64>,
) -> (BTreeMap<String, u64>, u64) {
    let live = windows
        .into_iter()
        .filter_map(|(window_id, growths)| Some((format!("{window_id:?}"), growths?)))
        .collect();
    (live, closed.unwrap_or(0))
}

/// The glyph atlas reading for sampling attempt `attempt`: the main window's label and, while the
/// App counts, each live window's counted growths and the closed windows' total.
pub(crate) fn atlas_reading(app: &sonicterm_app::app::App, attempt: u32) -> AtlasReading {
    let main_window = app.main_window().map(|window| format!("{:?}", window.id()));
    let (counted, closed) = match counted_atlas_growths(app) {
        Some((live, closed)) => (Some(live), Some(closed)),
        None => (None, None),
    };
    AtlasReading {
        attempt,
        main_window,
        counted_glyph_atlas_growths: counted,
        closed_glyph_atlas_growths: closed,
    }
}

/// Every live window's counted `glyph_atlas_growths` and the closed windows' total; `None` when
/// `app`'s counters are off.
#[cfg(feature = "perf-counters")]
pub(super) fn counted_atlas_growths(
    app: &sonicterm_app::app::App,
) -> Option<(BTreeMap<String, u64>, u64)> {
    let snapshot = app.frame_counters_snapshot()?;
    let windows = snapshot
        .windows
        .iter()
        .map(|(window_id, record)| (*window_id, record.count(GLYPH_ATLAS_GROWTHS)));
    Some(growth_counts(windows, snapshot.closed_windows.count(GLYPH_ATLAS_GROWTHS)))
}

/// This build has no counter API, so it counts no growths.
#[cfg(not(feature = "perf-counters"))]
pub(super) fn counted_atlas_growths(
    _app: &sonicterm_app::app::App,
) -> Option<(BTreeMap<String, u64>, u64)> {
    None
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
#[path = "counters_api_tests.rs"]
mod counters_api_tests;
