use crate::color::{SrgbaPixel, SrgbaTuple};
use cairo::{Context, Extend, LinearGradient, Matrix, Mesh, MeshCorner, Operator, RadialGradient};

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

/// Total Bezier patches one sweep gradient may contribute to its mesh.
///
/// A hostile color line can ask for billions of patches through extreme angles
/// or a near-zero tiling span. The budget bounds the work at a level far above
/// any well-formed gradient, which needs 16 patches for a full turn.
const MAX_SWEEP_PATCHES: usize = 4096;

/// Bezier splits one angular span may be divided into.
///
/// Splits are chosen from the span width, so an out-of-range angle produced by
/// a malformed offset would otherwise scale the count without limit. Clamping
/// degrades such a span to a coarse approximation instead of a hang.
const MAX_SWEEP_SPLITS: usize = 256;

/// Repeats of the stop list a Repeat/Reflect sweep may tile across the turn.
const MAX_SWEEP_TILES: usize = 1000;

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

#[derive(Copy, Clone, Debug)]
struct Point {
    horizontal: f64,
    vertical: f64,
}

impl Point {
    fn dot(&self, other: Self) -> f64 {
        (self.horizontal * other.horizontal) + (self.vertical * other.vertical)
    }

    fn normalize(self) -> Self {
        let len = self.dot(self).sqrt();
        Self { horizontal: self.horizontal / len, vertical: self.vertical / len }
    }

    pub fn sum(self, other: Self) -> Self {
        Self {
            horizontal: self.horizontal + other.horizontal,
            vertical: self.vertical + other.vertical,
        }
    }

    pub fn difference(self, other: Self) -> Self {
        Self {
            horizontal: self.horizontal - other.horizontal,
            vertical: self.vertical - other.vertical,
        }
    }

    pub fn scale(self, factor: f64) -> Self {
        Self { horizontal: self.horizontal * factor, vertical: self.vertical * factor }
    }

    /// Compute a vector from the supplied angle
    pub fn from_angle(angle: f64) -> Self {
        let (sine, cosine) = angle.sin_cos();
        Self { horizontal: cosine, vertical: sine }
    }
}

fn interpolate(start: f64, end: f64, fraction: f64) -> f64 {
    start + fraction * (end - start)
}

#[derive(Debug)]
struct Patch {
    start: Point,
    start_control: Point,
    end_control: Point,
    end: Point,
    color0: SrgbaTuple,
    color1: SrgbaTuple,
}

impl Patch {
    fn add_to_mesh(&self, center: Point, mesh: &Mesh) {
        mesh.begin_patch();
        mesh.move_to(center.horizontal, center.vertical);
        mesh.line_to(self.start.horizontal, self.start.vertical);
        mesh.curve_to(
            self.start_control.horizontal,
            self.start_control.vertical,
            self.end_control.horizontal,
            self.end_control.vertical,
            self.end.horizontal,
            self.end.vertical,
        );
        mesh.line_to(center.horizontal, center.vertical);

        fn set_corner_color(mesh: &Mesh, corner: MeshCorner, color: SrgbaTuple) {
            let SrgbaTuple(red, green, blue, alpha) = color;

            mesh.set_corner_color_rgba(corner, red.into(), green.into(), blue.into(), alpha.into());
        }

        set_corner_color(mesh, MeshCorner::MeshCorner0, self.color0);
        set_corner_color(mesh, MeshCorner::MeshCorner1, self.color0);
        set_corner_color(mesh, MeshCorner::MeshCorner2, self.color1);
        set_corner_color(mesh, MeshCorner::MeshCorner3, self.color1);

        mesh.end_patch();
    }
}

