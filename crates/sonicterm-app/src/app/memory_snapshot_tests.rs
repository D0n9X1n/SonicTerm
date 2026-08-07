use super::*;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sonicterm_types::ResourceAmount;
use tracing::field::{Field, Visit};

#[derive(Debug)]
struct FakeRenderer {
    id: u64,
}

fn fake_allocator(id: u64) -> sonicterm_gpu::core::AllocatorSnapshot {
    sonicterm_gpu::core::AllocatorSnapshot {
        allocated_bytes: id,
        reserved_bytes: id + 10,
        allocations: u32::try_from(id + 20).expect("fixture fits u32"),
        blocks: u32::try_from(id + 30).expect("fixture fits u32"),
        largest_block_bytes: id + 40,
    }
}

#[test]
fn shared_allocator_is_read_once_from_the_main_renderer_without_summing() {
    let calls = std::cell::Cell::new(0);
    let main = FakeRenderer { id: 1 };
    let visible_a = FakeRenderer { id: 2 };
    let visible_b = FakeRenderer { id: 3 };
    let warm = FakeRenderer { id: 4 };
    let visible = [("WindowId(3)", &visible_a), ("WindowId(2)", &visible_b)];
    let warm = [("0", &warm)];

    let reading =
        read_authoritative_allocator(Some(("WindowId(1)", &main)), &visible, &warm, |renderer| {
            calls.set(calls.get() + 1);
            Some(fake_allocator(renderer.id))
        })
        .expect("main renderer selected");

    assert_eq!(calls.get(), 1);
    assert_eq!(reading.source, AllocatorSource::MainWindow);
    assert_eq!(reading.label, "WindowId(1)");
    assert_eq!(reading.snapshot, Some(fake_allocator(1)));
}

#[test]
fn visible_allocator_selection_is_stable_across_input_order() {
    let a = FakeRenderer { id: 1 };
    let b = FakeRenderer { id: 2 };
    let c = FakeRenderer { id: 3 };
    let orders = [
        [("WindowId(9)", &a), ("WindowId(2)", &b), ("WindowId(5)", &c)],
        [("WindowId(5)", &c), ("WindowId(9)", &a), ("WindowId(2)", &b)],
        [("WindowId(2)", &b), ("WindowId(5)", &c), ("WindowId(9)", &a)],
    ];

    for visible in orders {
        let calls = std::cell::Cell::new(0);
        let reading = read_authoritative_allocator(None, &visible, &[], |renderer| {
            calls.set(calls.get() + 1);
            Some(fake_allocator(renderer.id))
        })
        .expect("visible renderer selected");
        assert_eq!(calls.get(), 1);
        assert_eq!(reading.source, AllocatorSource::VisibleWindow);
        assert_eq!(reading.label, "WindowId(2)");
        assert_eq!(reading.snapshot, Some(fake_allocator(2)));
    }
}

#[test]
fn allocator_selection_uses_warm_only_without_a_visible_renderer() {
    let calls = std::cell::Cell::new(0);
    let warm = FakeRenderer { id: 7 };
    let reading = read_authoritative_allocator(None, &[], &[("0", &warm)], |renderer| {
        calls.set(calls.get() + 1);
        Some(fake_allocator(renderer.id))
    })
    .expect("warm renderer selected");

    assert_eq!(calls.get(), 1);
    assert_eq!(reading.source, AllocatorSource::WarmPool);
    assert_eq!(reading.label, "0");
    assert_eq!(reading.snapshot, Some(fake_allocator(7)));
}

#[test]
fn allocator_selection_reads_nothing_when_no_renderer_exists() {
    let calls = std::cell::Cell::new(0);
    let reading = read_authoritative_allocator::<FakeRenderer>(None, &[], &[], |renderer| {
        calls.set(calls.get() + 1);
        Some(fake_allocator(renderer.id))
    });
    assert_eq!(calls.get(), 0);
    assert_eq!(reading, None);
}

#[test]
fn unsupported_allocator_report_is_still_exactly_one_read() {
    let calls = std::cell::Cell::new(0);
    let main = FakeRenderer { id: 1 };
    let reading = read_authoritative_allocator(Some(("WindowId(1)", &main)), &[], &[], |_| {
        calls.set(calls.get() + 1);
        None
    })
    .expect("main renderer selected");

    assert_eq!(calls.get(), 1);
    assert_eq!(reading.snapshot, None);
}
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// One captured `tracing` event: its message, target, and every field.
///
/// Fields arrive in three shapes and all three matter here. Byte counts are
/// integers, the `unsupported`/`unavailable` sentinels are `Display` values,
/// and the composed renderer breakdown is a `Display` value too — so a capture
/// that handled only integers would silently drop exactly the fields whose
/// presence this module is responsible for.
#[derive(Debug, Default, Clone)]
struct CapturedEvent {
    message: String,
    target: String,
    level: String,
    numbers: Vec<(String, u64)>,
    strings: Vec<(String, String)>,
}

