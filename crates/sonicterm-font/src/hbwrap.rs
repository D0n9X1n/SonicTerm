//! Higher level harfbuzz bindings

pub use harfbuzz::*;

use crate::color::SrgbaPixel;
use crate::locator::{FontDataHandle, FontDataSource};
use crate::rasterizer::colr::{ColorLine, ColorStop, DrawOp};
use anyhow::{ensure, Context, Error};
use cairo::Extend;
use std::ffi::CStr;
use std::io::Read;
use std::mem;
use std::ops::Range;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::Arc;

extern "C" {
    fn hb_ft_font_set_load_flags(font: *mut hb_font_t, load_flags: i32);
}

pub const IS_PNG: hb_tag_t = hb_tag(b'p', b'n', b'g', b' ');
#[allow(unused)]
pub const IS_SVG: hb_tag_t = hb_tag(b's', b'v', b'g', b' ');
pub const IS_BGRA: hb_tag_t = hb_tag(b'B', b'G', b'R', b'A');

fn checked_blob_len(len: usize) -> anyhow::Result<c_uint> {
    c_uint::try_from(len).context("font data exceeds HarfBuzz blob length")
}

pub fn language_from_string(s: &str) -> Result<hb_language_t, Error> {
    // SAFETY: `s.as_ptr()` names `s.len()` readable bytes for this synchronous call;
    // HarfBuzz uses the explicit length and does not retain the pointer.
    unsafe {
        let lang = hb_language_from_string(s.as_ptr() as *const c_char, s.len() as i32);
        ensure!(!lang.is_null(), "failed to convert {} to language", s);
        Ok(lang)
    }
}

pub fn feature_from_string(s: &str) -> Result<hb_feature_t, Error> {
    // SAFETY: `s.as_ptr()` names `s.len()` readable bytes and `feature` is writable output
    // storage that HarfBuzz initializes before a successful return.
    unsafe {
        let mut feature = mem::zeroed();
        ensure!(
            hb_feature_from_string(
                s.as_ptr() as *const c_char,
                s.len() as i32,
                &mut feature as *mut _,
            ) != 0,
            "failed to create feature from {}",
            s
        );
        Ok(feature)
    }
}

#[derive(Debug)]
pub struct Blob {
    blob: *mut hb_blob_t,
}

// Lifecycle: `Blob` releases its owned HarfBuzz reference with `hb_blob_destroy` once.
impl Drop for Blob {
    fn drop(&mut self) {
        // SAFETY: `self.blob` is the live owned reference held by this wrapper.
        unsafe {
            hb_blob_destroy(self.blob);
        }
    }
}

impl Clone for Blob {
    fn clone(&self) -> Self {
        // SAFETY: `self.blob` is live; incrementing its HarfBuzz reference count creates
        // the independent owned reference stored in the clone.
        unsafe { hb_blob_reference(self.blob) };
        Self { blob: self.blob }
    }
}

impl Blob {
    /// Take an owned reference to an existing HarfBuzz blob.
    ///
    /// # Safety
    ///
    /// `blob` must point to a live `hb_blob_t` for the duration of this call.
    // SAFETY: callers must provide a live `blob` for `hb_blob_reference`; the
    // returned `Blob` owns the reference later passed to `hb_blob_destroy`.
    pub unsafe fn with_reference(blob: *mut hb_blob_t) -> Self {
        // SAFETY: the caller guarantees a live blob; HarfBuzz increments its
        // reference count and returns the same owned object.
        unsafe { hb_blob_reference(blob) };
        Self { blob }
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `self.blob` is live; HarfBuzz returns `len` readable blob-owned bytes
        // whose lifetime lasts until the blob is mutated or destroyed, and this borrow prevents both.
        unsafe {
            let mut len = 0;
            let ptr = hb_blob_get_data(self.blob, &mut len);
            from_raw_parts(ptr as *const u8, len as usize)
        }
    }

