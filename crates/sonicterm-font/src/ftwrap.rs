//! Higher level freetype bindings

use crate::locator::{FontDataHandle, FontDataSource};
use crate::parser::ParsedFont;
use crate::rangeset::RangeSet;
use crate::rasterizer::colr::DrawOp;
use anyhow::{anyhow, Context};
use config::{configuration, FreeTypeLoadFlags, FreeTypeLoadTarget};
pub use freetype::*;
use std::collections::HashMap;
use std::convert::TryInto;
use std::ffi::{c_int, c_void, CStr};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::mem::MaybeUninit;
use std::os::raw::{c_long, c_uchar, c_ulong};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

/// Reports whether a FreeType status code represents success.
#[inline]
pub fn succeeded(error: FT_Error) -> bool {
    error == freetype::FT_Err_Ok as FT_Error
}

#[cfg(any(windows, target_pointer_width = "32"))]
fn freetype_pos_as_i64(pos: c_long) -> i64 {
    i64::from(pos)
}

#[cfg(all(not(windows), target_pointer_width = "64"))]
fn freetype_pos_as_i64(pos: c_long) -> i64 {
    pos
}

/// Translate an error and value into a result
pub fn ft_result<T>(err: FT_Error, value: T) -> anyhow::Result<T> {
    if succeeded(err) {
        Ok(value)
    } else {
        // When: `succeeded(err)` is false, convert the FreeType failure into Rust context.
        // SAFETY: `FT_Error_String` returns either null or a borrowed NUL-terminated
        // error string whose storage is managed by FreeType and remains valid here.
        unsafe {
            let reason = FT_Error_String(err);
            if reason.is_null() {
                Err(anyhow!("FreeType error {:?} 0x{:x}", err, err))
            } else {
                // When: `reason.is_null()` is false, include FreeType's NUL-terminated description.
                let reason = std::ffi::CStr::from_ptr(reason);
                Err(anyhow!("FreeType error {:?} 0x{:x}: {}", err, err, reason.to_string_lossy()))
            }
        }
    }
}

fn render_mode_to_load_target(render_mode: FT_Render_Mode) -> u32 {
    // enable FT_LOAD_TARGET bits.  There are no flags defined
    // for these in the bindings so we do some bit magic for
    // ourselves.  This is how the FT_LOAD_TARGET_() macro
    // assembles these bits.
    ((render_mode as u32) & 15) << 16
}

fn checked_bitmap_buffer_len(
    buffer: *mut c_uchar,
    rows: u32,
    pitch: usize,
) -> anyhow::Result<usize> {
    let len = rows as usize * pitch;
    anyhow::ensure!(len > 0, "rendered glyph bitmap is empty");
    anyhow::ensure!(!buffer.is_null(), "rendered glyph bitmap has a null buffer");
    Ok(len)
}

fn checked_palette_storage<T>(ptr: *const T, count: FT_UShort) -> anyhow::Result<usize> {
    anyhow::ensure!(count > 0, "palette storage is empty");
    anyhow::ensure!(!ptr.is_null(), "palette storage is null");
    Ok(count as usize)
}

/// Converts `data` with labels resolved by name ID from the records `records` reads. `records` is called at
/// most once, and only when a palette or entry label other than 0xFFFF is present, so a CPAL table without
/// labels never decodes the name table.
// SAFETY: each non-null array in `data` must hold its count of initialized elements and stay
// valid and unmutated for the call: `num_palettes` for names and flags, entries for entry labels.
unsafe fn palette_info_from(
    data: &FT_Palette_Data,
    records: impl FnOnce() -> Vec<NameRecord>,
) -> PaletteInfo {
    let labels = crate::parser::NameIdLabels::lazy(records);
    // SAFETY: the caller's guarantee for `data` is exactly `palettes_from_data`'s.
    unsafe { palettes_from_data(data, |name_id| labels.label(name_id)) }
}

