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
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// The palette footer and each command's detail row draw `·` separators in the footer's own
/// strike, so the helper must measure that symbol as a `PaletteFooter` key, or the real renderer
/// holds a footer key the helper never counted.
#[test]
fn the_working_set_covers_the_footer_separator() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let set = measure_glyph_working_set(
        &["hello"],
        &["shell"],
        "Rec Mono St.Helens",
        14.0,
        72,
        &packaged_fonts(),
    )
    .expect("the packaged family loads");
    assert!(
        set.tile_keys
            .iter()
            .any(|key| key.raster_variant == GlyphRasterVariant::PaletteFooter && key.ch == '·'),
        "the footer separator is measured in the footer strike"
    );
}

/// A character only a fallback face covers is measured as that face's real glyph on a cold stack:
/// the helper waits for fallback discovery instead of caching the frame path's tofu, so the
/// resident set holds the fallback slot's shaped key with a nonempty raster, in every style.
#[test]
fn a_cold_measurement_holds_the_fallback_face_glyph_not_tofu() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The primary face lacks é and the fixture's locator answers every fallback request with
    // Rec Mono, so é resolves only once the fallback worker publishes that face.
    let fixture = crate::lib_tests::fallback_stack("working-set-cold");
    let size = 14.0;
    let stacks = crate::core::renderer_font_views(Some(fixture.stack.clone()), size);
    let set =
        measure_with_stacks(&["é"], &["shell"], stacks, size, 96).expect("the fixture stack loads");
    for (bold, italic) in STYLES {
        let fallback = set.tile_keys.iter().find(|key| {
            key.ch == 'é'
                && key.raster_variant == GlyphRasterVariant::Normal
                && key.weight_bold == bold
                && key.italic == italic
                && key.glyph_id != 0
                && key.font_slot != 0
        });
        let key = fallback.unwrap_or_else(|| {
            panic!("é (bold {bold}, italic {italic}) is not a resident fallback glyph")
        });
        let [width_px, height_px] = set.tile_sizes[key];
        assert!(width_px > 0 && height_px > 0, "{key:?} has a real raster, not tofu");
    }
}

/// Answers every fallback request with one system face: the platform's color-emoji face.
#[cfg(any(target_os = "macos", target_os = "windows"))]
struct SystemEmojiLocator;

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl sonicterm_font::locator::FontLocator for SystemEmojiLocator {
    fn load_fonts(
        &self,
        _: &[config::FontAttributes],
        _: &mut std::collections::HashSet<config::FontAttributes>,
        _: u16,
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        Ok(Vec::new())
    }

    fn locate_fallback_for_codepoints(
        &self,
        _: &[char],
    ) -> anyhow::Result<Vec<sonicterm_font::parser::ParsedFont>> {
        use sonicterm_font::locator::{FontDataHandle, FontDataSource, FontOrigin};
        let path = if cfg!(target_os = "macos") {
            "/System/Library/Fonts/Apple Color Emoji.ttc"
        } else {
            // When: the platform is Windows, Segoe UI Emoji is its system color-emoji face.
            "C:\\Windows\\Fonts\\seguiemj.ttf"
        };
        let handle = FontDataHandle {
            source: FontDataSource::OnDisk(PathBuf::from(path)),
            index: 0,
            variation: 0,
            origin: FontOrigin::BuiltIn,
            coverage: None,
        };
        Ok(vec![sonicterm_font::parser::ParsedFont::from_locator(&handle)?])
    }
}

