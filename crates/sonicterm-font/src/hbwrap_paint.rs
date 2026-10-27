//! HarfBuzz color-paint and outline callbacks producing owned rendering operations.

use super::*;

impl Font {
    pub fn get_paint_ops_for_glyph(
        &self,
        glyph_pos: u32,
        palette_index: ::std::os::raw::c_uint,
        foreground: hb_color_t,
        // TODO: pass a callback for querying custom palette colors
        // from the application
    ) -> anyhow::Result<Vec<PaintOp>> {
        let mut ops = vec![];

        let funcs = FontFuncs::new()?;

        macro_rules! func {
            ($hbfunc:ident, $method:ident) => {
                $hbfunc(funcs.funcs, Some(PaintOp::$method), std::ptr::null_mut(), None);
            };
        }

        // SAFETY: `funcs.funcs` is live and each callback function has HarfBuzz's required
        // ABI; HarfBuzz stores function pointers but no borrowed Rust callback data.
        unsafe {
            func!(hb_paint_funcs_set_push_transform_func, push_transform);
            func!(hb_paint_funcs_set_pop_transform_func, pop_transform);
            func!(hb_paint_funcs_set_push_clip_glyph_func, push_clip_glyph);
            func!(hb_paint_funcs_set_push_clip_rectangle_func, push_clip_rect);
            func!(hb_paint_funcs_set_pop_clip_func, pop_clip);
            func!(hb_paint_funcs_set_color_func, paint_solid);
            func!(hb_paint_funcs_set_linear_gradient_func, paint_linear_gradient);
            func!(hb_paint_funcs_set_radial_gradient_func, paint_radial_gradient);
            func!(hb_paint_funcs_set_sweep_gradient_func, paint_sweep_gradient);
            func!(hb_paint_funcs_set_image_func, paint_image);
            func!(hb_paint_funcs_set_push_group_func, push_group);
            func!(hb_paint_funcs_set_pop_group_func, pop_group);

            // TODO: hb_paint_funcs_set_custom_palette_color_func
        }

        // SAFETY: the font and callback table are live; `&mut ops` remains uniquely valid
        // for all synchronous paint callbacks and is not retained after the call.
        unsafe {
            hb_font_paint_glyph(
                self.font,
                glyph_pos,
                funcs.funcs,
                &mut ops as *mut Vec<PaintOp> as *mut _,
                palette_index,
                foreground,
            )
        }

        Ok(ops)
    }
}

#[derive(Debug, Clone)]
pub enum PaintOp {
    PushTransform {
        /// Coefficient of input x in output x (HarfBuzz `xx`).
        scale_x: f32,
        /// Coefficient of input x in output y (HarfBuzz `yx`).
        skew_y: f32,
        /// Coefficient of input y in output x (HarfBuzz `xy`).
        skew_x: f32,
        /// Coefficient of input y in output y (HarfBuzz `yy`).
        scale_y: f32,
        /// Offset added to output x (HarfBuzz `dx`).
        translate_x: f32,
        /// Offset added to output y (HarfBuzz `dy`).
        translate_y: f32,
    },
    PopTransform,
    PushGlyphClip {
        #[allow(unused)]
        glyph: hb_codepoint_t,
        draw: Vec<DrawOp>,
    },
    PushRectClip {
        xmin: f32,
        ymin: f32,
        xmax: f32,
        ymax: f32,
    },
    PopClip,
    PaintSolid {
        #[allow(unused)]
        is_foreground: bool,
        color: hb_color_t,
    },
    PaintImage {
        image: Blob,
        #[allow(unused)]
        width: u32,
        #[allow(unused)]
        height: u32,
        format: hb_tag_t,
        slant: f32,
        extents: Option<hb_glyph_extents_t>,
    },
    PaintLinearGradient {
        /// Start anchor x coordinate (HarfBuzz `x0`).
        start_x: f32,
        /// Start anchor y coordinate (HarfBuzz `y0`).
        start_y: f32,
        /// End anchor x coordinate (HarfBuzz `x1`).
        end_x: f32,
        /// End anchor y coordinate (HarfBuzz `y1`).
        end_y: f32,
        /// Rotation anchor x coordinate (HarfBuzz `x2`).
        rotation_x: f32,
        /// Rotation anchor y coordinate (HarfBuzz `y2`).
        rotation_y: f32,
        color_line: ColorLine,
    },
    PaintRadialGradient {
        /// Start circle center x coordinate (HarfBuzz `x0`).
        start_x: f32,
        /// Start circle center y coordinate (HarfBuzz `y0`).
        start_y: f32,
        /// Start circle radius (HarfBuzz `r0`).
        start_radius: f32,
        /// End circle center x coordinate (HarfBuzz `x1`).
        end_x: f32,
        /// End circle center y coordinate (HarfBuzz `y1`).
        end_y: f32,
        /// End circle radius (HarfBuzz `r1`).
        end_radius: f32,
        color_line: ColorLine,
    },
    PaintSweepGradient {
        /// Sweep center x coordinate (HarfBuzz `x0`).
        center_x: f32,
        /// Sweep center y coordinate (HarfBuzz `y0`).
        center_y: f32,
        start_angle: f32,
        end_angle: f32,
        color_line: ColorLine,
    },
    PushGroup,
    PopGroup {
        mode: hb_paint_composite_mode_t,
    },
}