/// Builds one [`Palette`] per CPAL palette from FreeType's palette data, resolving labels with
/// `name_of`.
///
/// FreeType documents `palette_name_ids`, `palette_flags` and `palette_entry_name_ids` as optional:
/// a version-0 CPAL table, or a version-1 table without a given offset, leaves that array null.
/// An absent label reads as an empty name, absent flags as 0, and absent entry labels as no entry
/// names; none of them stops the face's color glyphs from rendering.
// SAFETY: each non-null array in `data` must hold its count of initialized elements and stay
// valid and unmutated for the call: `num_palettes` for names and flags, entries for entry labels.
unsafe fn palettes_from_data(
    data: &FT_Palette_Data,
    mut name_of: impl FnMut(FT_UShort) -> String,
) -> PaletteInfo {
    let palette_count = usize::from(data.num_palettes);
    let (name_ids, flags, entry_name_ids) =
        // SAFETY: the caller guarantees each non-null array's extent; `from_raw_parts` maps null to empty.
        unsafe {
            (
                from_raw_parts(data.palette_name_ids, palette_count),
                from_raw_parts(data.palette_flags, palette_count),
                from_raw_parts(data.palette_entry_name_ids, usize::from(data.num_palette_entries)),
            )
        };
    let entry_names: Vec<String> = entry_name_ids.iter().map(|&id| name_of(id)).collect();
    let palettes = (0..palette_count)
        .map(|palette_index| Palette {
            palette_index,
            flags: flags.get(palette_index).copied().unwrap_or(0),
            // An absent label array has no name ID, so nothing is looked up (never name ID 0).
            name: name_ids.get(palette_index).map(|&id| name_of(id)).unwrap_or_default(),
            entry_names: entry_names.clone(),
        })
        .collect();
    PaletteInfo { num_palettes: palette_count, palettes }
}

struct MmVarGuard {
    library: FT_Library,
    ptr: *mut FT_MM_Var,
}

impl MmVarGuard {
    fn from_success(library: FT_Library, ptr: *mut FT_MM_Var) -> anyhow::Result<Self> {
        anyhow::ensure!(!ptr.is_null(), "FT_Get_MM_Var returned null on success");
        Ok(Self { library, ptr })
    }

    fn get(&self) -> &FT_MM_Var {
        // SAFETY: `from_success` rejected null and this guard owns the allocation until Drop.
        unsafe { &*self.ptr }
    }
}

// Lifecycle: `MmVarGuard` releases its `ptr` with `FT_Done_MM_Var` exactly once.
impl Drop for MmVarGuard {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from successful `FT_Get_MM_Var` for this live `library`.
        unsafe {
            FT_Done_MM_Var(self.library, self.ptr);
        }
    }
}

/// Resolves configured FreeType load flags and the render mode for a glyph request.
pub fn compute_load_flags_from_config(
    freetype_load_flags: Option<FreeTypeLoadFlags>,
    freetype_load_target: Option<FreeTypeLoadTarget>,
    freetype_render_target: Option<FreeTypeLoadTarget>,
    dpi: Option<u32>,
) -> (i32, FT_Render_Mode) {
    let config = configuration();

    let load_flags = freetype_load_flags
        .or(config.freetype_load_flags)
        .unwrap_or_else(|| match dpi {
            Some(dpi) if dpi >= 100 => FreeTypeLoadFlags::default_hidpi(),
            _ => FreeTypeLoadFlags::default(),
        })
        .bits()
        | FT_LOAD_COLOR;

    fn target_to_render(target: FreeTypeLoadTarget) -> FT_Render_Mode {
        match target {
            FreeTypeLoadTarget::Mono => FT_Render_Mode::FT_RENDER_MODE_MONO,
            FreeTypeLoadTarget::Normal => FT_Render_Mode::FT_RENDER_MODE_NORMAL,
            FreeTypeLoadTarget::Light => FT_Render_Mode::FT_RENDER_MODE_LIGHT,
            FreeTypeLoadTarget::HorizontalLcd => FT_Render_Mode::FT_RENDER_MODE_LCD,
            FreeTypeLoadTarget::VerticalLcd => FT_Render_Mode::FT_RENDER_MODE_LCD_V,
        }
    }

    let load_target = target_to_render(freetype_load_target.unwrap_or(config.freetype_load_target));
    let render = target_to_render(
        freetype_render_target
            .unwrap_or(config.freetype_render_target.unwrap_or(config.freetype_load_target)),
    );

    let load_flags = load_flags | render_mode_to_load_target(load_target);

    (load_flags as i32, render)
}

pub struct Face {
    pub face: FT_Face,
    source: FontDataHandle,
    size: Option<FaceSize>,
    library: std::rc::Rc<LibraryInner>,
    palette: Option<&'static mut [FT_Color]>,
}

// Lifecycle: `Face` releases its owned `FT_Face` with `FT_Done_Face` exactly once.
impl Drop for Face {
    fn drop(&mut self) {
        // SAFETY: `self.library` keeps the creating FreeType library live; `self.face`
        // is uniquely owned here and `FT_Done_Face` consumes it with its stream.
        unsafe {
            FT_Done_Face(self.face);
        }
    }
}

struct FaceSize {
    size: f64,
    dpi: u32,
    cell_width: f64,
    cell_height: f64,
    cap_height: Option<f64>,
    cap_height_to_height_ratio: Option<f64>,
    is_scaled: bool,
}

