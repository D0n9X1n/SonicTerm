//! Presentation buffer allocation, growth, release and ledger contracts.

use super::super::wezterm_pipeline_tests::*;
use super::*;

/// An invalid uniform-buffer descriptor surfaces as a contained validation
/// error instead of a panic. `create_buffer_init` maps the new buffer and
/// panics through `expect` when that buffer is invalid.
#[test]
fn invalid_uniform_buffer_is_a_contained_error() {
    let device = uniform_test_device();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _buffer = create_uniform_buffer(&device, wgpu::BufferUsages::empty());
    let error = pollster::block_on(scope.pop());
    assert!(matches!(error, Some(wgpu::Error::Validation { .. })), "{error:?}");
}

/// The index pattern is uploaded on the first frame, once and for the whole capacity, and
/// draws the right quads; a second frame at the same capacity writes no index bytes.
#[test]
fn first_frame_uploads_the_index_pattern_once_and_the_next_uploads_nothing() {
    let mut harness = FrameHarness::new(8);

    let first = harness.draw(None, &[red_column(1), red_column(4)], &[]);
    assert_eq!(first.red_columns, vec![1, 4]);
    assert_eq!(first.stats.index_bytes, pattern_bytes(harness.pipeline.index_capacity));
    assert_eq!(
        harness.index_buffer(),
        build_indices((harness.pipeline.index_capacity / INDICES_PER_QUAD as u64) as usize),
        "the buffer holds the pattern for its whole quad capacity"
    );

    let second = harness.draw(None, &[red_column(2), red_column(5), red_column(6)], &[]);
    assert_eq!(second.red_columns, vec![2, 5, 6]);
    assert_eq!(second.stats.index_bytes, 0, "an unchanged capacity writes no index bytes");
}

/// Growth re-uploads the pattern for the new capacity, and a smaller frame afterwards draws
/// from the larger pattern's prefix without another upload.
#[test]
fn growth_uploads_the_larger_pattern_and_smaller_frames_draw_from_its_prefix() {
    let mut harness = FrameHarness::new(1);
    let small = harness.draw(None, &[red_column(0)], &[]);
    assert_eq!(small.red_columns, vec![0]);

    let wide: Vec<QuadInstance> = (0..5).map(red_column).collect();
    let grown = harness.draw(None, &wide, &[]);
    let new_index_capacity = harness.pipeline.index_capacity;
    assert!(new_index_capacity >= 5 * INDICES_PER_QUAD as u64, "the index buffer grew");
    assert_eq!(grown.red_columns, vec![0, 1, 2, 3, 4]);
    assert_eq!(grown.stats.index_bytes, pattern_bytes(new_index_capacity));
    assert_eq!(
        harness.index_buffer(),
        build_indices((new_index_capacity / INDICES_PER_QUAD as u64) as usize)
    );

    let after = harness.draw(None, &[red_column(7), red_column(3)], &[]);
    assert_eq!(after.red_columns, vec![3, 7]);
    assert_eq!(after.stats.index_bytes, 0);
}

/// At an unchanged capacity no frame writes index bytes, with or without a reset and with
/// overlay quads, and every frame still draws exactly its own quads.
#[test]
fn frames_at_unchanged_capacity_write_no_index_bytes() {
    let mut harness = FrameHarness::new(8);
    harness.draw(None, &[red_column(0)], &[]);
    let frames: [(Option<QuadInstance>, Vec<QuadInstance>, Vec<QuadInstance>, Vec<u32>); 3] = [
        (Some(black_reset()), vec![red_column(2)], vec![red_column(6)], vec![2, 6]),
        (None, vec![red_column(1), red_column(3), red_column(5)], vec![], vec![1, 3, 5]),
        (Some(black_reset()), vec![], vec![red_column(7)], vec![7]),
    ];
    for (reset, quads, overlay, expected) in frames {
        let frame = harness.draw(reset, &quads, &overlay);
        assert_eq!(frame.red_columns, expected);
        assert_eq!(frame.stats.index_bytes, 0);
        assert!(frame.stats.vertex_bytes > 0, "vertices are still uploaded every frame");
    }
}