impl CapturedEvent {
    fn number(&self, name: &str) -> Option<u64> {
        self.numbers.iter().find(|(key, _)| key == name).map(|(_, value)| *value)
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.strings.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    /// Every field name the event carried, whatever its shape.
    fn field_names(&self) -> Vec<&str> {
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
struct CaptureLayer {
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
fn capture(body: impl FnOnce()) -> Vec<CapturedEvent> {
    let layer = CaptureLayer::default();
    let subscriber = Registry::default().with(layer.clone());
    tracing::subscriber::with_default(subscriber, body);
    let events = layer.events.lock().expect("not poisoned").clone();
    events
}

/// Run `body` under the real filter a configured level produces.
///
/// Distinct from [`capture`], which admits everything. A test that only ever
/// runs under a permissive subscriber proves a line is emitted and says
/// nothing about whether any shipped configuration admits it.
fn capture_at(filter: &str, body: impl FnOnce()) -> Vec<CapturedEvent> {
    let layer = CaptureLayer::default();
    let subscriber = Registry::default()
        .with(EnvFilter::try_new(filter).expect("valid filter"))
        .with(layer.clone());
    tracing::subscriber::with_default(subscriber, body);
    let events = layer.events.lock().expect("not poisoned").clone();
    events
}

/// A snapshot with a distinct non-zero value in every field.
///
/// Distinct powers of two per pane seam: any subset sums to a unique value, so
/// a field dropped from the emitted line cannot be masked by the others.
fn populated_snapshot() -> MemorySnapshot {
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
                software_frame: ResourceAmount { bytes: 1_024, items: 1 },
            },
            RendererSummary {
                label: "0".to_string(),
                role: "warm",
                glyph_atlas: ResourceAmount { bytes: 128, items: 3 },
                image_atlas: ResourceAmount::default(),
                software_frame: ResourceAmount::default(),
            },
        ],
        allocator: Some(AllocatorReading {
            source: AllocatorSource::MainWindow,
            label: "WindowId(1)".to_string(),
            snapshot: Some(fake_allocator(9)),
        }),
        live_renderers: 2,
    }
}

/// A snapshot holding nothing at all.
fn empty_snapshot() -> MemorySnapshot {
    MemorySnapshot {
        process: ProcessMemory::unsupported(),
        session: PaneRetention::default(),
        panes_total: 0,
        panes_sampled: 0,
        panes_contended: 0,
        renderers: Vec::new(),
        allocator: None,
        live_renderers: 0,
    }
}

/// The snapshot is one INFO record on the `memory` target.
///
/// Both halves matter. INFO is what makes it survivable — the report exists
/// for a session that was killed, where nobody thought to raise the level
/// first. One record is what makes it diffable: split across several, a reader
/// would have to correlate timestamps to reconstruct a single instant.
#[test]
fn the_snapshot_is_a_single_info_record_on_the_memory_target() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));

    assert_eq!(events.len(), 1, "the aggregate must be exactly one record, not one per subject");
    assert_eq!(events[0].target, "memory");
    assert_eq!(
        events[0].level, "INFO",
        "the snapshot must be INFO; at DEBUG it is absent from every session that did not \
         predict its own crash"
    );
}

/// Every field is present, including the ones holding zero.
///
/// A field omitted when empty makes successive samples structurally different,
/// which defeats the diffing this line exists for. Worse, it leaves a reader
/// unable to tell "this class held nothing" from "this class stopped being
/// reported" — and those have opposite implications.
#[test]
fn every_field_is_emitted_even_when_it_holds_nothing() {
    let events = capture(|| emit_memory_snapshot(&empty_snapshot(), None));
    let event = &events[0];
    let present = event.field_names();

    for field in [
        "process_private_committed_bytes",
        "process_resident_bytes",
        "process_virtual_bytes",
        "process_private_committed_delta",
        "process_resident_delta",
        "process_virtual_delta",
        "session_total_bytes",
        "session_delta",
        "grid_visible_bytes",
        "grid_history_bytes",
        "grid_alternate_bytes",
        "parser_bytes",
        "hyperlink_bytes",
        "inline_media_bytes",
        "pty_output_bytes",
        "pty_input_bytes",
        "panes_total",
        "panes_sampled",
        "panes_contended",
        "renderer_total_bytes",
        "renderer_total_items",
        "renderer_delta",
        "live_renderers",
        "renderers",
        "allocator_state",
        "allocator_source",
        "allocator_label",
        "allocator_allocated_bytes",
        "allocator_reserved_bytes",
        "allocator_allocations",
        "allocator_blocks",
        "allocator_largest_block_bytes",
    ] {
        assert!(
            present.contains(&field),
            "{field:?} is missing from a zero-valued snapshot. Successive samples must have \
             the same shape or they cannot be diffed. Fields present: {present:?}"
        );
    }
}