#[derive(Debug)]
struct ComputedCellMetrics {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug)]
pub struct SelectedFontSize {
    pub width: f64,
    pub height: f64,
    pub cap_height: Option<f64>,
    pub cap_height_to_height_ratio: Option<f64>,
    pub is_scaled: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("Glyph is SVG")]
pub struct IsSvg;

#[derive(Debug, thiserror::Error)]
#[error("Glyph is COLR1 or later")]
pub struct IsColr1OrLater;

impl Face {
    /// Retrieves the root COLR paint handle for a glyph.
    pub fn get_color_glyph_paint(
        &mut self,
        glyph_index: FT_UInt,
        root_transform: FT_Color_Root_Transform,
    ) -> anyhow::Result<FT_Opaque_Paint_> {
        // SAFETY: `self.face` is live and `result` provides writable storage; FreeType fully
        // initializes it exactly when the returned status is one.
        unsafe {
            let mut result = MaybeUninit::<FT_Opaque_Paint_>::zeroed();
            let status = FT_Get_Color_Glyph_Paint(
                self.face,
                glyph_index,
                root_transform,
                result.as_mut_ptr(),
            );
            if status == 1 {
                Ok(result.assume_init())
            } else {
                // When: `status == 1` is false, FreeType did not initialize a root paint.
                anyhow::bail!("FT_Get_Color_Glyph_Paint for glyph {glyph_index} failed");
            }
        }
    }

    /// Retrieves the transformed COLR clip box for a glyph, or `None` when the glyph has none. The ClipList and
    /// its ClipBoxes are optional in COLRv1, so absence is not an error; see `clip_box_from_status`.
    pub fn get_color_glyph_clip_box(&mut self, glyph_index: FT_UInt) -> Option<FT_ClipBox_> {
        let mut result = MaybeUninit::<FT_ClipBox_>::zeroed();
        let status =
            // SAFETY: `self.face` is live and `result` provides writable storage for one clip box.
            unsafe { FT_Get_Color_Glyph_ClipBox(self.face, glyph_index, result.as_mut_ptr()) };
        clip_box_from_status(status, || {
            // SAFETY: FreeType fully initialized `result` because the status was 1, the only status
            // `clip_box_from_status` reads for.
            unsafe { result.assume_init() }
        })
    }

    /// Resolves a face-owned opaque COLR paint handle into its typed record.
    ///
    /// # Safety
    ///
    /// `paint` must come from this live face's COLRv1 paint graph and remain valid.
    // SAFETY: callers must preserve the face-specific provenance of `paint`.
    pub(crate) unsafe fn get_paint(
        &mut self,
        paint: FT_Opaque_Paint_,
    ) -> anyhow::Result<FT_COLR_Paint_> {
        // SAFETY: the caller establishes `paint` provenance; `result` is writable output
        // and FreeType fully initializes it exactly when status is one.
        unsafe {
            let mut result = MaybeUninit::<FT_COLR_Paint_>::zeroed();
            let status = FT_Get_Paint(self.face, paint, result.as_mut_ptr());
            if status == 1 {
                Ok(result.assume_init())
            } else {
                // When: `status == 1` is false, FreeType did not initialize a paint record.
                anyhow::bail!("FT_Get_Paint failed");
            }
        }
    }

    /// Replace any palette entry overrides and select the specified palette
    pub fn select_palette(&mut self, index: FT_UShort) -> anyhow::Result<()> {
        // SAFETY: `self.face` is live; successful palette queries initialize outputs,
        // checked non-empty storage remains face-owned for the face lifetime.
        unsafe {
            self.palette.take();

            let mut pdata = MaybeUninit::<FT_Palette_Data>::zeroed();
            ft_result(FT_Palette_Data_Get(self.face, pdata.as_mut_ptr()), ())
                .context("FT_Palette_Data_Get")?;
            let pdata = pdata.assume_init();

            let mut palette_ptr = std::ptr::null_mut();

            ft_result(FT_Palette_Select(self.face, index, &mut palette_ptr), ())
                .with_context(|| format!("FT_Palette_Select for index={index}. Note: {pdata:?}"))?;
            let palette_len = checked_palette_storage(palette_ptr, pdata.num_palette_entries)?;

            let palette = std::slice::from_raw_parts_mut(palette_ptr, palette_len);

            self.palette.replace(palette);

            Ok(())
        }
    }

