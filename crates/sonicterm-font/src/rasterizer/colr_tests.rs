use super::*;
use cairo::{Format, ImageSurface};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, FRAC_PI_8, PI};

/// Build a small ARGB32 surface plus context for gradient painting tests.
///
/// The surface is returned alongside the context because the context borrows
/// it; a 32x32 extent keeps `clip_extents` non-degenerate.
fn test_surface_and_context() -> (ImageSurface, Context) {
    let surface = ImageSurface::create(Format::ARgb32, 32, 32).expect("create test surface");
    let context = Context::new(&surface).expect("create test context");
    (surface, context)
}

fn opaque_red() -> SrgbaPixel {
    SrgbaPixel::rgba(255, 0, 0, 255)
}

fn opaque_green() -> SrgbaPixel {
    SrgbaPixel::rgba(0, 255, 0, 255)
}

fn opaque_blue() -> SrgbaPixel {
    SrgbaPixel::rgba(0, 0, 255, 255)
}

fn opaque_yellow() -> SrgbaPixel {
    SrgbaPixel::rgba(255, 255, 0, 255)
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

/// Four hard bands over the stop span, a quarter each: red, green, blue, yellow, as the COLR test font's sweep.
fn four_band_color_line() -> ColorLine {
    let mut color_stops = Vec::new();
    for (band, color) in
        [opaque_red(), opaque_green(), opaque_blue(), opaque_yellow()].into_iter().enumerate()
    {
        color_stops.push(stop(band as f64 * 0.25, color));
        color_stops.push(stop((band + 1) as f64 * 0.25, color));
    }
    ColorLine { color_stops, extend: Extend::Pad }
}

/// The (red, green, blue, alpha) bytes, premultiplied as Cairo stores them, of `surface`'s pixel at `column`, `row`.
fn pixel_rgba(surface: &ImageSurface, column: usize, row: usize) -> [u8; 4] {
    let stride = surface.stride() as usize;
    let mut rgba = [0; 4];
    surface
        .with_data(|bytes| {
            let at = row * stride + column * 4;
            let argb = u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
            rgba = [(argb >> 16) as u8, (argb >> 8) as u8, argb as u8, (argb >> 24) as u8];
        })
        .expect("read painted surface");
    rgba
}

/// The number of `surface`'s pixels whose alpha is fully opaque.
fn opaque_pixel_count(surface: &ImageSurface) -> usize {
    let (width, height, stride) =
        (surface.width() as usize, surface.height() as usize, surface.stride() as usize);
    let mut opaque = 0;
    surface
        .with_data(|bytes| {
            for row in 0..height {
                for column in 0..width {
                    let at = row * stride + column * 4;
                    opaque += usize::from(
                        u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap()) >> 24 == 255,
                    );
                }
            }
        })
        .expect("read painted surface");
    opaque
}