/// Representative CJK and emoji text, measured cold, is resident as real shaped glyphs with
/// nonempty rasters in the body strike, not as tofu. The packaged family covers CJK itself
/// (slot 0); emoji resolve only through the fallback worker, which the locator answers with the
/// platform's system color-emoji face (a nonzero slot), so the host's user-installed fonts cannot
/// change the outcome. Linux runners ship no color-emoji face, so the test is macOS and Windows.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn a_cold_measurement_holds_real_cjk_and_emoji_tiles() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let size = 14.0;
    let body = sonicterm_engine::FontStack::try_new_with_locator_for_test(
        "Rec Mono St.Helens",
        packaged_fonts(),
        std::sync::Arc::new(SystemEmojiLocator),
        f64::from(size),
        72,
    )
    .expect("the packaged family loads");
    let stacks = crate::core::renderer_font_views(Some(body), size);
    let set = measure_with_stacks(&["中文 漢字 \u{1F600}\u{1F680}"], &["shell"], stacks, size, 72)
        .expect("the packaged family loads");
    // (character, whether only a fallback face covers it)
    for (character, needs_fallback) in
        [('中', false), ('漢', false), ('\u{1F600}', true), ('\u{1F680}', true)]
    {
        let measured = set.tile_keys.iter().find(|key| {
            key.ch == character
                && key.raster_variant == GlyphRasterVariant::Normal
                && key.glyph_id != 0
                && (key.font_slot != 0) == needs_fallback
        });
        let key = measured.unwrap_or_else(|| {
            let held: Vec<_> = set.tile_keys.iter().filter(|key| key.ch == character).collect();
            panic!("{character:?} is not a resident real glyph; held {held:?}")
        });
        let [width_px, height_px] = set.tile_sizes[key];
        assert!(width_px > 0 && height_px > 0, "{key:?} has a real raster, not tofu");
    }
}

/// Shape `character` with `stack` (waiting for fallback), insert its shaped key into `atlas` with
/// `stack` as the rasterizer, and return the key with its tile identity.
fn shaped_tile(
    stack: &sonicterm_engine::FontStack,
    atlas: &mut GlyphAtlas,
    character: char,
    variant: GlyphRasterVariant,
) -> (GlyphKey, Option<TileIdentity>) {
    let shaped = stack.shape_text_with_style(&character.to_string(), false, false).unwrap();
    let glyph = shaped.iter().find(|glyph| glyph.glyph_pos != 0).expect("a real glyph");
    let key = GlyphKey::shaped(
        character,
        u8::try_from(glyph.font_idx).unwrap(),
        glyph.glyph_pos,
        false,
        false,
    )
    .with_raster_variant(variant);
    let mut raster = stack.clone();
    let info = atlas.get_or_insert(key, &mut raster).expect("the tile is placed");
    (key, tile_identity(stack, key, &info))
}

/// A tile's identity names its resolved face, glyph, strike and flags, never the configuration-local
/// font slot: é drawn from Rec Mono as a fallback (slot 1 behind a face lacking it) and as the
/// primary face (slot 0) is one identity, while another strike or raster variant is another.
#[test]
fn a_tile_identity_is_the_same_face_and_strike_whatever_slot_holds_it() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = crate::lib_tests::fallback_stack("tile-identity");
    let primary = |size| {
        sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
            &[("Rec Mono St.Helens", false)],
            packaged_fonts(),
            size,
            96,
            1.0,
        )
        .unwrap()
    };
    let mut atlas = GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM);
    let (fallback_key, fallback_identity) =
        shaped_tile(&fixture.stack, &mut atlas, 'é', GlyphRasterVariant::Normal);
    let (primary_key, primary_identity) =
        shaped_tile(&primary(14.0), &mut atlas, 'é', GlyphRasterVariant::Normal);
    assert_ne!(fallback_key.font_slot, primary_key.font_slot, "the slots differ");
    let identity = fallback_identity.expect("the fallback key resolves");
    assert_eq!(Some(&identity), primary_identity.as_ref(), "one face, glyph and strike");
    assert!(identity.source.face.source.ends_with("RecMonoSt.Helens-Regular.ttf"), "{identity:?}");
    let (_, larger) = shaped_tile(&primary(15.0), &mut atlas, 'é', GlyphRasterVariant::Normal);
    assert_ne!(larger.as_ref(), Some(&identity), "another strike is another tile");
    let (_, tab_title) = shaped_tile(&primary(14.0), &mut atlas, 'é', GlyphRasterVariant::TabTitle);
    assert_ne!(tab_title.as_ref(), Some(&identity), "another raster variant is another tile");
}
