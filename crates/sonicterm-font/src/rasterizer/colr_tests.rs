use super::*;
use cairo::{Format, ImageSurface};

/// Build a small ARGB32 surface plus context for gradient painting tests.
///
/// The surface is returned alongside the context because the context borrows
/// it; a 32x32 extent keeps `clip_extents` non-degenerate so the sweep radius
/// is a real number rather than zero.
fn test_surface_and_context() -> (ImageSurface, Context) {
    let surface = ImageSurface::create(Format::ARgb32, 32, 32).expect("create test surface");
    let context = Context::new(&surface).expect("create test context");
    (surface, context)
}

fn opaque_red() -> SrgbaPixel {
    SrgbaPixel::rgba(255, 0, 0, 255)
}

fn opaque_blue() -> SrgbaPixel {
    SrgbaPixel::rgba(0, 0, 255, 255)
}

fn stop(offset: f64, color: SrgbaPixel) -> ColorStop {
    ColorStop { offset, color }
}

fn empty_color_line(extend: Extend) -> ColorLine {
    ColorLine { color_stops: Vec::new(), extend }
}

fn two_stop_color_line(extend: Extend) -> ColorLine {
    ColorLine { color_stops: vec![stop(0., opaque_red()), stop(1., opaque_blue())], extend }
}

/// Number of patches in `mesh` and the strongest alpha across their corners.
///
/// A mesh with patches but zero alpha everywhere still renders blank, so the
/// visibility assertions check both numbers rather than the count alone.
fn mesh_stats(mesh: &Mesh) -> (usize, f64) {
    let count = mesh.patch_count().expect("mesh patch count");
    let mut max_alpha = 0.0f64;
    for patch in 0..count {
        for corner in [
            MeshCorner::MeshCorner0,
            MeshCorner::MeshCorner1,
            MeshCorner::MeshCorner2,
            MeshCorner::MeshCorner3,
        ] {
            if let Ok((_, _, _, alpha)) = mesh.corner_color_rgba(patch, corner) {
                max_alpha = max_alpha.max(alpha);
            }
        }
    }
    (count, max_alpha)
}

/// Collect every corner colour so two meshes can be compared for equality.
///
/// Reflect must mirror stop order on odd tiles; comparing the full colour
/// sequence against Repeat is what proves the mirroring actually happened.
fn mesh_corner_colors(mesh: &Mesh) -> Vec<(f64, f64, f64, f64)> {
    let count = mesh.patch_count().expect("mesh patch count");
    let mut colors = Vec::new();
    for patch in 0..count {
        for corner in [MeshCorner::MeshCorner0, MeshCorner::MeshCorner2] {
            if let Ok(color) = mesh.corner_color_rgba(patch, corner) {
                colors.push(color);
            }
        }
    }
    colors
}

fn sweep_mesh(color_line: ColorLine, start_angle: f64, end_angle: f64) -> Mesh {
    let mesh = Mesh::new();
    let center = Point { x: 16., y: 16. };
    apply_sweep_gradient_patches(&mesh, color_line, center, 32., start_angle, end_angle);
    mesh
}

fn painted_byte_count(extend: Extend) -> usize {
    let (surface, context) = test_surface_and_context();
    paint_sweep_gradient(
        &context,
        16.,
        16.,
        0.,
        std::f64::consts::FRAC_PI_2,
        two_stop_color_line(extend),
    )
    .expect("paint sweep gradient");

    let mut painted = 0;
    surface
        .with_data(|bytes| painted = bytes.iter().filter(|byte| **byte != 0).count())
        .expect("read painted surface");
    painted
}

/// A COLRv1 `ColorLine` may legally carry zero stops; painting one must be a
/// no-op rather than an out-of-bounds panic on `color_stops[0]`.
#[test]
fn empty_color_stops_linear_gradient_does_not_panic() {
    let (_surface, context) = test_surface_and_context();

    let result =
        paint_linear_gradient(&context, 0., 0., 32., 32., 0., 32., empty_color_line(Extend::Pad));

    assert!(result.is_ok(), "empty linear color line should paint nothing: {result:?}");
}

/// The radial entry point indexes the same stop vector as the linear one, so a
/// zero-stop color line must return cleanly instead of unwinding.
#[test]
fn empty_color_stops_radial_gradient_does_not_panic() {
    let (_surface, context) = test_surface_and_context();

    let result =
        paint_radial_gradient(&context, 0., 0., 0., 32., 32., 16., empty_color_line(Extend::Pad));

    assert!(result.is_ok(), "empty radial color line should paint nothing: {result:?}");
}

/// The sweep path indexes first and last stops while building patches, so an
/// empty color line must be rejected before any indexing occurs.
#[test]
fn empty_color_stops_sweep_gradient_does_not_panic() {
    let (_surface, context) = test_surface_and_context();

    let result =
        paint_sweep_gradient(&context, 16., 16., 0., PI_TIMES_2, empty_color_line(Extend::Pad));

    assert!(result.is_ok(), "empty sweep color line should paint nothing: {result:?}");
}

