//! FreeType face naming, variable axes, coverage and advertised strikes.

use super::*;

impl Face {
    /// Returns the face's family name, or an empty string when none is recorded.
    pub fn family_name(&self) -> String {
        // SAFETY: `self.face` is live for this borrow; a non-null `family_name`
        // points to face-owned NUL-terminated storage valid for the face lifetime.
        unsafe {
            if (*self.face).family_name.is_null() {
                "".to_string()
            } else {
                // When: the face record exposes a family-name string.
                let name = CStr::from_ptr((*self.face).family_name);
                name.to_string_lossy().to_string()
            }
        }
    }

    /// Returns the face's style name, or an empty string when none is recorded.
    pub fn style_name(&self) -> String {
        // SAFETY: `self.face` is live for this borrow; a non-null `style_name`
        // points to face-owned NUL-terminated storage valid for the face lifetime.
        unsafe {
            if (*self.face).style_name.is_null() {
                "".to_string()
            } else {
                // When: the face record exposes a style-name string.
                let name = CStr::from_ptr((*self.face).style_name);
                name.to_string_lossy().to_string()
            }
        }
    }

    /// Returns the face's PostScript name, or an empty string when unavailable.
    pub fn postscript_name(&self) -> String {
        // SAFETY: `self.face` is live; a non-null pointer returned by
        // `FT_Get_Postscript_Name` is face-owned NUL-terminated storage.
        unsafe {
            let name_ptr = FT_Get_Postscript_Name(self.face);
            if name_ptr.is_null() {
                "".to_string()
            } else {
                // When: `name_ptr.is_null()` is false, copy FreeType's PostScript name.
                let name = CStr::from_ptr(name_ptr);
                name.to_string_lossy().to_string()
            }
        }
    }

    /// Parses each named variation instance exposed by this variable font face.
    pub fn variations(&self) -> anyhow::Result<Vec<ParsedFont>> {
        let mut mm_var_ptr = std::ptr::null_mut();

        // SAFETY: only a successful `FT_Get_MM_Var` result is wrapped and read; the guard
        // releases that allocation, and the face is reset before any parse error is returned.
        unsafe {
            ft_result(FT_Get_MM_Var(self.face, &mut mm_var_ptr), ()).context("FT_Get_MM_Var")?;
            let guard = MmVarGuard::from_success(self.library.lib, mm_var_ptr)?;
            let num_styles = guard.get().num_namedstyles;
            let result = (|| {
                let mut res = vec![];
                for instance in 1..=num_styles {
                    FT_Set_Named_Instance(self.face, instance);
                    let source = FontDataHandle {
                        source: self.source.source.clone(),
                        index: self.source.index,
                        variation: instance,
                        origin: self.source.origin.clone(),
                        coverage: self.source.coverage.clone(),
                    };
                    res.push(ParsedFont::from_face(self, source)?);
                }
                Ok::<_, anyhow::Error>(res)
            })();
            FT_Set_Named_Instance(self.face, 0);
            let res = result?;

            log::debug!("Variations: {:#?}", res);

            Ok(res)
        }
    }

    /// Returns FreeType's name for a glyph index when the face provides one.
    pub fn get_glyph_name(&self, glyph_index: u32) -> Option<String> {
        let mut buf = [0u8; 128];
        let res =
            // SAFETY: `self.face` is live; `buf` provides `buf.len()` initialized writable bytes.
            unsafe {
                FT_Get_Glyph_Name(
                    self.face,
                    glyph_index,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as _,
                )
            };
        if res != 0 {
            None
        } else {
            // When: `res != 0` is false, FreeType successfully copied the glyph name into `buf`.
            Some(String::from_utf8_lossy(&buf).into_owned())
        }
    }