    /// Returns a copied color from the currently selected palette.
    pub fn get_palette_entry(&self, index: u32) -> anyhow::Result<FT_Color> {
        self.palette
            .as_ref()
            .and_then(|slice| slice.get(index as usize))
            .copied()
            .ok_or_else(|| anyhow::anyhow!("invalid palette entry {index}"))
    }

    /// Collects palette metadata and localized entry names for this face.
    pub fn get_palette_data(&self) -> anyhow::Result<PaletteInfo> {
        // SAFETY: `self.face` is live; `FT_Palette_Data_Get` initializes `result` on success, and
        // its arrays are face-owned for `num_palettes`/`num_palette_entries` elements or null.
        unsafe {
            let mut result = MaybeUninit::<FT_Palette_Data>::zeroed();
            ft_result(FT_Palette_Data_Get(self.face, result.as_mut_ptr()), ())
                .context("FT_Palette_Data_Get")?;
            let data = result.assume_init();
            // CPAL labels are name IDs, not name-table positions: every record, grouped by name ID.
            Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))
        }
    }

    /// Decodes one SFNT naming record by table index.
    pub fn get_sfnt_name(&self, name_index: FT_UInt) -> anyhow::Result<NameRecord> {
        // SAFETY: `self.face` is live; a successful query initializes `sfnt_name`, whose
        // string pointer refers to `string_len` face-owned readable bytes.
        unsafe {
            let mut sfnt_name = MaybeUninit::<FT_SfntName>::zeroed();
            ft_result(FT_Get_Sfnt_Name(self.face, name_index, sfnt_name.as_mut_ptr()), ())
                .context("FT_Get_Sfnt_Name")?;
            let sfnt_name = sfnt_name.assume_init();
            let bytes =
                from_raw_parts(sfnt_name.string as *const u8, sfnt_name.string_len as usize);

            let encoding = match (sfnt_name.platform_id as u32, sfnt_name.encoding_id as u32) {
                (TT_PLATFORM_MACINTOSH, TT_MAC_ID_JAPANESE)
                | (TT_PLATFORM_MICROSOFT, TT_MS_ID_SJIS) => encoding_rs::SHIFT_JIS,
                (TT_PLATFORM_MACINTOSH, TT_MAC_ID_SIMPLIFIED_CHINESE)
                | (TT_PLATFORM_MICROSOFT, TT_MS_ID_PRC) => encoding_rs::GBK,
                (TT_PLATFORM_MACINTOSH, TT_MAC_ID_TRADITIONAL_CHINESE)
                | (TT_PLATFORM_MICROSOFT, TT_MS_ID_BIG_5) => encoding_rs::BIG5,
                (
                    TT_PLATFORM_MICROSOFT,
                    TT_MS_ID_UCS_4 | TT_MS_ID_UNICODE_CS | TT_MS_ID_SYMBOL_CS,
                ) => encoding_rs::UTF_16BE,
                (TT_PLATFORM_MICROSOFT, TT_MS_ID_WANSUNG) => encoding_rs::EUC_KR,
                (TT_PLATFORM_APPLE_UNICODE | TT_PLATFORM_ISO, _) => encoding_rs::UTF_16BE,
                (TT_PLATFORM_MACINTOSH, TT_MAC_ID_ROMAN) => encoding_rs::MACINTOSH,
                _ => {
                    anyhow::bail!(
                        "Skipping name_id={} because platform_id={} encoding_id={}",
                        sfnt_name.name_id,
                        sfnt_name.platform_id,
                        sfnt_name.encoding_id
                    );
                }
            };

            let (name, _) = encoding.decode_with_bom_removal(bytes);

            Ok(NameRecord {
                platform_id: sfnt_name.platform_id,
                encoding_id: sfnt_name.encoding_id,
                name_id: sfnt_name.name_id,
                language_id: sfnt_name.language_id,
                name: name.to_string(),
            })
        }
    }

    /// Advances a face-owned COLR layer iterator and returns its next paint handle.
    ///
    /// # Safety
    ///
    /// `iter` must be initialized from this live face's COLRv1 paint graph.
    // SAFETY: callers must preserve the face-specific provenance of `iter`.
    pub(crate) unsafe fn get_paint_layers(
        &mut self,
        iter: &mut FT_LayerIterator_,
    ) -> anyhow::Result<FT_Opaque_Paint_> {
        // SAFETY: the caller establishes `iter` provenance; `result` is writable output
        // and FreeType fully initializes it exactly when status is one.
        unsafe {
            let mut result = MaybeUninit::<FT_Opaque_Paint_>::zeroed();
            let status = FT_Get_Paint_Layers(self.face, iter, result.as_mut_ptr());
            if status == 1 {
                Ok(result.assume_init())
            } else {
                // When: `status == 1` is false, the iterator produced no initialized paint.
                anyhow::bail!("FT_Get_Paint_Layers failed");
            }
        }
    }

    /// Decomposes a COLRv1 PaintGlyph outline into drawing operations in the face's own font units.
    ///
    /// The glyph is loaded with `FT_LOAD_NO_SCALE | FT_LOAD_IGNORE_TRANSFORM` on top of `load_flags`, so
    /// neither the face's size nor its transform (synthetic italic) is applied here: the caller's included
    /// root transform maps every paint operand, contours and gradients alike, from font units to device pixels
    /// exactly once. The face's size and transform stay set, since that root transform is built from them.
    pub fn load_glyph_outlines(
        &mut self,
        glyph_index: FT_UInt,
        load_flags: FT_Int32,
    ) -> anyhow::Result<Vec<DrawOp>> {
        // SAFETY: `self.face` is live; a successful glyph load initializes its face-owned slot,
        // and decomposition invokes the callbacks synchronously while `ops` is alive.
        unsafe {
            let unscaled_flags =
                load_flags | FT_LOAD_NO_SCALE as FT_Int32 | FT_LOAD_IGNORE_TRANSFORM as FT_Int32;
            ft_result(FT_Load_Glyph(self.face, glyph_index, unscaled_flags), ())
                .with_context(|| format!("FT_Load_Glyph {glyph_index}"))?;
            let slot = &mut *(*self.face).glyph;
            if slot.format != FT_Glyph_Format_::FT_GLYPH_FORMAT_OUTLINE {
                anyhow::bail!(
                    "Expected FT_COLR_PAINTFORMAT_GLYPH to be an outline, got {:?}",
                    slot.format
                );
            }

            let funcs = FT_Outline_Funcs_ {
                move_to: Some(move_to),
                line_to: Some(line_to),
                conic_to: Some(conic_to),
                cubic_to: Some(cubic_to),
                // Unscaled points are whole font units; a zero shift hands them over unchanged, so no
                // multiplication can overflow a 32-bit FT_Pos.
                shift: 0,
                delta: FT_Pos::from_font_units(0),
            };

            let mut ops = vec![];

            // SAFETY: FreeType calls this only with a readable `to` vector and the live
            // `Vec<DrawOp>` pointer supplied to `FT_Outline_Decompose` below.
            unsafe extern "C" fn move_to(to: *const FT_Vector, user: *mut c_void) -> c_int {
                // SAFETY: the callback contract above makes `to` readable and `user` a unique,
                // live pointer to the output vector for the duration of this call.
                unsafe {
                    let ops = user as *mut Vec<DrawOp>;
                    let (to_x, to_y) = outline_point_font_units(&*to);
                    (*ops).push(DrawOp::MoveTo { to_x, to_y });
                }
                0
            }
            // SAFETY: FreeType calls this only with a readable `to` vector and the live
            // `Vec<DrawOp>` pointer supplied to `FT_Outline_Decompose` below.
            unsafe extern "C" fn line_to(to: *const FT_Vector, user: *mut c_void) -> c_int {
                // SAFETY: the callback contract above makes `to` readable and `user` a unique,
                // live pointer to the output vector for the duration of this call.
                unsafe {
                    let ops = user as *mut Vec<DrawOp>;
                    let (to_x, to_y) = outline_point_font_units(&*to);
                    (*ops).push(DrawOp::LineTo { to_x, to_y });
                }
                0
            }
            // SAFETY: FreeType calls this only with readable `control` and `to` vectors
            // and the live `Vec<DrawOp>` pointer supplied below.
            unsafe extern "C" fn conic_to(
                control: *const FT_Vector,
                to: *const FT_Vector,
                user: *mut c_void,
            ) -> c_int {
                // SAFETY: the callback contract above makes both vectors readable and `user` a
                // unique, live pointer to the output vector for this call.
                unsafe {
                    let ops = user as *mut Vec<DrawOp>;
                    let (control_x, control_y) = outline_point_font_units(&*control);
                    let (to_x, to_y) = outline_point_font_units(&*to);
                    (*ops).push(DrawOp::QuadTo { control_x, control_y, to_x, to_y });
                }
                0
            }
            // SAFETY: FreeType calls this only with readable control and destination
            // vectors and the live `Vec<DrawOp>` pointer supplied below.
            unsafe extern "C" fn cubic_to(
                control1: *const FT_Vector,
                control2: *const FT_Vector,
                to: *const FT_Vector,
                user: *mut c_void,
            ) -> c_int {
                // SAFETY: the callback contract above makes all vectors readable and `user` a
                // unique, live pointer to the output vector for this call.
                unsafe {
                    let ops = user as *mut Vec<DrawOp>;
                    let (control1_x, control1_y) = outline_point_font_units(&*control1);
                    let (control2_x, control2_y) = outline_point_font_units(&*control2);
                    let (to_x, to_y) = outline_point_font_units(&*to);
                    (*ops).push(DrawOp::CubicTo {
                        control1_x,
                        control1_y,
                        control2_x,
                        control2_y,
                        to_x,
                        to_y,
                    });
                }
                0
            }

            ft_result(
                FT_Outline_Decompose(
                    &mut slot.outline,
                    &funcs,
                    &mut ops as *mut Vec<DrawOp> as *mut c_void,
                ),
                (),
            )
            .with_context(|| format!("FT_Outline_Decompose. ops so far: {ops:?}"))?;

            if !ops.is_empty() {
                ops.push(DrawOp::ClosePath);
            }

            Ok(ops)
        }
    }

    /// Loads and renders one glyph, surfacing SVG and COLR-v1 glyphs to the caller.
    pub fn load_and_render_glyph(
        &mut self,
        glyph_index: FT_UInt,
        load_flags: FT_Int32,
        render_mode: FT_Render_Mode,
        synthesize_bold: bool,
    ) -> anyhow::Result<&FT_GlyphSlotRec_> {
        // SAFETY: `self.face` is live; successful load/render calls initialize its face-owned
        // glyph slot, whose returned reference remains tied to this mutable face borrow.
        unsafe {
            if (*self.face).num_fixed_sizes > 0 {
                let preflight_flags = bitmap_metrics_preflight_flags(load_flags);
                ft_result(FT_Load_Glyph(self.face, glyph_index, preflight_flags), ())
                    .with_context(|| {
                        format!(
                            "load_and_render_glyph: bitmap metrics preflight glyph_index:{glyph_index}"
                        )
                    })?;
                let preflight = &*(*self.face).glyph;
                let width = usize::try_from(preflight.bitmap.width)
                    .context("negative FreeType preflight bitmap width")?;
                let rows = usize::try_from(preflight.bitmap.rows)
                    .context("negative FreeType preflight bitmap height")?;
                crate::rasterizer::checked_glyph_rgba_len(width, rows)
                    .context("FreeType embedded bitmap rejected before decode")?;
            }
            ft_result(
                FT_Load_Glyph(self.face, glyph_index, load_flags | FT_LOAD_NO_SVG as i32),
                (),
            )
            .with_context(|| {
                format!("load_and_render_glyph: FT_Load_Glyph glyph_index:{}", glyph_index)
            })?;
            let slot = &mut *(*self.face).glyph;

            if slot.format == FT_Glyph_Format_::FT_GLYPH_FORMAT_SVG {
                // When: `slot.format == FT_GLYPH_FORMAT_SVG`, defer SVG decoding to the caller.
                return Err(IsSvg.into());
            }

            if synthesize_bold {
                FT_GlyphSlot_Embolden(slot as *mut _);
            }

            // Current versions of freetype overload the operation of FT_LOAD_COLOR
            // and the resulting glyph format such that we cannot determine solely
            // from the flags whether we got a regular set of outlines,
            // or its COLR v0 synthesized glyphs, or whether it's COLR v1 or later
            // and it can't render the result.
            // So, we probe here to look for color layer information: if we find it,
            // we don't call freetype's renderer and instead bubble up an error
            // that the embedding application can trap and decide what to do.
            if slot.format == FT_Glyph_Format_::FT_GLYPH_FORMAT_OUTLINE
                && self
                    .get_color_glyph_paint(
                        glyph_index,
                        FT_Color_Root_Transform::FT_COLOR_NO_ROOT_TRANSFORM,
                    )
                    .is_ok()
            {
                // When: `slot.format == OUTLINE && get_color_glyph_paint(...).is_ok()`,
                // let the caller render COLR v1+.
                return Err(IsColr1OrLater.into());
            }

            if slot.format == FT_Glyph_Format_::FT_GLYPH_FORMAT_OUTLINE {
                crate::rasterizer::checked_freetype_26_6_extent(freetype_pos_as_i64(
                    slot.metrics.width.font_units(),
                ))
                .context("FreeType outline width rejected before rendering")?;
                crate::rasterizer::checked_freetype_26_6_extent(freetype_pos_as_i64(
                    slot.metrics.height.font_units(),
                ))
                .context("FreeType outline height rejected before rendering")?;
            }

            ft_result(FT_Render_Glyph(slot, render_mode), ())
                .context("load_and_render_glyph: FT_Render_Glyph")?;

            Ok(slot)
        }
    }
}

