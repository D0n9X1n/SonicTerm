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
    let set = measure_with_stacks(&["é"], &["shell"], stacks, size, 96, blocking_warm_up)
        .expect("the fixture stack loads");
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
    let set = measure_with_stacks(
        &["中文 漢字 \u{1F600}\u{1F680}"],
        &["shell"],
        stacks,
        size,
        72,
        blocking_warm_up,
    )
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

/// A warm-up that cannot shape a source is a failed measurement, never a silent fall-through to the
/// frame path: the helper names the source and style whose warm-up failed instead of classifying
/// whatever that style's frame-path layout happened to place.
#[test]
fn a_failing_warm_up_rejects_the_measurement() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let size = 14.0;
    let stacks = renderer_font_stacks("Rec Mono St.Helens", size, 72, 1.0, &packaged_fonts());
    let failing_bold = |stack: &sonicterm_engine::FontStack, text: &str, bold, italic| {
        if bold {
            // When: the style is bold, the fake shaper fails as a broken face would.
            anyhow::bail!("the bold face cannot shape")
        }
        blocking_warm_up(stack, text, bold, italic)
    };
    let outcome = measure_with_stacks(&["hello"], &["shell"], stacks, size, 72, failing_bold);
    assert!(
        matches!(
            outcome,
            Err(WorkingSetError::WarmUp {
                variant: GlyphRasterVariant::Normal,
                bold: true,
                italic: false,
                ..
            })
        ),
        "{outcome:?}"
    );
}

/// A warm-up that reports a real glyph for a cluster the frame path still draws as notdef did not
/// wait for that fallback face, so the atlas would hold tofu for a glyph the renderer later draws
/// for real: the measurement is rejected rather than counted. The warm-up here answers from the
/// packaged family, which covers é, so it never asks the cold fixture stack to discover its fallback.
#[test]
fn a_fallback_the_warm_up_did_not_wait_for_rejects_the_measurement() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = crate::lib_tests::fallback_stack("working-set-pending");
    let packaged = sonicterm_engine::FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        packaged_fonts(),
        14.0,
        96,
        1.0,
    )
    .expect("the packaged family loads");
    let size = 14.0;
    let stacks = crate::core::renderer_font_views(Some(fixture.stack.clone()), size);
    let not_waiting = |_stack: &sonicterm_engine::FontStack, text: &str, bold, italic| {
        blocking_warm_up(&packaged, text, bold, italic)
    };
    let outcome = measure_with_stacks(&["é"], &["shell"], stacks, size, 96, not_waiting);
    assert!(
        matches!(
            outcome,
            Err(WorkingSetError::FallbackPending {
                variant: GlyphRasterVariant::Normal,
                bold: false,
                italic: false,
                character: 'é',
            })
        ),
        "{outcome:?}"
    );
}

/// Answers every key with no raster, as a face whose glyph cannot be drawn does.
struct NoRaster;

impl sonicterm_text::glyph_atlas::Rasterizer for NoRaster {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        None
    }
}

/// A resolved glyph that rasterizes to nothing is drawn by the renderer as the same tofu box, so
/// the helper lists it as a raster failure (and a character with no face as unresolved) instead
/// of rejecting the measurement; a required tile the atlas never placed, with no eviction to
/// explain it, is still a rejection.
#[test]
fn raster_failures_are_listed_and_unplaced_tiles_are_rejected() {
    let mut atlas = GlyphAtlas::new(64, 64);
    let shaped = GlyphKey::shaped('\u{1F1EF}', 2, 2687, false, false);
    let notdef = GlyphKey::with_slot('\u{E000}', 0, false, false);
    let _shaped_info = atlas.get_or_insert(shaped, &mut NoRaster);
    let _notdef_info = atlas.get_or_insert(notdef, &mut NoRaster);
    let mut accounted = Accounted::default();
    account_tile(&atlas, shaped, &mut accounted).expect("a raster failure is listed");
    account_tile(&atlas, notdef, &mut accounted).expect("an uncovered character is listed");
    assert_eq!(accounted.raster_failed, HashSet::from([shaped]));
    assert_eq!(accounted.unresolved_chars, BTreeSet::from(['\u{E000}']));
    let never_inserted = GlyphKey::new('q', false, false);
    assert_eq!(
        account_tile(&atlas, never_inserted, &mut accounted),
        Err(WorkingSetError::NotPlaced { key: never_inserted })
    );
}

