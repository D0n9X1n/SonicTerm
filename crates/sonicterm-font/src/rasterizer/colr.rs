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
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color_line: ColorLine,
    },
    PaintRadialGradient {
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
        color_line: ColorLine,
    },
    PaintSweepGradient {
        x0: f32,
        y0: f32,
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
/// `(x0, y0)`/`(x1, y1)` are the gradient's start and end anchors and
/// `(x2, y2)` its rotation anchor; the three are reduced to the two-point form
/// Cairo accepts. `color_line` is normalized first, so its stops are sorted and
/// rescaled to 0..=1 and the anchors are re-interpolated across the original
/// offset span. Stop colours are applied as straight (non-premultiplied) sRGBA.
/// A color line left with no usable stop paints nothing.
#[allow(clippy::too_many_arguments)]
pub fn paint_linear_gradient(
    context: &Context,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    mut color_line: ColorLine,
) -> anyhow::Result<()> {
    if !prepare_color_stops(&mut color_line) {
        // When: prepare_color_stops leaves color_line with no usable stop, so
        // there is no gradient to describe and the clip is left untouched.
        return Ok(());
    }

    let (min_stop, max_stop) = normalize_color_line(&mut color_line);

    let anchors = reduce_anchors(ReduceAnchorsIn { x0, y0, x1, y1, x2, y2 });

    let xxx0 = anchors.xx0 + min_stop * (anchors.xx1 - anchors.xx0);
    let yyy0 = anchors.yy0 + min_stop * (anchors.yy1 - anchors.yy0);
    let xxx1 = anchors.xx0 + max_stop * (anchors.xx1 - anchors.xx0);
    let yyy1 = anchors.yy0 + max_stop * (anchors.yy1 - anchors.yy0);

    let pattern = LinearGradient::new(xxx0, yyy0, xxx1, yyy1);
    pattern.set_extend(color_line.extend);

    for stop in &color_line.color_stops {
        let (r, g, b, a) = stop.color.as_srgba_tuple();
        pattern.add_color_stop_rgba(stop.offset, r.into(), g.into(), b.into(), a.into());
    }

    context.set_source(pattern)?;
    context.paint()?;

    Ok(())
}

/// Paint a COLRv1 radial gradient over the current clip.
///
/// `(x0, y0, r0)` and `(x1, y1, r1)` are the start and end circles. As with the
/// linear case, `color_line` is normalized first and both centres and radii are
/// re-interpolated across the original offset span, so the drawn circles match
/// the range the stops actually cover. A color line left with no usable stop
/// paints nothing.
#[allow(clippy::too_many_arguments)]
pub fn paint_radial_gradient(
    context: &Context,
    x0: f64,
    y0: f64,
    r0: f64,
    x1: f64,
    y1: f64,
    r1: f64,
    mut color_line: ColorLine,
) -> anyhow::Result<()> {
    if !prepare_color_stops(&mut color_line) {
        // When: prepare_color_stops leaves color_line with no usable stop, so
        // there are no circles to interpolate and the clip is left untouched.
        return Ok(());
    }

    let (min_stop, max_stop) = normalize_color_line(&mut color_line);

    let xx0 = x0 + min_stop * (x1 - x0);
    let yy0 = y0 + min_stop * (y1 - y0);
    let xx1 = x0 + max_stop * (x1 - x0);
    let yy1 = y0 + max_stop * (y1 - y0);
    let rr0 = r0 + min_stop * (r1 - r0);
    let rr1 = r0 + max_stop * (r1 - r0);

    let pattern = RadialGradient::new(xx0, yy0, rr0, xx1, yy1, rr1);
    pattern.set_extend(color_line.extend);

    for stop in &color_line.color_stops {
        let (r, g, b, a) = stop.color.as_srgba_tuple();
        pattern.add_color_stop_rgba(stop.offset, r.into(), g.into(), b.into(), a.into());
    }

    context.set_source(pattern)?;
    context.paint()?;

    Ok(())
}

#[derive(Copy, Clone, Debug)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn dot(&self, other: Self) -> f64 {
        (self.x * other.x) + (self.y * other.y)
    }

    fn normalize(self) -> Self {
        let len = self.dot(self).sqrt();
        Self { x: self.x / len, y: self.y / len }
    }

    pub fn sum(self, other: Self) -> Self {
        Self { x: self.x + other.x, y: self.y + other.y }
    }

    pub fn difference(self, other: Self) -> Self {
        Self { x: self.x - other.x, y: self.y - other.y }
    }

    pub fn scale(self, factor: f64) -> Self {
        Self { x: self.x * factor, y: self.y * factor }
    }

    /// Compute a vector from the supplied angle
    pub fn from_angle(angle: f64) -> Self {
        let (y, x) = angle.sin_cos();
        Self { x, y }
    }
}