/// Draw ranges come from the vertices actually emitted: a skipped degenerate primitive
/// shortens the range, with or without a reset, and with overlays.
#[test]
fn draw_ranges_follow_emitted_vertices() {
    let surface = (HARNESS_WIDTH as f32, HARNESS_HEIGHT as f32);
    let quads = [red_column(0), degenerate_quad(), red_column(2)];
    let overlay = [degenerate_quad(), red_column(5)];
    let layers = PipelineLayers {
        quads: &quads,
        images: &[],
        glyphs: &[],
        overlay_quads: &overlay,
        overlay_glyphs: &[],
    };
    let cases =
        [(None, 0_u32, 18_u32), (Some(black_reset()), 6, 24), (Some(degenerate_quad()), 0, 18)];
    for (reset, reset_end, main_end) in cases {
        let mut vertices = Vec::new();
        let reset_vertices = assemble_vertices(
            &mut vertices,
            reset.as_ref(),
            &layers,
            surface.0,
            surface.1,
            SubpixelAaMode::Off,
        );
        let (reset_range, main_range) = draw_index_ranges(reset_vertices, vertices.len());
        assert_eq!(reset_range, 0..reset_end);
        assert_eq!(main_range, reset_end..main_end);
    }
}

/// A frame whose degenerate primitives are skipped draws only its own vertices: a range
/// sized from the input slices would reach the previous frame's stale vertices.
#[test]
fn skipped_primitives_never_draw_stale_vertices() {
    let mut harness = FrameHarness::new(8);
    let earlier: Vec<QuadInstance> = (0..4).map(red_column).collect();
    assert_eq!(harness.draw(None, &earlier, &[]).red_columns, vec![0, 1, 2, 3]);

    let frame = harness.draw(None, &[degenerate_quad(), red_column(5), degenerate_quad()], &[]);

    assert_eq!(frame.red_columns, vec![5]);
}

/// An invalid index-buffer descriptor, at creation or at growth, is a contained wgpu
/// validation error rather than a panic.
#[test]
fn invalid_index_buffer_is_a_contained_error() {
    let (device, _queue) = headless_device_and_queue();
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _buffer = create_index_buffer(&device, 6, wgpu::BufferUsages::empty());
    let error = pollster::block_on(scope.pop());
    assert!(matches!(error, Some(wgpu::Error::Validation { .. })), "{error:?}");

    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8Unorm, 1);
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let oversized = device.limits().max_buffer_size;
    pipeline.ensure_capacity(&device, 4, oversized);
    let error = pollster::block_on(scope.pop());
    assert!(matches!(error, Some(wgpu::Error::Validation { .. })), "{error:?}");
}

/// The vertex scratch is reused across frames without carrying the previous frame's
/// vertices, and grows when a frame needs more room.
#[test]
fn vertex_scratch_is_reused_without_stale_vertices_and_grows() {
    let mut harness = FrameHarness::new(8);
    harness.draw(None, &[red_column(0), red_column(1), red_column(2)], &[]);
    let reused = harness.pipeline.vertex_scratch.as_ptr();
    let capacity = harness.pipeline.vertex_scratch.capacity();
    assert!(capacity >= 3 * VERTICES_PER_QUAD, "the scratch holds the first frame");

    let frame = harness.draw(None, &[red_column(6)], &[]);
    assert_eq!(frame.red_columns, vec![6]);
    assert_eq!(harness.pipeline.vertex_scratch.len(), VERTICES_PER_QUAD, "no stale vertices");
    assert_eq!(harness.pipeline.vertex_scratch.as_ptr(), reused, "the allocation is reused");

    let wide: Vec<QuadInstance> = (0..8).map(red_column).collect();
    harness.draw(None, &wide, &[]);
    assert!(harness.pipeline.vertex_scratch.capacity() >= 8 * VERTICES_PER_QUAD);
}

/// The scratch is shrunk to twice a frame's use once its capacity exceeds four times that
/// use and 1 MiB, and kept otherwise; its capacity is the retained figure.
#[test]
fn vertex_scratch_release_policy_and_retained_figure() {
    let vertex_size = std::mem::size_of::<Vertex>();
    let large = (2 * 1024 * 1024) / vertex_size;
    let mut scratch: Vec<Vertex> = Vec::with_capacity(large);
    scratch.resize(100, Vertex::default());
    release_scratch_excess(&mut scratch, 100);
    assert!(scratch.capacity() >= 200 && scratch.capacity() < large, "oversized scratch shrinks");
    assert_eq!(scratch.len(), 100, "the frame's vertices are untouched");

    let small = (512 * 1024) / vertex_size;
    let mut under_floor: Vec<Vertex> = Vec::with_capacity(small);
    release_scratch_excess(&mut under_floor, 10);
    assert_eq!(under_floor.capacity(), small, "scratch under 1 MiB is kept");

    let mut busy: Vec<Vertex> = Vec::with_capacity(large);
    release_scratch_excess(&mut busy, large / 3);
    assert_eq!(busy.capacity(), large, "scratch within four times the frame's use is kept");

    let mut harness = FrameHarness::new(8);
    assert_eq!(harness.pipeline.vertex_scratch_retained(), ResourceAmount::default());
    harness.draw(None, &[red_column(0), red_column(1)], &[]);
    assert_eq!(
        harness.pipeline.vertex_scratch_retained(),
        ResourceAmount {
            bytes: harness.pipeline.vertex_scratch.capacity() * vertex_size,
            items: 1
        }
    );
}

