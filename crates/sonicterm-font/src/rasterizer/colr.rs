use crate::color::{SrgbaPixel, SrgbaTuple};
use cairo::{
    Context, Extend, Filter, Format, ImageSurface, LinearGradient, Matrix, Operator,
    RadialGradient, SurfacePattern,
};

#[cfg(test)]
#[path = "colr_tests.rs"]
mod colr_tests;

/* The gradient related routines in this file were ported from HarfBuzz, which
 * were in turn ported from BlackRenderer by Black Foundry.
 * Used by permission to relicense to HarfBuzz license,
 * which is in turn compatible with wezterm's license.
 *
 * https://github.com/BlackFoundryCom/black-renderer
 */

#[derive(Clone, Debug)]
pub struct ColorStop {
    pub offset: f64,
    pub color: SrgbaPixel,
}

#[derive(Clone, Debug)]
pub struct ColorLine {
    pub color_stops: Vec<ColorStop>,
    pub extend: Extend,
}

#[derive(Debug, Clone)]
pub enum PaintOp {
    PushTransform(Matrix),
    PopTransform,
    PushClip(Vec<DrawOp>),
    PopClip,
    PaintSolid(SrgbaPixel),
    PaintLinearGradient {
        /// Start anchor x coordinate (COLR `x0`).
        start_x: f32,
        /// Start anchor y coordinate (COLR `y0`).
        start_y: f32,
        /// End anchor x coordinate (COLR `x1`).
        end_x: f32,
        /// End anchor y coordinate (COLR `y1`).
        end_y: f32,
        /// Rotation anchor x coordinate (COLR `x2`).
        rotation_x: f32,
        /// Rotation anchor y coordinate (COLR `y2`).
        rotation_y: f32,
        color_line: ColorLine,
    },
    PaintRadialGradient {
        /// Start circle center x coordinate (COLR `x0`).
        start_x: f32,
        /// Start circle center y coordinate (COLR `y0`).
        start_y: f32,
        /// Start circle radius (COLR `radius0`).
        start_radius: f32,
        /// End circle center x coordinate (COLR `x1`).
        end_x: f32,
        /// End circle center y coordinate (COLR `y1`).
        end_y: f32,
        /// End circle radius (COLR `radius1`).
        end_radius: f32,
        color_line: ColorLine,
    },
    PaintSweepGradient {
        /// Sweep center x coordinate (COLR `centerX`).
        center_x: f32,
        /// Sweep center y coordinate (COLR `centerY`).
        center_y: f32,
        start_angle: f32,
        end_angle: f32,
        color_line: ColorLine,
    },
    PushGroup,
    PopGroup(Operator),
}

#[derive(Debug, Clone)]
pub enum DrawOp {
    MoveTo {
        to_x: f32,
        to_y: f32,
    },
    LineTo {
        to_x: f32,
        to_y: f32,
    },
    QuadTo {
        control_x: f32,
        control_y: f32,
        to_x: f32,
        to_y: f32,
    },
    CubicTo {
        control1_x: f32,
        control1_y: f32,
        control2_x: f32,
        control2_y: f32,
        to_x: f32,
        to_y: f32,
    },
    ClosePath,
}

/// Pixels a sweep gradient's raster may span on each axis: the glyph raster's own limit.
///
/// The raster covers the current clip in device space, so a clip larger than any glyph the rasterizer accepts
/// is painted only up to this bound and a malformed clip cannot allocate without limit.
const MAX_SWEEP_RASTER_DIMENSION: usize = crate::rasterizer::MAX_RASTERIZED_GLYPH_DIMENSION;

/// Drop stops carrying a non-finite offset and report whether any remain.
///
/// A COLRv1 `ColorLine` may legally carry zero stops, and a malformed one may
/// carry NaN offsets that no ordering can place. Both must degrade to painting
/// nothing rather than panicking on an empty index or an unwrapped comparison.
fn prepare_color_stops(color_line: &mut ColorLine) -> bool {
    color_line.color_stops.retain(|stop| stop.offset.is_finite());
    !color_line.color_stops.is_empty()
}

