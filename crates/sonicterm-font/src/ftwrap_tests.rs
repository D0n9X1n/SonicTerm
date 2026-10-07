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

#[test]
fn palette_data_reads_its_optional_metadata_without_requiring_it() {
    // The required-storage check guards the selected palette's colours only; get_palette_data must
    // convert through `palettes_from_data` and never apply that check to the optional arrays.
    const SOURCE: &str = include_str!("ftwrap.rs");
    let body = SOURCE
        .split("pub fn get_palette_data(")
        .nth(1)
        .and_then(|rest| rest.split("\n    pub fn ").next())
        .expect("get_palette_data is defined");
    assert!(body.contains("palettes_from_data(&data,"));
    assert!(!body.contains("checked_palette_storage"));
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