    pub fn from_source(source: &FontDataSource) -> anyhow::Result<Self> {
        let blob = match source {
            FontDataSource::OnDisk(p) => {
                // When: `source` is `OnDisk`, load the blob from the named font file.
                let mut file = std::fs::File::open(p)
                    .with_context(|| format!("opening file {}", p.display()))?;

                let meta = file
                    .metadata()
                    .with_context(|| format!("querying metadata for {}", p.display()))?;

                if !meta.is_file() {
                    anyhow::bail!("{} is not a file", p.display());
                }

                let len = meta.len();
                if len as usize > c_uint::MAX as usize {
                    anyhow::bail!(
                        "{} is too large to pass to harfbuzz! (len={})",
                        p.display(),
                        len
                    );
                }

                let mut data = vec![];
                file.read_to_end(&mut data)
                    .with_context(|| format!("reading font file {}", p.display()))?;
                let data_len = checked_blob_len(data.len()).with_context(|| {
                    format!("font file grew too large while reading {}", p.display())
                })?;
                let data = Arc::new(data);

                let data_ptr = data.as_ptr();
                let user_data: *const Vec<u8> = Arc::into_raw(data);

                extern "C" fn release_arc_vec(user_data: *mut c_void) {
                    let user_data = user_data as *mut Vec<u8>;
                    let user_data: Arc<Vec<u8>> =
                        // SAFETY: HarfBuzz passes the unreclaimed pointer produced by the
                        // matching `Arc::into_raw`, and invokes this callback at most once.
                        unsafe { Arc::from_raw(user_data) };
                    drop(user_data);
                }

                // SAFETY: `data_ptr` names `data_len` readable owned bytes; the raw Arc keeps
                // them alive until HarfBuzz invokes the callback.
                unsafe {
                    hb_blob_create_or_fail(
                        data_ptr as *const _,
                        data_len,
                        hb_memory_mode_t::HB_MEMORY_MODE_READONLY,
                        user_data as *mut _,
                        Some(release_arc_vec),
                    )
                }
            }
            FontDataSource::BuiltIn { data, .. } => {
                // When: `source` is `BuiltIn`, borrow its static bytes without a release callback.
                let data_len = checked_blob_len(data.len())?;
                // SAFETY: `data` is static and names `data_len` readable bytes, so it outlives
                // the HarfBuzz blob even though no user-data owner or destroy callback is supplied.
                unsafe {
                    hb_blob_create_or_fail(
                        data.as_ptr() as *const _,
                        data_len,
                        hb_memory_mode_t::HB_MEMORY_MODE_READONLY,
                        std::ptr::null_mut(),
                        None,
                    )
                }
            }
            FontDataSource::Memory { data, .. } => {
                // When: `source` is `Memory`, retain an Arc for the HarfBuzz blob lifetime.
                let data_ptr = data.as_ptr();
                let data_len = checked_blob_len(data.len())?;
                let user_data: *const Box<[u8]> = Arc::into_raw(Arc::clone(data));

                extern "C" fn release_arc(user_data: *mut c_void) {
                    let user_data = user_data as *const Box<[u8]>;
                    let user_data: Arc<Box<[u8]>> =
                        // SAFETY: HarfBuzz passes the unreclaimed pointer produced by the
                        // matching `Arc::into_raw`, and invokes this callback at most once.
                        unsafe { Arc::from_raw(user_data) };
                    drop(user_data);
                }

                // SAFETY: `data_ptr` names `data_len` readable boxed bytes; the raw Arc keeps
                // them alive until HarfBuzz invokes the callback.
                unsafe {
                    hb_blob_create_or_fail(
                        data_ptr as *const _,
                        data_len,
                        hb_memory_mode_t::HB_MEMORY_MODE_READONLY,
                        user_data as *mut _,
                        Some(release_arc),
                    )
                }
            }
        };

        if blob.is_null() {
            anyhow::bail!("failed to wrap font as blob");
        }

        Ok(Self { blob })
    }
}

pub struct Face {
    face: *mut hb_face_t,
}

// Lifecycle: `Face` releases its owned HarfBuzz reference with `hb_face_destroy` once.
impl Drop for Face {
    fn drop(&mut self) {
        // SAFETY: `self.face` is the live owned reference held by this wrapper.
        unsafe {
            hb_face_destroy(self.face);
        }
    }
}

