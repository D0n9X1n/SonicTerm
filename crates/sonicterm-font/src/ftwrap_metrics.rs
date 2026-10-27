//! FreeType size selection and raster-derived cell and cap-height metrics.

use super::*;

impl Face {
    /// This is a wrapper around set_char_size and select_size
    /// that accounts for some weirdness with eg: color emoji
    pub fn set_font_size(&mut self, point_size: f64, dpi: u32) -> anyhow::Result<SelectedFontSize> {
        // When: `self.size.as_ref()` is `Some`, compare its key before recomputing.
        if let Some(face_size) = self.size.as_ref() {
            if face_size.size == point_size && face_size.dpi == dpi {
                // When: `face_size.size == point_size && face_size.dpi == dpi`, reuse cached metrics.
                return Ok(SelectedFontSize {
                    width: face_size.cell_width,
                    height: face_size.cell_height,
                    is_scaled: face_size.is_scaled,
                    cap_height: face_size.cap_height,
                    cap_height_to_height_ratio: face_size.cap_height_to_height_ratio,
                });
            }
        }

        let pixel_height = point_size * dpi as f64 / 72.0;
        log::debug!(
            "set_char_size computing {} dpi={} (pixel height={})",
            point_size,
            dpi,
            pixel_height
        );

        // Scaling before truncating to integer minimizes the chances of hitting
        // the fallback code for set_pixel_sizes below.
        let size = FT_F26Dot6::from_num(point_size);

        let selected_size = match self.set_char_size(size, size, dpi, dpi) {
            Ok(_) => {
                // Compute metrics for the nominal monospace cell
                let ComputedCellMetrics { width, height } = self.cell_metrics();
                SelectedFontSize {
                    width,
                    height,
                    cap_height: None,
                    cap_height_to_height_ratio: None,
                    is_scaled: true,
                }
            }
            Err(err) => {
                // When: `set_char_size(...)` returns `Err`, inspect fixed bitmap strikes instead.
                log::debug!("set_char_size: {:?}, will inspect strikes", err);

                let sizes =
                    // SAFETY: `available_sizes` names `num_fixed_sizes` live face-owned records.
                    unsafe {
                        let rec = &(*self.face);
                        from_raw_parts(rec.available_sizes, rec.num_fixed_sizes as usize)
                    };
                if sizes.is_empty() {
                    // When: `sizes.is_empty()` is true, there is no bitmap fallback to select.
                    return Err(err);
                }
                // Find the best matching size; we look for the strike whose height
                // is closest to the desired size.
                struct Best {
                    idx: usize,
                    distance: usize,
                    height: i16,
                    width: i16,
                }
                let mut best: Option<Best> = None;

                for (idx, info) in sizes.iter().enumerate() {
                    log::debug!("idx={} info={:?}", idx, info);
                    let distance = (info.height - (pixel_height as i16)).unsigned_abs() as usize;
                    let candidate = Best { idx, distance, height: info.height, width: info.width };

                    match best.take() {
                        Some(existing) => {
                            best.replace(if candidate.distance < existing.distance {
                                candidate
                            } else {
                                existing
                            });
                        }
                        None => {
                            best.replace(candidate);
                        }
                    }
                }
                let best = best.unwrap();
                self.select_size(best.idx)?;
                // Compute the cell metrics at this size.
                // This stuff is a bit weird; for GohuFont.otb, cell_metrics()
                // returns (8.0, 0.0) when the selected bitmap strike is (4, 14).
                // 4 pixels is too thin for this font, so we take the max of the
                // known dimensions to produce the size.
                let metrics = self.cell_metrics();
                let height = f64::from(best.height).max(metrics.height);
                SelectedFontSize {
                    width: f64::from(best.width).max(metrics.width),
                    height,
                    is_scaled: false,
                    cap_height: None,
                    cap_height_to_height_ratio: None,
                }
            }
        };

        self.size.replace(FaceSize {
            size: point_size,
            dpi,
            cell_width: selected_size.width,
            cap_height: None,
            cap_height_to_height_ratio: None,
            cell_height: selected_size.height,
            is_scaled: selected_size.is_scaled,
        });

        // Can't compute cap height until after we've assigned self.size
        if let Ok(cap_height) = self.compute_cap_height() {
            let cap_height_to_height_ratio = cap_height / selected_size.height;

            self.size.replace(FaceSize {
                size: point_size,
                dpi,
                cell_width: selected_size.width,
                cap_height: Some(cap_height),
                cap_height_to_height_ratio: Some(cap_height_to_height_ratio),
                cell_height: selected_size.height,
                is_scaled: selected_size.is_scaled,
            });

            Ok(SelectedFontSize {
                cap_height: Some(cap_height),
                cap_height_to_height_ratio: Some(cap_height_to_height_ratio),
                ..selected_size
            })
        } else {
            // When: `compute_cap_height()` returns `Err`, keep metrics without cap height.
            Ok(selected_size)
        }
    }

