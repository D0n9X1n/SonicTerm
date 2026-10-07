//! Unit tests for the pure variation-axis scaling helpers in `ftwrap`.
//!
//! These exercise the safe core of `Face::weight_and_width` without touching
//! FreeType: `scaled_weight_and_width` and `AxisScaling::scale`. The unsafe
//! `FT_Get_MM_Var`/`FT_Done_MM_Var` collection path is a link/build gate, not a
//! hollow unit test.

use super::*;

fn wght_tag() -> FT_ULong {
    ft_make_tag(b'w', b'g', b'h', b't')
}

fn wdth_tag() -> FT_ULong {
    ft_make_tag(b'w', b'd', b't', b'h')
}

fn axis(tag: FT_ULong, value: f64, default_value: f64) -> AxisScaling {
    AxisScaling { tag, value, default_value }
}

#[test]
fn no_scalings_returns_rounded_base() {
    // The non-variable path (and the OS/2 fallback of 400/5) must pass through
    // unchanged.
    assert_eq!(scaled_weight_and_width(400., 5., &[]), (400, 5));
}

#[test]
fn metadata_error_retains_rounded_base_metrics() {
    assert_eq!(weight_and_width_with_variation(400.4, 4.6, Err(())), (400, 5));
}

#[test]
fn usable_metadata_applies_axis_scaling() {
    let axes = vec![axis(wght_tag(), 700., 400.)];
    assert_eq!(weight_and_width_with_variation(400., 5., Ok(axes)), (700, 5));
}