impl PaintOp {
    // SAFETY: `data` must be the unique live `Vec<PaintOp>` pointer supplied to the active
    // synchronous HarfBuzz paint call and remain valid for the returned borrow.
    unsafe fn paint_data(data: *mut ::std::os::raw::c_void) -> &'static mut Vec<PaintOp> {
        // SAFETY: the function contract guarantees the pointer's type, unique access, and lifetime.
        unsafe { &mut *(data as *mut Vec<PaintOp>) }
    }

    // SAFETY: HarfBuzz invokes this during painting with the live `paint_data` pointer
    // supplied by `get_paint_ops_for_glyph`; it is not retained beyond the callback.
    unsafe extern "C" fn push_transform(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        scale_x: f32,
        skew_y: f32,
        skew_x: f32,
        scale_y: f32,
        translate_x: f32,
        translate_y: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PushTransform {
            scale_x,
            skew_y,
            skew_x,
            scale_y,
            translate_x,
            translate_y,
        });
    }

    // SAFETY: HarfBuzz invokes this during painting with the live `paint_data` pointer
    // supplied by `get_paint_ops_for_glyph`; it is not retained beyond the callback.
    unsafe extern "C" fn pop_transform(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PopTransform);
    }

    // SAFETY: HarfBuzz invokes this during painting with the live `paint_data` pointer
    // supplied by `get_paint_ops_for_glyph`; it is not retained beyond the callback.
    unsafe extern "C" fn push_clip_rect(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        xmin: f32,
        ymin: f32,
        xmax: f32,
        ymax: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PushRectClip { xmin, ymin, xmax, ymax });
    }

    // SAFETY: HarfBuzz invokes this during painting with the live `paint_data` pointer
    // supplied by `get_paint_ops_for_glyph`; it is not retained beyond the callback.
    unsafe extern "C" fn pop_clip(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PopClip);
    }

    // SAFETY: HarfBuzz invokes this during painting with the live `paint_data` pointer
    // supplied by `get_paint_ops_for_glyph`; it is not retained beyond the callback.
    unsafe extern "C" fn paint_solid(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        is_foreground: hb_bool_t,
        color: hb_color_t,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PaintSolid { is_foreground: is_foreground != 0, color });
    }

    // SAFETY: HarfBuzz supplies a live color-line pointer and the unique live paint vector
    // during this synchronous callback; neither pointer is retained.
    unsafe extern "C" fn paint_linear_gradient(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        color_line: *mut hb_color_line_t,
        start_x: f32,
        start_y: f32,
        end_x: f32,
        end_y: f32,
        rotation_x: f32,
        rotation_y: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        let color_line =
            // SAFETY: HarfBuzz keeps `color_line` live for the callback; conversion copies it.
            unsafe { ColorLine::new_from_hb(color_line) };
        ops.push(Self::PaintLinearGradient {
            color_line,
            start_x,
            start_y,
            end_x,
            end_y,
            rotation_x,
            rotation_y,
        });
    }

    // SAFETY: HarfBuzz supplies a live color-line pointer and the unique live paint vector
    // during this synchronous callback; neither pointer is retained.
    unsafe extern "C" fn paint_radial_gradient(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        color_line: *mut hb_color_line_t,
        start_x: f32,
        start_y: f32,
        start_radius: f32,
        end_x: f32,
        end_y: f32,
        end_radius: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        let color_line =
            // SAFETY: HarfBuzz keeps `color_line` live for the callback; conversion copies it.
            unsafe { ColorLine::new_from_hb(color_line) };
        ops.push(Self::PaintRadialGradient {
            color_line,
            start_x,
            start_y,
            start_radius,
            end_x,
            end_y,
            end_radius,
        });
    }

    // SAFETY: HarfBuzz supplies a live color-line pointer and the unique live paint vector
    // during this synchronous callback; neither pointer is retained.
    unsafe extern "C" fn paint_sweep_gradient(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        color_line: *mut hb_color_line_t,
        center_x: f32,
        center_y: f32,
        start_angle: f32,
        end_angle: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        let color_line =
            // SAFETY: HarfBuzz keeps `color_line` live for the callback; conversion copies it.
            unsafe { ColorLine::new_from_hb(color_line) };
        ops.push(Self::PaintSweepGradient {
            color_line,
            center_x,
            center_y,
            start_angle,
            end_angle,
        });
    }

    // SAFETY: HarfBuzz supplies live `image` and optional `extents` pointers plus the unique
    // live paint vector during this synchronous callback; no borrowed pointer is retained.
    unsafe extern "C" fn paint_image(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        image: *mut hb_blob_t,
        width: ::std::os::raw::c_uint,
        height: ::std::os::raw::c_uint,
        format: hb_tag_t,
        slant: f32,
        extents: *mut hb_glyph_extents_t,
        _user_data: *mut ::std::os::raw::c_void,
    ) -> hb_bool_t {
        if format != IS_PNG && format != IS_BGRA {
            // When: `format != IS_PNG && format != IS_BGRA`, reject unsupported image data.
            // We only support PNG and BGRA
            return 0;
        }

        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        let image =
            // SAFETY: HarfBuzz supplies a live blob for the duration of this
            // callback; `with_reference` takes the owned share retained by the op.
            unsafe { Blob::with_reference(image) };
        let extents = if extents.is_null() {
            None
        } else {
            // When: `extents.is_null()` is false, copy the initialized callback record.
            Some(
                // SAFETY: HarfBuzz supplies a readable `hb_glyph_extents_t` for this callback.
                unsafe { *extents },
            )
        };
        ops.push(Self::PaintImage { image, extents, width, height, format, slant });

        1
    }

    // SAFETY: HarfBuzz invokes this with the unique live paint vector pointer supplied to
    // the active synchronous paint call; it is not retained.
    unsafe extern "C" fn push_group(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PushGroup);
    }

    // SAFETY: HarfBuzz invokes this with the unique live paint vector pointer supplied to
    // the active synchronous paint call; it is not retained.
    unsafe extern "C" fn pop_group(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        mode: hb_paint_composite_mode_t,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PopGroup { mode });
    }

    // SAFETY: HarfBuzz supplies a live font and the unique live paint vector for this
    // synchronous callback; nested draw callbacks complete before either pointer expires.
    unsafe extern "C" fn push_clip_glyph(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        glyph: hb_codepoint_t,
        font: *mut hb_font_t,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };

        let mut draw = vec![];

        let funcs = DrawFuncs::new().unwrap();
        macro_rules! func {
            ($hbfunc:ident, $method:ident) => {
                // SAFETY: `funcs.funcs` is live and each callback has HarfBuzz's required
                // ABI; no borrowed Rust user data is registered.
                unsafe {
                    $hbfunc(funcs.funcs, Some(DrawOp::$method), std::ptr::null_mut(), None);
                }
            };
        }
        func!(hb_draw_funcs_set_move_to_func, move_to);
        func!(hb_draw_funcs_set_line_to_func, line_to);
        func!(hb_draw_funcs_set_quadratic_to_func, quad_to);
        func!(hb_draw_funcs_set_cubic_to_func, cubic_to);
        func!(hb_draw_funcs_set_close_path_func, close_path);

        // SAFETY: `font` and `funcs.funcs` are live; `&mut draw` remains uniquely valid for
        // every synchronous callback and is not retained after drawing returns.
        unsafe {
            hb_font_draw_glyph(font, glyph, funcs.funcs, &mut draw as *mut Vec<DrawOp> as *mut _);
        }

        ops.push(Self::PushGlyphClip { glyph, draw });
    }
}