impl Face {
    pub fn from_locator(handle: &FontDataHandle) -> anyhow::Result<Self> {
        let blob = Blob::from_source(&handle.source)?;
        let mut index = handle.index;
        if handle.variation != 0 {
            index |= handle.variation << 16;
        }

        let face =
            // SAFETY: `blob.blob` is live for the call; HarfBuzz takes its own reference and
            // returns a newly owned face for the selected index.
            unsafe { hb_face_create(blob.blob, index) };
        if face.is_null() {
            anyhow::bail!("failed to create harfbuzz Face");
        }

        Ok(Self { face })
    }

    #[allow(dead_code)]
    pub fn get_upem(&self) -> c_uint {
        // SAFETY: `self.face` is a live HarfBuzz face pointer.
        unsafe { hb_face_get_upem(self.face) }
    }
}

pub struct Font {
    font: *mut hb_font_t,
}

// Lifecycle: `Font` releases its owned HarfBuzz reference with `hb_font_destroy` once.
impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: `self.font` is the live owned reference held by this wrapper.
        unsafe {
            hb_font_destroy(self.font);
        }
    }
}

impl Font {
    /// Create a HarfBuzz font that retains an existing FreeType face.
    ///
    /// # Safety
    ///
    /// `face` must point to a live `FT_Face` that may be referenced by
    /// HarfBuzz during this call.
    // SAFETY: callers must provide a live `face` for
    // `hb_ft_font_create_referenced`; the returned `Font` owns that reference.
    pub unsafe fn new(face: freetype::FT_Face) -> Font {
        Font {
            font:
                // SAFETY: the caller guarantees a live FreeType face; HarfBuzz
                // takes its own reference for the returned font.
                unsafe { hb_ft_font_create_referenced(face as _) },
        }
    }

    pub fn from_locator(handle: &FontDataHandle) -> anyhow::Result<Self> {
        let face = Face::from_locator(handle)?;
        let font =
            // SAFETY: `face.face` is live for the call; HarfBuzz takes its own face reference and
            // returns a newly owned font pointer.
            unsafe { hb_font_create(face.face) };
        if font.is_null() {
            anyhow::bail!("failed to create harfbuzz Font");
        }
        Ok(Self { font })
    }

    #[allow(dead_code)]
    pub fn get_face(&self) -> Face {
        let face =
            // SAFETY: `self.font` is live; the returned face pointer is borrowed from it.
            unsafe { hb_font_get_face(self.font) };
        // SAFETY: `face` is live through `self.font`; incrementing its reference count creates
        // the owned reference returned in the `Face` wrapper.
        unsafe {
            hb_face_reference(face);
        }
        Face { face }
    }

    pub fn set_ot_funcs(&mut self) {
        // SAFETY: `self.font` is live and HarfBuzz mutates only its function table.
        unsafe {
            hb_ot_font_set_funcs(self.font);
        }
    }

    #[allow(dead_code)]
    pub fn set_ft_funcs(&mut self) {
        // SAFETY: `self.font` is live and backed by the referenced FreeType face.
        unsafe {
            hb_ft_font_set_funcs(self.font);
        }
    }

    pub fn set_synthetic_slant(&mut self, slant: f32) {
        // SAFETY: `self.font` is live; `slant` is copied into its synthetic-style state.
        unsafe {
            hb_font_set_synthetic_slant(self.font, slant);
        }
    }

    pub fn set_synthetic_bold(&mut self, x_embolden: f32, y_embolden: f32, in_place: bool) {
        // SAFETY: `self.font` is live and HarfBuzz copies all scalar style parameters.
        unsafe {
            hb_font_set_synthetic_bold(
                self.font,
                x_embolden,
                y_embolden,
                if in_place { 1 } else { 0 },
            );
        }
    }

    pub fn set_font_scale(&self, x_scale: c_int, y_scale: c_int) {
        // SAFETY: `self.font` is live and HarfBuzz copies both scalar scale values.
        unsafe {
            hb_font_set_scale(self.font, x_scale, y_scale);
        }
    }

    pub fn set_ppem(&self, x_ppem: u32, y_ppem: u32) {
        // SAFETY: `self.font` is live and HarfBuzz copies both scalar ppem values.
        unsafe {
            hb_font_set_ppem(self.font, x_ppem, y_ppem);
        }
    }