#[test]
fn wght_axis_scales_only_weight() {
    // value/default = 700/400 = 1.75; 400 * 1.75 = 700. Width is untouched.
    let axes = [axis(wght_tag(), 700., 400.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (700, 5));
}

#[test]
fn wdth_axis_scales_only_width() {
    // value/default = 200/100 = 2.0; 5 * 2.0 = 10. Weight is untouched.
    let axes = [axis(wdth_tag(), 200., 100.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 10));
}

#[test]
fn wght_and_wdth_scale_independently() {
    // weight: 400 * (800/400 = 2.0) = 800
    // width:    5 * (75/100 = 0.75) = 3.75 -> rounds to 4
    let axes = [axis(wght_tag(), 800., 400.), axis(wdth_tag(), 75., 100.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (800, 4));
}

#[test]
fn zero_default_yields_neutral_scale() {
    // A zero axis default must not divide by zero; the scale is 1.0 so the base
    // weight/width are preserved.
    assert_eq!(axis(wght_tag(), 700., 0.).scale(), 1.);
    let axes = [axis(wght_tag(), 700., 0.), axis(wdth_tag(), 200., 0.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}

#[test]
fn unrelated_axes_are_ignored() {
    // ital/slnt/opsz carry real scales but must not affect weight or width.
    let ital = ft_make_tag(b'i', b't', b'a', b'l');
    let slnt = ft_make_tag(b's', b'l', b'n', b't');
    let opsz = ft_make_tag(b'o', b'p', b's', b'z');
    let axes = [axis(ital, 1., 0.5), axis(slnt, -10., 5.), axis(opsz, 8., 12.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}

#[test]
fn faces_retain_shared_library_ownership() {
    const SOURCE: &str = include_str!("ftwrap.rs");

    assert!(SOURCE.contains("struct LibraryInner"));
    assert!(SOURCE.contains("library: std::rc::Rc<LibraryInner>"));
    assert!(SOURCE.contains("library: std::rc::Rc::clone(&self.inner)"));
}

#[test]
fn face_remains_valid_after_creating_library_drops() {
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/fonts/RecMonoSt.Helens-Regular.ttf"),
        ),
        index: 0,
        variation: 0,
        origin: crate::locator::FontOrigin::FontDirs,
        coverage: None,
    };
    let face = {
        let library = Library::new().unwrap();
        library.face_from_locator(&handle).unwrap()
    };

    assert!(!face.family_name().is_empty());
    drop(face);
}

#[test]
fn bitmap_storage_checks_reject_empty_and_null_buffers() {
    assert!(checked_bitmap_buffer_len(std::ptr::null_mut(), 0, 0).is_err());
    assert!(checked_bitmap_buffer_len(std::ptr::null_mut(), 1, 1).is_err());

    let mut byte = 0u8;
    assert_eq!(checked_bitmap_buffer_len(&mut byte, 1, 1).unwrap(), 1);
}

#[test]
fn palette_storage_checks_reject_empty_and_null_buffers() {
    assert!(checked_palette_storage(std::ptr::null_mut::<FT_Color>(), 0).is_err());
    assert!(checked_palette_storage(std::ptr::null_mut::<FT_Color>(), 1).is_err());

    let mut color = MaybeUninit::<FT_Color>::uninit();
    assert_eq!(checked_palette_storage(color.as_mut_ptr(), 1).unwrap(), 1);

    // Required colour storage also rejects a non-null pointer with no entries.
    assert!(checked_palette_storage(color.as_mut_ptr(), 0).is_err());
}

/// Palette data over `names`, `flags` and `entry_names`, any of which may be absent (null).
fn palette_data(
    palette_count: FT_UShort,
    names: Option<&[FT_UShort]>,
    flags: Option<&[FT_UShort]>,
    entry_count: FT_UShort,
    entry_names: Option<&[FT_UShort]>,
) -> FT_Palette_Data {
    FT_Palette_Data {
        num_palettes: palette_count,
        palette_name_ids: names.map_or(std::ptr::null(), <[FT_UShort]>::as_ptr),
        palette_flags: flags.map_or(std::ptr::null(), <[FT_UShort]>::as_ptr),
        num_palette_entries: entry_count,
        palette_entry_name_ids: entry_names.map_or(std::ptr::null(), <[FT_UShort]>::as_ptr),
    }
}

/// Converts `data` with a name lookup that records every ID it is asked for.
fn convert(data: &FT_Palette_Data) -> (PaletteInfo, Vec<FT_UShort>) {
    let mut asked = Vec::new();
    let info =
        // SAFETY: every non-null array in `data` is a live test slice of exactly its count.
        unsafe {
            palettes_from_data(data, |name_id| {
                asked.push(name_id);
                format!("name-{name_id}")
            })
        };
    (info, asked)
}

#[test]
fn a_version_0_cpal_table_yields_one_palette_per_count() {
    // CPAL version 0 carries palettes but no label, flag or entry-label arrays, so FreeType leaves
    // all three null. Conversion must succeed, or every COLRv1 glyph of the face draws as tofu.
    let (info, asked) = convert(&palette_data(2, None, None, 5921, None));
    assert_eq!(info.num_palettes, 2);
    let summary: Vec<_> = info
        .palettes
        .iter()
        .map(|palette| (palette.palette_index, palette.flags, palette.name.as_str()))
        .collect();
    assert_eq!(summary, [(0, 0, ""), (1, 0, "")]);
    assert!(info.palettes.iter().all(|palette| palette.entry_names.is_empty()));
    // No name ID exists, so none is looked up; in particular, never name ID 0.
    assert!(asked.is_empty());
}

#[test]
fn each_optional_cpal_array_may_be_absent_on_its_own() {
    // Version 1 offsets are independent, so each array can be missing while the others are present.
    // Unequal, distinct values catch a swap of names and flags.
    let names: [FT_UShort; 2] = [256, 257];
    let flags: [FT_UShort; 2] = [1, 2];
    let entries: [FT_UShort; 3] = [300, 301, 302];
    let cases = [
        ("all present", Some(&names[..]), Some(&flags[..]), Some(&entries[..])),
        ("no names", None, Some(&flags[..]), Some(&entries[..])),
        ("no flags", Some(&names[..]), None, Some(&entries[..])),
        ("no entry labels", Some(&names[..]), Some(&flags[..]), None),
    ];
    for (label, name_ids, flag_values, entry_ids) in cases {
        let (info, _) = convert(&palette_data(2, name_ids, flag_values, 3, entry_ids));
        let expect_names = if name_ids.is_some() { ["name-256", "name-257"] } else { ["", ""] };
        let expect_flags = if flag_values.is_some() { [1, 2] } else { [0, 0] };
        let expect_entries: Vec<String> =
            entry_ids.map_or(Vec::new(), |ids| ids.iter().map(|id| format!("name-{id}")).collect());
        assert_eq!(info.palettes.len(), 2, "{label}");
        for (palette_index, palette) in info.palettes.iter().enumerate() {
            assert_eq!(palette.palette_index, palette_index, "{label}");
            assert_eq!(palette.name, expect_names[palette_index], "{label}");
            assert_eq!(palette.flags, expect_flags[palette_index], "{label}");
            assert_eq!(palette.entry_names, expect_entries, "{label}");
        }
    }
}

#[test]
fn zero_palettes_or_entries_read_as_empty() {
    // Zero counts expose no elements and trigger no name lookup, even with non-null pointers.
    let names: [FT_UShort; 1] = [256];
    let (info, asked) =
        convert(&palette_data(0, Some(&names[..]), Some(&names[..]), 0, Some(&names[..])));
    assert_eq!(info.num_palettes, 0);
    assert!(info.palettes.is_empty());
    assert!(asked.is_empty());
}

/// `source` with every comment (line, block, nested block and doc) and every string, raw string and character
/// literal replaced by spaces of the same byte length; line breaks are kept, so offsets and lines still match
/// and a search over it sees only code. A lifetime such as `'a` is code and is kept.
fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut masked = bytes.to_vec();
    let blank = |masked: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut masked[from..to.min(bytes.len())] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let identifier = |at: usize| bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_';
    let mut index = 0;
    while index < bytes.len() {
        let next = bytes.get(index + 1).copied();
        let end = match bytes[index] {
            b'/' if next == Some(b'/') => {
                source[index..].find('\n').map_or(bytes.len(), |at| index + at)
            }
            b'/' if next == Some(b'*') => {
                // Block comments nest in Rust, so the comment ends where its depth returns to zero.
                let (mut depth, mut cursor) = (0_usize, index);
                while cursor < bytes.len() {
                    if bytes[cursor..].starts_with(b"/*") {
                        depth += 1;
                        cursor += 2;
                    } else if bytes[cursor..].starts_with(b"*/") {
                        depth -= 1;
                        cursor += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        cursor += 1;
                    }
                }
                cursor
            }
            // A raw string, raw byte string (`br`) or raw C string (`cr`), the prefix letter standing alone.
            b'r' if (index == 0
                || !identifier(index - 1)
                || (matches!(bytes[index - 1], b'b' | b'c')
                    && (index < 2 || !identifier(index - 2))))
                && matches!(next, Some(b'"' | b'#')) =>
            {
                let hashes = bytes[index + 1..].iter().take_while(|byte| **byte == b'#').count();
                if bytes.get(index + 1 + hashes) != Some(&b'"') {
                    // When: `r#` is not followed by a quote, it is a raw identifier, which is code.
                    index += 1;
                    continue;
                }
                let closing = format!("\"{}", "#".repeat(hashes));
                let body = index + 2 + hashes;
                source[body..].find(&closing).map_or(bytes.len(), |at| body + at + closing.len())
            }
            b'"' => {
                let mut cursor = index + 1;
                while cursor < bytes.len() && bytes[cursor] != b'"' {
                    cursor += if bytes[cursor] == b'\\' { 2 } else { 1 };
                }
                cursor + 1
            }
            b'\'' if next == Some(b'\\') => {
                // An escaped character literal: the escape's own character is skipped, then the closing quote.
                source[index + 3..].find('\'').map_or(bytes.len(), |at| index + 3 + at + 1)
            }
            b'\'' => {
                let width = source[index + 1..].chars().next().map_or(0, char::len_utf8);
                if width > 0 && bytes.get(index + 1 + width) == Some(&b'\'') {
                    index + 2 + width
                } else {
                    // When: no closing quote follows one character, this is a lifetime, which is code.
                    index += 1;
                    continue;
                }
            }
            _ => {
                index += 1;
                continue;
            }
        };
        blank(&mut masked, index, end);
        index = end.max(index + 1);
    }
    String::from_utf8(masked).expect("blanking whole characters keeps UTF-8")
}

/// The body of the item starting at `head` in `source`, searched and returned as code only (`code_only`, read
/// as LF) and ending at its first closing brace at the item's own indentation, so neither a comment nor a
/// string counts. The signature must occur exactly once in code: a second copy, in an unused macro or a
/// cfg-disabled impl, makes the source ambiguous, and the pin fails closed rather than guess which one compiles.
fn item_body(source: &str, head: &str) -> Result<String, String> {
    let code = code_only(&source.replace("\r\n", "\n"));
    let starts: Vec<usize> = code.match_indices(head).map(|(at, _)| at).collect();
    let &[start] = starts.as_slice() else {
        return Err(if starts.is_empty() {
            format!("{head} is not defined")
        } else {
            format!("ambiguous source: {head} occurs {} times", starts.len())
        });
    };
    let line_start = code[..start].rfind('\n').map_or(0, |at| at + 1);
    let indent = &code[line_start..start];
    let rest = &code[start..];
    let end =
        rest.find(&format!("\n{indent}}}\n")).ok_or_else(|| format!("{head} does not end"))?;
    Ok(rest[..end].to_owned())
}

/// The bodies of `source`'s get_palette_data and palette_info_from, or why either cannot be read unambiguously.
fn palette_bodies(source: &str) -> Result<(String, String), Vec<String>> {
    match (
        item_body(source, "pub fn get_palette_data("),
        item_body(source, "unsafe fn palette_info_from("),
    ) {
        (Ok(query), Ok(wiring)) => Ok((query, wiring)),
        (query, wiring) => Err([query.err(), wiring.err()].into_iter().flatten().collect()),
    }
}

/// Why `source`'s palette wiring is not the name-ID route, or empty: get_palette_data must hand every name
/// record to `palette_info_from` and never look a label up through `get_sfnt_name` or the five-ID
/// `get_sfnt_names`; `palette_info_from` must resolve through the lazy `NameIdLabels` and `palettes_from_data`.
fn palette_wiring_problems(source: &str) -> Vec<String> {
    let (query, wiring) = match palette_bodies(source) {
        Ok(bodies) => bodies,
        Err(problems) => return problems,
    };
    let mut problems: Vec<&str> = Vec::new();
    if !query.contains("palette_info_from(&data, || self.sfnt_name_records(|_| true))") {
        problems.push("get_palette_data does not hand every name record to palette_info_from");
    }
    if query.contains("get_sfnt_name(") || query.contains("get_sfnt_names(") {
        problems.push("get_palette_data looks labels up by record index or the five font-name IDs");
    }
    if !wiring.contains("NameIdLabels::lazy(records)") || !wiring.contains("labels.label(name_id)")
    {
        problems.push("palette_info_from does not resolve through the lazy name-ID labels");
    }
    if !wiring.contains("palettes_from_data(data,") {
        problems.push("palette_info_from does not convert through palettes_from_data");
    }
    if query.contains("checked_palette_storage") || wiring.contains("checked_palette_storage") {
        problems.push("the optional palette arrays are held to the required-storage check");
    }
    problems.into_iter().map(str::to_owned).collect()
}

/// Why `source`'s palette query would hold the optional CPAL arrays to the required-storage check, or empty:
/// get_palette_data converts through `palette_info_from`, which converts through `palettes_from_data`, and
/// neither applies `checked_palette_storage`. Read as code only and unambiguously, like the wiring pin.
fn optional_metadata_problems(source: &str) -> Vec<String> {
    let (query, wiring) = match palette_bodies(source) {
        Ok(bodies) => bodies,
        Err(problems) => return problems,
    };
    let mut problems = Vec::new();
    if !query.contains("palette_info_from(&data,") {
        problems.push("get_palette_data does not convert through palette_info_from".to_owned());
    }
    if !wiring.contains("palettes_from_data(data,") {
        problems.push("palette_info_from does not convert through palettes_from_data".to_owned());
    }
    if query.contains("checked_palette_storage") || wiring.contains("checked_palette_storage") {
        problems
            .push("the optional palette arrays are held to the required-storage check".to_owned());
    }
    problems
}

/// get_palette_data's labels come from every name record, resolved by name ID: the record source keeps every
/// ID (not the five font-name IDs) and goes through `palette_info_from`, never a lookup by record index.
#[test]
fn palette_labels_read_every_name_record_by_name_id() {
    assert_eq!(palette_wiring_problems(include_str!("ftwrap.rs")), Vec::<String>::new());
}

/// A decoy in an earlier comment or string never stands in for the method: with the old record-index callback
/// in the real get_palette_data and a method-shaped copy of the new call above it in a block comment, a line
/// comment, a doc comment and a string literal, the pin still fails; the same source with the real call passes.
#[test]
fn a_commented_or_quoted_decoy_never_satisfies_the_palette_pin() {
    let decoy = "/* pub fn get_palette_data(&self) {\n        Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))\n    }\n*/\n\
                 // pub fn get_palette_data(&self) { palette_info_from(&data, || self.sfnt_name_records(|_| true)) }\n\
                 /// pub fn get_palette_data(&self) {\n\
                 const TEXT: &str = \"    pub fn get_palette_data(&self) {\\n        palette_info_from(&data, || self.sfnt_name_records(|_| true))\\n    }\\n\";\n";
    let wiring = "unsafe fn palette_info_from(data: &FT_Palette_Data) -> PaletteInfo {\n    let labels = crate::parser::NameIdLabels::lazy(records);\n    unsafe { palettes_from_data(data, |name_id| labels.label(name_id)) }\n}\n";
    let query = |call: &str| {
        format!("impl Face {{\n    pub fn get_palette_data(&self) -> anyhow::Result<PaletteInfo> {{\n        {call}\n    }}\n}}\n")
    };
    let old_call = "Ok(palettes_from_data(&data, |name_id| self.get_sfnt_name(name_id as _).map(|rec| rec.name).unwrap_or_default()))";
    let new_call = "Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))";
    let fooled = format!("{decoy}{}{wiring}", query(old_call));
    assert!(
        fooled.find("pub fn get_palette_data(").unwrap()
            < fooled.rfind("pub fn get_palette_data(").unwrap()
    );
    assert!(!palette_wiring_problems(&fooled).is_empty(), "a decoy satisfied the pin");
    assert_eq!(
        palette_wiring_problems(&format!("{decoy}{}{wiring}", query(new_call))),
        Vec::<String>::new()
    );
    assert_eq!(
        code_only("let lifetime: &'static str = \"x\"; let quote = '\\''; let raw = r#\"//\"#;"),
        "let lifetime: &'static str =    ; let quote =     ; let raw =        ;"
    );
}

/// A CPAL table without label arrays never reads the name table, one whose labels are all 0xFFFF never reads
/// it either, and one with labels reads it exactly once for the whole query, resolving each label by name ID.
#[test]
fn palette_labels_read_the_name_table_at_most_once_and_only_when_needed() {
    let reads = std::cell::Cell::new(0_u32);
    let record = |name_id, name: &str| NameRecord {
        platform_id: TT_PLATFORM_MICROSOFT as u16,
        encoding_id: 0,
        language_id: 0x409,
        name_id,
        name: name.to_owned(),
    };
    let source = || {
        reads.set(reads.get() + 1);
        vec![record(1, "Family"), record(256, "Light"), record(257, "Dark"), record(258, "Ink")]
    };
    let flags = [0_u16; 2];
    let unlabelled = palette_data(2, None, Some(&flags), 3, None);
    let info =
        // SAFETY: every non-null array in `unlabelled` is a live test slice of exactly its count.
        unsafe { palette_info_from(&unlabelled, source) };
    assert_eq!((info.palettes.len(), reads.get()), (2, 0), "no label array, no name-table read");
    let sentinel_names = [0xFFFF_u16, 0xFFFF];
    let sentinel_only = palette_data(2, Some(&sentinel_names), None, 0, None);
    let info =
        // SAFETY: every non-null array in `sentinel_only` is a live test slice of exactly its count.
        unsafe { palette_info_from(&sentinel_only, source) };
    assert_eq!(
        (info.palettes[0].name.as_str(), reads.get()),
        ("", 0),
        "0xFFFF labels read nothing"
    );
    let names = [256_u16, 257];
    let entries = [258_u16, 0xFFFF];
    let labelled = palette_data(2, Some(&names), None, 2, Some(&entries));
    let info =
        // SAFETY: every non-null array in `labelled` is a live test slice of exactly its count.
        unsafe { palette_info_from(&labelled, source) };
    assert_eq!(reads.get(), 1, "one read for every label of the query");
    assert_eq!((info.palettes[0].name.as_str(), info.palettes[1].name.as_str()), ("Light", "Dark"));
    assert_eq!(info.palettes[0].entry_names, ["Ink", ""]);
}

#[test]
fn palette_data_reads_its_optional_metadata_without_requiring_it() {
    // The required-storage check guards the selected palette's colours only; get_palette_data must
    // convert through `palette_info_from`, which converts through `palettes_from_data`, and neither may
    // apply that check to the optional arrays. Read as code only and unambiguously, so no comment, string,
    // macro or disabled impl can stand in.
    assert_eq!(optional_metadata_problems(include_str!("ftwrap.rs")), Vec::<String>::new());
}

/// An unused macro, a cfg-disabled impl, a raw C string or a plain C string never stands in for the method. With
/// the old record-index callback in the real get_palette_data, each decoy, before or after it, leaves both
/// palette pins failing: a second definition in code is an ambiguous source, and a C string's contents are a
/// literal, masked away. The real source with the new call and no decoy passes both.
#[test]
fn a_macro_disabled_impl_or_c_string_decoy_never_satisfies_the_palette_pins() {
    let macro_decoy = r###"macro_rules! decoy {
    () => {
        impl Face {
            pub fn get_palette_data(&self) -> anyhow::Result<PaletteInfo> {
                Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))
            }
        }
    };
}
"###;
    let disabled_impl = r###"#[cfg(any())]
impl Face {
    pub fn get_palette_data(&self) -> anyhow::Result<PaletteInfo> {
        Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))
    }
}
"###;
    let raw_c_string = r###"const DECOY: &std::ffi::CStr = cr##"prefix "
    pub fn get_palette_data(&self) -> anyhow::Result<PaletteInfo> {
        Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))
    }
