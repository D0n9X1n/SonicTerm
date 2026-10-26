//! Bounds, optional clip boxes and Cairo replay for FreeType COLR paint operations.

use super::*;

/// Device-pixel corners of a COLR ClipBox, in Cairo's y-down output space.
///
/// FreeType reports the corners in 26.6 device pixels, y up, with the face's size, transform and
/// translation already applied, so they are only rescaled and flipped here, never transformed again.
pub(super) fn clip_box_corners_px(clip_box: &FT_ClipBox_) -> [(f64, f64); 4] {
    [clip_box.bottom_left, clip_box.top_left, clip_box.top_right, clip_box.bottom_right]
        .map(|corner| (corner.x.font_units() as f64 / 64.0, -(corner.y.font_units() as f64) / 64.0))
}

/// Integer pixel rectangle covering a recording's ink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PixelBounds {
    pub(super) left: i64,
    pub(super) top: i64,
    pub(super) width: usize,
    pub(super) height: usize,
}

/// Rounds Cairo ink extents outward to whole pixels, so a fractional edge keeps its pixel.
///
/// Cairo reports a recording it cannot bound as a negative size; that, a non-finite value or a span
/// past the glyph limit is an error. Zero area is a valid empty glyph.
pub(super) fn pixel_bounds(
    left: f64,
    top: f64,
    width: f64,
    height: f64,
) -> anyhow::Result<PixelBounds> {
    if ![left, top, width, height].iter().all(|value| value.is_finite())
        || width < 0.0
        || height < 0.0
    {
        // Non-finite or negative extents mean Cairo could not bound the ink.
        bail!("invalid color glyph extents {width}x{height}");
    }
    if width == 0.0 || height == 0.0 {
        // When: `width` or `height` is 0.0, the recording covers no area, so the glyph is empty.
        return Ok(PixelBounds { left: 0, top: 0, width: 0, height: 0 });
    }
    let limit = MAX_RASTERIZED_GLYPH_DIMENSION as f64;
    let (min_x, min_y) = (left.floor(), top.floor());
    let (span_x, span_y) = ((left + width).ceil() - min_x, (top + height).ceil() - min_y);
    if span_x > limit
        || span_y > limit
        || min_x.abs() > i64::MAX as f64
        || min_y.abs() > i64::MAX as f64
    {
        // A span past the glyph limit, or an origin no i64 can hold, is rejected before allocation.
        bail!("color glyph extents {span_x}x{span_y} exceed the {limit}px glyph limit");
    }
    Ok(PixelBounds {
        left: min_x as i64,
        top: min_y as i64,
        width: span_x as usize,
        height: span_y as usize,
    })
}

/// Rasterizes a COLRv1 glyph's paint ops, clipped to its ClipBox when it has one and unclipped when it has
/// none. Without a ClipBox a graph Cairo cannot bound has no finite rendering, so it fails with that reason.
pub(super) fn rasterize_colr(
    ops: Vec<PaintOp>,
    scale_x: f64,
    scale_y: f64,
    clip_box: Option<&FT_ClipBox_>,
) -> anyhow::Result<RasterizedGlyph> {
    let unclipped = clip_box.is_none();
    rasterize_from_ops(ops, scale_x, scale_y, clip_box.map(clip_box_corners_px)).map_err(|error| {
        let unbounded = unclipped && error.to_string().starts_with("invalid color glyph extents");
        if !unbounded {
            // When: `unbounded` is false, the failure is not a missing ClipBox's, so it passes through unchanged.
            return error;
        }
        anyhow::anyhow!("COLRv1 glyph has no ClipBox to bound its paint: {error}")
    })
}