    pub fn set_ptem(&self, ptem: f32) {
        // SAFETY: `self.font` is live and HarfBuzz copies the scalar point size.
        unsafe {
            hb_font_set_ptem(self.font, ptem);
        }
    }

    pub fn font_changed(&mut self) {
        // SAFETY: `self.font` is live and retains its referenced FreeType face.
        unsafe {
            hb_ft_font_changed(self.font);
        }
    }

    pub fn set_load_flags(&mut self, load_flags: freetype::FT_Int32) {
        // SAFETY: `self.font` is live and the load flags are copied into its FreeType backend.
        unsafe {
            hb_ft_font_set_load_flags(self.font, load_flags);
        }
    }

    /// Perform shaping.  On entry, Buffer holds the text to shape.
    /// Once done, Buffer holds the output glyph and position info
    pub fn shape(&mut self, buf: &mut Buffer, features: &[hb_feature_t]) {
        // SAFETY: the font and buffer are live; `features.as_ptr()` names `features.len()`
        // initialized records for this synchronous shaping call.
        unsafe { hb_shape(self.font, buf.buf, features.as_ptr(), features.len() as u32) }
    }

    /// Fetches a list of the caret positions defined for a ligature glyph in the GDEF table of the
    /// font. The list returned will begin at the offset provided.
    /// Note that a ligature that is formed from n characters will have n-1 caret positions. The
    /// first character is not represented in the array, since its caret position is the glyph
    /// position.
    /// The positions returned by this function are 'unshaped', and will have to be fixed up for
    /// kerning that may be applied to the ligature glyp
    #[allow(dead_code)]
    pub fn get_ligature_carets(
        &self,
        direction: hb_direction_t,
        glyph_pos: u32,
    ) -> Vec<hb_position_t> {
        let mut positions = [0 as hb_position_t; 8];

        // SAFETY: `self.font` is live and each positions buffer supplies its declared
        // writable element count for the duration of the synchronous HarfBuzz calls.
        unsafe {
            let mut array_size = positions.len() as c_uint;
            let n_carets = hb_ot_layout_get_ligature_carets(
                self.font,
                direction,
                glyph_pos,
                0,
                &mut array_size,
                positions.as_mut_ptr(),
            ) as usize;

            if n_carets > positions.len() {
                // When: `n_carets > positions.len()`, allocate the complete reported result.
                let mut positions = vec![0 as hb_position_t; n_carets];
                array_size = positions.len() as c_uint;
                hb_ot_layout_get_ligature_carets(
                    self.font,
                    direction,
                    glyph_pos,
                    0,
                    &mut array_size,
                    positions.as_mut_ptr(),
                );

                return positions;
            }

            positions[..n_carets].to_vec()
        }
    }

    #[allow(unused)]
    pub fn draw_glyph(&self, glyph_pos: u32, funcs: &DrawFuncs, draw_data: *mut c_void) {
        // SAFETY: the font and callback table are live; callers keep `draw_data` valid for every
        // synchronous callback made by `hb_font_draw_glyph`.
        unsafe { hb_font_draw_glyph(self.font, glyph_pos, funcs.funcs, draw_data) }
    }