/// A frame that emits no vertices still applies the scratch release policy: after a frame
/// large enough to leave over 1 MiB of scratch, an empty frame and an all-degenerate frame
/// each release the allocation (twice zero use is zero), and the retained figure that
/// `retained_amounts` reads drops to nothing. Both exits of `draw_frame` are covered.
#[test]
fn zero_vertex_frames_release_a_large_scratch() {
    const LARGE_QUADS: u32 = 5_000;
    let large: Vec<QuadInstance> = (0..LARGE_QUADS).map(|index| red_column(index % 8)).collect();
    let empty: Vec<QuadInstance> = Vec::new();
    let degenerate = vec![degenerate_quad(); 3];
    for (case, zero_use) in [("empty", &empty), ("all-degenerate", &degenerate)] {
        let mut harness = FrameHarness::new(8);
        harness.draw(None, &large, &[]);
        let peak = harness.pipeline.vertex_scratch_retained();
        assert!(peak.bytes > 1024 * 1024, "{case}: the large frame retains {} bytes", peak.bytes);

        let frame = harness.draw(None, zero_use, &[]);

        assert!(frame.red_columns.is_empty(), "{case}: nothing is drawn");
        assert_eq!(harness.pipeline.vertex_scratch.capacity(), 0, "{case}: allocation released");
        assert_eq!(
            harness.pipeline.vertex_scratch_retained(),
            ResourceAmount::default(),
            "{case}: the retained report shows the released scratch"
        );
    }
}

/// The 600-draw hysteresis: a steady 20,000-quad load never shrinks; a 30,000-quad window then a
/// 2,000-quad window shrinks exactly once, at call 1,200, to `next_pow2(2 × 2,000)` floored at the
/// initial 4,096; alternating windows of 30,000 and 20,000 never shrink.
#[test]
fn present_buffers_shrink_once_after_a_quiet_window() {
    let mut window = ShrinkWindow::default();
    let mut capacity = 4096;
    assert!(feed(&mut window, &mut capacity, 600, 20_000, 0).is_empty(), "steady load");

    let mut window = ShrinkWindow::default();
    let mut capacity = 4096;
    assert!(feed(&mut window, &mut capacity, 600, 30_000, 0).is_empty());
    assert_eq!(feed(&mut window, &mut capacity, 600, 2_000, 600), [(1200, 4096)]);
    assert!(feed(&mut window, &mut capacity, 600, 2_000, 1200).is_empty(), "already small");

    let mut window = ShrinkWindow::default();
    let mut capacity = 4096;
    for (index, quads) in [30_000, 20_000, 30_000, 20_000].into_iter().enumerate() {
        let offset = 600 * index as u32;
        assert!(feed(&mut window, &mut capacity, 600, quads, offset).is_empty(), "{quads}");
    }
}