impl DrawOp {
    // SAFETY: `data` must be the unique live `Vec<DrawOp>` pointer supplied to the active
    // synchronous HarfBuzz draw call and remain valid for the returned borrow.
    unsafe fn draw_data(data: *mut ::std::os::raw::c_void) -> &'static mut Vec<DrawOp> {
        // SAFETY: the function contract guarantees the pointer's type, unique access, and lifetime.
        unsafe { &mut *(data as *mut Vec<DrawOp>) }
    }

    // SAFETY: HarfBuzz invokes this with the unique live draw vector supplied to the
    // active synchronous draw call; the pointer is not retained.
    unsafe extern "C" fn move_to(
        _dfuncs: *mut hb_draw_funcs_t,
        draw_data: *mut ::std::os::raw::c_void,
        _st: *mut hb_draw_state_t,
        to_x: f32,
        to_y: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live draw vector pointer.
            unsafe { Self::draw_data(draw_data) };
        ops.push(Self::MoveTo { to_x, to_y });
    }

    // SAFETY: HarfBuzz invokes this with the unique live draw vector supplied to the
    // active synchronous draw call; the pointer is not retained.
    unsafe extern "C" fn line_to(
        _dfuncs: *mut hb_draw_funcs_t,
        draw_data: *mut ::std::os::raw::c_void,
        _st: *mut hb_draw_state_t,
        to_x: f32,
        to_y: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live draw vector pointer.
            unsafe { Self::draw_data(draw_data) };
        ops.push(Self::LineTo { to_x, to_y });
    }

    // SAFETY: HarfBuzz invokes this with the unique live draw vector supplied to the
    // active synchronous draw call; the pointer is not retained.
    unsafe extern "C" fn quad_to(
        _dfuncs: *mut hb_draw_funcs_t,
        draw_data: *mut ::std::os::raw::c_void,
        _st: *mut hb_draw_state_t,
        control_x: f32,
        control_y: f32,
        to_x: f32,
        to_y: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live draw vector pointer.
            unsafe { Self::draw_data(draw_data) };
        ops.push(Self::QuadTo { control_x, control_y, to_x, to_y });
    }

    // SAFETY: HarfBuzz invokes this with the unique live draw vector supplied to the
    // active synchronous draw call; the pointer is not retained.
    unsafe extern "C" fn cubic_to(
        _dfuncs: *mut hb_draw_funcs_t,
        draw_data: *mut ::std::os::raw::c_void,
        _st: *mut hb_draw_state_t,
        control1_x: f32,
        control1_y: f32,
        control2_x: f32,
        control2_y: f32,
        to_x: f32,
        to_y: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live draw vector pointer.
            unsafe { Self::draw_data(draw_data) };
        ops.push(Self::CubicTo { control1_x, control1_y, control2_x, control2_y, to_x, to_y });
    }

    // SAFETY: HarfBuzz invokes this with the unique live draw vector supplied to the
    // active synchronous draw call; the pointer is not retained.
    unsafe extern "C" fn close_path(
        _dfuncs: *mut hb_draw_funcs_t,
        draw_data: *mut ::std::os::raw::c_void,
        _st: *mut hb_draw_state_t,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live draw vector pointer.
            unsafe { Self::draw_data(draw_data) };
        ops.push(Self::ClosePath);
    }
}