/// Asserts `color`'s straight channels are within 1e-4 of `expected`, naming `context` on failure.
fn assert_color(color: SrgbaTuple, expected: [f32; 4], context: &str) {
    let SrgbaTuple(red, green, blue, alpha) = color;
    let actual = [red, green, blue, alpha];
    for (channel, want) in actual.iter().zip(expected) {
        assert!((channel - want).abs() < 1e-4, "{context}: {actual:?}, expected {expected:?}");
    }
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

/// A sweep whose color line has no stop has no colour for any angle: the lookup is never built, and painting it
/// returns cleanly and leaves the surface transparent.
#[test]
fn empty_color_stops_sweep_gradient_paints_nothing() {
    let (surface, context) = test_surface_and_context();

    assert!(SweepColors::new(empty_color_line(Extend::Pad), 0., PI_TIMES_2).is_none());
    let result =
        paint_sweep_gradient(&context, 16., 16., 0., PI_TIMES_2, empty_color_line(Extend::Pad));

    assert!(result.is_ok(), "empty sweep color line should paint nothing: {result:?}");
    assert_eq!(opaque_pixel_count(&surface), 0);
}

/// The four hard bands of a full-turn sweep land in the quadrants Cairo's angles name: with y down, red below
/// and right of the centre, green below and left, blue above and left, yellow above and right. Ubuntu 26.04's
/// Cairo 1.18.4 drew the first band yellow when the sweep was a Bezier-patch mesh.
#[test]
fn sweep_bands_land_in_their_quadrants_on_a_plain_surface() {
    let surface = ImageSurface::create(Format::ARgb32, 64, 64).expect("create test surface");
    let context = Context::new(&surface).expect("create test context");

    paint_sweep_gradient(&context, 32., 32., 0., PI_TIMES_2, four_band_color_line())
        .expect("paint sweep");

    for ((column, row), expected) in [
        ((48, 48), [255, 0, 0, 255]),
        ((16, 48), [0, 255, 0, 255]),
        ((16, 16), [0, 0, 255, 255]),
        ((48, 16), [255, 255, 0, 255]),
    ] {
        assert_eq!(pixel_rgba(&surface, column, row), expected, "pixel ({column}, {row})");
    }
    assert_eq!(opaque_pixel_count(&surface), 64 * 64, "a full Pad turn covers every pixel");
}

/// The sweep turns in user space. Under the y-flipping transform glyph rendering uses, a device pixel above and
/// right of the centre has a positive user y, so it takes the first band, red; one below and right takes yellow.
#[test]
fn sweep_follows_a_y_flipping_transform() {
    let surface = ImageSurface::create(Format::ARgb32, 64, 64).expect("create test surface");
    let context = Context::new(&surface).expect("create test context");
    context.translate(32., 32.);
    context.scale(1., -1.);

    paint_sweep_gradient(&context, 0., 0., 0., PI_TIMES_2, four_band_color_line())
        .expect("paint sweep");

    assert_eq!(pixel_rgba(&surface, 48, 16), [255, 0, 0, 255]);
    assert_eq!(pixel_rgba(&surface, 48, 48), [255, 255, 0, 255]);
}

/// The sweep paints only inside the current clip; the pixels outside it stay transparent.
#[test]
fn sweep_paints_only_inside_the_clip() {
    let surface = ImageSurface::create(Format::ARgb32, 64, 64).expect("create test surface");
    let context = Context::new(&surface).expect("create test context");
    context.rectangle(0., 0., 32., 32.);
    context.clip();

    paint_sweep_gradient(&context, 32., 32., 0., PI_TIMES_2, four_band_color_line())
        .expect("paint sweep");

    assert_eq!(pixel_rgba(&surface, 16, 16), [0, 0, 255, 255]);
    assert_eq!(pixel_rgba(&surface, 48, 48), [0, 0, 0, 0]);
    assert_eq!(opaque_pixel_count(&surface), 32 * 32);
}

/// Pad clamps an angle before the first stop to its colour and one past the last to the last colour, and
/// interpolates between stops on premultiplied channels: halfway from red to blue is half of each.
#[test]
fn pad_sweep_clamps_and_interpolates_between_stops() {
    let colors =
        SweepColors::new(two_stop_color_line(Extend::Pad), FRAC_PI_2, PI).expect("a pad sweep");

    assert_color(colors.color_at(0.1), [1., 0., 0., 1.], "before the first stop");
    assert_color(colors.color_at(3. * FRAC_PI_4), [0.5, 0., 0.5, 1.], "halfway");
    assert_color(colors.color_at(5.), [0., 0., 1., 1.], "past the last stop");
}

/// Repeat tiles the stop span around the turn; Reflect runs every other tile backwards, so one angle past the
/// span takes Repeat's colour from its start and Reflect's from its end.
#[test]
fn repeat_and_reflect_tile_the_stop_span() {
    let repeat = SweepColors::new(two_stop_color_line(Extend::Repeat), 0., FRAC_PI_2)
        .expect("a repeat sweep");
    let reflect = SweepColors::new(two_stop_color_line(Extend::Reflect), 0., FRAC_PI_2)
        .expect("a reflect sweep");

    for angle in [FRAC_PI_8, FRAC_PI_8 + FRAC_PI_2, FRAC_PI_8 + PI] {
        assert_color(repeat.color_at(angle), [0.75, 0., 0.25, 1.], &format!("Repeat at {angle}"));
    }
    assert_color(reflect.color_at(FRAC_PI_8), [0.75, 0., 0.25, 1.], "Reflect's first tile");
    assert_color(
        reflect.color_at(FRAC_PI_2 + FRAC_PI_8),
        [0.25, 0., 0.75, 1.],
        "Reflect's mirrored tile",
    );
}

/// A Repeat line whose stops lie whole turns before zero still tiles into the visible turn, from its own phase.
#[test]
fn repeat_reaches_the_turn_from_stops_far_before_it() {
    let color_line = ColorLine {
        color_stops: vec![stop(-4., opaque_red()), stop(-3., opaque_blue())],
        extend: Extend::Repeat,
    };
    let colors = SweepColors::new(color_line, 0., FRAC_PI_2).expect("a repeat sweep");

    assert_color(colors.color_at(FRAC_PI_8), [0.75, 0., 0.25, 1.], "a quarter into the tile");
}

/// A Repeat or Reflect span a tiny fraction of the turn wide, starting half a turn on, still colours every pixel:
/// each pixel's tile is computed from its angle, so no tile or patch budget leaves part of the turn bare.
#[test]
fn narrow_distant_repeat_and_reflect_paint_every_pixel() {
    for extend in [Extend::Repeat, Extend::Reflect] {
        let (surface, context) = test_surface_and_context();
        let color_line = ColorLine {
            color_stops: vec![stop(0., opaque_red()), stop(0.0005, opaque_blue())],
            extend,
        };

        paint_sweep_gradient(&context, 16., 16., PI, PI_TIMES_2, color_line).expect("paint sweep");

        assert_eq!(opaque_pixel_count(&surface), 32 * 32, "{extend:?} must colour every pixel");
    }
}

/// A Repeat or Reflect line with one stop, or with every stop at one offset, spans no angle and has no period
/// to tile, so it paints nothing; a one-stop Pad line fills the turn with its colour.
#[test]
fn zero_span_repeat_and_reflect_paint_nothing() {
    for extend in [Extend::Repeat, Extend::Reflect] {
        let one_stop = ColorLine { color_stops: vec![stop(0., opaque_red())], extend };
        let collapsed = ColorLine {
            color_stops: vec![stop(0.5, opaque_red()), stop(0.5, opaque_blue())],
            extend,
        };
        assert!(SweepColors::new(one_stop, 0.5, 1.).is_none(), "one-stop {extend:?}");
        assert!(SweepColors::new(collapsed, 0., PI_TIMES_2).is_none(), "collapsed {extend:?}");
    }
    let pad = ColorLine { color_stops: vec![stop(0., opaque_red())], extend: Extend::Pad };
    let colors = SweepColors::new(pad, 0.5, 1.).expect("a one-stop pad sweep");
    assert_color(colors.color_at(3.), [1., 0., 0., 1.], "one-stop Pad");
}

/// Extreme finite angles still colour each angle in constant time. Non-finite angles, a span whose stop angles
/// overflow, and a non-finite centre paint nothing, and a NaN stop offset is dropped rather than panicking.
#[test]
fn extreme_and_non_finite_sweeps_paint_safely() {
    let extreme =
        SweepColors::new(two_stop_color_line(Extend::Pad), -5., 1e9).expect("an extreme pad sweep");
    let SrgbaTuple(red, _, _, alpha) = extreme.color_at(1.);
    assert!(red > 0.99 && alpha == 1., "an angle near the first stop is nearly the first colour");

    for (start, end) in
        [(f64::NEG_INFINITY, 1.), (0., f64::INFINITY), (0., f64::NAN), (-f64::MAX, f64::MAX)]
    {
        assert!(
            SweepColors::new(two_stop_color_line(Extend::Pad), start, end).is_none(),
            "{start}..{end}"
        );
    }

    let (surface, context) = test_surface_and_context();
    paint_sweep_gradient(&context, f64::NAN, 16., 0., PI_TIMES_2, two_stop_color_line(Extend::Pad))
        .expect("a non-finite centre paints nothing");
    assert_eq!(opaque_pixel_count(&surface), 0);

    let nan_offset = ColorLine {
        color_stops: vec![stop(f64::NAN, opaque_red()), stop(1., opaque_blue())],
        extend: Extend::Pad,
    };
    let colors = SweepColors::new(nan_offset, 0., PI_TIMES_2).expect("the finite stop remains");
    assert_color(colors.color_at(1.), [0., 0., 1., 1.], "the remaining stop");
}

/// A sweep from 2 pi back to 0 runs its bands the other way round: the last band, yellow, fills the first
/// quarter turn and the first band, red, the last.
#[test]
fn a_backwards_sweep_runs_its_bands_the_other_way() {
    let colors =
        SweepColors::new(four_band_color_line(), PI_TIMES_2, 0.).expect("a backwards sweep");

    for (angle, expected) in [
        (FRAC_PI_4, [1., 1., 0., 1.]),
        (3. * FRAC_PI_4, [0., 0., 1., 1.]),
        (5. * FRAC_PI_4, [0., 1., 0., 1.]),
        (7. * FRAC_PI_4, [1., 0., 0., 1.]),
    ] {
        assert_color(colors.color_at(angle), expected, &format!("angle {angle}"));
    }
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

/// `normalize_color_line` is reached only with a non-empty list in production,
/// but it must stay total so a future caller cannot reintroduce the panic.
#[test]
fn normalize_empty_color_line_is_total() {
    let mut color_line = empty_color_line(Extend::Pad);

    let (min_stop, max_stop) = normalize_color_line(&mut color_line);

    assert_eq!(min_stop, 0., "empty color line should report an identity span");
    assert_eq!(max_stop, 1., "empty color line should report an identity span");
}