/// A shrink and a reset both clear the index pattern, so the next draw writes the pattern for the
/// new capacity and the index buffer reads back as `build_indices`.
#[test]
fn a_shrink_or_a_reset_rewrites_the_index_pattern() {
    let mut harness = FrameHarness::new(1);
    let wide: Vec<QuadInstance> = (0..64).map(|index| red_column(index % HARNESS_WIDTH)).collect();
    let _grown = harness.draw(None, &wide, &[]);
    assert!(harness.pipeline.index_capacity >= 64 * INDICES_PER_QUAD as u64, "precondition");
    // The wide draw opened the first window, so that window's peak is 64 and it closes without a
    // shrink; the next window of single-quad draws closes oversized and shrinks to twice its peak.
    for _ in 0..2 * SHRINK_WINDOW_CALLS - 2 {
        let _frame = harness.draw(None, &[red_column(0)], &[]);
    }
    assert!(harness.pipeline.index_capacity >= 64 * INDICES_PER_QUAD as u64, "no shrink yet");
    let closing = harness.draw(None, &[red_column(1)], &[]);
    assert_eq!(closing.red_columns, vec![1], "the closing frame still draws its quad");
    assert_eq!(harness.pipeline.index_capacity, 2 * INDICES_PER_QUAD as u64, "shrunk to 2 quads");
    // The shrink cleared the pattern before this draw's upload, so the closing draw rewrote it.
    assert_eq!(closing.stats.index_bytes, pattern_bytes(harness.pipeline.index_capacity));
    assert_eq!(harness.index_buffer(), build_indices(2));
    let next = harness.draw(None, &[red_column(2), red_column(3)], &[]);
    assert_eq!(next.red_columns, vec![2, 3]);
    assert_eq!(next.stats.index_bytes, 0, "the rewritten pattern covers the new capacity");

    let _grown = harness.draw(None, &wide, &[]);
    let released = harness.pipeline.reset_to_initial(&harness.device);
    assert!(released > 0, "the reset releases the grown buffers");
    assert_eq!(harness.pipeline.index_pattern_quads, 0, "the reset cleared the pattern");
    assert_eq!(harness.pipeline.vertex_scratch.capacity(), 0, "the scratch is emptied");
    let after = harness.draw(None, &[red_column(5)], &[]);
    assert_eq!(after.red_columns, vec![5]);
    assert_eq!(harness.index_buffer(), build_indices(1));
}

/// Empty draws count toward the 600-call shrink window: after one wide draw, a window made only of
/// empty draws closes oversized and shrinks the buffers, and the next non-empty draw rewrites the
/// index pattern for the new capacity.
#[test]
fn a_window_of_empty_draws_still_shrinks_the_buffers() {
    let mut harness = FrameHarness::new(1);
    let wide: Vec<QuadInstance> = (0..64).map(|index| red_column(index % HARNESS_WIDTH)).collect();
    let _grown = harness.draw(None, &wide, &[]);
    assert!(harness.pipeline.index_capacity >= 64 * INDICES_PER_QUAD as u64, "precondition");
    // The wide draw opened the first window (peak 64), which closes unshrunk; the second window
    // holds only empty draws, so its peak is zero and it closes oversized.
    for _ in 0..2 * SHRINK_WINDOW_CALLS - 1 {
        let empty = harness.draw(None, &[], &[]);
        assert!(empty.red_columns.is_empty(), "an empty draw draws nothing");
    }
    assert_eq!(harness.pipeline.index_capacity, INDICES_PER_QUAD as u64, "shrunk to 1 quad");
    let next = harness.draw(None, &[red_column(4)], &[]);
    assert_eq!(next.red_columns, vec![4]);
    assert_eq!(harness.index_buffer(), build_indices(1));
}

/// Contract: the present pipeline records its own buffer requests at the real creation sites:
/// attaching reports all three buffers, a growth replaces the grown buffer, the covered-window
/// trim's reset returns both to their initial size, and the ledger then holds exactly the
/// pipeline's current request; releasing everything, as the renderer's drop does, reads zero.
#[test]
fn present_buffer_growth_and_reset_move_the_graphics_ledger() {
    use crate::graphics_ledger::{GraphicsClass, GraphicsLedgerHandle};
    let device = uniform_test_device();
    let mut pipeline = WeztermPipeline::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb, 4);
    let ledger = GraphicsLedgerHandle::default();
    pipeline.attach_graphics_ledger(ledger.clone());
    let uniform_bytes = std::mem::size_of::<ShaderUniform>() as u64;
    let present =
        |ledger: &GraphicsLedgerHandle| ledger.snapshot().class(GraphicsClass::PresentBuffers);
    let initial = present(&ledger);
    assert_eq!(initial.live_bytes, pipeline.present_buffer_bytes() + uniform_bytes);
    assert_eq!(initial.live_resources, 3);

    pipeline.ensure_capacity(&device, 4 * 4 * 64, 6 * 64);
    let grown = present(&ledger);
    assert_eq!(grown.live_bytes, pipeline.present_buffer_bytes() + uniform_bytes);
    assert!(grown.live_bytes > initial.live_bytes);
    assert_eq!(grown.created_resources, 5, "three at attach, then one vertex and one index growth");

    let released = pipeline.reset_to_initial(&device);
    let reset = present(&ledger);
    assert_eq!(reset.live_bytes, initial.live_bytes);
    assert_eq!(
        grown.live_bytes - reset.live_bytes,
        released,
        "the trim's figure matches the ledger"
    );

    ledger.release_all();
    let dropped = present(&ledger);
    assert_eq!((dropped.live_bytes, dropped.live_resources), (0, 0));
    assert_eq!(dropped.created_bytes, dropped.released_bytes);
}