    /// Collects supported SFNT naming records grouped by their name identifier.
    pub fn get_sfnt_names(&self) -> HashMap<u32, Vec<NameRecord>> {
        let mut names = HashMap::new();
        let font_names = self.sfnt_name_records(|name_id| {
            matches!(
                name_id,
                TT_NAME_ID_TYPOGRAPHIC_FAMILY
                    | TT_NAME_ID_TYPOGRAPHIC_SUBFAMILY
                    | TT_NAME_ID_FONT_FAMILY
                    | TT_NAME_ID_FONT_SUBFAMILY
                    | TT_NAME_ID_PS_NAME
            )
        });
        for record in font_names {
            names.entry(u32::from(record.name_id)).or_insert_with(Vec::new).push(record);
        }
        names
    }

    /// Decodes the supported SFNT naming records whose name ID `keep` accepts, in name-table order.
    pub(super) fn sfnt_name_records(&self, keep: impl Fn(u32) -> bool) -> Vec<NameRecord> {
        let num_names =
            // SAFETY: `self.face` is a live FreeType face, the only query input.
            unsafe { FT_Get_Sfnt_Name_Count(self.face) };

        let mut records = Vec::new();

        let mut sfnt_name = FT_SfntName {
            platform_id: 0,
            encoding_id: 0,
            language_id: 0,
            name_id: 0,
            string: std::ptr::null_mut(),
            string_len: 0,
        };

        for name_index in 0..num_names {
            if
            // SAFETY: `self.face` is live; `sfnt_name` is writable output storage.
            unsafe { FT_Get_Sfnt_Name(self.face, name_index, &mut sfnt_name) } != 0 {
                // When: `FT_Get_Sfnt_Name(...) != 0`, this record could not be read.
                continue;
            }

            if sfnt_name.string.is_null() {
                // When: FreeType returned a record without string storage.
                continue;
            }

            if !keep(u32::from(sfnt_name.name_id)) {
                // When: `keep(name_id)` is false, the caller does not want this record.
                continue;
            }

            let bytes =
                // SAFETY: `string` names `string_len` readable face-owned bytes after success.
                unsafe {
                    from_raw_parts(sfnt_name.string as *const u8, sfnt_name.string_len as usize)
                };

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
                    // When: `(platform_id, encoding_id)` matches no supported decoder tuple.
                    log::trace!(
                        "Skipping name_id={} because platform_id={} encoding_id={}",
                        sfnt_name.name_id,
                        sfnt_name.platform_id,
                        sfnt_name.encoding_id
                    );
                    continue;
                }
            };

            let (name, _) = encoding.decode_with_bom_removal(bytes);