/// Approximate the span `start_angle`..`end_angle` with Bezier patches around `center`.
///
/// `budget` is the mesh's remaining patch allowance, decremented as patches are
/// emitted; the split count is clamped to `MAX_SWEEP_SPLITS` so a malformed
/// angle cannot scale the loop without limit. A non-finite span yields no
/// patches because Cairo cannot represent its coordinates.
#[allow(clippy::too_many_arguments)]
fn add_sweep_gradient_patches(
    mesh: &Mesh,
    center: Point,
    radius: f64,
    start_angle: f64,
    start_color: SrgbaTuple,
    end_angle: f64,
    end_color: SrgbaTuple,
    budget: &mut usize,
) {
    if !start_angle.is_finite() || !end_angle.is_finite() {
        // When: start_angle or end_angle is non-finite, Cairo cannot represent the
        // patch coordinates, so the span contributes nothing.
        return;
    }
    const MAX_ANGLE: f64 = std::f64::consts::PI / 8.;
    let num_splits = (((end_angle - start_angle).abs() / MAX_ANGLE).ceil() as usize)
        .min(MAX_SWEEP_SPLITS)
        .min(*budget);

    let mut start_direction = Point::from_angle(start_angle);
    let mut color0 = start_color;

    for idx in 0..num_splits {
        let fraction = (idx as f64 + 1.) / num_splits as f64;

        let angle1 = interpolate(start_angle, end_angle, fraction);
        let color1 = start_color.interpolate(end_color, fraction);

        let end_direction = Point::from_angle(angle1);

        let bisector = start_direction.sum(end_direction).normalize();
        let tangent = Point { horizontal: -bisector.vertical, vertical: bisector.horizontal };

        fn compute_control(
            bisector: Point,
            tangent: Point,
            direction: Point,
            center: Point,
            radius: f64,
        ) -> Point {
            let intersection = bisector.sum(
                tangent
                    .scale(direction.difference(bisector).dot(direction) / tangent.dot(direction)),
            );
            intersection
                .difference(direction)
                .scale(0.33333)
                .sum(intersection)
                .scale(radius)
                .sum(center)
        }

        let patch = Patch {
            color0,
            color1,
            start: center.sum(start_direction.scale(radius)),
            end: center.sum(end_direction.scale(radius)),
            start_control: compute_control(bisector, tangent, start_direction, center, radius),
            end_control: compute_control(bisector, tangent, end_direction, center, radius),
        };

        patch.add_to_mesh(center, mesh);
        *budget -= 1;

        start_direction = end_direction;
        color0 = color1;
    }
}

/// Find the tile index whose stop list first reaches the visible turn.
///
/// The index is derived in constant time so a valid narrow span can begin more
/// than `MAX_SWEEP_TILES` copies away without being mistaken for unbounded work.
/// Emission remains separately bounded by the tile and patch budgets. A
/// degenerate, reversed, non-finite, or unrepresentable span yields no tile.
fn first_visible_tile(first_angle: f64, last_angle: f64, span: f64) -> Option<isize> {
    if !span.is_finite() || span <= 0. || !first_angle.is_finite() || !last_angle.is_finite() {
        // When: the stop span is not finite and forward-moving, so it cannot
        // define a stable Repeat or Reflect tile sequence.
        return None;
    }

    // When: first_angle or last_angle determines which span endpoint must be shifted to the visible turn.
    let tile = if first_angle >= 0. {
        -(first_angle / span).ceil()
    } else if last_angle < 0. {
        (-last_angle / span).ceil()
    } else {
        0.
    };

    if !tile.is_finite() || tile < isize::MIN as f64 || tile >= isize::MAX as f64 {
        // When: tile is non-finite or outside isize, the bounded emission loop cannot represent it.
        return None;
    }

    Some(tile as isize)
}

const PI_TIMES_2: f64 = std::f64::consts::PI * 2.;