    #[allow(unused)]
    pub fn paint_glyph(
        &self,
        glyph_pos: u32,
        funcs: &FontFuncs,
        paint_data: *mut c_void,
        palette_index: ::std::os::raw::c_uint,
        foreground: hb_color_t,
    ) {
        // SAFETY: the font and callback table are live; callers keep `paint_data` valid for every
        // synchronous callback made by `hb_font_paint_glyph`.
        unsafe {
            hb_font_paint_glyph(
                self.font,
                glyph_pos,
                funcs.funcs,
                paint_data,
                palette_index,
                foreground,
            )
        }
    }

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
        xx: f32,
        yx: f32,
        xy: f32,
        yy: f32,
        dx: f32,
        dy: f32,
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
        xx: f32,
        yx: f32,
        xy: f32,
        yy: f32,
        dx: f32,
        dy: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        ops.push(Self::PushTransform { xx, yx, xy, yy, dx, dy });
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
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        let color_line =
            // SAFETY: HarfBuzz keeps `color_line` live for the callback; conversion copies it.
            unsafe { ColorLine::new_from_hb(color_line) };
        ops.push(Self::PaintLinearGradient { color_line, x0, y0, x1, y1, x2, y2 });
    }

    // SAFETY: HarfBuzz supplies a live color-line pointer and the unique live paint vector
    // during this synchronous callback; neither pointer is retained.
    unsafe extern "C" fn paint_radial_gradient(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        color_line: *mut hb_color_line_t,
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
        _user_data: *mut ::std::os::raw::c_void,
    ) {
        let ops =
            // SAFETY: this callback receives the unique live paint vector pointer.
            unsafe { Self::paint_data(paint_data) };
        let color_line =
            // SAFETY: HarfBuzz keeps `color_line` live for the callback; conversion copies it.
            unsafe { ColorLine::new_from_hb(color_line) };
        ops.push(Self::PaintRadialGradient { color_line, x0, y0, r0, x1, y1, r1 });
    }

    // SAFETY: HarfBuzz supplies a live color-line pointer and the unique live paint vector
    // during this synchronous callback; neither pointer is retained.
    unsafe extern "C" fn paint_sweep_gradient(
        _funcs: *mut hb_paint_funcs_t,
        paint_data: *mut ::std::os::raw::c_void,
        color_line: *mut hb_color_line_t,
        x0: f32,
        y0: f32,
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
        ops.push(Self::PaintSweepGradient { color_line, x0, y0, start_angle, end_angle });
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

pub struct Buffer {
    buf: *mut hb_buffer_t,
}

// Lifecycle: `Buffer` releases its owned HarfBuzz buffer with `hb_buffer_destroy` once.
impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: `self.buf` is the live owned buffer held by this wrapper.
        unsafe {
            hb_buffer_destroy(self.buf);
        }
    }
}

#[allow(dead_code)]
extern "C" fn log_buffer_message(
    _buf: *mut hb_buffer_t,
    _font: *mut hb_font_t,
    message: *const c_char,
    _user_data: *mut c_void,
) -> i32 {
    // SAFETY: HarfBuzz supplies either null or a callback-scoped NUL-terminated message pointer.
    unsafe {
        if !message.is_null() {
            let message = CStr::from_ptr(message);
            log::info!("{message:?}");
        }
    }

    1
}

impl Buffer {
    /// Create a new buffer
    pub fn new() -> Result<Buffer, Error> {
        let buf =
            // SAFETY: HarfBuzz returns an owned buffer pointer or its inert failure object.
            unsafe { hb_buffer_create() };
        ensure!(
            // SAFETY: `buf` is the pointer returned by `hb_buffer_create` and remains live.
            unsafe { hb_buffer_allocation_successful(buf) } != 0,
            "hb_buffer_create failed"
        );
        // SAFETY: allocation succeeded, so `buf` is live and accepts content-type mutation.
        unsafe {
            hb_buffer_set_content_type(
                buf,
                harfbuzz::hb_buffer_content_type_t::HB_BUFFER_CONTENT_TYPE_UNICODE,
            );

            // hb_buffer_set_message_func(buf, Some(log_buffer_message), std::ptr::null_mut(), None);
        };
        Ok(Buffer { buf })
    }

