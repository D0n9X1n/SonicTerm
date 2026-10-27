//! Run and checkpoint JSON serialization with explicit presenter capabilities.

use super::*;

impl CompletenessRecord {
    /// A certified reading; only the cfg-on mapping and the tests build one.
    #[cfg(any(test, perf_completeness_api))]
    pub(crate) fn certified(
        missing_terminal: usize,
        missing_chrome: usize,
        (atlas_width, atlas_height): (u32, u32),
        scale: f64,
    ) -> Self {
        Self {
            state: "certified",
            reason: None,
            missing_terminal: Some(missing_terminal),
            missing_chrome: Some(missing_chrome),
            atlas_width: Some(atlas_width),
            atlas_height: Some(atlas_height),
            scale,
        }
    }
}

impl CompletenessRecord {
    /// An unavailable reading with its reason.
    pub(crate) fn unavailable(reason: &'static str, scale: f64) -> Self {
        Self {
            state: "unavailable",
            reason: Some(reason),
            missing_terminal: None,
            missing_chrome: None,
            atlas_width: None,
            atlas_height: None,
            scale,
        }
    }
}

/// Whether a checkpoint takes a completeness reading: only S9's and S12's `end`, the fixtures the
/// glyph atlas start size is chosen from.
pub(crate) fn takes_completeness(scenario: &str, label: &str) -> bool {
    label == "end" && matches!(scenario, "S9" | "S12")
}

/// The reading of a build without `perf_completeness_api`: it never calls the renderer.
#[cfg(any(test, not(perf_completeness_api)))]
pub(crate) fn completeness_api_disabled(scale: f64) -> CompletenessRecord {
    CompletenessRecord::unavailable(COMPLETENESS_API_DISABLED, scale)
}

/// The record of one renderer checkpoint, its reason kept by the renderer's own name.
#[cfg(perf_completeness_api)]
pub(crate) fn completeness_from_checkpoint(
    checkpoint: sonicterm_gpu::completeness::CompletenessCheckpoint,
    scale: f64,
) -> CompletenessRecord {
    use sonicterm_gpu::completeness::CompletenessCheckpoint;
    match checkpoint {
        CompletenessCheckpoint::Certified(counts) => CompletenessRecord::certified(
            counts.missing_terminal,
            counts.missing_chrome,
            counts.atlas_dims,
            scale,
        ),
        CompletenessCheckpoint::Unavailable(reason) => {
            CompletenessRecord::unavailable(reason.reason(), scale)
        }
    }
}

impl Status {
    /// The process exit code for this status.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Self::Valid => 0,
            Self::Invalid => 3,
            Self::Timeout => 4,
            Self::Blocked => 5,
        }
    }
}

impl Status {
    /// The status's name in `result.json`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Timeout => "timeout",
            Self::Blocked => "blocked",
        }
    }
}

impl RunResult {
    /// The `result.json` document, schema version 1.
    pub(crate) fn to_json(&self) -> Value {
        let mut document = Map::new();
        let mut put = |key: &str, value: Value| {
            document.insert(key.to_owned(), value);
        };
        put("schema_version", json!(SCHEMA_VERSION));
        put("harness_hash", json!(self.harness_hash));
        put("scenario", json!(self.scenario));
        put("variant", json!(self.variant));
        put("managed", json!(self.managed));
        put("short", json!(self.short));
        put("laps", json!(self.laps));
        put("alloc_counting", json!(self.alloc_counting));
        put("status", json!(self.status.as_str()));
        put("invalid_reason", json!(self.invalid_reason));
        put("exit_code", json!(self.status.exit_code()));
        put("harness_pid", json!(self.harness_pid));
        put(
            "grid",
            self.grid.map_or(Value::Null, |(cols, rows)| json!({"cols": cols, "rows": rows})),
        );
        put(
            "monitor",
            self.monitor.as_ref().map_or(Value::Null, |monitor| {
                json!({
                    "name": monitor.name,
                    "refresh_rate_millihertz": monitor.refresh_rate_millihertz,
                    "scale_factor": monitor.scale_factor,
                })
            }),
        );
        put("presenter", json!(self.presenter));
        put("window_path", json!(self.window_path));
        put("synthetic_occlusion", json!(self.synthetic_occlusion));
        put("native_focus_events_dropped", json!(self.native_focus_events_dropped));
        put("native_cursor_rest_events_dropped", json!(self.native_cursor_rest_events_dropped));
        put("finish_session_settled", json!(self.finish_session_settled));
        put("checkpoint_memory", json!(checkpoint_memory_support()));
        put("hooks", json!({ "trim": self.trim_hook.as_str() }));
        // Whether this build calls the App's attribution API: the effective `perf_s10_attribution_api` cfg.
        put("s10_attribution_api", json!(crate::attribution::API_ENABLED));
        put("trim_experiment", json!(self.trim_experiment));
        put("trim_seq_after_hook", json!(self.trim_seq_after_hook));
        if let Some(recovery) = &self.atlas_recovery {
            // When: the run recorded recovery episodes; every other result omits the key.
            put("atlas_recovery", recovery.clone());
        }
        // The measurement fields come from the serializer progress.json streams, so both
        // documents record them identically. Every field converts; a non-finite float is null.
        let measured =
            serde_json::to_value(self.measurements()).expect("measurements always convert to JSON");
        if let Value::Object(fields) = measured {
            for (key, value) in fields {
                put(key.as_str(), value);
            }
        }
        put("notes", json!(self.notes));
        Value::Object(document)
    }
}

