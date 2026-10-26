//! Renderer allocator authority, accounting and graphics log contracts.

use super::super::memory_snapshot_tests::*;
use super::*;

/// The main renderer supplies the shared-device report exactly once.
///
/// A counted accessor and unique fixture values fail if production reads peer renderers or sums
/// their reports.
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

/// Visible fallback chooses the lowest renderer label regardless of input order.
///
/// Three permutations model nondeterministic `HashMap` iteration and must produce one identical
/// read and attribution.
#[test]
fn visible_allocator_selection_is_stable_across_input_order() {
    let highest = FakeRenderer { id: 1 };
    let lowest = FakeRenderer { id: 2 };
    let middle = FakeRenderer { id: 3 };
    let orders = [
        [("WindowId(9)", &highest), ("WindowId(2)", &lowest), ("WindowId(5)", &middle)],
        [("WindowId(5)", &middle), ("WindowId(9)", &highest), ("WindowId(2)", &lowest)],
        [("WindowId(2)", &lowest), ("WindowId(5)", &middle), ("WindowId(9)", &highest)],
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

/// The first warm renderer is used only when no visible renderer is available.
///
/// The counted accessor proves the fallback performs one query against warm slot zero.
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

/// A renderer-less snapshot performs no allocator query and returns no reading.
///
/// The accessor counter stays zero, distinguishing absence from an unsupported backend report.
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

/// An unsupported backend is still queried once and retained as an explicit absent report.
///
/// Returning `None` from the counted accessor separates backend capability from no-renderer state.
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
        "renderer_row_glyph_cache_bytes",
        "renderer_row_glyph_cache_items",
        "renderer_row_quad_cache_bytes",
        "renderer_row_quad_cache_items",
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

/// Contract: the graphics fields are appended. Every field a periodic line carried before them
/// keeps its name and position, so a reader or tool that parses the line by its existing fields
/// is unaffected, and the eight graphics fields follow in a fixed order.
#[test]
fn graphics_fields_are_appended_after_the_unchanged_prefix() {
    let prefix = [
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
        "renderer_row_glyph_cache_bytes",
        "renderer_row_glyph_cache_items",
        "renderer_row_quad_cache_bytes",
        "renderer_row_quad_cache_items",
        "renderer_delta",
        "live_renderers",
        "live_fg_probe_workers",
        "renderers",
        "allocator_state",
        "allocator_source",
        "allocator_label",
        "allocator_allocated_bytes",
        "allocator_reserved_bytes",
        "allocator_allocations",
        "allocator_blocks",
        "allocator_largest_block_bytes",
    ];
    let graphics_fields = [
        "graphics_os_source",
        "graphics_os_footprint_bytes",
        "graphics_os_nofootprint_bytes",
        "graphics_os_dedicated_bytes",
        "graphics_os_shared_bytes",
        "graphics_requested_live_bytes",
        "graphics_requested_live_resources",
        "graphics_renderers",
        "graphics_requested_created",
        "graphics_requested_released",
        "graphics_os_unavailable_reason",
    ];
    let names = ordered_field_names(|| emit_memory_snapshot(&populated_snapshot(), None));
    assert_eq!(names[..prefix.len()], prefix.map(str::to_string));
    assert_eq!(names[prefix.len()..], graphics_fields.map(str::to_string));
}

/// Contract: the graphics fields carry the OS figure under its source and each renderer's
/// requested bytes by class; a figure the OS did not give is `unsupported`, never `0`.
#[test]
fn graphics_fields_carry_the_os_figure_and_each_renderer_ledger() {
    use sonicterm_gpu::graphics_ledger::{GraphicsLedgerSnapshot, GraphicsTally};
    let mut ledger = GraphicsLedgerSnapshot::default();
    ledger.classes[0] = GraphicsTally {
        live_bytes: 300,
        live_resources: 3,
        created_bytes: 600,
        created_resources: 6,
        released_bytes: 300,
        released_resources: 3,
    };
    ledger.classes[2] = GraphicsTally { live_bytes: 40, live_resources: 1, ..Default::default() };
    let graphics = GraphicsReading {
        os: GraphicsMemory {
            footprint: MemoryMetric::Bytes(4096),
            ..GraphicsMemory::unavailable(GraphicsSource::TaskVmInfo)
        },
        renderers: vec![RendererGraphics { label: "WindowId(1)".into(), role: "visible", ledger }],
    };
    let events = capture(|| {
        emit_memory_snapshot_with_graphics(&populated_snapshot(), None, &graphics);
    });
    let event = &events[0];
    assert_eq!(event.text("graphics_os_source"), Some("task_vm_info"));
    assert_eq!(event.text("graphics_os_footprint_bytes"), Some("4096"));
    assert_eq!(event.text("graphics_os_nofootprint_bytes"), Some("unsupported"));
    assert_eq!(event.text("graphics_os_dedicated_bytes"), Some("unsupported"));
    assert_eq!(event.number("graphics_requested_live_bytes"), Some(340));
    assert_eq!(event.number("graphics_requested_live_resources"), Some(4));
    assert_eq!(
        event.text("graphics_renderers"),
        Some(
            "visible[WindowId(1)] surface=300/3 frame_texture=0/0 glyph_atlas=40/1 \
             image_atlas=0/0 present_buffers=0/0 requested_total=340/4 surface_count=exact"
        )
    );
    assert_eq!(
        event.text("graphics_requested_created"),
        Some("surface=600/6 frame_texture=0/0 glyph_atlas=0/0 image_atlas=0/0 present_buffers=0/0")
    );
    assert_eq!(
        event.text("graphics_requested_released"),
        Some("surface=300/3 frame_texture=0/0 glyph_atlas=0/0 image_atlas=0/0 present_buffers=0/0")
    );
    assert_eq!(event.text("graphics_os_unavailable_reason"), Some("none"));
    let unsampled = capture(|| emit_memory_snapshot(&populated_snapshot(), None));
    assert_eq!(unsampled[0].text("graphics_os_source"), Some("none"));
    assert_eq!(unsampled[0].text("graphics_renderers"), Some("none"));
}