    /// Reset the buffer back to its initial post-creation state
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        // SAFETY: `self.buf` is a live HarfBuzz buffer.
        unsafe {
            hb_buffer_reset(self.buf);
        }
    }

    pub fn set_cluster_level(&mut self, level: hb_buffer_cluster_level_t) {
        // SAFETY: `self.buf` is live and `level` is a HarfBuzz enum value copied by the call.
        unsafe {
            hb_buffer_set_cluster_level(self.buf, level);
        }
    }

    pub fn set_direction(&mut self, direction: hb_direction_t) {
        // SAFETY: `self.buf` is live and `direction` is copied into its segment properties.
        unsafe {
            hb_buffer_set_direction(self.buf, direction);
        }
    }

    #[allow(dead_code)]
    pub fn set_script(&mut self, script: hb_script_t) {
        // SAFETY: `self.buf` is live and `script` is copied into its segment properties.
        unsafe {
            hb_buffer_set_script(self.buf, script);
        }
    }

    pub fn set_language(&mut self, lang: hb_language_t) {
        // SAFETY: `self.buf` is live and `lang` is HarfBuzz-managed interned language storage.
        unsafe {
            hb_buffer_set_language(self.buf, lang);
        }
    }

    #[allow(dead_code)]
    pub fn add(&mut self, codepoint: hb_codepoint_t, cluster: u32) {
        // SAFETY: `self.buf` is live and both scalar values are copied into the buffer.
        unsafe {
            hb_buffer_add(self.buf, codepoint, cluster);
        }
    }

    #[allow(dead_code)]
    pub fn reverse(&mut self) {
        // SAFETY: `self.buf` is live and HarfBuzz mutates only its owned contents.
        unsafe {
            hb_buffer_reverse_clusters(self.buf);
        }
    }

    pub fn add_str(&mut self, paragraph: &str, range: Range<usize>) {
        let bytes = paragraph.as_bytes();
        // SAFETY: `bytes.as_ptr()` names `bytes.len()` readable bytes; `range` supplies the
        // requested subrange, and HarfBuzz copies the text during this call.
        unsafe {
            hb_buffer_add_utf8(
                self.buf,
                bytes.as_ptr() as *const c_char,
                bytes.len() as i32,
                range.start as u32,
                (range.end - range.start) as i32,
            );
        }
    }

    /// Returns glyph information.  This is only valid after calling
    /// font->shape() on this buffer instance.
    pub fn glyph_infos(&self) -> &[hb_glyph_info_t] {
        // SAFETY: `self.buf` is live; HarfBuzz returns `len` initialized buffer-owned records
        // valid until the buffer is mutated, which this shared borrow prevents.
        unsafe {
            let mut len: u32 = 0;
            let info = hb_buffer_get_glyph_infos(self.buf, &mut len as *mut _);
            from_raw_parts(info, len as usize)
        }
    }

    /// Returns glyph positions.  This is only valid after calling
    /// font->shape() on this buffer instance.
    pub fn glyph_positions(&self) -> &[hb_glyph_position_t] {
        // SAFETY: `self.buf` is live; HarfBuzz returns `len` initialized buffer-owned records
        // valid until the buffer is mutated, which this shared borrow prevents.
        unsafe {
            let mut len: u32 = 0;
            let pos = hb_buffer_get_glyph_positions(self.buf, &mut len as *mut _);
            from_raw_parts(pos, len as usize)
        }
    }

    #[allow(dead_code)]
    pub fn serialize(&self, font: Option<&Font>) -> String {
        // SAFETY: the buffer and optional font are live; `text` supplies `buf_len` writable bytes
        // and HarfBuzz initializes at most the returned `text_len` prefix synchronously.
        unsafe {
            let len = hb_buffer_get_length(self.buf);
            let mut text = vec![0u8; len as usize * 16];
            let buf_len = text.len();
            let mut text_len = 0;
            hb_buffer_serialize(
                self.buf,
                0,
                len,
                text.as_mut_ptr() as *mut _,
                buf_len as _,
                &mut text_len,
                match font {
                    Some(f) => f.font,
                    None => std::ptr::null_mut(),
                },
                harfbuzz::hb_buffer_serialize_format_t::HB_BUFFER_SERIALIZE_FORMAT_TEXT,
                harfbuzz::hb_buffer_serialize_flags_t::HB_BUFFER_SERIALIZE_FLAG_DEFAULT,
            );
            String::from_utf8_lossy(&text[0..text_len as usize]).into()
        }
    }

    pub fn guess_segment_properties(&mut self) {
        // SAFETY: `self.buf` is live and HarfBuzz mutates only its segment properties.
        unsafe {
            hb_buffer_guess_segment_properties(self.buf);
        }
    }
}

fn owned_callback_table<T>(table: *mut T, empty: *mut T, name: &str) -> anyhow::Result<*mut T> {
    anyhow::ensure!(!table.is_null() && table != empty, "{name} failed");
    Ok(table)
}

pub struct FontFuncs {
    funcs: *mut hb_paint_funcs_t,
}