/// Answers every key with a 3x1 colour tile whose alphas are 0, 128 and 255, premultiplied.
struct RampColour;

impl sonicterm_text::glyph_atlas::Rasterizer for RampColour {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 3,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance: 3.0,
            coverage: vec![0, 0, 0, 0, 64, 32, 16, 128, 255, 128, 0, 255],
            is_color: true,
            is_subpixel: false,
        })
    }
}

/// The colour-path check reads the selected colour tile's own pixels from the atlas: a resident
/// colour tile of the character is found and its alpha census counts the transparent, translucent
/// (antialiased edge) and opaque pixels inside its rectangle only. A coverage tile of the same
/// character, or a character with no tile, is not a colour tile.
#[test]
fn a_resident_colour_tile_reports_its_alpha_census() {
    let mut atlas = GlyphAtlas::new(64, 64);
    // A coverage neighbour placed first, so the colour tile does not sit at the atlas origin.
    let _coverage = atlas.get_or_insert(
        GlyphKey::with_slot('a', 0, false, false),
        &mut sonicterm_text::glyph_atlas::SyntheticRasterizer::default(),
    );
    let colour_key = GlyphKey::shaped('\u{1F600}', 1, 42, false, false);
    let _colour = atlas.get_or_insert(colour_key, &mut RampColour);
    let census = colour_tile_alpha(&atlas, '\u{1F600}').expect("a resident colour tile");
    assert_eq!(census.key, colour_key);
    assert_eq!(
        (census.transparent, census.translucent, census.opaque),
        (1, 1, 1),
        "only the tile's own three pixels are counted"
    );
    assert!(colour_tile_alpha(&atlas, 'a').is_none(), "a coverage tile is not a colour tile");
    assert!(colour_tile_alpha(&atlas, 'z').is_none(), "no tile at all");
}

/// A tab draws its program icon in the tab-title strike even when no fixture title holds it (the
/// generic shell glyph U+F489 when a pane reports no process or directory), so the helper's set
/// must hold every icon the tab-title mapping can return, as a real glyph at scale 1 and 2.
#[test]
fn the_tab_title_set_holds_every_program_icon_at_both_scales() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for dpi in [72, 144] {
        let set = measure_glyph_working_set(
            &["ls"],
            &["shell"],
            "Rec Mono St.Helens",
            14.0,
            dpi,
            &packaged_fonts(),
        )
        .expect("the packaged family loads");
        let tab_title_real = |icon: char| {
            set.tile_keys.iter().any(|key| {
                key.ch == icon
                    && key.raster_variant == GlyphRasterVariant::TabTitle
                    && key.glyph_id != 0
            })
        };
        assert!(tab_title_real('\u{F489}'), "the generic shell icon at {dpi} dpi");
        let absent: Vec<char> = sonicterm_render_model::boundary::ui::tab_title::PROGRAM_ICONS
            .iter()
            .copied()
            .filter(|icon| {
                !set.tile_keys.iter().any(|key| {
                    key.ch == *icon && key.raster_variant == GlyphRasterVariant::TabTitle
                })
            })
            .collect();
        assert!(absent.is_empty(), "icons never measured at {dpi} dpi: {absent:?}");
    }
}

/// Answers every key with a 16x16 tile, larger than an 8x8 fixed atlas can ever place.
struct LargeTiles;

impl sonicterm_text::glyph_atlas::Rasterizer for LargeTiles {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 16,
            height: 16,
            offset_x: 0,
            offset_y: -16,
            advance: 16.0,
            coverage: vec![255; 256],
            is_color: false,
            is_subpixel: false,
        })
    }
}