/// An empty color line reaching the patch builder directly must emit no
/// patches rather than panicking on `color_stops[0]` in the degenerate branch.
#[test]
fn empty_color_stops_sweep_patches_emit_nothing() {
    let mesh = sweep_mesh(empty_color_line(Extend::Pad), 0., 0.);

    let (count, _) = mesh_stats(&mesh);
    assert_eq!(count, 0, "empty color line should produce no patches");
}

/// A quarter-turn Repeat sweep tiles its stops around the full turn; the tiling
/// loop must actually iterate, not collapse to an empty range and render blank.
#[test]
fn repeat_sweep_gradient_emits_visible_patches() {
    let mesh = sweep_mesh(two_stop_color_line(Extend::Repeat), 0., std::f64::consts::FRAC_PI_2);

    let (count, max_alpha) = mesh_stats(&mesh);
    assert!(count > 0, "Repeat sweep must emit patches, got {count}");
    assert!(max_alpha > 0., "Repeat sweep patches must carry visible alpha, got {max_alpha}");
}

/// Reflect tiles alternate stop direction. It must emit visible patches and
/// must differ from Repeat, which proves odd tiles are genuinely mirrored.
#[test]
fn reflect_sweep_gradient_emits_visible_mirrored_patches() {
    let reflect = sweep_mesh(two_stop_color_line(Extend::Reflect), 0., std::f64::consts::FRAC_PI_2);
    let repeat = sweep_mesh(two_stop_color_line(Extend::Repeat), 0., std::f64::consts::FRAC_PI_2);

    let (count, max_alpha) = mesh_stats(&reflect);
    assert!(count > 0, "Reflect sweep must emit patches, got {count}");
    assert!(max_alpha > 0., "Reflect sweep patches must carry visible alpha, got {max_alpha}");

    assert_ne!(
        mesh_corner_colors(&reflect),
        mesh_corner_colors(&repeat),
        "Reflect must mirror stop order on odd tiles, so it cannot match Repeat"
    );
}

/// Repeat and Reflect must survive Cairo mesh painting, not merely build patches.
#[test]
fn repeat_and_reflect_sweeps_paint_visible_surface_pixels() {
    for extend in [Extend::Repeat, Extend::Reflect] {
        let painted = painted_byte_count(extend);
        assert!(painted > 0, "{extend:?} sweep must paint visible surface pixels");
    }
}

/// A color line beginning far before zero needs a negative first tile index.
/// The bounded loop must start there and reach the visible turn instead of
/// treating the cap as an absolute upper endpoint.
#[test]
fn negative_first_tile_repeat_reaches_visible_turn() {
    let color_line = ColorLine {
        color_stops: vec![stop(-4., opaque_red()), stop(-3., opaque_blue())],
        extend: Extend::Repeat,
    };

    let mesh = sweep_mesh(color_line, 0., std::f64::consts::FRAC_PI_2);
    let (count, max_alpha) = mesh_stats(&mesh);
    assert!(count > 0, "negative first tile must still reach the visible turn");
    assert!(count <= MAX_SWEEP_PATCHES, "negative first tile must stay bounded, got {count}");
    assert!(max_alpha > 0., "negative first tile must carry visible alpha");
}

/// Pad is the branch that already worked; it must keep emitting visible patches
/// so the tiling fix is not mistaken for a change in Pad behavior.
#[test]
fn pad_sweep_gradient_behavior_is_unchanged() {
    let mesh = sweep_mesh(two_stop_color_line(Extend::Pad), 0., PI_TIMES_2);

    let (count, max_alpha) = mesh_stats(&mesh);
    assert!(count > 0, "Pad sweep must emit patches, got {count}");
    assert!(max_alpha > 0., "Pad sweep patches must carry visible alpha, got {max_alpha}");
}

/// A very narrow Repeat sweep would need thousands of tiles to cover the turn.
/// The tiling must stop at the documented cap instead of running unbounded.
#[test]
fn repeat_sweep_gradient_patch_count_stays_bounded() {
    let mesh = sweep_mesh(two_stop_color_line(Extend::Repeat), 0., 0.001);

    let (count, _) = mesh_stats(&mesh);
    assert!(count > 0, "narrow Repeat sweep should still emit ink, got {count}");
    assert!(count <= MAX_SWEEP_PATCHES, "tiling must stay bounded, got {count}");
}

/// A valid narrow color line can begin thousands of tiles before the visible
/// turn. The first tile must be derived directly rather than capped as a search.
#[test]
fn distant_narrow_repeat_and_reflect_sweeps_reach_visible_turn() {
    for extend in [Extend::Repeat, Extend::Reflect] {
        let color_line = ColorLine {
            color_stops: vec![stop(0., opaque_red()), stop(0.0005, opaque_blue())],
            extend,
        };
        let mesh = sweep_mesh(color_line, std::f64::consts::PI, PI_TIMES_2);

        let (count, max_alpha) = mesh_stats(&mesh);
        assert!(count > 0, "distant narrow {extend:?} sweep must reach the visible turn");
        assert!(count <= MAX_SWEEP_PATCHES, "distant narrow {extend:?} sweep must stay bounded");
        assert!(max_alpha > 0., "distant narrow {extend:?} sweep must carry visible alpha");
    }
}