fn interpolate(f0: f64, f1: f64, f: f64) -> f64 {
    f0 + f * (f1 - f0)
}

#[derive(Debug)]
struct Patch {
    p0: Point,
    c0: Point,
    c1: Point,
    p1: Point,
    color0: SrgbaTuple,
    color1: SrgbaTuple,
}

impl Patch {
    fn add_to_mesh(&self, center: Point, mesh: &Mesh) {
        mesh.begin_patch();
        mesh.move_to(center.x, center.y);
        mesh.line_to(self.p0.x, self.p0.y);
        mesh.curve_to(self.c0.x, self.c0.y, self.c1.x, self.c1.y, self.p1.x, self.p1.y);
        mesh.line_to(center.x, center.y);

        fn set_corner_color(mesh: &Mesh, corner: MeshCorner, color: SrgbaTuple) {
            let SrgbaTuple(r, g, b, a) = color;

            mesh.set_corner_color_rgba(corner, r.into(), g.into(), b.into(), a.into());
        }

        set_corner_color(mesh, MeshCorner::MeshCorner0, self.color0);
        set_corner_color(mesh, MeshCorner::MeshCorner1, self.color0);
        set_corner_color(mesh, MeshCorner::MeshCorner2, self.color1);
        set_corner_color(mesh, MeshCorner::MeshCorner3, self.color1);

        mesh.end_patch();
    }
}