/// Paint a COLRv1 linear gradient over the current clip.
///
/// `(start_x, start_y)`/`(end_x, end_y)` are the gradient's start and end anchors and
/// `(rotation_x, rotation_y)` its rotation anchor; the three are reduced to the two-point form
/// Cairo accepts. `color_line` is normalized first, so its stops are sorted and
/// rescaled to 0..=1 and the anchors are re-interpolated across the original
/// offset span. Stop colours are applied as straight (non-premultiplied) sRGBA.
/// A color line left with no usable stop paints nothing.
#[allow(clippy::too_many_arguments)]
pub fn paint_linear_gradient(
    context: &Context,
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
    rotation_x: f64,
    rotation_y: f64,
    mut color_line: ColorLine,
) -> anyhow::Result<()> {
    if !prepare_color_stops(&mut color_line) {
        // When: prepare_color_stops leaves color_line with no usable stop, so
        // there is no gradient to describe and the clip is left untouched.
        return Ok(());
    }

    let (min_stop, max_stop) = normalize_color_line(&mut color_line);

    let anchors =
        reduce_anchors(ReduceAnchorsIn { start_x, start_y, end_x, end_y, rotation_x, rotation_y });

    let min_stop_x = anchors.start_x + min_stop * (anchors.end_x - anchors.start_x);
    let min_stop_y = anchors.start_y + min_stop * (anchors.end_y - anchors.start_y);
    let max_stop_x = anchors.start_x + max_stop * (anchors.end_x - anchors.start_x);
    let max_stop_y = anchors.start_y + max_stop * (anchors.end_y - anchors.start_y);

    let pattern = LinearGradient::new(min_stop_x, min_stop_y, max_stop_x, max_stop_y);
    pattern.set_extend(color_line.extend);

    for stop in &color_line.color_stops {
        let (red, green, blue, alpha) = stop.color.as_srgba_tuple();
        pattern.add_color_stop_rgba(
            stop.offset,
            red.into(),
            green.into(),
            blue.into(),
            alpha.into(),
        );
    }

    context.set_source(pattern)?;
    context.paint()?;

    Ok(())
}

/// Paint a COLRv1 radial gradient over the current clip.
///
/// `(start_x, start_y, start_radius)` and `(end_x, end_y, end_radius)` are the
/// start and end circles. As with the linear case, `color_line` is normalized
/// first and both centres and radii are re-interpolated across the original
/// offset span, so the drawn circles match the range the stops actually cover.
/// A color line left with no usable stop paints nothing.
#[allow(clippy::too_many_arguments)]
pub fn paint_radial_gradient(
    context: &Context,
    start_x: f64,
    start_y: f64,
    start_radius: f64,
    end_x: f64,
    end_y: f64,
    end_radius: f64,
    mut color_line: ColorLine,
) -> anyhow::Result<()> {
    if !prepare_color_stops(&mut color_line) {
        // When: prepare_color_stops leaves color_line with no usable stop, so
        // there are no circles to interpolate and the clip is left untouched.
        return Ok(());
    }

    let (min_stop, max_stop) = normalize_color_line(&mut color_line);

    let min_stop_x = start_x + min_stop * (end_x - start_x);
    let min_stop_y = start_y + min_stop * (end_y - start_y);
    let max_stop_x = start_x + max_stop * (end_x - start_x);
    let max_stop_y = start_y + max_stop * (end_y - start_y);
    let min_stop_radius = start_radius + min_stop * (end_radius - start_radius);
    let max_stop_radius = start_radius + max_stop * (end_radius - start_radius);

    let pattern = RadialGradient::new(
        min_stop_x,
        min_stop_y,
        min_stop_radius,
        max_stop_x,
        max_stop_y,
        max_stop_radius,
    );
    pattern.set_extend(color_line.extend);

    for stop in &color_line.color_stops {
        let (red, green, blue, alpha) = stop.color.as_srgba_tuple();
        pattern.add_color_stop_rgba(
            stop.offset,
            red.into(),
            green.into(),
            blue.into(),
            alpha.into(),
        );
    }

    context.set_source(pattern)?;
    context.paint()?;

    Ok(())
}

const PI_TIMES_2: f64 = std::f64::consts::PI * 2.;

/// The colour a COLRv1 sweep gradient gives each angle around its centre.
///
/// A stop sits at `start_angle + offset * (end_angle - start_angle)`, and the stops are kept in angle order.
/// Between two stops the colour is interpolated on premultiplied channels. Pad clamps an angle to the first or
/// last stop; Reflect mirrors every other repeat of the stop span; Repeat, and any other extend, repeats it.
/// Angles are measured in user space from the positive x axis towards positive y, the way Cairo turns.
struct SweepColors {
    angles: Vec<f64>,
    colors: Vec<SrgbaTuple>,
    extend: Extend,
}