pub(super) fn rasterize_from_ops(
    ops: Vec<PaintOp>,
    scale_x: f64,
    scale_y: f64,
    clip: Option<[(f64, f64); 4]>,
) -> anyhow::Result<RasterizedGlyph> {
    let (surface, has_color) = record_to_cairo_surface(ops, scale_x, scale_y, clip)?;
    let (left, top, width, height) = surface.ink_extents();
    log::trace!("extents: left={left} top={top} width={width} height={height}");

    let bounds = pixel_bounds(left, top, width, height)?;
    let (left, top) = (bounds.left as f64, bounds.top as f64);
    let width_px = bounds.width;
    let height_px = bounds.height;
    if width_px == 0 || height_px == 0 {
        // When: width_px or height_px collapsed to zero, so the paint ops
        // covered no pixels and an empty bitmap stands in.
        return Ok(RasterizedGlyph {
            data: vec![],
            height: 0,
            width: 0,
            bearing_x: PixelLength::new(0.),
            bearing_y: PixelLength::new(0.),
            has_color: false,
            is_scaled: true,
        });
    }
    checked_glyph_rgba_len(width_px, height_px)?;

    let mut bounds_adjust = Matrix::identity();
    bounds_adjust.translate(-left, -top);
    log::trace!("dims: {width}x{height} {bounds_adjust:?}");

    let target = ImageSurface::create(
        Format::ARgb32,
        i32::try_from(width_px).context("color glyph width exceeds i32")?,
        i32::try_from(height_px).context("color glyph height exceeds i32")?,
    )?;
    {
        let context = Context::new(&target)?;
        context.transform(bounds_adjust);
        context.set_antialias(cairo::Antialias::Best);
        context.set_source_surface(surface, 0., 0.)?;
        context.paint()?;
    }

    let mut data = target.take_data()?.to_vec();
    argb_to_rgba(&mut data);

    Ok(RasterizedGlyph {
        data,
        height: height_px,
        width: width_px,
        bearing_x: PixelLength::new(left.min(0.)),
        bearing_y: PixelLength::new(-top),
        has_color,
        is_scaled: true,
    })
}
pub(super) fn record_to_cairo_surface(
    paint_ops: Vec<PaintOp>,
    scale_x: f64,
    scale_y: f64,
    clip: Option<[(f64, f64); 4]>,
) -> anyhow::Result<(RecordingSurface, bool)> {
    let mut has_color = false;
    let surface = RecordingSurface::create(Content::ColorAlpha, None)?;
    let context = Context::new(&surface)?;
    if let Some(corners) = clip {
        // A ClipBox bounds every paint and group, including composites Cairo cannot bound
        // itself, so it is set at identity, in device pixels, outside every save.
        if !corners.iter().all(|(x_px, y_px)| x_px.is_finite() && y_px.is_finite()) {
            // A non-finite corner cannot form a clip path.
            bail!("invalid color glyph clip box {corners:?}");
        }
        let [first, rest @ ..] = corners;
        context.move_to(first.0, first.1);
        for (x_px, y_px) in rest {
            context.line_to(x_px, y_px);
        }
        context.close_path();
        context.clip();
    }
    context.scale(scale_x, scale_y);
    context.set_antialias(cairo::Antialias::Best);

    for pop in paint_ops {
        match pop {
            PaintOp::PushTransform(matrix) => {
                context.save()?;
                context.transform(matrix);
            }
            PaintOp::PopTransform => {
                context.restore()?;
            }
            PaintOp::PushClip(draw) => {
                context.save()?;
                apply_draw_ops_to_context(&draw, &context)?;
                context.clip();
            }
            PaintOp::PopClip => {
                context.restore()?;
            }
            PaintOp::PushGroup => {
                context.save()?;
                context.push_group();
            }
            PaintOp::PopGroup(operator) => {
                context.pop_group_to_source()?;
                context.set_operator(operator);
                context.paint()?;
                context.restore()?;
            }
            PaintOp::PaintSolid(color) => {
                if color.as_srgba32() != 0xffffffff {
                    has_color = true;
                }
                let (red, green, blue, alpha) = color.as_srgba_tuple();
                context.set_source_rgba(red.into(), green.into(), blue.into(), alpha.into());
                context.paint()?;
            }
            PaintOp::PaintLinearGradient {
                start_x,
                start_y,
                end_x,
                end_y,
                rotation_x,
                rotation_y,
                color_line,
            } => {
                has_color = true;
                paint_linear_gradient(
                    &context,
                    start_x.into(),
                    start_y.into(),
                    end_x.into(),
                    end_y.into(),
                    rotation_x.into(),
                    rotation_y.into(),
                    color_line,
                )?;
            }
            PaintOp::PaintRadialGradient {
                start_x,
                start_y,
                start_radius,
                end_x,
                end_y,
                end_radius,
                color_line,
            } => {
                has_color = true;
                paint_radial_gradient(
                    &context,
                    start_x.into(),
                    start_y.into(),
                    start_radius.into(),
                    end_x.into(),
                    end_y.into(),
                    end_radius.into(),
                    color_line,
                )?;
            }
            PaintOp::PaintSweepGradient {
                center_x,
                center_y,
                start_angle,
                end_angle,
                color_line,
            } => {
                has_color = true;
                paint_sweep_gradient(
                    &context,
                    center_x.into(),
                    center_y.into(),
                    start_angle.into(),
                    end_angle.into(),
                    color_line,
                )?;
            }
        }
    }

    Ok((surface, has_color))
}

#[cfg(test)]
#[path = "freetype_surface_tests.rs"]
mod freetype_surface_tests;