impl ColorLine {
    /// # Safety
    ///
    /// `line` must point to a valid HarfBuzz color line for the duration of this call.
    // SAFETY: `line` is a live HarfBuzz color-line pointer for all synchronous queries;
    // each output buffer supplies the initialized element capacity declared to HarfBuzz.
    pub unsafe fn new_from_hb(line: *mut hb_color_line_t) -> Self {
        let num_stops =
            // SAFETY: the caller guarantees `line` is live; null outputs request only the count.
            unsafe {
            hb_color_line_get_color_stops(line, 0, std::ptr::null_mut(), std::ptr::null_mut())
        };
        let mut color_stops = Vec::with_capacity(num_stops as usize);
        color_stops
            .resize(num_stops as usize, hb_color_stop_t { offset: 0., is_foreground: 0, color: 0 });

        // SAFETY: `line` is live and `color_stops.as_mut_ptr()` provides `num_stops`
        // writable records, with `count` initialized to that capacity.
        unsafe {
            let mut count = num_stops;
            hb_color_line_get_color_stops(line, 0, &mut count, color_stops.as_mut_ptr());
        }

        let extend =
            // SAFETY: the caller guarantees `line` remains live through this query.
            unsafe { hb_color_line_get_extend(line) };

        Self {
            color_stops: color_stops
                .into_iter()
                .map(|stop| ColorStop {
                    offset: stop.offset.into(),
                    color: if stop.is_foreground != 0 {
                        SrgbaPixel::rgba(0xff, 0xff, 0xff, 0xff)
                    } else {
                        // When: `stop.is_foreground != 0` is false, use the stop's packed color.
                        hb_color_to_srgba_pixel(stop.color)
                    },
                })
                .collect(),
            extend: hb_extend_to_cairo(extend),
        }
    }
}

fn hb_color_to_srgba_pixel(color: hb_color_t) -> SrgbaPixel {
    let red =
        // SAFETY: `color` is a by-value packed HarfBuzz color with no pointer lifetime.
        unsafe { hb_color_get_red(color) };
    let green =
        // SAFETY: `color` is a by-value packed HarfBuzz color with no pointer lifetime.
        unsafe { hb_color_get_green(color) };
    let blue =
        // SAFETY: `color` is a by-value packed HarfBuzz color with no pointer lifetime.
        unsafe { hb_color_get_blue(color) };
    let alpha =
        // SAFETY: `color` is a by-value packed HarfBuzz color with no pointer lifetime.
        unsafe { hb_color_get_alpha(color) };
    SrgbaPixel::rgba(red, green, blue, alpha)
}

fn hb_extend_to_cairo(extend: hb_paint_extend_t) -> Extend {
    match extend {
        hb_paint_extend_t::HB_PAINT_EXTEND_PAD => Extend::Pad,
        hb_paint_extend_t::HB_PAINT_EXTEND_REPEAT => Extend::Repeat,
        hb_paint_extend_t::HB_PAINT_EXTEND_REFLECT => Extend::Reflect,
    }
}