/// A one-stop Repeat or Reflect line has no tiling span. Both valid forms must
/// terminate without patches instead of searching forever for a visible tile.
#[test]
fn one_stop_repeat_and_reflect_sweeps_terminate() {
    for extend in [Extend::Repeat, Extend::Reflect] {
        let color_line = ColorLine { color_stops: vec![stop(0., opaque_red())], extend };
        let mesh = sweep_mesh(color_line, 0.5, 1.);

        assert_eq!(mesh_stats(&mesh).0, 0, "one-stop {extend:?} sweep must paint nothing");
    }
}

/// Stops collapsing onto a single angle give the tiling search a zero-width
/// span; it must terminate and emit nothing rather than looping forever.
#[test]
fn degenerate_zero_span_repeat_sweep_terminates() {
    let color_line = ColorLine {
        color_stops: vec![stop(0.5, opaque_red()), stop(0.5, opaque_blue())],
        extend: Extend::Repeat,
    };

    let mesh = sweep_mesh(color_line, 0., PI_TIMES_2);

    let (count, _) = mesh_stats(&mesh);
    assert_eq!(count, 0, "zero-width tiling span should emit nothing");
}

/// A hostile end angle makes the per-span split count astronomically large.
/// The split cap must bound the work instead of effectively hanging.
#[test]
fn extreme_angles_terminate_under_patch_cap() {
    let mesh = sweep_mesh(two_stop_color_line(Extend::Pad), -5., 1e9);

    let (count, _) = mesh_stats(&mesh);
    assert!(count <= MAX_SWEEP_PATCHES, "extreme angles must stay under the cap, got {count}");
}

/// A malformed font can supply a NaN stop offset. Sorting must not panic on the
/// incomparable value; the paint call fails soft instead.
#[test]
fn nan_color_stop_offsets_do_not_panic() {
    let (_surface, context) = test_surface_and_context();
    let color_line = ColorLine {
        color_stops: vec![stop(f64::NAN, opaque_red()), stop(1., opaque_blue())],
        extend: Extend::Pad,
    };

    let result = paint_linear_gradient(&context, 0., 0., 32., 32., 0., 32., color_line);

    assert!(result.is_ok(), "NaN stop offsets should fail soft, got {result:?}");
}

/// Sorting must establish both original endpoints before normalization; seeding
/// the minimum from the pre-sort first stop loses a smaller stop moved to index 0.
#[test]
fn normalize_unsorted_color_line_preserves_the_true_span() {
    let mut color_line = ColorLine {
        color_stops: vec![
            stop(0.5, opaque_red()),
            stop(0.2, opaque_blue()),
            stop(0.9, opaque_red()),
        ],
        extend: Extend::Pad,
    };

    let (min_stop, max_stop) = normalize_color_line(&mut color_line);

    assert_eq!(min_stop, 0.2);
    assert_eq!(max_stop, 0.9);
    assert_eq!(color_line.color_stops[0].offset, 0.);
    assert!((color_line.color_stops[1].offset - 3. / 7.).abs() < f64::EPSILON);
    assert_eq!(color_line.color_stops[2].offset, 1.);
}

/// Non-finite sweep endpoints cannot produce representable Cairo coordinates;
/// they must consume no patch budget rather than emitting NaN geometry.
#[test]
fn non_finite_sweep_endpoints_emit_no_patches() {
    for (start, end) in [(f64::NEG_INFINITY, 1.), (0., f64::INFINITY), (0., f64::NAN)] {
        let mesh = sweep_mesh(two_stop_color_line(Extend::Pad), start, end);
        assert_eq!(mesh_stats(&mesh).0, 0, "non-finite span {start}..{end} must paint nothing");
    }
}

/// Finite input geometry can overflow while deriving one stop angle. The
/// low-level patch helper must reject that non-finite endpoint even when the
/// outer sweep inputs themselves were finite.
#[test]
fn patch_helper_rejects_overflowed_non_finite_endpoint() {
    let mesh = Mesh::new();
    let mut budget = MAX_SWEEP_PATCHES;
    add_sweep_gradient_patches(
        &mesh,
        Point { x: 16., y: 16. },
        32.,
        0.,
        opaque_red().into(),
        f64::INFINITY,
        opaque_blue().into(),
        &mut budget,
    );

    assert_eq!(mesh_stats(&mesh).0, 0);
    assert_eq!(budget, MAX_SWEEP_PATCHES, "rejected geometry must consume no budget");
}

/// `normalize_color_line` is reached only with a non-empty list in production,
/// but it must stay total so a future caller cannot reintroduce the panic.
#[test]
fn normalize_empty_color_line_is_total() {
    let mut color_line = empty_color_line(Extend::Pad);

    let (min_stop, max_stop) = normalize_color_line(&mut color_line);

    assert_eq!(min_stop, 0., "empty color line should report an identity span");
    assert_eq!(max_stop, 1., "empty color line should report an identity span");
}