/// Tile `color_line` into `mesh` as the Bezier approximation of a sweep.
///
/// Emits nothing for a color line left with no usable stop, so the first/last
/// stop reads below cannot index an empty vector. Total emission is capped at
/// `MAX_SWEEP_PATCHES`, which bounds the work for a hostile color line without
/// affecting a well-formed one.
fn apply_sweep_gradient_patches(
    mesh: &Mesh,
    mut color_line: ColorLine,
    center: Point,
    radius: f64,
    mut start_angle: f64,
    mut end_angle: f64,
) {
    if !prepare_color_stops(&mut color_line) {
        // When: prepare_color_stops leaves color_line with no usable stop, so
        // the first/last stop reads below would index an empty vector.
        return;
    }
    if !center.horizontal.is_finite()
        || !center.vertical.is_finite()
        || !radius.is_finite()
        || !start_angle.is_finite()
        || !end_angle.is_finite()
    {
        // When: center, radius, start_angle, or end_angle is non-finite, Cairo cannot represent the sweep coordinates.
        return;
    }

    let mut budget = MAX_SWEEP_PATCHES;

    if start_angle == end_angle {
        // When: start_angle equals end_angle the sweep has no width, so only
        // Pad's flat fill outside the degenerate sweep can contribute.
        if color_line.extend == Extend::Pad {
            if start_angle > 0. {
                let first_color = color_line.color_stops[0].color.into();
                add_sweep_gradient_patches(
                    mesh,
                    center,
                    radius,
                    0.,
                    first_color,
                    start_angle,
                    first_color,
                    &mut budget,
                );
            }
            if end_angle < PI_TIMES_2 {
                let last = color_line.color_stops.len() - 1;
                let last_color = color_line.color_stops[last].color.into();
                add_sweep_gradient_patches(
                    mesh,
                    center,
                    radius,
                    end_angle,
                    last_color,
                    PI_TIMES_2,
                    last_color,
                    &mut budget,
                );
            }
        }
        return;
    }

    if end_angle < start_angle {
        std::mem::swap(&mut start_angle, &mut end_angle);
        color_line.color_stops.reverse();
        for stop in &mut color_line.color_stops {
            stop.offset = 1.0 - stop.offset;
        }
    }

    let angles: Vec<f64> = color_line
        .color_stops
        .iter()
        .map(|stop| start_angle + stop.offset * (end_angle - start_angle))
        .collect();
    let colors: Vec<SrgbaTuple> =
        color_line.color_stops.iter().map(|stop| stop.color.into()).collect();

    let n_stops = angles.len();

    if color_line.extend == Extend::Pad {
        // When: color_line uses Extend::Pad, so angles outside the sweep are
        // filled with the nearest end colour rather than repeated.
        let mut color0 = colors[0];
        let mut pos = 0;
        while pos < n_stops {
            if angles[pos] >= 0. {
                // When: angles reached the visible range at pos, so the scan
                // for the first drawable stop ends here.
                if pos > 0 {
                    let fraction = (0. - angles[pos - 1]) / (angles[pos] - angles[pos - 1]);

                    color0 = colors[pos - 1].interpolate(colors[pos], fraction);
                }
                break;
            }
            pos += 1;
        }
        if pos == n_stops {
            // When: pos ran past the last stop, so the whole colour line sits
            // behind zero and its final colour fills the full turn.

            /* everything is below 0 */
            color0 = colors[n_stops - 1];
            add_sweep_gradient_patches(
                mesh,
                center,
                radius,
                0.,
                color0,
                PI_TIMES_2,
                color0,
                &mut budget,
            );
            return;
        }

        add_sweep_gradient_patches(
            mesh,
            center,
            radius,
            0.,
            color0,
            angles[pos],
            colors[pos],
            &mut budget,
        );

        pos += 1;
        while pos < n_stops {
            if angles[pos] <= PI_TIMES_2 {
                add_sweep_gradient_patches(
                    mesh,
                    center,
                    radius,
                    angles[pos - 1],
                    colors[pos - 1],
                    angles[pos],
                    colors[pos],
                    &mut budget,
                );
            } else {
                // When: angles[pos] overshot a full turn, so the span is cut at
                // 2*PI with an interpolated colour and the scan stops.
                let fraction = (PI_TIMES_2 - angles[pos - 1]) / (angles[pos] - angles[pos - 1]);
                let color1 = colors[pos - 1].interpolate(colors[pos], fraction);
                add_sweep_gradient_patches(
                    mesh,
                    center,
                    radius,
                    angles[pos - 1],
                    colors[pos - 1],
                    PI_TIMES_2,
                    color1,
                    &mut budget,
                );
                break;
            }
            pos += 1;
        }

        if pos == n_stops {
            /* everything is below 2*M_PI */
            color0 = colors[n_stops - 1];
            add_sweep_gradient_patches(
                mesh,
                center,
                radius,
                angles[n_stops - 1],
                color0,
                PI_TIMES_2,
                color0,
                &mut budget,
            );
        }
    } else {
        // When: color_line extends by Repeat or Reflect, so the stop list is
        // tiled across the turn instead of padded.
        let span = angles[n_stops - 1] - angles[0];
        let Some(first_tile) = first_visible_tile(angles[0], angles[n_stops - 1], span) else {
            // When: no tile index brings the stop list into the visible turn, so
            // the sweep contributes nothing rather than tiling a zero-width span.
            return;
        };
        let span = span.abs();

        // Tiling runs forward from the first visible tile. The upper bound is
        // offset from `first_tile`, because a bound of `first_tile.min(..)` can
        // never exceed `first_tile` and so yields an empty range for every
        // `first_tile`, emitting no patches at all.
        let tile_end = first_tile.saturating_add(MAX_SWEEP_TILES as isize);

        for tile in first_tile..tile_end {
            if budget == 0 {
                // When: the patch budget is spent, so further tiles cannot add
                // ink and the loop stops instead of scanning to the cap.
                return;
            }
            for stop_index in 1..n_stops {
                let (
                    segment_start_angle,
                    segment_end_angle,
                    segment_start_color,
                    segment_end_color,
                );

                if tile % 2 != 0 && color_line.extend == Extend::Reflect {
                    segment_start_angle = angles[0] + angles[n_stops - 1]
                        - angles[n_stops - 1 - (stop_index - 1)]
                        + (tile as f64) * span;
                    segment_end_angle = angles[0] + angles[n_stops - 1]
                        - angles[n_stops - 1 - stop_index]
                        + (tile as f64) * span;
                    segment_start_color = colors[n_stops - 1 - (stop_index - 1)];
                    segment_end_color = colors[n_stops - 1 - stop_index];
                } else {
                    // When: this is an even tile, or color_line does not
                    // Reflect, so stop order runs forward unmirrored.
                    segment_start_angle = angles[stop_index - 1] + (tile as f64) * span;
                    segment_end_angle = angles[stop_index] + (tile as f64) * span;
                    segment_start_color = colors[stop_index - 1];
                    segment_end_color = colors[stop_index];
                }

                if segment_end_angle < 0. {
                    // When: segment_end_angle is still behind zero, so this whole
                    // tile segment lies outside the visible turn.
                    continue;
                }

                if segment_start_angle < 0. {
                    let fraction =
                        (0. - segment_start_angle) / (segment_end_angle - segment_start_angle);
                    let color = segment_start_color.interpolate(segment_end_color, fraction);
                    add_sweep_gradient_patches(
                        mesh,
                        center,
                        radius,
                        0.,
                        color,
                        segment_end_angle,
                        segment_end_color,
                        &mut budget,
                    );
                } else if segment_end_angle >= PI_TIMES_2 {
                    // When: segment_end_angle reaches a full turn, so this segment
                    // closes the sweep and no later tile can contribute.
                    let fraction = (PI_TIMES_2 - segment_start_angle)
                        / (segment_end_angle - segment_start_angle);
                    let color = segment_start_color.interpolate(segment_end_color, fraction);
                    add_sweep_gradient_patches(
                        mesh,
                        center,
                        radius,
                        segment_start_angle,
                        segment_start_color,
                        PI_TIMES_2,
                        color,
                        &mut budget,
                    );
                    return;
                } else {
                    // When: segment_start_angle and segment_end_angle both sit inside
                    // the visible turn, so the segment is drawn whole with no clipping.
                    add_sweep_gradient_patches(
                        mesh,
                        center,
                        radius,
                        segment_start_angle,
                        segment_start_color,
                        segment_end_angle,
                        segment_end_color,
                        &mut budget,
                    );
                }
            }
        }
    }
}