            records.push(NameRecord {
                platform_id: sfnt_name.platform_id,
                encoding_id: sfnt_name.encoding_id,
                name_id: sfnt_name.name_id,
                language_id: sfnt_name.language_id,
                name: name.to_string(),
            });
        }
        records
    }

    /// Returns the face-owned OpenType `OS/2` table when the font contains one.
    pub fn get_os2_table(&self) -> Option<&TT_OS2> {
        // SAFETY: `self.face` is live; a non-null table pointer returned by
        // FreeType has `TT_OS2` layout and remains owned by the face.
        unsafe {
            let table: *const TT_OS2 = FT_Get_Sfnt_Table(self.face, FT_Sfnt_Tag::FT_SFNT_OS2) as _;
            if table.is_null() {
                None
            } else {
                // When: `table.is_null()` is false, return this face's initialized `OS/2` table.
                Some(&*table)
            }
        }
    }

    /// Whether the face carries an OpenType `MATH` table.
    ///
    /// The table's presence is what distinguishes a math font from a text
    /// font that happens to cover mathematical codepoints: STIX Two Math has
    /// it, while STIX Two Text, Apple Symbols, and Menlo do not. Nothing in
    /// `OS/2` separates them — every one of those faces reports
    /// `sFamilyClass` 0/0 — so this table is the only structural signal
    /// available.
    ///
    /// It matters because a math font draws to the em square rather than to a
    /// text advance: its glyphs are sized for display equations, and dropping
    /// one into a terminal cell produces ink several times the cell's width.
    ///
    /// Queried with a zero-length read, which asks FreeType whether the table
    /// exists without loading any of it.
    pub fn has_math_table(&self) -> bool {
        let tag = ft_make_tag(b'M', b'A', b'T', b'H');
        let mut len: FT_ULong = 0;
        // SAFETY: `self.face` and `&mut len` are valid; a null buffer together
        // with zero initial length is FreeType's documented table-existence query.
        unsafe { succeeded(FT_Load_Sfnt_Table(self.face, tag, 0, ptr::null_mut(), &mut len)) }
    }

    /// Returns the cap_height/units_per_EM ratio if known
    pub fn cap_height(&self) -> Option<f64> {
        // SAFETY: `self.face` is live and `get_os2_table` returns storage owned
        // by that same face, so both records remain valid for this borrow.
        unsafe {
            let table = self.get_os2_table()?;
            let units_per_em = (*self.face).units_per_EM;
            if units_per_em == 0 || table.sCapHeight == 0 {
                // When: `units_per_em == 0 || table.sCapHeight == 0`, no ratio is usable.
                return None;
            }
            Some(table.sCapHeight as f64 / units_per_em as f64)
        }
    }

    /// Returns the face's weight and width classes after named-instance scaling.
    pub fn weight_and_width(&self) -> (u16, u16) {
        let (base_weight, base_width) = self
            .get_os2_table()
            .map(|table| (table.usWeightClass as f64, table.usWidthClass as f64))
            .unwrap_or((400., 5.));

        weight_and_width_with_variation(base_weight, base_width, self.variation_axis_scalings())
    }

    /// Collect the wght/wdth-relevant variation axis scalings for the named
    /// instance selected on this face. A FreeType error, null child pointer,
    /// empty axis table, out-of-range named-instance index, or missing axis
    /// coordinates is unusable and returns `Err`, causing the caller to retain
    /// the base OS/2/default metrics. Any non-null `FT_MM_Var` allocation is
    /// freed exactly once before returning, including FreeType's error path.
    fn variation_axis_scalings(&self) -> Result<Vec<AxisScaling>, ()> {
        // SAFETY: `self.face` and `self.library.lib` are live handles from the same
        // FreeType library; only a successful MM query is wrapped and read.
        unsafe {
            let index = (*self.face).face_index;
            let variation = index >> 16;
            if variation <= 0 {
                // When: face-index bits do not select a named variation instance.
                return Ok(Vec::new());
            }
            let vidx = (variation - 1) as usize;

            let mut mm_var_ptr = std::ptr::null_mut();
            if !succeeded(FT_Get_MM_Var(self.face, &mut mm_var_ptr)) {
                // When: `succeeded(FT_Get_MM_Var(...))` is false, `mm_var_ptr` has no usable
                // provenance.
                return Err(());
            }
            let guard = MmVarGuard::from_success(self.library.lib, mm_var_ptr).map_err(|_| ())?;
            let mm_var = guard.get();
            if mm_var.num_namedstyles == 0
                || mm_var.namedstyle.is_null()
                || mm_var.num_axis == 0
                || mm_var.axis.is_null()
            {
                // When: required named-style or variation-axis storage is absent.
                return Err(());
            }

            let styles = from_raw_parts(mm_var.namedstyle, mm_var.num_namedstyles as usize);
            let instance = styles.get(vidx).ok_or(())?;
            if instance.coords.is_null() {
                // When: the selected named instance has no coordinate array.
                return Err(());
            }

            let axes = from_raw_parts(mm_var.axis, mm_var.num_axis as usize);
            let coords = from_raw_parts(instance.coords, mm_var.num_axis as usize);
            Ok(axes
                .iter()
                .zip(coords.iter())
                .map(|(axis, &coord)| AxisScaling {
                    tag: axis.tag,
                    value: coord.to_num::<f64>(),
                    default_value: axis.def.to_num::<f64>(),
                })
                .collect())
        }
    }

    /// The horizontal advance of `character` in font units, or `None` when the face has no glyph
    /// for it or the glyph does not load.
    // `FT_Pos` stores `i64` on Unix but `i32` on Windows, so the `i64::from` is needed there.
    #[allow(clippy::useless_conversion)]
    pub fn unscaled_advance(&mut self, character: char) -> Option<i64> {
        let glyph_index =
            // SAFETY: `self.face` is a live, initialized face; the lookup only reads its charmap.
            unsafe { FT_Get_Char_Index(self.face, character as FT_ULong) };
        if glyph_index == 0 {
            // When: `glyph_index` is 0, the face has no glyph for `character`.
            return None;
        }
        // SAFETY: `self.face` is live and exclusively borrowed; an unscaled load writes only its
        // glyph slot, which is read before the borrow ends.
        unsafe {
            if !succeeded(FT_Load_Glyph(self.face, glyph_index, FT_LOAD_NO_SCALE as FT_Int32)) {
                // When: `FT_Load_Glyph` fails, the glyph has no advance to report.
                return None;
            }
            Some(i64::from((*(*self.face).glyph).advance.x.font_units()))
        }
    }

    /// Reports whether FreeType marks this face as italic.
    pub fn italic(&self) -> bool {
        // SAFETY: `self.face` is a live pointer to an initialized `FT_FaceRec_`.
        unsafe { ((*self.face).style_flags & FT_STYLE_FLAG_ITALIC as FT_Long) != 0 }
    }

    /// Computes the Unicode and symbol character coverage advertised by the face.
    pub fn compute_coverage(&self) -> RangeSet<u32> {
        if let Some(coverage) = self.source.coverage.as_ref() {
            // When: the locator already supplied authoritative cached coverage.
            return coverage.clone();
        }
        let mut coverage = RangeSet::new();

        for encoding in &[FT_Encoding::FT_ENCODING_UNICODE, FT_Encoding::FT_ENCODING_MS_SYMBOL] {
            if
            // SAFETY: `self.face` is live and `encoding` is a valid FreeType enum value.
            unsafe { FT_Select_Charmap(self.face, *encoding) } != 0 {
                // When: this face does not expose the requested charmap encoding.
                continue;
            }

            let mut glyph = 0;
            let mut ucs4 =
                // SAFETY: `self.face` is live and `glyph` is writable output storage.
                unsafe { FT_Get_First_Char(self.face, &mut glyph) };
            if glyph == 0 {
                // When: the selected charmap has no first mapped glyph.
                break;
            }
            let mut ucs4_range_start = ucs4;
            loop {
                let ucs4_new =
                    // SAFETY: the face is live and `glyph` is writable iterator output storage.
                    unsafe { FT_Get_Next_Char(self.face, ucs4, &mut glyph) };
                if glyph == 0 {
                    // When: `glyph == 0`, FreeType reached the end of the selected charmap.
                    break;
                }
                if ucs4_new - ucs4 != 1 {
                    coverage.add_range_unchecked(ucs4_range_start as u32..(ucs4 + 1) as u32);
                    ucs4_range_start = ucs4_new;
                }
                ucs4 = ucs4_new;
            }
            coverage.add_range_unchecked(ucs4_range_start as u32..(ucs4 + 1) as u32);

            if *encoding == FT_Encoding::FT_ENCODING_MS_SYMBOL {
                // Fontconfig duplicates F000..F0FF to 0000..00FF
                for ucs4 in 0xf00..0xf100 {
                    if coverage.contains(ucs4) {
                        coverage.add(ucs4 - 0xf000);
                    }
                }
            }
        }
        coverage
    }

    /// Returns the bitmap strike sizes in this font
    pub fn pixel_sizes(&self) -> Vec<u16> {
        let sizes =
            // SAFETY: `available_sizes` names `num_fixed_sizes` live face-owned records.
            unsafe {
                let rec = &(*self.face);
                from_raw_parts(rec.available_sizes, rec.num_fixed_sizes as usize)
            };
        sizes
            .iter()
            .filter_map(|info| {
                // When: `info.height > 0`, the strike height can be represented as `u16`.
                if info.height > 0 {
                    Some(info.height as u16)
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "ftwrap_names_tests.rs"]
mod ftwrap_names_tests;