impl SweepColors {
    /// Build the colour lookup, or None when the sweep paints nothing: no usable stop, a non-finite angle, or a
    /// Repeat or Reflect line whose stops span no finite angle.
    fn new(mut color_line: ColorLine, start_angle: f64, end_angle: f64) -> Option<Self> {
        if !prepare_color_stops(&mut color_line)
            || !start_angle.is_finite()
            || !end_angle.is_finite()
        {
            // When: color_line has no usable stop or an angle is non-finite, so no angle has a colour.
            return None;
        }
        let span = end_angle - start_angle;
        let mut stops: Vec<(f64, SrgbaTuple)> = color_line
            .color_stops
            .iter()
            .map(|stop| (start_angle + stop.offset * span, stop.color.into()))
            .collect();
        if stops.iter().any(|(angle, _)| !angle.is_finite()) {
            // When: an offset times the sweep overflowed, so a stop has no representable angle.
            return None;
        }
        // A backwards sweep puts later offsets at smaller angles; reversing first keeps a hard stop's pair in
        // angle order through the stable sort below.
        if span < 0. {
            stops.reverse();
        }
        stops.sort_by(|left, right| left.0.total_cmp(&right.0));
        let extend = color_line.extend;
        let stop_span = stops[stops.len() - 1].0 - stops[0].0;
        if extend != Extend::Pad && !(stop_span.is_finite() && stop_span > 0.) {
            // When: extend repeats the stops but stop_span is not a finite positive angle, so there is no period to tile.
            return None;
        }
        let (angles, colors) = stops.into_iter().unzip();
        Some(Self { angles, colors, extend })
    }

    /// The straight sRGBA colour at `angle`, in radians.
    fn color_at(&self, angle: f64) -> SrgbaTuple {
        let first = self.angles[0];
        let last = self.angles[self.angles.len() - 1];
        let position = match self.extend {
            Extend::Pad => angle,
            Extend::Reflect => {
                let period = last - first;
                let tiles = ((angle - first) / period).floor();
                let local = (angle - first) - tiles * period;
                // An odd repeat runs the stop span backwards from the last stop.
                if tiles.rem_euclid(2.) == 1. {
                    last - local
                } else {
                    // When: tiles is even, so this repeat runs the stop span forwards from the first stop.
                    first + local
                }
            }
            _ => first + (angle - first).rem_euclid(last - first),
        };
        if position < first {
            // When: the position lies before the first stop, which only Pad reaches, so the first colour fills it.
            return self.colors[0];
        }
        if position >= last {
            // When: the position lies at or past the last stop, so the last colour fills it.
            return self.colors[self.colors.len() - 1];
        }
        // The first stop past the position bounds its segment; at a hard stop that is the later colour's stop.
        let upper = self.angles.partition_point(|stop_angle| *stop_angle <= position);
        let lower = upper - 1;
        let fraction = (position - self.angles[lower]) / (self.angles[upper] - self.angles[lower]);
        self.colors[lower].interpolate(self.colors[upper], fraction)
    }
}

/// One premultiplied Cairo ARGB32 pixel from a straight sRGBA colour.
fn premultiplied_argb(color: SrgbaTuple) -> u32 {
    let SrgbaTuple(red, green, blue, alpha) = color;
    let alpha = alpha.clamp(0., 1.);
    let channel = |value: f32| (value.clamp(0., 1.) * alpha * 255.).round() as u32;
    (((alpha * 255.).round() as u32) << 24)
        | (channel(red) << 16)
        | (channel(green) << 8)
        | channel(blue)
}