" suffix"##;
"###;
    let plain_c_string = r###"const PLAIN: &std::ffi::CStr = c"\n    pub fn get_palette_data(&self) -> anyhow::Result<PaletteInfo> {\n        Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))\n    }\n";
"###;
    for (label, literal) in [("raw C string", raw_c_string), ("plain C string", plain_c_string)] {
        let masked = code_only(literal);
        assert!(
            masked.starts_with("const "),
            "{label}: the item around the literal stays code: {masked}"
        );
        assert!(
            !masked.contains("get_palette_data") && !masked.contains("palette_info_from"),
            "{label}: the literal's contents are masked: {masked}"
        );
    }
    let wiring = "unsafe fn palette_info_from(data: &FT_Palette_Data) -> PaletteInfo {\n    let labels = crate::parser::NameIdLabels::lazy(records);\n    unsafe { palettes_from_data(data, |name_id| labels.label(name_id)) }\n}\n";
    let real = |call: &str| {
        format!("impl Face {{\n    pub fn get_palette_data(&self) -> anyhow::Result<PaletteInfo> {{\n        {call}\n    }}\n}}\n{wiring}")
    };
    let old_call = "Ok(palettes_from_data(&data, |name_id| self.get_sfnt_name(name_id as _).map(|rec| rec.name).unwrap_or_default()))";
    let new_call = "Ok(palette_info_from(&data, || self.sfnt_name_records(|_| true)))";
    for (label, decoy) in [
        ("macro", macro_decoy),
        ("disabled impl", disabled_impl),
        ("raw C string", raw_c_string),
        ("plain C string", plain_c_string),
    ] {
        for (place, source) in [
            ("before", format!("{decoy}{}", real(old_call))),
            ("after", format!("{}{decoy}", real(old_call))),
        ] {
            let wiring_problems = palette_wiring_problems(&source);
            let metadata_problems = optional_metadata_problems(&source);
            assert!(!wiring_problems.is_empty(), "{label} {place}: the wiring pin passed");
            assert!(
                !metadata_problems.is_empty(),
                "{label} {place}: the optional-metadata pin passed"
            );
            if matches!(label, "macro" | "disabled impl") {
                let ambiguous = |problems: &[String]| {
                    problems.iter().any(|problem| problem.starts_with("ambiguous source"))
                };
                assert!(
                    ambiguous(&wiring_problems) && ambiguous(&metadata_problems),
                    "{label} {place}: {wiring_problems:?}"
                );
            }
        }
    }
    assert_eq!(palette_wiring_problems(&real(new_call)), Vec::<String>::new());
    assert_eq!(optional_metadata_problems(&real(new_call)), Vec::<String>::new());
}