struct LibraryInner {
    lib: FT_Library,
}

// Lifecycle: `LibraryInner` releases its `FT_Library` with `FT_Done_FreeType` after all shared owners drop.
impl Drop for LibraryInner {
    fn drop(&mut self) {
        // SAFETY: `self.lib` is the live handle returned by `FT_Init_FreeType`, and the
        // last shared owner is its unique release point.
        unsafe {
            FT_Done_FreeType(self.lib);
        }
    }
}

pub struct Library {
    inner: std::rc::Rc<LibraryInner>,
}

/// Wrapper around std::slice::from_raw_parts that allows for ptr to be
/// null. In the null ptr case, an empty slice is returned.
/// This is necessary because it is common for freetype to encode
/// empty arrays in that way, and rust 1.78 will panic if a null
/// ptr is passed in.
// SAFETY: for a non-null `ptr`, the caller must provide `size` initialized contiguous `T` values
// that remain readable and unmutated for the returned lifetime; null is accepted as empty.
pub(crate) unsafe fn from_raw_parts<'a, T>(ptr: *const T, size: usize) -> &'a [T] {
    if ptr.is_null() {
        &[]
    } else {
        // When: `ptr.is_null()` is false, expose the caller-guaranteed initialized elements.
        // SAFETY: the function contract requires `ptr` to name `size` initialized contiguous
        // elements that remain valid and unmutated for the returned lifetime.
        unsafe { std::slice::from_raw_parts(ptr, size) }
    }
}