/// Paint a COLRv1 sweep gradient over the current clip.
///
/// Cairo has no sweep-gradient primitive. Each device pixel of the current clip's extents is mapped back to
/// user space and takes the colour `SweepColors` gives its angle around `(center_x, center_y)`; the raster
/// is then painted as an image source under the clip. Computing every pixel's colour directly keeps the
/// result independent of Cairo's mesh rasterizer, which some Cairo builds (Ubuntu 26.04's 1.18.4) draw with
/// whole quadrants wrong. A color line left with no usable stop, a non-finite centre or angle, a zero-span
/// Repeat or Reflect line, a singular transform, and a clip covering no pixel paint nothing.
pub fn paint_sweep_gradient(
    context: &Context,
    center_x: f64,
    center_y: f64,
    start_angle: f64,
    end_angle: f64,
    color_line: ColorLine,
) -> anyhow::Result<()> {
    if !center_x.is_finite() || !center_y.is_finite() {
        // When: center_x or center_y is non-finite, so no pixel has a representable angle around the centre.
        return Ok(());
    }
    let Some(colors) = SweepColors::new(color_line, start_angle, end_angle) else {
        // When: SweepColors has no colour for any angle, so the clip is left untouched.
        return Ok(());
    };
    let user_to_device = context.matrix();
    let Ok(device_to_user) = user_to_device.try_invert() else {
        // When: user_to_device has no inverse, so no device pixel maps back to a user-space angle.
        return Ok(());
    };

    let (clip_left, clip_top, clip_right, clip_bottom) = context.clip_extents()?;
    let corners = [
        (clip_left, clip_top),
        (clip_right, clip_top),
        (clip_left, clip_bottom),
        (clip_right, clip_bottom),
    ]
    .map(|(corner_x, corner_y)| user_to_device.transform_point(corner_x, corner_y));
    let device_left = corners.iter().map(|corner| corner.0).fold(f64::INFINITY, f64::min).floor();
    let device_top = corners.iter().map(|corner| corner.1).fold(f64::INFINITY, f64::min).floor();
    let device_right =
        corners.iter().map(|corner| corner.0).fold(f64::NEG_INFINITY, f64::max).ceil();
    let device_bottom =
        corners.iter().map(|corner| corner.1).fold(f64::NEG_INFINITY, f64::max).ceil();
    if ![device_left, device_top, device_right, device_bottom].iter().all(|edge| edge.is_finite()) {
        // When: a clip edge has no finite device position, so the raster has no extent to cover.
        return Ok(());
    }
    let width_px = ((device_right - device_left).max(0.) as usize).min(MAX_SWEEP_RASTER_DIMENSION);
    let height_px = ((device_bottom - device_top).max(0.) as usize).min(MAX_SWEEP_RASTER_DIMENSION);
    if width_px == 0 || height_px == 0 {
        // When: width_px or height_px is zero, the clip covers no device pixel and there is nothing to paint.
        return Ok(());
    }

    let mut raster = ImageSurface::create(Format::ARgb32, width_px as i32, height_px as i32)?;
    let stride = raster.stride() as usize;
    {
        let mut pixels =
            raster.data().map_err(|error| anyhow::anyhow!("sweep raster: {error:?}"))?;
        for row in 0..height_px {
            for column in 0..width_px {
                // Each pixel takes the colour at its centre, mapped back into the sweep's user space.
                let (user_x, user_y) = device_to_user.transform_point(
                    device_left + column as f64 + 0.5,
                    device_top + row as f64 + 0.5,
                );
                let angle = (user_y - center_y).atan2(user_x - center_x).rem_euclid(PI_TIMES_2);
                let at = row * stride + column * 4;
                pixels[at..at + 4]
                    .copy_from_slice(&premultiplied_argb(colors.color_at(angle)).to_ne_bytes());
            }
        }
    }

    // The raster's pixel (column, row) covers device pixel (device_left + column, device_top + row), so the
    // source is placed in device space with the user transform reset for the paint.
    let pattern = SurfacePattern::create(&raster);
    let mut placement = Matrix::identity();
    placement.translate(-device_left, -device_top);
    pattern.set_matrix(placement);
    pattern.set_filter(Filter::Nearest);
    context.save()?;
    context.identity_matrix();
    let painted = context.set_source(&pattern).and_then(|()| context.paint());
    context.restore()?;
    painted?;
    Ok(())
}

/// Sort `color_line`'s stops and rescale their offsets onto 0..=1.
///
/// Returns the original smallest and largest offsets so callers can
/// re-interpolate their anchors across the span the stops actually covered. An
/// empty color line reports the identity span rather than indexing stop zero,
/// which keeps the function total for any caller. Offsets are ordered by
/// `total_cmp`, so a non-finite offset cannot panic an unwrapped comparison.
fn normalize_color_line(color_line: &mut ColorLine) -> (f64, f64) {
    if color_line.color_stops.is_empty() {
        // When: color_stops is empty, so there is no offset span to measure and
        // the identity range leaves any caller's anchors unchanged.
        return (0., 1.);
    }

    color_line.color_stops.sort_by(|left, right| left.offset.total_cmp(&right.offset));
    let smallest = color_line.color_stops[0].offset;
    let largest = color_line.color_stops[color_line.color_stops.len() - 1].offset;

    if smallest != largest {
        for stop in &mut color_line.color_stops {
            stop.offset = (stop.offset - smallest) / (largest - smallest);
        }
    }

    (smallest, largest)
}