#[test]
fn mm_var_cleanup_and_colr_provenance_are_explicit() {
    const SOURCE: &str = include_str!("ftwrap.rs");

    assert!(SOURCE.contains("struct MmVarGuard"));
    assert!(SOURCE.contains("pub(crate) unsafe fn get_paint"));
    assert!(SOURCE.contains("pub(crate) unsafe fn get_paint_layers"));
}

#[test]
fn disk_streams_use_callback_io_and_size_proofs_name_the_real_initializer() {
    const SOURCE: &str = include_str!("ftwrap.rs");
    const RASTERIZER: &str = include_str!("rasterizer/freetype.rs");

    assert!(!SOURCE.contains("MmapOptions"));
    assert!(!SOURCE.contains("StreamBacking::Map"));
    assert!(!RASTERIZER.contains("face_from_locator set a character size"));
}

#[test]
fn bitmap_preflight_loads_metrics_without_rendering_pixels() {
    let flags = bitmap_metrics_preflight_flags(FT_LOAD_RENDER as FT_Int32);

    assert_ne!(flags & FT_LOAD_BITMAP_METRICS_ONLY as FT_Int32, 0);
    assert_ne!(flags & FT_LOAD_NO_SVG as FT_Int32, 0);
    assert_eq!(flags & FT_LOAD_RENDER as FT_Int32, 0);
}

