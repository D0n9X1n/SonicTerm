use super::*;

use std::sync::{Arc, Mutex};

use sonicterm_types::ResourceAmount;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

/// One captured `tracing` event: its message and every field on it.
#[derive(Debug, Default, Clone)]
struct CapturedEvent {
    message: String,
    target: String,
    fields: Vec<(String, u64)>,
    strings: Vec<(String, String)>,
}

impl CapturedEvent {
    fn field(&self, name: &str) -> Option<u64> {
        self.fields.iter().find(|(key, _)| key == name).map(|(_, value)| *value)
    }
}

impl Visit for CapturedEvent {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push((field.name().to_string(), value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push((field.name().to_string(), value.unsigned_abs()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.strings.push((field.name().to_string(), value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

/// Collects every event a block emits, so a test can assert on the real macro
/// rather than on a value the emitter returned.
///
/// Nothing in the workspace captured event *fields* before this: the existing
/// subscriber tests install a registry only to open the level gate. Asserting
/// that a line carries the fields a user greps for needs the fields themselves.
#[derive(Clone, Default)]
struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut captured =
            CapturedEvent { target: event.metadata().target().to_string(), ..Default::default() };
        event.record(&mut captured);
        self.events.lock().expect("capture mutex").push(captured);
    }
}

fn capture(body: impl FnOnce()) -> Vec<CapturedEvent> {
    let layer = CaptureLayer::default();
    let events = Arc::clone(&layer.events);
    tracing::subscriber::with_default(Registry::default().with(layer), body);
    let captured = events.lock().expect("capture mutex").clone();
    captured
}

fn retention(glyph: usize, image: usize, frame: usize) -> RendererRetention {
    RendererRetention {
        glyph_atlas: ResourceAmount { bytes: glyph, items: 7 },
        image_atlas: ResourceAmount { bytes: image, items: 3 },
        software_frame: ResourceAmount { bytes: frame, items: usize::from(frame > 0) },
    }
}

/// The renderer's retained figures reach the memory log, under the names a
/// user greps for.
///
/// This is the defect in one assertion: the figures were computed, correct, and
/// tested, and reached no report. A test that only checked the arithmetic would
/// have passed against the broken code, because the arithmetic was never what
/// was broken.
#[test]
fn the_renderer_figures_reach_the_memory_log_by_name() {
    let events = capture(|| {
        emit_renderer_retention(
            "WindowId(1)",
            "visible",
            &retention(16_777_216, 524_288, 33_177_600),
        );
    });

    let line = events
        .iter()
        .find(|event| event.message == "renderer retention")
        .expect("the renderer's retained storage must reach the memory log");

    assert_eq!(line.target, "memory", "triage reads the `memory` target; another target is unread");

    for (field, expected) in [
        ("glyph_atlas_bytes", 16_777_216),
        ("image_atlas_bytes", 524_288),
        ("software_frame_bytes", 33_177_600),
        ("total_bytes", 16_777_216 + 524_288 + 33_177_600),
    ] {
        assert_eq!(
            line.field(field),
            Some(expected),
            "`{field}` must carry the renderer's measured figure"
        );
    }
}

/// Atlas entry counts travel with their bytes.
///
/// Bytes alone say an atlas is large. Whether that is a large glyph set or a
/// small one inside an oversized allocation is the entry count, and the remedy
/// differs between them.
#[test]
fn the_atlas_lines_carry_resident_entry_counts() {
    let events =
        capture(|| emit_renderer_retention("WindowId(1)", "visible", &retention(4096, 2048, 0)));
    let line = events.iter().find(|event| event.message == "renderer retention").expect("emitted");

    assert_eq!(line.field("glyph_atlas_items"), Some(7));
    assert_eq!(line.field("image_atlas_items"), Some(3));
}

/// The software frame is reported even when it is zero.
///
/// Zero and absent are different answers. On a non-Windows build the frame
/// genuinely holds nothing, and a line that omitted the field would be
/// indistinguishable from the reporting gap this exists to close — which is
/// precisely how the gap survived: nobody could tell a missing figure from a
/// figure that was not there.
#[test]
fn a_zero_software_frame_is_reported_rather_than_omitted() {
    let events =
        capture(|| emit_renderer_retention("WindowId(1)", "visible", &retention(4096, 0, 0)));
    let line = events.iter().find(|event| event.message == "renderer retention").expect("emitted");

    assert_eq!(
        line.field("software_frame_bytes"),
        Some(0),
        "a zero frame must be reported as zero, not left out"
    );
}

/// The sampling path must *call* the reporter.
///
/// The emitter having correct fields proves nothing on its own — that was true
/// of the renderer's own figures for as long as the defect existed. What was
/// missing was a call site, so a call site is what this pins: delete the call
/// from the sampling path and this fails, while every assertion above still
/// passes.
///
/// Source text rather than behaviour because the runtime path needs a live
/// `GpuRenderer`, which needs a real window and event loop. No test in the
/// workspace constructs one.
#[test]
fn the_sampling_path_calls_the_renderer_reporter() {
    const SAMPLING_SOURCE: &str = include_str!("retention.rs");

    assert!(
        SAMPLING_SOURCE.contains("self.log_renderer_retention()"),
        "the sampling path must call `log_renderer_retention`; without the call the renderer's \
         figures are computed and reported to nobody, which is the defect this closes"
    );
}

/// The reporter runs only when a sample was actually taken.
///
/// The sampling function is also the idle-wake path, and an accounting walk
/// placed above its cadence check is what made that path expensive before.
///
/// The cadence gate is an early return at the top of the function, so
/// "a sample was taken" and "control reached the tail" are the same statement
/// — the reporter needs no branch of its own, and what has to be asserted is
/// that it sits *after* the gate rather than inside any particular branch.
///
/// Anchored on the early return rather than on the level gate: the level
/// gate's text also appears in an earlier function in this file, so a position
/// compared against "the first gate" would be compared against the wrong one
/// and a hoisted call would read as correctly placed.
#[test]
fn the_reporter_runs_only_on_a_taken_sample() {
    const SAMPLING_SOURCE: &str = include_str!("retention.rs");

    let body_start = SAMPLING_SOURCE
        .find("fn sample_pane_retention")
        .expect("the sampling entry point must exist");
    let body = &SAMPLING_SOURCE[body_start..];

    let call = body
        .find("self.log_renderer_retention()")
        .expect("the sampling path must call the reporter");
    let cadence = body
        .find("if !retention_sample_due(")
        .expect("the sampling path must gate on the interval before doing any work");

    assert!(
        call > cadence,
        "the renderer report must sit below the cadence gate: above it, every idle wake pays \
         for a diagnostic that is only read once every thirty seconds"
    );

    // The gate is only a gate if it returns. A cadence check whose body did
    // not leave the function would let everything below it run on every wake
    // while still satisfying the ordering assertion above.
    let gate_body = &body[cadence..call];
    assert!(
        gate_body.contains("return false;"),
        "the cadence check must return, or the reporter is below a gate that does not gate"
    );
}

/// Every renderer is reported, not only the ones a user can see.
///
/// A warm renderer is fully constructed and holds the same full-size glyph
/// atlas as a visible one, so a reporter walking only `self.windows` omits a
/// live multi-megabyte buffer per pooled entry — and omits it silently, which
/// is the exact shape of the defect this module exists to close. Worse, the
/// omission misleads: a user summing the visible lines gets a figure below what
/// the process holds, and the remedy those lines imply cannot reach a warm
/// renderer.
///
/// Source text rather than behaviour: a `WarmWindow` owns a `GpuRenderer` by
/// value, and constructing one needs a live device and window that no test in
/// the workspace can build.
#[test]
fn both_the_visible_windows_and_the_warm_pool_are_reported() {
    const SOURCE: &str = include_str!("renderer_retention.rs");

    let body_start = SOURCE.find("fn log_renderer_retention").expect("the reporter must exist");
    let body = &SOURCE[body_start..];

    assert!(body.contains("&self.windows"), "the reporter must walk the visible windows");
    assert!(
        body.contains("self.warm_window_pool"),
        "the reporter must walk the warm pool: a pooled renderer holds a full-size glyph atlas, \
         and omitting it reports less memory than the process holds while pointing the reader at \
         a window they cannot close"
    );
}

/// A warm renderer is labelled as one.
///
/// Both roles hold the same buffers and the remedy differs: a visible renderer
/// is freed by closing its window, a warm one only by lowering the pool size.
/// A line that did not say which it was would send a reader to a window that
/// does not exist.
#[test]
fn the_role_distinguishes_a_warm_renderer_from_a_visible_one() {
    let events = capture(|| {
        emit_renderer_retention("warm[0]", "warm", &retention(16_777_216, 0, 0));
    });
    let line = events.iter().find(|event| event.message == "renderer retention").expect("emitted");

    assert!(
        line.strings.iter().any(|(key, value)| key == "role" && value == "warm"),
        "a warm renderer's line must name its role, or a reader cannot tell it from a window"
    );
}

/// Every field this line emits must be documented in both language halves.
///
/// A user following the memory-triage procedure reads the fields by name. A
/// field the procedure does not name is a field they cannot act on, and a
/// bilingual page drifts one half at a time.
///
/// Checked against **table rows** rather than the whole page. A guard that
/// accepts any mention is satisfied by the sample log block, so deleting the
/// table row it exists to protect leaves it green — the check exempted from the
/// thing it checks.
#[test]
fn the_wiki_documents_every_renderer_field_the_log_emits() {
    const WIKI: &str = include_str!("../../../../wiki/Logging.md");
    const SOURCE: &str = include_str!("renderer_retention.rs");

    let emitted: Vec<&str> = SOURCE
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let name = line.split(" = ").next()?;
            (name.ends_with("_bytes") && line.contains(" = retention.")).then_some(name)
        })
        .collect();

    assert!(!emitted.is_empty(), "the scan must find emitted fields, or it asserts nothing");

    for field in &emitted {
        let rows = WIKI
            .lines()
            .filter(|line| line.trim_start().starts_with("| `") && line.contains(field))
            .count();
        assert!(
            rows >= 2,
            "`{field}` is emitted by the renderer retention line but appears in {rows} \
             documentation table row(s) in wiki/Logging.md; both the English and 中文 tables must \
             describe it, and a mention in the sample block is not a description"
        );
    }
}