    fn set_char_size(
        &mut self,
        char_width: FT_F26Dot6,
        char_height: FT_F26Dot6,
        horz_resolution: FT_UInt,
        vert_resolution: FT_UInt,
    ) -> anyhow::Result<()> {
        ft_result(
            // SAFETY: `self.face` is live and the scalar size/resolution values match
            // the `FT_Set_Char_Size` parameter types.
            unsafe {
                FT_Set_Char_Size(
                    self.face,
                    char_width,
                    char_height,
                    horz_resolution,
                    vert_resolution,
                )
            },
            (),
        )
        .context("FT_Set_Char_Size")?;

        // SAFETY: `self.face` remains live and points to an initialized face record.
        unsafe {
            if (*self.face).height == 0 {
                anyhow::bail!("font has 0 height, fallback to bitmaps");
            }
        }

        Ok(())
    }

    fn select_size(&mut self, idx: usize) -> anyhow::Result<()> {
        ft_result(
            // SAFETY: `self.face` is live; `idx` indexes its fixed-size array.
            unsafe { FT_Select_Size(self.face, idx as i32) },
            (),
        )
        .context("FT_Select_Size")
    }

    /// Replaces the face transform, using FreeType's identity transform when absent.
    pub fn set_transform(&mut self, matrix: Option<FT_Matrix>) {
        let mut matrix = matrix;
        // SAFETY: `self.face` is live; the optional matrix pointer remains valid for the call,
        // and FreeType copies its values rather than retaining the pointer.
        unsafe {
            FT_Set_Transform(
                self.face,
                match &mut matrix {
                    Some(transform) => transform as *mut _,
                    None => std::ptr::null_mut(),
                },
                std::ptr::null_mut(),
            )
        }
    }
    /// Compute the cap-height metric in pixels.
    /// This is pixel-perfect based on the rendered glyph data for `I`,
    /// which is a technique that works for any font regardless
    /// of the integrity of its internal cap-height metric or
    /// whether the font is a bitmap font.
    /// `I` is chosen rather than `O` as `O` glyphs are often optically
    /// compensated and overshoot a little.
    fn compute_cap_height(&mut self) -> anyhow::Result<f64> {
        let glyph_pos =
            // SAFETY: `self.face` is live; the character code is a scalar input.
            unsafe { FT_Get_Char_Index(self.face, b'I' as _) };
        if glyph_pos == 0 {
            anyhow::bail!("no I from which to compute cap height");
        }
        let (load_flags, render_mode) = compute_load_flags_from_config(None, None, None, None);
        let ft_glyph = self.load_and_render_glyph(glyph_pos, load_flags, render_mode, false)?;

        let mode: FT_Pixel_Mode =
            // SAFETY: successful rendering initializes `pixel_mode` to an enum discriminant.
            unsafe { std::mem::transmute(u32::from(ft_glyph.bitmap.pixel_mode)) };

        // pitch is the number of bytes per source row
        let pitch = ft_glyph.bitmap.pitch.unsigned_abs() as usize;
        let data_len =
            checked_bitmap_buffer_len(ft_glyph.bitmap.buffer, ft_glyph.bitmap.rows, pitch)?;
        let data =
            // SAFETY: `checked_bitmap_buffer_len` established a non-null buffer naming
            // `data_len` live slot-owned writable bytes.
            unsafe { std::slice::from_raw_parts_mut(ft_glyph.bitmap.buffer, data_len) };

        let mut first_row = None;
        let mut last_row = None;

        match mode {
            FT_Pixel_Mode::FT_PIXEL_MODE_LCD => {
                // When: `mode` is LCD, scan three coverage bytes per logical pixel.
                let width = ft_glyph.bitmap.width as usize / 3;
                let height = ft_glyph.bitmap.rows as usize;

                'next_line_lcd: for row in 0..height {
                    let src_offset = row * pitch as usize;
                    for column in 0..width {
                        if data[src_offset + (column * 3)] != 0
                            || data[src_offset + (column * 3) + 1] != 0
                            || data[src_offset + (column * 3) + 2] != 0
                        {
                            // When: any `data[...] != 0` LCD term is true, this row has ink.
                            if first_row.is_none() {
                                first_row.replace(row);
                            }
                            last_row.replace(row);
                            continue 'next_line_lcd;
                        }
                    }
                }
            }

            FT_Pixel_Mode::FT_PIXEL_MODE_BGRA => {
                // When: `mode` is BGRA, scan each pixel's alpha byte for visible ink.
                let width = ft_glyph.bitmap.width as usize;
                let height = ft_glyph.bitmap.rows as usize;
                'next_line_bgra: for row in 0..height {
                    let src_offset = row * pitch as usize;
                    for column in 0..width {
                        let alpha = data[src_offset + (column * 4) + 3];
                        if alpha != 0 {
                            // When: `alpha != 0`, this BGRA row contains visible ink.
                            if first_row.is_none() {
                                first_row.replace(row);
                            }
                            last_row.replace(row);
                            continue 'next_line_bgra;
                        }
                    }
                }
            }
            FT_Pixel_Mode::FT_PIXEL_MODE_GRAY => {
                // When: `mode` is GRAY, each nonzero byte marks covered ink.
                let width = ft_glyph.bitmap.width as usize;
                let height = ft_glyph.bitmap.rows as usize;
                'next_line_gray: for row in 0..height {
                    let src_offset = row * pitch;
                    for column in 0..width {
                        if data[src_offset + column] != 0 {
                            // When: `data[src_offset + column] != 0`, this grayscale row
                            // contains ink.
                            if first_row.is_none() {
                                first_row.replace(row);
                            }
                            last_row.replace(row);
                            continue 'next_line_gray;
                        }
                    }
                }
            }
            FT_Pixel_Mode::FT_PIXEL_MODE_MONO => {
                // When: `mode` is MONO, scan packed high-bit-first coverage bits.
                let width = ft_glyph.bitmap.width as usize;
                let height = ft_glyph.bitmap.rows as usize;
                'next_line_mono: for row in 0..height {
                    let src_offset = row * pitch;
                    let mut column = 0;
                    for byte_index in 0..pitch {
                        if column >= width {
                            // When: `column >= width`, ignore pitch padding after the logical row.
                            break;
                        }
                        let mut bits = data[src_offset + byte_index];
                        for _ in 0..8 {
                            if column >= width {
                                // When: `column >= width`, stop before testing padding bits.
                                break;
                            }
                            if bits & 0x80 == 0x80 {
                                // When: `bits & 0x80 == 0x80`, this monochrome row contains ink.
                                if first_row.is_none() {
                                    first_row.replace(row);
                                }
                                last_row.replace(row);
                                continue 'next_line_mono;
                            }
                            bits <<= 1;
                            column += 1;
                        }
                    }
                }
            }
            _ => anyhow::bail!("unhandled pixel mode {:?}", mode),
        }

        match (first_row, last_row) {
            (Some(first), Some(last)) => Ok((last - first) as f64),
            _ => anyhow::bail!("didn't find any rasterized rows?"),
        }
    }

    fn cell_metrics(&mut self) -> ComputedCellMetrics {
        // SAFETY: `self.face` and its selected size are live; successful glyph loads initialize
        // the face-owned slot before its metrics are read.
        unsafe {
            let metrics = &(*(*self.face).size).metrics;
            let height = metrics.y_scale.to_num::<f64>() * f64::from((*self.face).height) / 64.0;

            let mut width = 0.0;
            let mut num_examined = 0;
            for char_code in 32..128 {
                let glyph_pos = FT_Get_Char_Index(self.face, char_code);
                if glyph_pos == 0 {
                    // When: `glyph_pos == 0`, this ASCII character is absent from the face.
                    continue;
                }
                let res = FT_Load_Glyph(self.face, glyph_pos, FT_LOAD_COLOR as i32);
                if succeeded(res) {
                    num_examined += 1;
                    let glyph = &(*(*self.face).glyph);
                    if glyph.metrics.horiAdvance.font_units() as f64 > width {
                        width = glyph.metrics.horiAdvance.font_units() as f64;
                    }
                }
            }
            if width == 0.0 {
                // Most likely we're looking at a symbol font with no latin
                // glyphs at all. Let's just pick a selection of glyphs
                for glyph_pos in 1..8 {
                    let res = FT_Load_Glyph(self.face, glyph_pos, FT_LOAD_COLOR as i32);
                    if succeeded(res) {
                        num_examined += 1;
                        let glyph = &(*(*self.face).glyph);
                        if glyph.metrics.horiAdvance.font_units() as f64 > width {
                            width = glyph.metrics.horiAdvance.font_units() as f64;
                        }
                    }
                }
                if width == 0.0 {
                    log::error!(
                        "Couldn't find usable advance metrics out of {} glyphs \
                        sampled from the font, so guessing width == height",
                        num_examined,
                    );
                    width = height * 64.;
                }
            }

            ComputedCellMetrics { width: width / 64.0, height }
        }
    }
}