/// Each pane class carries its own figure, not a share of the total.
///
/// Distinct powers of two, so a class wired to the wrong source produces a
/// value no other class can account for. A total-only check would pass while
/// every class pointed at the wrong subsystem.
#[test]
fn each_pane_class_reports_its_own_bytes() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    let event = &events[0];

    assert_eq!(event.number("grid_visible_bytes"), Some(1));
    assert_eq!(event.number("grid_history_bytes"), Some(2));
    assert_eq!(event.number("grid_alternate_bytes"), Some(4));
    assert_eq!(event.number("parser_bytes"), Some(8));
    assert_eq!(event.number("hyperlink_bytes"), Some(16));
    assert_eq!(event.number("inline_media_bytes"), Some(32));
    assert_eq!(event.number("pty_output_bytes"), Some(64));
    assert_eq!(event.number("pty_input_bytes"), Some(128));
    assert_eq!(
        event.number("session_total_bytes"),
        Some(255),
        "the session total must fold all eight seams; a missing term leaves a gap the other \
         seven cannot produce"
    );
    assert_eq!(event.number("panes_total"), Some(4));
    assert_eq!(event.number("panes_sampled"), Some(3));
}

/// A contended pane is counted, so a partial total says it is partial.
///
/// Measurement takes `try_lock` and skips what it cannot read. A busy pane
/// holds more memory than an idle one, so a silently-omitted pane understates
/// the session at exactly the moment it is largest.
#[test]
fn panes_skipped_for_lock_contention_are_reported() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    assert_eq!(
        events[0].number("panes_contended"),
        Some(1),
        "a skipped pane must be visible in the line; otherwise a partial total is \
         indistinguishable from a complete one"
    );
}

/// Both renderer roles appear, each with its own identity and figures.
///
/// A warm renderer holds a full-size glyph atlas exactly like a visible one,
/// so a report covering only visible windows understates the process — and the
/// remedy the visible lines imply, closing a window, cannot reach a warm
/// renderer at all.
#[test]
fn visible_and_warm_renderers_are_both_reported_with_their_roles() {
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
    assert!(rendered.contains("software=1024/1"), "software frame bytes/items: {rendered}");
    assert!(rendered.contains("glyph=128/3"), "warm glyph atlas bytes/items: {rendered}");
}

/// Renderer totals fold every renderer and every part.
#[test]
fn renderer_breakdown_order_is_stable_across_input_order() {
    let make = |label: &str, role| RendererSummary {
        label: label.to_string(),
        role,
        glyph_atlas: ResourceAmount::default(),
        image_atlas: ResourceAmount::default(),
        software_frame: ResourceAmount::default(),
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

#[test]
fn renderer_totals_fold_every_renderer() {
    let events = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    let event = &events[0];

    // 512 + 256 + 1024 (visible) + 128 (warm)
    assert_eq!(event.number("renderer_total_bytes"), Some(1_920));
    // 5 + 2 + 1 (visible) + 3 (warm)
    assert_eq!(event.number("renderer_total_items"), Some(11));
}

/// The live-renderer count is read from the renderer crate, not derived.
///
/// The two figures agreeing is the useful signal. A leaked renderer is alive
/// without being reachable from any window, so it raises the live count while
/// contributing no summary line — a discrepancy that is invisible if the count
/// is computed from the summaries.
#[test]
fn the_live_renderer_count_is_reported_independently_of_the_summaries() {
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
fn a_session_with_no_renderer_says_none() {
    let events = capture(|| emit_memory_snapshot(&empty_snapshot(), None));
    let event = &events[0];
    assert_eq!(event.text("renderers"), Some("none"));
    assert_eq!(event.text("allocator_state"), Some("none"));
    assert_eq!(event.text("allocator_source"), Some("none"));
    assert_eq!(event.text("allocator_label"), Some("none"));
    assert_eq!(event.text("allocator_allocated_bytes"), Some("none"));
}

#[test]
fn measured_allocator_is_emitted_once_without_renderer_multiplication() {
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

#[test]
fn unsupported_allocator_report_is_explicit_not_zero() {
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
fn the_first_sample_reports_deltas_as_unavailable() {
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
fn a_later_sample_reports_signed_movement() {
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
fn an_unsupported_process_figure_never_produces_a_numeric_delta() {
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
fn a_configured_info_session_records_the_snapshot() {
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
fn an_info_session_does_not_record_the_debug_detail() {
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
fn the_snapshot_carries_no_user_content() {
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
fn inline_media_contention_marks_the_snapshot_partial() {
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
fn sampling_repeats_on_the_shared_cadence() {
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