/// A required tile too large for the atlas draws nothing in the renderer, so the helper lists it as
/// an incomplete required tile instead of passing it as a resident empty glyph; a placed tile is
/// not listed.
#[test]
fn an_oversize_required_tile_is_listed_as_incomplete() {
    let mut atlas = GlyphAtlas::new(8, 8);
    let oversize = GlyphKey::shaped('用', 1, 4242, false, false);
    let _info = atlas.get_or_insert(oversize, &mut LargeTiles);
    let mut accounted = Accounted::default();
    account_tile(&atlas, oversize, &mut accounted).expect("an oversize tile is listed");
    assert_eq!(accounted.oversize_required, HashSet::from([oversize]));
    assert!(accounted.raster_failed.is_empty() && accounted.unresolved_chars.is_empty());
    let mut roomy = GlyphAtlas::new(64, 64);
    let _placed = roomy.get_or_insert(oversize, &mut LargeTiles);
    let mut placed = Accounted::default();
    account_tile(&roomy, oversize, &mut placed).expect("a placed tile is resident");
    assert!(placed.oversize_required.is_empty(), "a placed tile is not oversize");
}

/// A codepoint resolves, through the stack the renderer builds, to the identity of the real tile it
/// draws (a nonzero glyph id from a named face); a codepoint no face covers resolves to nothing.
#[test]
fn a_representative_codepoint_resolves_to_its_real_tile_identity() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let resolved = representative_identities(
        &['中', '\u{F0000}'],
        "Rec Mono St.Helens",
        14.0,
        72,
        &packaged_fonts(),
    );
    assert_eq!(resolved.len(), 2);
    let (character, identity) = &resolved[0];
    let identity = identity.as_ref().unwrap_or_else(|| panic!("{character:?} resolves"));
    assert_eq!(*character, '中');
    assert!(identity.source.glyph_id != 0, "a real glyph: {identity:?}");
    assert_eq!(identity.raster_variant, GlyphRasterVariant::Normal);
    assert_eq!(resolved[1], ('\u{F0000}', None), "no face covers U+F0000");
}

/// Each representative codepoint must be resident in the renderer's atlas with a real raster: an
/// unresolved codepoint, an identity the atlas does not hold, and a resident zero-size tile are each
/// a named gap, and a resident real tile is not.
#[test]
fn a_representative_gap_names_each_unresolved_absent_or_empty_tile() {
    let identity = |glyph_id: u32| TileIdentity {
        source: sonicterm_engine::ResolvedGlyphFace {
            face: sonicterm_engine::FaceIdentity { source: "face.ttf".into(), face_index: 0 },
            glyph_id,
            strike_px_milli: 14_000,
        },
        raster_variant: GlyphRasterVariant::Normal,
        bold: false,
        italic: false,
        is_color: false,
        is_subpixel: false,
    };
    let resident = HashMap::from([(identity(1), [8, 12]), (identity(3), [0, 12])]);
    let resolved = vec![
        ('a', Some(identity(1))),
        ('b', None),
        ('c', Some(identity(2))),
        ('d', Some(identity(3))),
    ];
    assert_eq!(
        representative_gaps(&resolved, &resident),
        vec![('b', "no real glyph"), ('c', "not resident"), ('d', "no pixels")]
    );
}

/// A codepoint whose shaped tile is missing or too large to place draws no real tile, so it resolves
/// to no identity; the same codepoint in a roomy atlas resolves.
#[test]
fn a_representative_codepoint_without_a_real_tile_resolves_to_nothing() {
    let _lock = crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stack = renderer_font_stacks("Rec Mono St.Helens", 14.0, 72, 1.0, &packaged_fonts())
        .body
        .expect("the packaged family loads");
    let mut roomy = GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM);
    assert!(codepoint_identity(&stack, &mut roomy, '中').is_some(), "a placed tile resolves");
    let mut tiny = GlyphAtlas::new(8, 8);
    assert_eq!(codepoint_identity(&stack, &mut tiny, '中'), None, "an oversize tile does not");
    // The same shaped key, cached as a raster failure first, is a missing tile.
    let mut seeded = GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM);
    let (key, _) = shaped_tile(
        &stack,
        &mut GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM),
        '中',
        GlyphRasterVariant::Normal,
    );
    let _sentinel = seeded.get_or_insert(key, &mut NoRaster);
    assert_eq!(codepoint_identity(&stack, &mut seeded, '中'), None, "a missing tile does not");
}