#[derive(Debug)]
pub struct PaletteInfo {
    pub num_palettes: usize,
    /// One entry per palette; an absent CPAL label or flag array leaves that field empty or 0.
    pub palettes: Vec<Palette>,
}

#[derive(Debug)]
pub struct Palette {
    pub palette_index: usize,
    pub name: String,
    pub flags: u16,
    pub entry_names: Vec<String>,
}

#[derive(Debug)]
pub struct NameRecord {
    pub platform_id: u16,
    pub encoding_id: u16,
    pub language_id: u16,
    pub name_id: u16,
    pub name: String,
}

/// The ClipBox FreeType returned, or `None` when it returned 0. FreeType returns 0 for a glyph with no
/// ClipList entry, and equally for a face with no COLR table, a missing or malformed ClipList, or a non-SFNT
/// face; the status cannot tell these apart, so every 0 reads as "no ClipBox". `read` runs only for status 1,
/// the only status for which FreeType initialized the box.
pub(crate) fn clip_box_from_status(
    status: FT_Bool,
    read: impl FnOnce() -> FT_ClipBox_,
) -> Option<FT_ClipBox_> {
    (status == 1).then(read)
}

/// An unscaled outline point as `(x, y)` in font units: `FT_LOAD_NO_SCALE` leaves each coordinate a whole number
/// of font units, which the zero-shift decomposition passes through unchanged.
fn outline_point_font_units(vector: &FT_Vector) -> (f32, f32) {
    (vector.x.font_units() as f32, vector.y.font_units() as f32)
}