/// Approximate the span `a0`..`a1` with Bezier patches around `center`.
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
    a0: f64,
    c0: SrgbaTuple,
    a1: f64,
    c1: SrgbaTuple,
    budget: &mut usize,
) {
    if !a0.is_finite() || !a1.is_finite() {
        // When: either sweep endpoint is non-finite, Cairo cannot represent the
        // patch coordinates, so the span contributes nothing.
        return;
    }
    const MAX_ANGLE: f64 = std::f64::consts::PI / 8.;
    let num_splits =
        (((a1 - a0).abs() / MAX_ANGLE).ceil() as usize).min(MAX_SWEEP_SPLITS).min(*budget);

    let mut p0 = Point::from_angle(a0);
    let mut color0 = c0;

    for idx in 0..num_splits {
        let k = (idx as f64 + 1.) / num_splits as f64;

        let angle1 = interpolate(a0, a1, k);
        let color1 = c0.interpolate(c1, k);

        let p1 = Point::from_angle(angle1);

        let a = p0.sum(p1).normalize();
        let u = Point { x: -a.y, y: a.x };

        fn compute_control(a: Point, u: Point, p: Point, center: Point, radius: f64) -> Point {
            let c = a.sum(u.scale(p.difference(a).dot(p) / u.dot(p)));
            c.difference(p).scale(0.33333).sum(c).scale(radius).sum(center)
        }

        let patch = Patch {
            color0,
            color1,
            p0: center.sum(p0.scale(radius)),
            p1: center.sum(p1.scale(radius)),
            c0: compute_control(a, u, p0, center, radius),
            c1: compute_control(a, u, p1, center, radius),
        };

        patch.add_to_mesh(center, mesh);
        *budget -= 1;

        p0 = p1;
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

    let tile = if first_angle >= 0. {
        // When: the first stop starts at or after zero, choose the nearest tile
        // shifted backward until that stop reaches the visible turn.
        -(first_angle / span).ceil()
    } else if last_angle < 0. {
        // When: the last stop remains behind zero, choose the nearest tile
        // shifted forward until that stop reaches the visible turn.
        (-last_angle / span).ceil()
    } else {
        // When: the unshifted stop span already crosses zero, tile zero is the
        // first visible copy.
        0.
    };

    if !tile.is_finite() || tile < isize::MIN as f64 || tile >= isize::MAX as f64 {
        // When: the derived tile cannot fit in the integer used by the bounded
        // emission loop, so the malformed span contributes no mesh.
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
    if !center.x.is_finite()
        || !center.y.is_finite()
        || !radius.is_finite()
        || !start_angle.is_finite()
        || !end_angle.is_finite()
    {
        // When: sweep geometry is non-finite, Cairo cannot represent its patch
        // coordinates, so the gradient contributes no mesh.
        return;
    }

    let mut budget = MAX_SWEEP_PATCHES;

    if start_angle == end_angle {
        // When: start_angle equals end_angle the sweep has no width, so only
        // Pad's flat fill outside the degenerate sweep can contribute.
        if color_line.extend == Extend::Pad {
            if start_angle > 0. {
                let c = color_line.color_stops[0].color.into();
                add_sweep_gradient_patches(
                    mesh,
                    center,
                    radius,
                    0.,
                    c,
                    start_angle,
                    c,
                    &mut budget,
                );
            }
            if end_angle < PI_TIMES_2 {
                let last = color_line.color_stops.len() - 1;
                let c = color_line.color_stops[last].color.into();
                add_sweep_gradient_patches(
                    mesh,
                    center,
                    radius,
                    end_angle,
                    c,
                    PI_TIMES_2,
                    c,
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
                    let k = (0. - angles[pos - 1]) / (angles[pos] - angles[pos - 1]);

                    color0 = colors[pos - 1].interpolate(colors[pos], k);
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
                let k = (PI_TIMES_2 - angles[pos - 1]) / (angles[pos] - angles[pos - 1]);
                let color1 = colors[pos - 1].interpolate(colors[pos], k);
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
        let Some(k) = first_visible_tile(angles[0], angles[n_stops - 1], span) else {
            // When: no tile index brings the stop list into the visible turn, so
            // the sweep contributes nothing rather than tiling a zero-width span.
            return;
        };
        let span = span.abs();

        // Tiling runs forward from the first visible tile. The upper bound is
        // offset from k, because a bound of k.min(..) can never exceed k and so
        // yields an empty range for every k, emitting no patches at all.
        let tile_end = k.saturating_add(MAX_SWEEP_TILES as isize);

        for l in k..tile_end {
            if budget == 0 {
                // When: the patch budget is spent, so further tiles cannot add
                // ink and the loop stops instead of scanning to the cap.
                return;
            }
            for i in 1..n_stops {
                let (a0, a1, c0, c1);

                if l % 2 != 0 && color_line.extend == Extend::Reflect {
                    a0 = angles[0] + angles[n_stops - 1] - angles[n_stops - 1 - (i - 1)]
                        + (l as f64) * span;
                    a1 = angles[0] + angles[n_stops - 1] - angles[n_stops - 1 - i]
                        + (l as f64) * span;
                    c0 = colors[n_stops - 1 - (i - 1)];
                    c1 = colors[n_stops - 1 - i];
                } else {
                    // When: this is an even tile, or color_line does not
                    // Reflect, so stop order runs forward unmirrored.
                    a0 = angles[i - 1] + (l as f64) * span;
                    a1 = angles[i] + (l as f64) * span;
                    c0 = colors[i - 1];
                    c1 = colors[i];
                }

                if a1 < 0. {
                    // When: a1 is still behind zero, so this whole tile segment
                    // lies outside the visible turn.
                    continue;
                }

                if a0 < 0. {
                    let f = (0. - a0) / (a1 - a0);
                    let color = c0.interpolate(c1, f);
                    add_sweep_gradient_patches(
                        mesh,
                        center,
                        radius,
                        0.,
                        color,
                        a1,
                        c1,
                        &mut budget,
                    );
                } else if a1 >= PI_TIMES_2 {
                    // When: a1 reaches a full turn, so this segment closes the
                    // sweep and no later tile can contribute.
                    let f = (PI_TIMES_2 - a0) / (a1 - a0);
                    let color = c0.interpolate(c1, f);
                    add_sweep_gradient_patches(
                        mesh,
                        center,
                        radius,
                        a0,
                        c0,
                        PI_TIMES_2,
                        color,
                        &mut budget,
                    );
                    return;
                } else {
                    // When: a0 and a1 both sit inside the visible turn, so the
                    // segment is drawn whole with no clipping.
                    add_sweep_gradient_patches(mesh, center, radius, a0, c0, a1, c1, &mut budget);
                }
            }
        }
    }
}

/// Paint a COLRv1 sweep gradient over the current clip.
///
/// Cairo has no sweep-gradient primitive, so the sweep is approximated by a
/// mesh of Bezier patches spanning `start_angle`..`end_angle` around
/// `(x0, y0)`. The radius is taken from the farthest corner of the current clip
/// extents, so the mesh always covers the region being painted; the color
/// line's extend mode decides how angles outside the sweep are filled. A color
/// line left with no usable stop paints nothing.
pub fn paint_sweep_gradient(
    context: &Context,
    x0: f64,
    y0: f64,
    start_angle: f64,
    end_angle: f64,
    mut color_line: ColorLine,
) -> anyhow::Result<()> {
    if !prepare_color_stops(&mut color_line) {
        // When: prepare_color_stops leaves color_line with no usable stop, so
        // the mesh would be empty and the clip is left untouched.
        return Ok(());
    }

    let (x1, y1, x2, y2) = context.clip_extents()?;

    let max_x = ((x1 - x0) * (x1 - x0)).max((x2 - x0) * (x2 - x0));
    let max_y = ((y1 - y0) * (y1 - y0)).max((y2 - y0) * (y2 - y0));
    let radius = (max_x + max_y).sqrt();

    let mesh = Mesh::new();
    let center = Point { x: x0, y: y0 };
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

    color_line.color_stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    let smallest = color_line.color_stops[0].offset;
    let largest = color_line.color_stops[color_line.color_stops.len() - 1].offset;

    if smallest != largest {
        for stop in &mut color_line.color_stops {
            stop.offset = (stop.offset - smallest) / (largest - smallest);
        }
    }

    (smallest, largest)
}

struct ReduceAnchorsIn {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

struct ReduceAnchorsOut {
    xx0: f64,
    yy0: f64,
    xx1: f64,
    yy1: f64,
}

fn reduce_anchors(ReduceAnchorsIn { x0, y0, x1, y1, x2, y2 }: ReduceAnchorsIn) -> ReduceAnchorsOut {
    let q2x = x2 - x0;
    let q2y = y2 - y0;
    let q1x = x1 - x0;
    let q1y = y1 - y0;

    let s = q2x * q2x + q2y * q2y;
    if s < 0.000001 {
        // When: s is degenerate, the rotation anchor coincides with p0, so the
        // anchors pass through unprojected rather than dividing by it.
        return ReduceAnchorsOut { xx0: x0, yy0: y0, xx1: x1, yy1: y1 };
    }

    let k = (q2x * q1x + q2y * q1y) / s;
    ReduceAnchorsOut { xx0: x0, yy0: y0, xx1: x1 - k * q2x, yy1: y1 - k * q2y }
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
    for op in ops {
        match op {
            DrawOp::MoveTo { to_x, to_y } => {
                context.move_to((*to_x).into(), (*to_y).into());
                current.replace((to_x, to_y));
            }
            DrawOp::LineTo { to_x, to_y } => {
                context.line_to((*to_x).into(), (*to_y).into());
                current.replace((to_x, to_y));
            }
            DrawOp::QuadTo { control_x, control_y, to_x, to_y } => {
                let (x, y) =
                    current.ok_or_else(|| anyhow::anyhow!("QuadTo has no current position"))?;
                // Express quadratic as a cubic
                // <https://stackoverflow.com/a/55034115/149111>

                context.curve_to(
                    (x + (2. / 3.) * (control_x - x)).into(),
                    (y + (2. / 3.) * (control_y - y)).into(),
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
