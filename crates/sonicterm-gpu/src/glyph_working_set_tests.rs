use super::*;

/// The packaged family, so the test does not depend on the host's fonts.
fn packaged_fonts() -> Vec<PathBuf> {
    vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")]
}

/// An ASCII fixture's working set holds palette-footer keys drawn at max(body − 1, 1) and
/// tab-title keys drawn at body + 1, so the footer is never measured at the tab-title size; the
/// grid's fast-path character keys are present too, and the set fits below the maximum.
#[test]
fn the_working_set_covers_the_footer_and_tab_title_at_their_own_sizes() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK.lock().unwrap();
    let size = 14.0;
    let set = measure_glyph_working_set(
        &["hello world"],
        &["shell", "build", "logs"],
        "Rec Mono St.Helens",
        size,
        72,
        &packaged_fonts(),
    )
    .expect("the packaged family loads");
    let has = |variant| set.tile_keys.iter().any(|key| key.raster_variant == variant);
    assert!(has(GlyphRasterVariant::PaletteFooter), "footer keys are measured");
    assert!(has(GlyphRasterVariant::TabTitle), "tab-title keys are measured");
    let size_of = |wanted| {
        set.variant_sizes.iter().find(|(variant, _)| *variant == wanted).map(|(_, pt)| *pt)
    };
    assert_eq!(size_of(GlyphRasterVariant::PaletteFooter), Some(f32::max(size - 1.0, 1.0)));
    assert_eq!(size_of(GlyphRasterVariant::TabTitle), Some(size + 1.0));
    assert_eq!(size_of(GlyphRasterVariant::Normal), Some(size));
    assert!(set.tile_keys.contains(&GlyphKey::new('h', false, false)), "fast-path key");
    assert!(set.tile_keys.contains(&GlyphKey::new('h', true, true)), "bold italic fast path");
    assert!(set.packed_pixels > 0);
    // A small ASCII fixture fits well below the maximum with a quarter of its height free.
    assert!(
        matches!(set.fit_outcome, FitOutcome::Fits(dim) if dim < ATLAS_DIM),
        "{:?} {:?} {}",
        set.fit_outcome,
        set.max_tile_dims,
        set.packed_pixels
    );
}