/// Converts a FreeType 16.16 vector into floating-point coordinates.
pub fn vector_x_y(vector: &FT_Vector) -> (f32, f32) {
    (vector.x.f16d16().to_num(), vector.y.f16d16().to_num())
}

/// Maps a FreeType COLR composite mode to the equivalent Cairo operator.
pub fn composite_mode_to_operator(mode: FT_Composite_Mode) -> cairo::Operator {
    use cairo::Operator;
    use FT_Composite_Mode::*;
    match mode {
        FT_COLR_COMPOSITE_CLEAR => Operator::Clear,
        FT_COLR_COMPOSITE_SRC => Operator::Source,
        FT_COLR_COMPOSITE_DEST => Operator::Dest,
        FT_COLR_COMPOSITE_SRC_OVER => Operator::Over,
        FT_COLR_COMPOSITE_DEST_OVER => Operator::DestOver,
        FT_COLR_COMPOSITE_SRC_IN => Operator::In,
        FT_COLR_COMPOSITE_DEST_IN => Operator::DestIn,
        FT_COLR_COMPOSITE_SRC_OUT => Operator::Out,
        FT_COLR_COMPOSITE_DEST_OUT => Operator::DestOut,
        FT_COLR_COMPOSITE_SRC_ATOP => Operator::Atop,
        FT_COLR_COMPOSITE_DEST_ATOP => Operator::DestAtop,
        FT_COLR_COMPOSITE_XOR => Operator::Xor,
        FT_COLR_COMPOSITE_PLUS => Operator::Add,
        FT_COLR_COMPOSITE_SCREEN => Operator::Screen,
        FT_COLR_COMPOSITE_OVERLAY => Operator::Overlay,
        FT_COLR_COMPOSITE_DARKEN => Operator::Darken,
        FT_COLR_COMPOSITE_LIGHTEN => Operator::Lighten,
        FT_COLR_COMPOSITE_COLOR_DODGE => Operator::ColorDodge,
        FT_COLR_COMPOSITE_COLOR_BURN => Operator::ColorBurn,
        FT_COLR_COMPOSITE_HARD_LIGHT => Operator::HardLight,
        FT_COLR_COMPOSITE_SOFT_LIGHT => Operator::SoftLight,
        FT_COLR_COMPOSITE_DIFFERENCE => Operator::Difference,
        FT_COLR_COMPOSITE_EXCLUSION => Operator::Exclusion,
        FT_COLR_COMPOSITE_MULTIPLY => Operator::Multiply,
        FT_COLR_COMPOSITE_HSL_HUE => Operator::HslHue,
        FT_COLR_COMPOSITE_HSL_SATURATION => Operator::HslSaturation,
        FT_COLR_COMPOSITE_HSL_COLOR => Operator::HslColor,
        FT_COLR_COMPOSITE_HSL_LUMINOSITY => Operator::HslLuminosity,
        _ => unreachable!(),
    }
}