/// Paint a COLRv1 sweep gradient over the current clip.
///
/// Cairo has no sweep-gradient primitive, so the sweep is approximated by a
/// mesh of Bezier patches spanning `start_angle`..`end_angle` around
/// `(center_x, center_y)`. The radius is taken from the farthest corner of the current clip
/// extents, so the mesh always covers the region being painted; the color
/// line's extend mode decides how angles outside the sweep are filled. A color
/// line left with no usable stop paints nothing.
pub fn paint_sweep_gradient(
    context: &Context,
    center_x: f64,
    center_y: f64,
    start_angle: f64,
    end_angle: f64,
    mut color_line: ColorLine,
) -> anyhow::Result<()> {
    if !prepare_color_stops(&mut color_line) {
        // When: prepare_color_stops leaves color_line with no usable stop, so
        // the mesh would be empty and the clip is left untouched.
        return Ok(());
    }

    let (clip_left, clip_top, clip_right, clip_bottom) = context.clip_extents()?;

    let max_x = ((clip_left - center_x) * (clip_left - center_x))
        .max((clip_right - center_x) * (clip_right - center_x));
    let max_y = ((clip_top - center_y) * (clip_top - center_y))
        .max((clip_bottom - center_y) * (clip_bottom - center_y));
    let radius = (max_x + max_y).sqrt();

    let mesh = Mesh::new();
    let center = Point { horizontal: center_x, vertical: center_y };
    apply_sweep_gradient_patches(&mesh, color_line, center, radius, start_angle, end_angle);
    context.set_source(mesh)?;
    context.paint()?;

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