/// The presenter record for a run on `host`: Windows GDI only for a degraded Windows run, since
/// every other host's degrade path stays on wgpu.
pub(crate) fn presenter_record_for(
    host: Host,
    software_render_mode: &'static str,
    software_rendering: bool,
    degraded: bool,
) -> PresenterRecord {
    PresenterRecord {
        software_render_mode,
        software_rendering,
        software_render_degraded: degraded,
        windows_gdi: host == Host::Windows && degraded,
    }
}

/// Seconds in a FILETIME duration given as its two 32-bit words; it counts 100 ns ticks.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn filetime_seconds(low: u32, high: u32) -> f64 {
    ((u64::from(high) << 32) | u64::from(low)) as f64 / 1e7
}

/// Why a Windows run cannot measure its presenter variant, naming the variant and the field that
/// missed it: `gdi` without Windows GDI, or `wgpu` on the degrade path. None when it can.
pub(crate) fn presenter_blocked(
    presentation: Presentation,
    presenter: &PresenterRecord,
    host: Host,
) -> Option<String> {
    if host != Host::Windows {
        // When: off Windows the presenter variants are refused before the run, so nothing is judged.
        return None;
    }
    match presentation {
        Presentation::ForceGdi if !presenter.windows_gdi => Some(format!(
            "the gdi variant forced the software presenter, but presenter.windows_gdi is false \
             (software_render_degraded {}), so the run did not present through Windows GDI",
            presenter.software_render_degraded
        )),
        Presentation::ForceWgpu if presenter.software_render_degraded => Some(format!(
            "the wgpu variant turned the software presenter off, but presenter.software_render_degraded \
             is true (windows_gdi {}), so the run did not present through wgpu",
            presenter.windows_gdi
        )),
        _ => None,
    }
}

/// `result.json`'s `checkpoint_memory`: `supported` when this build has the hook, else `unsupported`.
/// The serializer reads it here, so no caller can record a value the build does not have.
pub(crate) fn checkpoint_memory_support() -> &'static str {
    if CHECKPOINT_MEMORY {
        "supported"
    } else {
        // When: `CHECKPOINT_MEMORY` is false, this build takes no checkpoint sample.
        "unsupported"
    }
}

impl TrimHookOutcome {
    /// The outcome's name in `result.json`'s `hooks.trim`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotReached => "not-reached",
            Self::Unsupported => "unsupported",
            Self::Skipped => "skipped",
            Self::Trimmed => "trimmed",
        }
    }
}

impl RunResult {
    /// This result's measurements: the fields `progress.json` also records.
    pub(super) fn measurements(&self) -> Measurements<'_> {
        Measurements {
            frame_counters: self.frame_counters,
            phases: &self.phases,
            latency: self.latency.as_deref(),
            throughput: self.throughput,
            uncover_ms: self.uncover_ms,
            scrollback_rows_retained: self.scrollback_rows_retained,
            checkpoints: &self.checkpoints,
        }
    }
}

/// Stream `progress.json`: the schema version, the harness hash, status `running` and every
/// measurement completed so far, each field in `result.json`'s shape. Serializing straight into
/// `writer` allocates no document-sized buffer between phases.
pub(crate) fn write_progress(
    writer: impl std::io::Write,
    harness_hash: Option<&str>,
    measured: Measurements<'_>,
) -> std::io::Result<()> {
    let progress =
        Progress { schema_version: SCHEMA_VERSION, harness_hash, status: "running", measured };
    serde_json::to_writer_pretty(writer, &progress)?;
    Ok(())
}