fn ft_make_tag(first: u8, second: u8, third: u8, fourth: u8) -> FT_ULong {
    (first as FT_ULong) << 24
        | (second as FT_ULong) << 16
        | (third as FT_ULong) << 8
        | (fourth as FT_ULong)
}

/// A single variation-axis observation for a selected named instance: its
/// registered tag plus the instance's coordinate and the axis default, both
/// already converted out of FreeType fixed point. This is a plain data view so
/// the wght/wdth scaling math can be exercised without any FFI.
#[derive(Debug, Clone, Copy)]
struct AxisScaling {
    tag: FT_ULong,
    value: f64,
    default_value: f64,
}

impl AxisScaling {
    /// Ratio of the instance coordinate to the axis default. A zero (or
    /// otherwise unusable) default yields a neutral scale of 1.0 so weight and
    /// width are left unchanged rather than producing NaN/infinity.
    fn scale(&self) -> f64 {
        if self.default_value != 0. {
            self.value / self.default_value
        } else {
            // When: `self.default_value != 0.` is false, preserve a neutral scale.
            1.
        }
    }
}

/// Apply usable variation metadata or retain the base OS/2/default metrics
/// when probing failed. This keeps the fallback decision independently
/// testable without fabricating FreeType pointers.
fn weight_and_width_with_variation(
    base_weight: f64,
    base_width: f64,
    scalings: Result<Vec<AxisScaling>, ()>,
) -> (u16, u16) {
    match scalings {
        Ok(scalings) => scaled_weight_and_width(base_weight, base_width, &scalings),
        Err(()) => (base_weight.round() as u16, base_width.round() as u16),
    }
}

/// Scale the base OS/2 (or default) weight and width by the `wght`/`wdth`
/// variation axes, rounding to the nearest integer class. Axes other than
/// `wght`/`wdth` are ignored, and an empty `scalings` slice returns the base
/// metrics rounded. This is the pure core of `Face::weight_and_width`.
fn bitmap_metrics_preflight_flags(load_flags: FT_Int32) -> FT_Int32 {
    (load_flags & !(FT_LOAD_RENDER as FT_Int32))
        | FT_LOAD_BITMAP_METRICS_ONLY as FT_Int32
        | FT_LOAD_NO_SVG as FT_Int32
}

fn scaled_weight_and_width(
    base_weight: f64,
    base_width: f64,
    scalings: &[AxisScaling],
) -> (u16, u16) {
    let wght = ft_make_tag(b'w', b'g', b'h', b't');
    let wdth = ft_make_tag(b'w', b'd', b't', b'h');

    let mut weight = base_weight;
    let mut width = base_width;

    for axis in scalings {
        let scale = axis.scale();
        if axis.tag == wght {
            weight *= scale;
        }
        if axis.tag == wdth {
            width *= scale;
        }
    }

    (weight.round() as u16, width.round() as u16)
}

#[cfg(test)]
#[path = "ftwrap_tests.rs"]
mod ftwrap_tests;

#[path = "ftwrap_library.rs"]
mod library;
#[path = "ftwrap_metrics.rs"]
mod metrics;
#[path = "ftwrap_names.rs"]
mod names;
