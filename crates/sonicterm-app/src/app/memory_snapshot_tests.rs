use super::*;

pub(super) use std::sync::{Arc, Mutex};
pub(super) use std::time::{Duration, Instant};

pub(super) use sonicterm_types::ResourceAmount;
pub(super) use tracing::field::{Field, Visit};

#[derive(Debug)]
pub(super) struct FakeRenderer {
    pub(super) id: u64,
}

/// Build a unique five-field allocator reading so selection and summing mistakes are visible.
pub(super) fn fake_allocator(id: u64) -> sonicterm_gpu::core::AllocatorSnapshot {
    sonicterm_gpu::core::AllocatorSnapshot {
        allocated_bytes: id,
        reserved_bytes: id + 10,
        allocations: u32::try_from(id + 20).expect("fixture fits u32"),
        blocks: u32::try_from(id + 30).expect("fixture fits u32"),
        largest_block_bytes: id + 40,
    }
}
pub(super) use tracing_subscriber::layer::{Context, SubscriberExt};
pub(super) use tracing_subscriber::{EnvFilter, Layer, Registry};

/// One captured `tracing` event: its message, target, and every field.
///
/// Fields arrive in three shapes and all three matter here. Byte counts are
/// integers, the `unsupported`/`unavailable` sentinels are `Display` values,
/// and the composed renderer breakdown is a `Display` value too — so a capture
/// that handled only integers would silently drop exactly the fields whose
/// presence this module is responsible for.
#[derive(Debug, Default, Clone)]
pub(super) struct CapturedEvent {
    message: String,
    pub(super) target: String,
    pub(super) level: String,
    numbers: Vec<(String, u64)>,
    strings: Vec<(String, String)>,
}

impl CapturedEvent {
    pub(super) fn number(&self, name: &str) -> Option<u64> {
        self.numbers.iter().find(|(key, _)| key == name).map(|(_, value)| *value)
    }