#[test]
fn unrelated_axis_does_not_leak_into_weight_or_width() {
    // A mix: only the wght axis applies; the optical-size axis is inert.
    let opsz = ft_make_tag(b'o', b'p', b's', b'z');
    let axes = [axis(opsz, 8., 12.), axis(wght_tag(), 600., 400.)];
    // 400 * (600/400 = 1.5) = 600; width stays 5.
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (600, 5));
}

#[test]
fn scaling_rounds_half_away_from_zero() {
    // 401 * (3/2 = 1.5) = 601.5 -> rounds up to 602.
    let axes = [axis(wght_tag(), 3., 2.)];
    assert_eq!(scaled_weight_and_width(401., 5., &axes), (602, 5));
}

#[test]
fn scaling_rounds_fraction_down() {
    // 400 * (1001/1000 = 1.001) = 400.4 -> rounds down to 400.
    let axes = [axis(wght_tag(), 1001., 1000.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}

#[test]
fn identity_scale_leaves_base_unchanged() {
    // value == default => scale 1.0 for both axes.
    assert_eq!(axis(wght_tag(), 400., 400.).scale(), 1.);
    let axes = [axis(wght_tag(), 400., 400.), axis(wdth_tag(), 100., 100.)];
    assert_eq!(scaled_weight_and_width(400., 5., &axes), (400, 5));
}
