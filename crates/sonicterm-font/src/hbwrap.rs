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

pub fn language_from_string(text: &str) -> Result<hb_language_t, Error> {
    // SAFETY: `text.as_ptr()` names `text.len()` readable bytes for this synchronous call;
    // HarfBuzz uses the explicit length and does not retain the pointer.
    unsafe {
        let lang = hb_language_from_string(text.as_ptr() as *const c_char, text.len() as i32);
        ensure!(!lang.is_null(), "failed to convert {} to language", text);
        Ok(lang)
    }
}

pub fn feature_from_string(text: &str) -> Result<hb_feature_t, Error> {
    // SAFETY: `text.as_ptr()` names `text.len()` readable bytes and `feature` is writable output
    // storage that HarfBuzz initializes before a successful return.
    unsafe {
        let mut feature = mem::zeroed();
        ensure!(
            hb_feature_from_string(
                text.as_ptr() as *const c_char,
                text.len() as i32,
                &mut feature as *mut _,
            ) != 0,
            "failed to create feature from {}",
            text
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

    /// Wrap the font bytes of face `face_index` of `source` as a read-only blob.
    ///
    /// An on-disk face that lists an `SVG ` or `COLR` table wraps the file's shared buffer from
    /// [`crate::font_bytes`]; any other on-disk face reads the file into a blob of its own.
    pub fn from_source(source: &FontDataSource, face_index: u32) -> anyhow::Result<Self> {
        let source = crate::font_bytes::pin_colour_face(source, face_index);
        let blob = match source.as_ref() {
            FontDataSource::OnDisk(path) => {
                // When: `source` is `OnDisk`, load the blob from the named font file.
                let mut file = std::fs::File::open(path)
                    .with_context(|| format!("opening file {}", path.display()))?;

                let meta = file
                    .metadata()
                    .with_context(|| format!("querying metadata for {}", path.display()))?;

                if !meta.is_file() {
                    anyhow::bail!("{} is not a file", path.display());
                }

                let len = meta.len();
                if len as usize > c_uint::MAX as usize {
                    anyhow::bail!(
                        "{} is too large to pass to harfbuzz! (len={})",
                        path.display(),
                        len
                    );
                }

                let mut data = vec![];
                file.read_to_end(&mut data)
                    .with_context(|| format!("reading font file {}", path.display()))?;
                let data_len = checked_blob_len(data.len()).with_context(|| {
                    format!("font file grew too large while reading {}", path.display())
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
        let mut index = handle.index;
        if handle.variation != 0 {
            index |= handle.variation << 16;
        }
        let blob = Blob::from_source(&handle.source, index)?;

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
                    Some(font) => font.font,
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

pub const fn hb_tag(first: u8, second: u8, third: u8, fourth: u8) -> hb_tag_t {
    ((first as u32) << 24) | ((second as u32) << 16) | ((third as u32) << 8) | (fourth as u32)
}

pub fn hb_color(blue: u8, green: u8, red: u8, alpha: u8) -> hb_tag_t {
    hb_tag(blue, green, red, alpha)
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

#[path = "hbwrap_paint.rs"]
mod paint;

pub use paint::PaintOp;