    pub(super) fn text(&self, name: &str) -> Option<&str> {
        self.strings.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    /// Every field name the event carried, whatever its shape.
    pub(super) fn field_names(&self) -> Vec<&str> {
        self.numbers
            .iter()
            .map(|(key, _)| key.as_str())
            .chain(self.strings.iter().map(|(key, _)| key.as_str()))
            .collect()
    }
}

impl Visit for CapturedEvent {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.numbers.push((field.name().to_string(), value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.numbers.push((field.name().to_string(), value.unsigned_abs()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.strings.push((field.name().to_string(), value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            // `%`-sigil fields (Display) land here. Formatted rather than
            // discarded: the `unsupported` and `unavailable` sentinels are the
            // whole point of those fields.
            self.strings.push((field.name().to_string(), format!("{value:?}")));
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut captured = CapturedEvent {
            target: event.metadata().target().to_string(),
            level: event.metadata().level().to_string(),
            ..Default::default()
        };
        event.record(&mut captured);
        self.events.lock().expect("not poisoned").push(captured);
    }
}

/// Run `body` with a capturing subscriber and return every event it emitted.
pub(super) fn capture(body: impl FnOnce()) -> Vec<CapturedEvent> {
    let layer = CaptureLayer::default();
    let subscriber = Registry::default().with(layer.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let events = layer.events.lock().expect("not poisoned").clone();
    events
}

/// Run `body` under the real filter a configured level produces.
///
/// Distinct from [`capture`], which admits everything. A test that only ever
/// runs under a permissive subscriber proves a line is emitted and says
/// nothing about whether any shipped configuration admits it.
pub(super) fn capture_at(filter: &str, body: impl FnOnce()) -> Vec<CapturedEvent> {
    let layer = CaptureLayer::default();
    let subscriber = Registry::default()
        .with(EnvFilter::try_new(filter).expect("valid filter"))
        .with(layer.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let events = layer.events.lock().expect("not poisoned").clone();
    events
}

/// A snapshot with a distinct non-zero value in every field.
///
/// Distinct powers of two per pane seam: any subset sums to a unique value, so
/// a field dropped from the emitted line cannot be masked by the others.
pub(super) fn populated_snapshot() -> MemorySnapshot {
    MemorySnapshot {
        process: ProcessMemory {
            private_committed: MemoryMetric::Bytes(1_000),
            resident: MemoryMetric::Bytes(2_000),
            virtual_bytes: MemoryMetric::Bytes(4_000),
        },
        session: PaneRetention {
            grid_visible: ResourceAmount { bytes: 1, items: 1 },
            grid_history: ResourceAmount { bytes: 2, items: 2 },
            grid_alternate: ResourceAmount { bytes: 4, items: 4 },
            parser: ResourceAmount { bytes: 8, items: 8 },
            hyperlinks: ResourceAmount { bytes: 16, items: 16 },
            inline_media: ResourceAmount { bytes: 32, items: 32 },
            pty_output: ResourceAmount { bytes: 64, items: 64 },
            pty_input: ResourceAmount { bytes: 128, items: 128 },
        },
        panes_total: 4,
        panes_sampled: 3,
        panes_contended: 1,
        renderers: vec![
            RendererSummary {
                label: "WindowId(1)".to_string(),
                role: "visible",
                glyph_atlas: ResourceAmount { bytes: 512, items: 5 },
                image_atlas: ResourceAmount { bytes: 256, items: 2 },
                row_glyph_cache: ResourceAmount { bytes: 64, items: 4 },
                row_quad_cache: ResourceAmount { bytes: 32, items: 3 },
                software_frame: ResourceAmount { bytes: 1_024, items: 1 },
                vertex_scratch: ResourceAmount { bytes: 272, items: 1 },
                row_ink: ResourceAmount { bytes: 48, items: 40 },
                frame_scratch: ResourceAmount { bytes: 96, items: 2 },
                chrome_cache: ResourceAmount { bytes: 24, items: 3 },
                glyph_atlas_facts: sonicterm_gpu::core::GlyphAtlasFacts {
                    dim: 512,
                    packed_pixels: 4_000,
                    growths: 1,
                    evictions: 0,
                    fit: "512".to_string(),
                    max_tile: [25, 16],
                },
                trimmed: false,
                gpu_released_requested_bytes: 0,
            },
            RendererSummary {
                label: "0".to_string(),
                role: "warm",
                glyph_atlas: ResourceAmount { bytes: 128, items: 3 },
                image_atlas: ResourceAmount::default(),
                row_glyph_cache: ResourceAmount { bytes: 16, items: 2 },
                row_quad_cache: ResourceAmount { bytes: 8, items: 1 },
                software_frame: ResourceAmount::default(),
                vertex_scratch: ResourceAmount::default(),
                row_ink: ResourceAmount::default(),
                frame_scratch: ResourceAmount::default(),
                chrome_cache: ResourceAmount::default(),
                glyph_atlas_facts: Default::default(),
                trimmed: false,
                gpu_released_requested_bytes: 0,
            },
        ],
        trim_seq: 0,
        trim_source: None,
        allocator: Some(AllocatorReading {
            source: AllocatorSource::MainWindow,
            label: "WindowId(1)".to_string(),
            snapshot: Some(fake_allocator(9)),
        }),
        live_renderers: 2,
        live_fg_probe_workers: 0,
    }
}

/// A snapshot holding nothing at all.
pub(super) fn empty_snapshot() -> MemorySnapshot {
    MemorySnapshot {
        process: ProcessMemory::unsupported(),
        session: PaneRetention::default(),
        panes_total: 0,
        panes_sampled: 0,
        panes_contended: 0,
        renderers: Vec::new(),
        trim_seq: 0,
        trim_source: None,
        allocator: None,
        live_renderers: 0,
        live_fg_probe_workers: 0,
    }
}

/// Both renderer roles appear, each with its own identity and figures.
///
/// A warm renderer holds a full-size glyph atlas exactly like a visible one,
/// so a report covering only visible windows understates the process — and the
/// remedy the visible lines imply, closing a window, cannot reach a warm
/// renderer at all.
#[test]
pub(super) fn visible_and_warm_renderers_are_both_reported_with_their_roles() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    let rendered = events[0].text("renderers").expect("the breakdown field is emitted");

    assert!(rendered.contains("visible[WindowId(1)]"), "missing the visible renderer: {rendered}");
    assert!(
        rendered.contains("warm[0]"),
        "missing the warm renderer; the warm pool holds full-size atlases and a report that \
         omits them understates the process: {rendered}"
    );
    assert!(rendered.contains("glyph=512/5"), "visible glyph atlas bytes/items: {rendered}");
    assert!(rendered.contains("image=256/2"), "visible image atlas bytes/items: {rendered}");
    assert!(rendered.contains("row_glyph=64/4"), "visible glyph-cache bytes/items: {rendered}");
    assert!(rendered.contains("row_quad=32/3"), "visible quad-cache bytes/items: {rendered}");
    assert!(rendered.contains("software=1024/1"), "software frame bytes/items: {rendered}");
    assert!(rendered.contains("vertex=272/1"), "visible vertex scratch bytes/items: {rendered}");
    // Per-row ink records are a renderer part, so the breakdown and its total carry them.
    assert!(rendered.contains("row_ink=48/40"), "visible row ink bytes/items: {rendered}");
    // The frame scratch and chrome caches are renderer parts too, inside the same total.
    assert!(rendered.contains("frame_scratch=96/2"), "frame scratch bytes/items: {rendered}");
    assert!(rendered.contains("chrome_cache=24/3"), "chrome cache bytes/items: {rendered}");
    // The six glyph atlas facts each renderer reports, in the order perf-compare reads them.
    for field in [
        "glyph_atlas_dim=512",
        "glyph_atlas_packed_pixels=4000",
        "glyph_atlas_growths=1",
        "glyph_atlas_evictions=0",
        "glyph_atlas_fit=512",
        "glyph_atlas_max_tile=25x16",
    ] {
        assert!(rendered.contains(field), "{field}: {rendered}");
    }
    assert!(rendered.contains("glyph=128/3"), "warm glyph atlas bytes/items: {rendered}");
    assert!(rendered.contains("row_glyph=16/2"), "warm glyph-cache bytes/items: {rendered}");
    assert!(rendered.contains("row_quad=8/1"), "warm quad-cache bytes/items: {rendered}");
}

/// Renderer breakdown text is stable even when summaries arrive in a different order.
///
/// Two snapshots with reversed inputs exercise the production serializer, which sorts by role and
/// label before joining entries.
#[test]
pub(super) fn renderer_breakdown_order_is_stable_across_input_order() {
    let make = |label: &str, role| RendererSummary {
        label: label.to_string(),
        role,
        glyph_atlas: ResourceAmount::default(),
        image_atlas: ResourceAmount::default(),
        row_glyph_cache: ResourceAmount::default(),
        row_quad_cache: ResourceAmount::default(),
        software_frame: ResourceAmount::default(),
        vertex_scratch: ResourceAmount::default(),
        row_ink: ResourceAmount::default(),
        frame_scratch: ResourceAmount::default(),
        chrome_cache: ResourceAmount::default(),
        glyph_atlas_facts: Default::default(),
        trimmed: false,
        gpu_released_requested_bytes: 0,
    };
    let mut first = empty_snapshot();
    first.renderers = vec![
        make("WindowId(9)", "visible"),
        make("1", "warm"),
        make("WindowId(2)", "visible"),
        make("0", "warm"),
    ];
    let mut second = empty_snapshot();
    second.renderers = vec![
        make("0", "warm"),
        make("WindowId(2)", "visible"),
        make("1", "warm"),
        make("WindowId(9)", "visible"),
    ];

    assert_eq!(first.render_renderers(), second.render_renderers());
}

/// Renderer totals fold every renderer and every retained part.
#[test]
pub(super) fn renderer_totals_fold_every_renderer() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    let event = &events[0];

    // 512 + 256 + 64 + 32 + 1024 + 272 + 48 + 96 + 24 (visible) + 128 + 16 + 8 (warm)
    assert_eq!(event.number("renderer_total_bytes"), Some(2_480));
    // 5 + 2 + 4 + 3 + 1 + 1 + 40 + 2 + 3 (visible) + 3 + 2 + 1 (warm)
    assert_eq!(event.number("renderer_total_items"), Some(67));
    assert_eq!(event.number("renderer_row_glyph_cache_bytes"), Some(80));
    assert_eq!(event.number("renderer_row_glyph_cache_items"), Some(6));
    assert_eq!(event.number("renderer_row_quad_cache_bytes"), Some(40));
    assert_eq!(event.number("renderer_row_quad_cache_items"), Some(4));
}

/// The live-renderer count is read from the renderer crate, not derived.
///
/// The two figures agreeing is the useful signal. A leaked renderer is alive
/// without being reachable from any window, so it raises the live count while
/// contributing no summary line — a discrepancy that is invisible if the count
/// is computed from the summaries.
#[test]
pub(super) fn the_live_renderer_count_is_reported_independently_of_the_summaries() {
    let mut snapshot = populated_snapshot();
    snapshot.live_renderers = 7;

    let events = capture(|| emit_memory_snapshot(&snapshot, None));
    assert_eq!(
        events[0].number("live_renderers"),
        Some(7),
        "the live count must be reported as measured, not recomputed from the two summaries; \
         a leaked renderer shows up only as a difference between them"
    );
}

/// A renderer-less session says so rather than emitting a blank field.
///
/// A blank field reads as a bug in the logger. `none` is a statement.
#[test]
pub(super) fn a_session_with_no_renderer_says_none() {
    let events = capture(|| emit_memory_snapshot(&empty_snapshot(), None));
    let event = &events[0];
    assert_eq!(event.text("renderers"), Some("none"));
    assert_eq!(event.text("allocator_state"), Some("none"));
    assert_eq!(event.text("allocator_source"), Some("none"));
    assert_eq!(event.text("allocator_label"), Some("none"));
    assert_eq!(event.text("allocator_allocated_bytes"), Some("none"));
}

/// A measured allocator report emits one complete scalar field set.
///
/// Distinct fixture values prove every field comes from the single authoritative reading rather
/// than one copy per visible or warm renderer.
#[test]
pub(super) fn measured_allocator_is_emitted_once_without_renderer_multiplication() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    let event = &events[0];

    assert_eq!(event.text("allocator_state"), Some("measured"));
    assert_eq!(event.text("allocator_source"), Some("main"));
    assert_eq!(event.text("allocator_label"), Some("WindowId(1)"));
    assert_eq!(event.text("allocator_allocated_bytes"), Some("9"));
    assert_eq!(event.text("allocator_reserved_bytes"), Some("19"));
    assert_eq!(event.text("allocator_allocations"), Some("29"));
    assert_eq!(event.text("allocator_blocks"), Some("39"));
    assert_eq!(event.text("allocator_largest_block_bytes"), Some("49"));
}

/// Backend-unavailable allocator telemetry is explicit and keeps its reader identity.
///
/// Clearing only the report exercises the unsupported state without turning it into no-renderer
/// or measured zeroes.
#[test]
pub(super) fn unsupported_allocator_report_is_explicit_not_zero() {
    let mut snapshot = populated_snapshot();
    snapshot.allocator.as_mut().expect("allocator reading").snapshot = None;
    let events = capture(|| emit_memory_snapshot(&snapshot, None));
    let event = &events[0];
    assert_eq!(event.text("allocator_state"), Some("unsupported"));
    assert_eq!(event.text("allocator_source"), Some("main"));
    assert_eq!(event.text("allocator_label"), Some("WindowId(1)"));
    assert_eq!(event.text("allocator_allocated_bytes"), Some("unsupported"));
    assert_eq!(event.text("allocator_reserved_bytes"), Some("unsupported"));
}

/// The first sample of a session has nothing to compare against, and says so.
///
/// `+0` would claim the process had not moved, which is a measurement. There
/// is no measurement here — there is no earlier sample.
#[test]
pub(super) fn the_first_sample_reports_deltas_as_unavailable() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    let event = &events[0];

    for field in [
        "process_private_committed_delta",
        "process_resident_delta",
        "process_virtual_delta",
        "session_delta",
        "renderer_delta",
    ] {
        assert_eq!(
            event.text(field),
            Some("unavailable"),
            "{field:?} must be explicitly unavailable on the first sample, not +0; the latter \
             claims the process did not move"
        );
    }
}

/// A later sample reports signed movement against the previous one.
#[test]
pub(super) fn a_later_sample_reports_signed_movement() {
    let first = populated_snapshot();
    let mut second = populated_snapshot();
    second.process.resident = MemoryMetric::Bytes(3_500);
    second.session.grid_history = ResourceAmount { bytes: 1_002, items: 2 };
    second.renderers[0].glyph_atlas = ResourceAmount { bytes: 12, items: 5 };

    let events = capture(|| emit_memory_snapshot(&second, Some(first.totals())));
    let event = &events[0];

    // 3500 - 2000
    assert_eq!(event.text("process_resident_delta"), Some("+1500"));
    // session grew by 1000 in the history seam
    assert_eq!(event.text("session_delta"), Some("+1000"));
    // visible glyph atlas fell 512 -> 12
    assert_eq!(
        event.text("renderer_delta"),
        Some("-500"),
        "a shrinking figure must report a negative delta; direction is the whole signal in a \
         growth investigation"
    );
}

/// An unsupported process figure yields an unavailable delta, not a zero.
///
/// The two sides of this are separate claims: the platform not exposing a
/// figure is not the same as the figure not having moved, and a reader acting
/// on "did not move" would rule out the wrong subsystem.
#[test]
pub(super) fn an_unsupported_process_figure_never_produces_a_numeric_delta() {
    let mut first = populated_snapshot();
    first.process.private_committed = MemoryMetric::Unsupported;
    let mut second = populated_snapshot();
    second.process.private_committed = MemoryMetric::Unsupported;

    let events = capture(|| emit_memory_snapshot(&second, Some(first.totals())));
    let event = &events[0];

    assert_eq!(event.text("process_private_committed_bytes"), Some("unsupported"));
    assert_eq!(
        event.text("process_private_committed_delta"),
        Some("unavailable"),
        "an unsupported figure cannot have moved by a measurable amount"
    );
    assert_eq!(
        event.text("process_resident_delta"),
        Some("+0"),
        "a supported figure that genuinely did not move reports +0, which is a measurement \
         and must stay distinguishable from `unavailable`"
    );
}

/// A configured `level = "info"` session admits the snapshot.
///
/// Driven through the real filter the logging crate produces rather than a
/// hand-written directive: a test that writes its own filter proves the gate
/// behind it works and says nothing about whether any shipped configuration
/// opens that gate.
#[test]
pub(super) fn a_configured_info_session_records_the_snapshot() {
    let filter = sonicterm_logging::filter_for_level(sonicterm_logging::LogLevel::Info);
    let events = capture_at(filter, || emit_memory_snapshot(&populated_snapshot(), None));

    assert_eq!(
        events.len(),
        1,
        "`level = \"info\"` must admit the aggregate snapshot; it is the report a growth \
         investigation reads after the fact"
    );
    assert_eq!(events[0].target, "memory");
}

/// Admitting the snapshot at info must not drag the DEBUG detail in.
///
/// The per-pane and per-renderer lines fire once per pane and once per
/// renderer on every cycle. Opening the target wholesale would satisfy the
/// test above while flooding a session that merely turned on informational
/// logging.
#[test]
pub(super) fn an_info_session_does_not_record_the_debug_detail() {
    let filter = sonicterm_logging::filter_for_level(sonicterm_logging::LogLevel::Info);
    let events = capture_at(filter, || {
        emit_memory_snapshot(&populated_snapshot(), None);
        super::super::renderer_retention::emit_renderer_retention(
            "WindowId(1)",
            "visible",
            &sonicterm_gpu::core::RendererRetention::default(),
        );
        super::super::retention::log_pane_retention("w/1", &PaneRetention::default());
    });

    assert_eq!(
        events.len(),
        1,
        "exactly the aggregate should survive an info session; the DEBUG per-pane and \
         per-renderer lines belong to someone already investigating. Captured: {:?}",
        events.iter().map(|event| event.message.clone()).collect::<Vec<_>>()
    );
}

/// The snapshot carries no user content.
///
/// Class names, roles, identifiers, counts, and byte sizes only. A window
/// *title* is user content and a window *id* is not, which is why the renderer
/// breakdown carries the latter. This scans every emitted field value for a
/// sentinel that would only appear if titles or terminal text ever reached it.
#[test]
pub(super) fn the_snapshot_carries_no_user_content() {
    let mut snapshot = populated_snapshot();
    // A label shaped like the identifiers production emits. If a future change
    // substituted a window title here, the sentinel below would surface in the
    // rendered field.
    snapshot.renderers[0].label = "WindowId(42)".to_string();

    let events = capture(|| emit_memory_snapshot(&snapshot, None));
    let event = &events[0];

    let rendered: String = event
        .strings
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ");

    for forbidden in ["$", "~/", "export ", "PASSWORD", "token", "/Users/", "C:\\Users\\"] {
        assert!(
            !rendered.contains(forbidden),
            "the snapshot emitted {forbidden:?}, which can only come from shell, path, or \
             credential content. Line was: {rendered}"
        );
    }
}

#[test]
pub(super) fn inline_media_contention_marks_the_snapshot_partial() {
    let mut app = super::super::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let window = app.__test_seed_child_window(&["one"]);
    let pane_id = app.__test_child_pane_ids(window).expect("child exists")[0];
    let held = app
        .windows
        .get(&window)
        .and_then(|window| window.panes.get(&pane_id))
        .expect("pane exists")
        .inline_images
        .lock();

    let snapshot = app.build_memory_snapshot();

    assert_eq!(snapshot.panes_total, 1);
    assert_eq!(snapshot.panes_sampled, 0);
    assert_eq!(snapshot.panes_contended, 1);
    assert_eq!(snapshot.session_bytes(), 0, "a partial pane must not contribute invented zeros");
    drop(held);
}

/// Sampling repeats on the cadence without any redraw being involved.
///
/// The cadence is shared with the existing retention pass rather than given
/// its own timer, which is what keeps a default session paying one comparison
/// per wake. Pinned here against the real gate so a change to the interval
/// cannot silently make the snapshot per-wake.
#[test]
pub(super) fn sampling_repeats_on_the_shared_cadence() {
    let start = Instant::now();
    let mut last: Option<Instant> = None;

    assert!(
        super::super::retention::retention_sample_due(&mut last, start),
        "the first sample of a session is always due"
    );
    assert!(
        !super::super::retention::retention_sample_due(&mut last, start + Duration::from_secs(29)),
        "a sample inside the interval must not fire; the walk is far too costly per wake"
    );
    assert!(
        super::super::retention::retention_sample_due(&mut last, start + Duration::from_secs(30)),
        "an idle session must keep sampling; a growth curve needs more than one point"
    );
    assert!(
        super::super::retention::retention_sample_due(&mut last, start + Duration::from_secs(60)),
        "and must keep sampling indefinitely, not twice"
    );
}

/// The fields a checkpoint sample adds to the periodic line in a build with the trim hook, as
/// every unit-test build is: four checkpoint tags and three trim tags.
pub(super) const CHECKPOINT_FIELDS: [&str; 7] = [
    "checkpoint_index",
    "checkpoint_label",
    "checkpoint_attempt",
    "checkpoint_complete",
    "trimmed",
    "trim_source",
    "trim_seq",
];

/// A one-pane app, for the checkpoint hook.
pub(super) fn app_with_one_pane() -> (super::super::App, winit::window::WindowId, u64) {
    let mut app = super::super::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let window = app.__test_seed_child_window(&["one"]);
    let pane_id = app.__test_child_pane_ids(window).expect("child exists")[0];
    (app, window, pane_id)
}

/// One checkpoint call emits exactly one INFO `memory` line: the periodic line's fields plus the
/// four checkpoint tags, with the index, label and attempt it was given, complete when no pane is
/// contended, and the three trim tags. A periodic line carries none of the tags.
#[test]
pub(super) fn a_checkpoint_sample_is_the_periodic_line_plus_four_tags() {
    let (mut app, _window, _pane) = app_with_one_pane();
    let mut returned = None;
    let events = capture(|| returned = Some(app.__perf_checkpoint_memory(2, "end", 3)));
    let periodic = capture(|| emit_memory_snapshot(&app.build_memory_snapshot(), None));

    assert_eq!(events.len(), 1, "one call, one line");
    let line = &events[0];
    assert_eq!((line.target.as_str(), line.level.as_str()), ("memory", "INFO"));
    assert_eq!(line.message, periodic[0].message);
    assert_eq!(line.number("checkpoint_index"), Some(2));
    assert_eq!(line.text("checkpoint_label"), Some("end"));
    assert_eq!(line.number("checkpoint_attempt"), Some(3));
    assert_eq!(line.text("checkpoint_complete"), Some("true"));
    assert_eq!(returned, Some(CheckpointMemory { complete: true, panes_contended: 0 }));

    let mut tagged: Vec<&str> = line.field_names();
    tagged.retain(|name| !CHECKPOINT_FIELDS.contains(name));
    let mut plain = periodic[0].field_names();
    tagged.sort_unstable();
    plain.sort_unstable();
    assert_eq!(tagged, plain, "the checkpoint line carries every periodic field");
    for name in CHECKPOINT_FIELDS {
        assert!(!periodic[0].field_names().contains(&name), "a periodic line has no {name}");
    }
}

/// A pane whose parser lock is held during the call makes the sample partial: the line says
/// `checkpoint_complete=false` and the call reports the contended pane.
#[test]
pub(super) fn a_contended_pane_makes_the_checkpoint_sample_partial() {
    let (mut app, window, pane_id) = app_with_one_pane();
    let parser = Arc::clone(
        &app.windows
            .get(&window)
            .and_then(|window| window.panes.get(&pane_id))
            .expect("pane")
            .parser,
    );
    let held = parser.lock();
    let mut returned = None;
    let events = capture(|| returned = Some(app.__perf_checkpoint_memory(0, "end", 1)));
    drop(held);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].text("checkpoint_complete"), Some("false"));
    assert_eq!(events[0].number("panes_contended"), Some(1));
    assert_eq!(returned, Some(CheckpointMemory { complete: false, panes_contended: 1 }));
}

/// One `bytes`-byte image appended to `pane`, trimmed to its budget as a decode does.
pub(super) fn decode_image(pane: &super::super::PaneState, id: u64, bytes: usize) {
    let evicted = {
        let mut images = pane.inline_images.lock();
        images.push(sonicterm_render_model::InlineImage {
            id,
            row: 0,
            col: 0,
            width: 1,
            height: 1,
            bgra: Arc::from(vec![0u8; bytes]),
        });
        crate::app::media::trim_inline_images_charged(&mut images, &pane.inline_media_charge)
    };
    drop(evicted);
}

/// A checkpoint sample only measures, even when the retention pass has work waiting. The app has a
/// periodic cadence and previous-cycle totals set, a pane whose stalled capture the next reclaim
/// would cancel, and media over the process ceiling that the next trim would discard (private pools,
/// so siblings cannot share them). After the call every one of those is exactly as before: the
/// cadence, the totals, the capture and its stall evidence, the pane's media and the pool's bytes.
#[test]
pub(super) fn a_checkpoint_sample_leaves_the_retention_pass_untouched() {
    use crate::app::media::{InlineMediaPool, MAX_PROCESS_INLINE_MEDIA_BYTES};
    const IMAGE_BYTES: usize = 1024 * 1024;
    let pool = InlineMediaPool::new();
    let mut app = super::super::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_capture_staging_pool(sonicterm_vt::vt::CaptureStagingPool::new())
    .with_inline_media_pool(pool.clone());
    let window = app.__test_seed_child_window(&["one"]);
    let pane_id = app.__test_child_pane_ids(window).expect("child exists")[0];

    // A capture still open, with stall evidence one sample short of cancellation.
    let mut chunk = Vec::with_capacity(512 * 1024);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(512 * 1024, b'A');
    {
        let pane = app
            .windows
            .get_mut(&window)
            .and_then(|state| state.panes.get_mut(&pane_id))
            .expect("pane");
        let progress = {
            let mut parser = pane.parser.lock();
            parser.advance(&chunk);
            parser.capture_progress()
        };
        pane.last_capture_progress = Some(progress);
        pane.capture_stall_samples = super::super::retention::STALL_SAMPLES_BEFORE_CANCEL - 1;
    }
    assert_eq!(
        app.__test_pane_capture_count(window, pane_id),
        Some(1),
        "precondition: a capture is open"
    );

    // The pane fills early; later panes push the pool over its ceiling, so a trim would cut it back.
    let mut next_id = 0u64;
    for _ in 0..64 {
        next_id += 1;
        decode_image(&app.windows[&window].panes[&pane_id], next_id, IMAGE_BYTES);
    }
    let mut crowd = Vec::new();
    while pool.bytes() <= MAX_PROCESS_INLINE_MEDIA_BYTES && crowd.len() < 256 {
        let pane = super::super::PaneState::new_with_media_pool(
            Arc::new(parking_lot::Mutex::new(sonicterm_vt::vt::Parser::new(
                sonicterm_grid::grid::Grid::new(80, 24),
            ))),
            None,
            &pool,
        );
        for _ in 0..8 {
            next_id += 1;
            decode_image(&pane, next_id, IMAGE_BYTES);
        }
        crowd.push(pane);
    }
    assert!(
        pool.bytes() > MAX_PROCESS_INLINE_MEDIA_BYTES,
        "precondition: the pool is over its ceiling"
    );
    let media_bytes = |app: &super::super::App| {
        crate::app::media::retained_inline_media(
            &app.windows[&window].panes[&pane_id].inline_images.lock(),
        )
        .bytes
    };
    let pane_media = media_bytes(&app);
    assert!(
        pane_media > crate::app::media::MIN_PANE_INLINE_MEDIA_BYTES,
        "precondition: a trim would cut it"
    );

    let cadence = Some(Instant::now() - Duration::from_secs(5));
    app.last_retention_sample = cadence;
    app.last_memory_totals = Some(app.build_memory_snapshot().totals());
    let totals = app.last_memory_totals;
    let pool_bytes = pool.bytes();

    let _events = capture(|| {
        app.__perf_checkpoint_memory(0, "end", 1);
    });

    assert_eq!(app.last_retention_sample, cadence, "the periodic cadence is not reset");
    assert_eq!(app.last_memory_totals, totals, "the next periodic delta keeps its baseline");
    assert_eq!(
        app.__test_pane_capture_count(window, pane_id),
        Some(1),
        "the capture was not reclaimed"
    );
    let pane = &app.windows[&window].panes[&pane_id];
    assert_eq!(
        pane.capture_stall_samples,
        super::super::retention::STALL_SAMPLES_BEFORE_CANCEL - 1,
        "the stall evidence was not advanced"
    );
    assert_eq!(media_bytes(&app), pane_media, "the pane's media was not trimmed");
    assert_eq!(pool.bytes(), pool_bytes, "nothing was discarded from the pool");
    assert_eq!(app.__test_pane_charge_total(window, pane_id), Some(0), "nothing was charged");
    drop(crowd);
}

/// A trimmed visible renderer reports `renderer_trimmed=true` and its released GPU request size
/// inside the renderer breakdown, and that GPU figure never enters `renderer_total_bytes`, which
/// counts CPU storage only.
#[test]
pub(super) fn a_trimmed_renderer_reports_its_gpu_release_outside_the_renderer_total() {
    let untrimmed = populated_snapshot();
    let mut trimmed = populated_snapshot();
    trimmed.renderers[0].trimmed = true;
    trimmed.renderers[0].gpu_released_requested_bytes = 8_388_608;
    assert_eq!(trimmed.renderer_bytes(), untrimmed.renderer_bytes());

    let events = capture(|| emit_memory_snapshot(&trimmed, None));
    let breakdown = events[0].text("renderers").expect("the breakdown is emitted").to_string();
    assert!(
        breakdown.contains("renderer_trimmed=true renderer_gpu_released_requested_bytes=8388608"),
        "{breakdown}"
    );
    assert!(
        breakdown.contains("renderer_trimmed=false renderer_gpu_released_requested_bytes=0"),
        "{breakdown}"
    );
    // No renderer entry may carry a bare `trimmed=`: a reader scanning the line for the top-level
    // checkpoint tag would find it first.
    assert!(
        !breakdown.split_whitespace().any(|field| field.starts_with("trimmed=")),
        "{breakdown}"
    );
    assert_eq!(events[0].number("renderer_total_bytes"), Some(untrimmed.renderer_bytes() as u64));
}

/// A checkpoint line carries `trimmed`, `trim_source` and `trim_seq` from the App's trim state:
/// before any trim they read false, `none` and 0; after a trim they read the trimmed window, the
/// source that requested it and its number. A periodic line carries none of the three.
#[test]
pub(super) fn a_checkpoint_line_carries_the_trim_state_and_a_periodic_line_does_not() {
    let (mut app, window, _pane) = app_with_one_pane();
    let before = capture(|| {
        app.__perf_checkpoint_memory(0, "covered", 1);
    });
    assert_eq!(before[0].text("trimmed"), Some("false"));
    assert_eq!(before[0].text("trim_source"), Some("none"));
    assert_eq!(before[0].number("trim_seq"), Some(0));

    // The test window has no renderer, so the App's trim state and the trimmed renderer's summary
    // are set as a trim would set them.
    app.trim_seq = 3;
    app.last_trim_source = Some(super::super::retention::TrimSource::Hook);
    let mut snapshot = app.build_memory_snapshot();
    snapshot.renderers.push(RendererSummary {
        label: format!("{window:?}"),
        role: "visible",
        glyph_atlas: ResourceAmount::default(),
        image_atlas: ResourceAmount::default(),
        row_glyph_cache: ResourceAmount::default(),
        row_quad_cache: ResourceAmount::default(),
        software_frame: ResourceAmount::default(),
        vertex_scratch: ResourceAmount::default(),
        row_ink: ResourceAmount::default(),
        frame_scratch: ResourceAmount::default(),
        chrome_cache: ResourceAmount::default(),
        glyph_atlas_facts: Default::default(),
        trimmed: true,
        gpu_released_requested_bytes: 4_096,
    });
    let tagged = capture(|| {
        emit_tagged_memory_snapshot(
            &snapshot,
            None,
            Some(CheckpointTag { index: 1, label: "covered", attempt: 1 }),
        );
    });
    assert_eq!(tagged[0].text("trimmed"), Some("true"));
    assert_eq!(tagged[0].text("trim_source"), Some("hook"));
    assert_eq!(tagged[0].number("trim_seq"), Some(3));

    let periodic = capture(|| emit_memory_snapshot(&snapshot, None));
    for name in ["trimmed", "trim_source", "trim_seq"] {
        assert!(!periodic[0].field_names().contains(&name), "a periodic line has no {name}");
    }
}

/// One trimmed renderer entry exactly as the breakdown writes it. The comparison script's parser
/// tests build their lines from this same text, so a rename on either side fails one of them.
pub(super) const TRIMMED_RENDERER_ENTRY: &str =
    "visible[WindowId(1)] glyph=512/5 image=256/2 row_glyph=64/4 \
     row_quad=32/3 software=1024/1 vertex=272/1 row_ink=48/40 frame_scratch=96/2 chrome_cache=24/3 \
     total=2328/61 glyph_atlas_dim=512 glyph_atlas_packed_pixels=4000 glyph_atlas_growths=1 \
     glyph_atlas_evictions=0 glyph_atlas_fit=512 glyph_atlas_max_tile=25x16 renderer_trimmed=true \
     renderer_gpu_released_requested_bytes=8388608";

/// The renderer entry's trim fields keep renderer-local names, byte for byte as the comparison
/// script's tests expect them, so its top-level `trimmed` tag can never be read from a renderer.
#[test]
pub(super) fn the_renderer_entry_names_its_trim_fields_apart_from_the_checkpoint_tags() {
    let mut summary = populated_snapshot().renderers.remove(0);
    summary.trimmed = true;
    summary.gpu_released_requested_bytes = 8_388_608;
    assert_eq!(summary.render(), TRIMMED_RENDERER_ENTRY);
    let python_tests = include_str!("../../../../scripts/perf-compare_tests.py");
    assert!(
        python_tests.contains(TRIMMED_RENDERER_ENTRY),
        "perf-compare_tests.py builds its renderer entries from this exact text"
    );
}

/// Every field name an event records, in record order; the message is not a field here.
#[derive(Default)]
pub(super) struct OrderedNames(Vec<String>);

impl Visit for OrderedNames {
    fn record_debug(&mut self, field: &Field, _value: &dyn std::fmt::Debug) {
        if field.name() != "message" {
            self.0.push(field.name().to_string());
        }
    }
}

/// Layer that records each event's field names in order.
#[derive(Clone, Default)]
pub(super) struct OrderLayer {
    names: Arc<Mutex<Vec<Vec<String>>>>,
}

impl<S: tracing::Subscriber> Layer<S> for OrderLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut names = OrderedNames::default();
        event.record(&mut names);
        self.names.lock().expect("not poisoned").push(names.0);
    }
}

/// The field names of the one event `body` emits, in order.
pub(super) fn ordered_field_names(body: impl FnOnce()) -> Vec<String> {
    let layer = OrderLayer::default();
    let subscriber = Registry::default().with(layer.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let mut events = layer.names.lock().expect("not poisoned").clone();
    assert_eq!(events.len(), 1);
    events.remove(0)
}