/// The three COLR linear-gradient anchors that `reduce_anchors` projects onto two.
struct ReduceAnchorsIn {
    /// Start anchor x coordinate (COLR `x0`).
    start_x: f64,
    /// Start anchor y coordinate (COLR `y0`).
    start_y: f64,
    /// End anchor x coordinate (COLR `x1`).
    end_x: f64,
    /// End anchor y coordinate (COLR `y1`).
    end_y: f64,
    /// Rotation anchor x coordinate (COLR `x2`).
    rotation_x: f64,
    /// Rotation anchor y coordinate (COLR `y2`).
    rotation_y: f64,
}

/// The two-point gradient line Cairo draws once the rotation anchor is projected away.
struct ReduceAnchorsOut {
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
}

fn reduce_anchors(
    ReduceAnchorsIn { start_x, start_y, end_x, end_y, rotation_x, rotation_y }: ReduceAnchorsIn,
) -> ReduceAnchorsOut {
    let rotation_offset_x = rotation_x - start_x;
    let rotation_offset_y = rotation_y - start_y;
    let end_offset_x = end_x - start_x;
    let end_offset_y = end_y - start_y;

    let rotation_length_squared =
        rotation_offset_x * rotation_offset_x + rotation_offset_y * rotation_offset_y;
    if rotation_length_squared < 0.000001 {
        // When: rotation_length_squared is degenerate, the rotation anchor sits on the
        // start anchor, so the anchors pass through unprojected rather than dividing by it.
        return ReduceAnchorsOut { start_x, start_y, end_x, end_y };
    }

    let projection = (rotation_offset_x * end_offset_x + rotation_offset_y * end_offset_y)
        / rotation_length_squared;
    ReduceAnchorsOut {
        start_x,
        start_y,
        end_x: end_x - projection * rotation_offset_x,
        end_y: end_y - projection * rotation_offset_y,
    }
}

/// Replay a COLR glyph outline onto `context` as a fresh path.
///
/// Starts a new path, so any path already on `context` is discarded. Quadratic
/// segments are raised to the equivalent cubic because Cairo has no quadratic
/// primitive, which requires a current point — a `QuadTo` before any `MoveTo`
/// is an error rather than a silent no-op.
pub fn apply_draw_ops_to_context(ops: &[DrawOp], context: &Context) -> anyhow::Result<()> {
    let mut current = None;
    context.new_path();
    for draw_op in ops {
        match draw_op {
            DrawOp::MoveTo { to_x, to_y } => {
                context.move_to((*to_x).into(), (*to_y).into());
                current.replace((to_x, to_y));
            }
            DrawOp::LineTo { to_x, to_y } => {
                context.line_to((*to_x).into(), (*to_y).into());
                current.replace((to_x, to_y));
            }
            DrawOp::QuadTo { control_x, control_y, to_x, to_y } => {
                let (current_x, current_y) =
                    current.ok_or_else(|| anyhow::anyhow!("QuadTo has no current position"))?;
                // Express quadratic as a cubic
                // <https://stackoverflow.com/a/55034115/149111>

                context.curve_to(
                    (current_x + (2. / 3.) * (control_x - current_x)).into(),
                    (current_y + (2. / 3.) * (control_y - current_y)).into(),
                    (to_x + (2. / 3.) * (control_x - to_x)).into(),
                    (to_y + (2. / 3.) * (control_y - to_y)).into(),
                    (*to_x).into(),
                    (*to_y).into(),
                );
                current.replace((to_x, to_y));
            }
            DrawOp::CubicTo { control1_x, control1_y, control2_x, control2_y, to_x, to_y } => {
                context.curve_to(
                    (*control1_x).into(),
                    (*control1_y).into(),
                    (*control2_x).into(),
                    (*control2_y).into(),
                    (*to_x).into(),
                    (*to_y).into(),
                );
                current.replace((to_x, to_y));
            }
            DrawOp::ClosePath => {
                context.close_path();
            }
        }
    }
    Ok(())
}