// Lifecycle: `FontFuncs` releases its owned callback table with `hb_paint_funcs_destroy` once.
impl Drop for FontFuncs {
    fn drop(&mut self) {
        // SAFETY: `self.funcs` is the live owned callback table held by this wrapper.
        unsafe {
            hb_paint_funcs_destroy(self.funcs);
        }
    }
}

impl FontFuncs {
    pub fn new() -> anyhow::Result<Self> {
        let funcs =
            // SAFETY: this constructor takes no pointers and returns either an
            // owned mutable table or HarfBuzz's immutable empty singleton.
            unsafe { hb_paint_funcs_create() };
        let empty =
            // SAFETY: HarfBuzz returns its process-lifetime empty singleton.
            unsafe { hb_paint_funcs_get_empty() };
        let funcs = owned_callback_table(funcs, empty, "hb_paint_funcs_create")?;
        Ok(Self { funcs })
    }
}

pub struct DrawFuncs {
    funcs: *mut hb_draw_funcs_t,
}

// Lifecycle: `DrawFuncs` releases its owned callback table with `hb_draw_funcs_destroy` once.
impl Drop for DrawFuncs {
    fn drop(&mut self) {
        // SAFETY: `self.funcs` is the live owned callback table held by this wrapper.
        unsafe {
            hb_draw_funcs_destroy(self.funcs);
        }
    }
}

impl DrawFuncs {
    pub fn new() -> anyhow::Result<Self> {
        let funcs =
            // SAFETY: this constructor takes no pointers and returns either an
            // owned mutable table or HarfBuzz's immutable empty singleton.
            unsafe { hb_draw_funcs_create() };
        let empty =
            // SAFETY: HarfBuzz returns its process-lifetime empty singleton.
            unsafe { hb_draw_funcs_get_empty() };
        let funcs = owned_callback_table(funcs, empty, "hb_draw_funcs_create")?;
        Ok(Self { funcs })
    }
}

pub struct TagString([u8; 4]);

impl std::convert::AsRef<str> for TagString {
    fn as_ref(&self) -> &str {
        std::str::from_utf8(&self.0).expect("tag to be valid ascii")
    }
}

impl std::ops::Deref for TagString {
    type Target = str;
    fn deref(&self) -> &str {
        std::str::from_utf8(&self.0).expect("tag to be valid ascii")
    }
}

impl std::fmt::Display for TagString {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.as_ref().fmt(fmt)
    }
}

pub const fn hb_tag(c1: u8, c2: u8, c3: u8, c4: u8) -> hb_tag_t {
    ((c1 as u32) << 24) | ((c2 as u32) << 16) | ((c3 as u32) << 8) | (c4 as u32)
}

pub fn hb_color(b: u8, g: u8, r: u8, a: u8) -> hb_tag_t {
    hb_tag(b, g, r, a)
}

pub fn hb_tag_to_string(tag: hb_tag_t) -> TagString {
    let mut buf = [0u8; 4];

    // SAFETY: `buf` provides exactly four writable bytes, which is the fixed output
    // layout required by `hb_tag_to_string`; HarfBuzz retains no pointer.
    unsafe {
        harfbuzz::hb_tag_to_string(tag, &mut buf as *mut u8 as *mut c_char);
    }
    TagString(buf)
}

/// Wrapper around std::slice::from_raw_parts that allows for ptr to be
/// null. In the null ptr case, an empty slice is returned.
/// This is necessary because harfbuzz may sometimes encode
/// empty arrays in that way, and rust 1.78 will panic if a null
/// ptr is passed in.
// SAFETY: for non-null `ptr`, callers provide `size` initialized contiguous `T` values
// that remain readable and unmutated for the returned lifetime; null represents an empty array.
pub(crate) unsafe fn from_raw_parts<'a, T>(ptr: *const T, size: usize) -> &'a [T] {
    if ptr.is_null() {
        &[]
    } else {
        // When: `ptr.is_null()` is false, expose the caller-guaranteed initialized elements.
        // SAFETY: the function contract guarantees `size` readable contiguous elements whose
        // storage remains valid and unmutated for the returned lifetime.
        unsafe { std::slice::from_raw_parts(ptr, size) }
    }
}

#[cfg(test)]
#[path = "hbwrap_tests.rs"]
mod hbwrap_tests;
