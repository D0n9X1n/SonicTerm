use super::*;
use crate::rasterizer::colr::DrawOp;
use crate::source_pin::item_body;

fn bgra_fixture(width: u32, height: u32, data: &[u8]) -> RasterizedGlyph {
    let mut slot: FT_GlyphSlotRec_ =
        // SAFETY: the C slot permits null pointers; rasterize_bgra reads only the bitmap metadata initialized below.
        unsafe { std::mem::zeroed() };
    slot.bitmap.width = width as _;
    slot.bitmap.rows = height as _;
    slot.bitmap_left = -3;
    slot.bitmap_top = 9;
    FreeTypeRasterizer::rasterize_bgra(width as usize * 4, &slot, data, true, true).unwrap()
}

// Removed margins translate bearings independently of the cropped dimensions and channel swap.
#[test]
fn bgra_crop_translates_bearings_and_preserves_premultiplied_ink() {
    let mut image = image::ImageBuffer::from_pixel(7, 6, image::Rgba([0, 0, 0, 0]));
    for row in 1..3 {
        for column in 2..5 {
            image.put_pixel(column, row, image::Rgba([8, 16, 32, 64]));
        }
    }
    let raster = bgra_fixture(7, 6, image.as_raw());
    assert_eq!((raster.bearing_x.get(), raster.bearing_y.get()), (-1.0, 8.0));
    assert_eq!((raster.width, raster.height), (3, 2));
    assert!(raster.data.as_chunks::<4>().0.iter().all(|pixel| *pixel == [32, 16, 8, 64]));
    assert!(raster.has_color && raster.is_scaled);
}

// An uncropped blank or opaque glyph keeps native bearings and valid owned RGBA storage.
#[test]
fn bgra_no_crop_preserves_metrics_and_blank_policy() {
    for pixel in [[0, 0, 0, 0], [8, 16, 32, 64]] {
        let source = pixel.repeat(9);
        let raster = bgra_fixture(3, 3, &source);
        assert_eq!((raster.width, raster.height), (3, 3));
        assert_eq!((raster.bearing_x.get(), raster.bearing_y.get()), (-3.0, 9.0));
        assert_eq!(raster.data, [pixel[2], pixel[1], pixel[0], pixel[3]].repeat(9));
        assert!(raster.has_color && raster.is_scaled);
    }
}

/// The BGRA image owns its bytes before the face-owned source can change or expire.
#[test]
fn bgra_image_copies_borrowed_glyph_bytes() {
    let mut source = vec![1, 2, 3, 4];
    let image = owned_bgra_image(1, 1, &source).expect("one BGRA pixel builds an image");

    source.fill(0);

    assert_eq!(image.as_raw(), &[1, 2, 3, 4]);
}

/// Draw ops tracing the axis-aligned rectangle `left..right` by `top..bottom`.
fn rect_path(left: f32, top: f32, right: f32, bottom: f32) -> Vec<DrawOp> {
    vec![
        DrawOp::MoveTo { to_x: left, to_y: top },
        DrawOp::LineTo { to_x: right, to_y: top },
        DrawOp::LineTo { to_x: right, to_y: bottom },
        DrawOp::LineTo { to_x: left, to_y: bottom },
        DrawOp::ClosePath,
    ]
}

/// Paint ops filling the rectangle with opaque red.
fn opaque_square(left: f32, top: f32, right: f32, bottom: f32) -> Vec<PaintOp> {
    vec![
        PaintOp::PushClip(rect_path(left, top, right, bottom)),
        PaintOp::PaintSolid(SrgbaPixel::rgba(255, 0, 0, 255)),
        PaintOp::PopClip,
    ]
}

/// The 🇯 shape: an unclipped gradient composited `In` over a bounded backdrop. Cairo's `In` is
/// unbounded, so without an outer clip the recording has no finite ink extents.
fn unbounded_in_composite() -> Vec<PaintOp> {
    let color_line = ColorLine {
        color_stops: vec![
            ColorStop { offset: 0.0, color: SrgbaPixel::rgba(0, 0, 255, 255) },
            ColorStop { offset: 1.0, color: SrgbaPixel::rgba(0, 0, 255, 255) },
        ],
        extend: Extend::Pad,
    };
    let mut ops = opaque_square(2.0, 2.0, 6.0, 6.0);
    ops.push(PaintOp::PushGroup);
    ops.push(PaintOp::PaintLinearGradient {
        start_x: 0.0,
        start_y: 0.0,
        end_x: 10.0,
        end_y: 0.0,
        rotation_x: 0.0,
        rotation_y: 10.0,
        color_line,
    });
    ops.push(PaintOp::PopGroup(Operator::In));
    ops
}

/// Alpha of the pixel at (`x_px`, `y_px`) in an RGBA glyph.
fn alpha_at(glyph: &RasterizedGlyph, x_px: usize, y_px: usize) -> u8 {
    glyph.data[(y_px * glyph.width + x_px) * 4 + 3]
}

/// Axis-aligned clip corners for the rectangle `left..right` by `top..bottom`.
fn rect_clip(left: f64, top: f64, right: f64, bottom: f64) -> [(f64, f64); 4] {
    [(left, bottom), (left, top), (right, top), (right, bottom)]
}

#[test]
fn a_clip_box_bounds_a_composite_cairo_cannot() {
    // The 🇯 regression: an unbounded `In` composite has no finite extents and failed with -1x-1.
    // Its ClipBox bounds it, and the artwork stays visible inside the backdrop, transparent outside.
    let unclipped = rasterize_from_ops(unbounded_in_composite(), 1.0, 1.0, None);
    assert!(
        matches!(&unclipped, Err(err) if err.to_string().contains("invalid color glyph extents"))
    );

    let clip = rect_clip(0.0, 0.0, 10.0, 10.0);
    let glyph = rasterize_from_ops(unbounded_in_composite(), 1.0, 1.0, Some(clip)).unwrap();
    assert_eq!((glyph.width, glyph.height), (10, 10));
    assert_eq!((glyph.bearing_x.get(), glyph.bearing_y.get()), (0.0, 0.0));
    assert_eq!(alpha_at(&glyph, 4, 4), 255, "inside the backdrop the gradient shows");
    // Source-in keeps the gradient's colour, opaque blue, not the red backdrop.
    let interior = (4 * glyph.width + 4) * 4;
    assert_eq!(&glyph.data[interior..interior + 4], &[0, 0, 255, 255]);
    assert!(glyph.has_color);
    assert_eq!(alpha_at(&glyph, 8, 8), 0, "outside the backdrop `In` leaves nothing");
}

#[test]
fn a_skewed_clip_box_clips_to_its_quad_not_its_bounding_box() {
    // An italic ClipBox is a parallelogram. A pixel inside its bounding box but outside the
    // parallelogram must stay transparent; replacing the quad with its bounds fails this.
    let clip = [(0.0, 10.0), (4.0, 0.0), (14.0, 0.0), (10.0, 10.0)];
    let glyph =
        rasterize_from_ops(opaque_square(-5.0, -5.0, 20.0, 20.0), 1.0, 1.0, Some(clip)).unwrap();
    assert_eq!((glyph.width, glyph.height), (14, 10));
    assert_eq!(alpha_at(&glyph, 7, 5), 255, "inside the parallelogram");
    assert_eq!(alpha_at(&glyph, 0, 0), 0, "inside the bounds, left of the slanted edge");
    assert_eq!(alpha_at(&glyph, 13, 9), 0, "inside the bounds, right of the slanted edge");
}

#[test]
fn pixel_bounds_round_fractional_extents_outward() {
    // Truncating would drop a fractional edge pixel, or collapse a sliver to nothing.
    assert_eq!(
        pixel_bounds(-0.5, -1.25, 3.0, 2.5).unwrap(),
        PixelBounds { left: -1, top: -2, width: 4, height: 4 }
    );
    assert_eq!(
        pixel_bounds(2.25, 0.5, 0.25, 1.0).unwrap(),
        PixelBounds { left: 2, top: 0, width: 1, height: 2 }
    );
}

#[test]
fn pixel_bounds_reject_unbounded_or_oversized_extents() {
    // Cairo's unbounded sentinel, non-finite values and spans past the glyph limit are errors,
    // reported before any allocation. Zero area is a valid empty glyph.
    assert!(pixel_bounds(-8388608.0, -8388608.0, -1.0, -1.0).is_err());
    assert!(pixel_bounds(f64::NAN, 0.0, 1.0, 1.0).is_err());
    assert!(pixel_bounds(0.0, 0.0, f64::INFINITY, 1.0).is_err());
    let limit = MAX_RASTERIZED_GLYPH_DIMENSION as f64;
    assert!(pixel_bounds(0.0, 0.0, limit + 1.0, 1.0).is_err());
    assert_eq!(
        pixel_bounds(5.5, 7.0, 0.0, 3.0).unwrap(),
        PixelBounds { left: 0, top: 0, width: 0, height: 0 }
    );
}

#[test]
fn the_glyph_limit_bounds_the_bitmap_not_its_origin() {
    // A small glyph far from the pen still fits: only its size is limited.
    assert_eq!(
        pixel_bounds(131073.0, -200000.0, 1.0, 1.0).unwrap(),
        PixelBounds { left: 131073, top: -200000, width: 1, height: 1 }
    );
}

#[test]
fn a_non_finite_clip_box_corner_is_rejected() {
    // A corner FreeType could never produce must not reach Cairo as a clip path.
    let mut clip = rect_clip(0.0, 0.0, 10.0, 10.0);
    clip[2].0 = f64::NAN;
    let result = rasterize_from_ops(opaque_square(0.0, 0.0, 4.0, 4.0), 1.0, 1.0, Some(clip));
    assert!(
        matches!(&result, Err(err) if err.to_string().contains("invalid color glyph clip box"))
    );
}

#[test]
fn ink_outside_the_clip_box_is_empty_and_an_oversized_fill_is_rejected() {
    // Ink wholly outside the ClipBox yields the empty glyph; a fill covering a huge ClipBox is
    // rejected by the glyph limit before the target surface is allocated.
    let disjoint = rasterize_from_ops(
        opaque_square(20.0, 20.0, 24.0, 24.0),
        1.0,
        1.0,
        Some(rect_clip(0.0, 0.0, 10.0, 10.0)),
    )
    .unwrap();
    assert_eq!((disjoint.width, disjoint.height), (0, 0));
    assert!(disjoint.data.is_empty());

    let huge = rect_clip(0.0, 0.0, 3000.0, 3000.0);
    let oversized =
        rasterize_from_ops(opaque_square(-1.0, -1.0, 3001.0, 3001.0), 1.0, 1.0, Some(huge));
    assert!(matches!(&oversized, Err(err) if err.to_string().contains("glyph limit")));
}

#[test]
fn a_bounded_glyph_inside_its_clip_box_renders_unchanged() {
    // The ClipBox must not change a glyph that already rendered: same pixels and bearings, for ink
    // left of the origin (negative bearing) and right of it (the bearing rule clamps to 0).
    for (left, expect_bearing_x) in [(-3.0_f32, -3.0), (2.0, 0.0)] {
        let ops = opaque_square(left, -2.0, left + 4.0, 3.0);
        let plain = rasterize_from_ops(ops.clone(), 1.0, 1.0, None).unwrap();
        let clipped =
            rasterize_from_ops(ops, 1.0, 1.0, Some(rect_clip(-20.0, -20.0, 20.0, 20.0))).unwrap();
        assert_eq!((plain.width, plain.height), (4, 5), "left={left}");
        assert_eq!((clipped.width, clipped.height), (plain.width, plain.height), "left={left}");
        assert_eq!(clipped.data, plain.data, "left={left}");
        assert_eq!(clipped.bearing_x.get(), expect_bearing_x, "left={left}");
        assert_eq!(plain.bearing_x.get(), expect_bearing_x, "left={left}");
        assert_eq!((clipped.bearing_y.get(), plain.bearing_y.get()), (2.0, 2.0), "left={left}");
    }
}

#[test]
fn clip_box_corners_stay_in_device_pixels_under_the_paint_scale() {
    // FreeType's ClipBox is 26.6 device pixels, y up. The production conversion divides by 64 and
    // flips y, and the clip is set before the paint scale, so the scale never moves it.
    let corner = |x_26_6: i64, y_26_6: i64| ::freetype::FT_Vector {
        x: ::freetype::FT_Pos::from_font_units(x_26_6 as _),
        y: ::freetype::FT_Pos::from_font_units(y_26_6 as _),
    };
    let clip_box = FT_ClipBox_ {
        bottom_left: corner(64, 64),
        top_left: corner(64, 256),
        top_right: corner(448, 256),
        bottom_right: corner(448, 64),
    };
    let corners = clip_box_corners_px(&clip_box);
    assert_eq!(corners, [(1.0, -1.0), (1.0, -4.0), (7.0, -4.0), (7.0, -1.0)]);

    // A fill far larger than the clip, under the production scale sign convention (x positive,
    // y negative): the result is exactly the clip, wherever the scale would have put it.
    let scale = 16.0 / 13.0;
    let glyph = rasterize_from_ops(
        opaque_square(-100.0, -100.0, 100.0, 100.0),
        scale,
        -scale,
        Some(corners),
    )
    .unwrap();
    assert_eq!((glyph.width, glyph.height), (6, 3));
    assert_eq!((glyph.bearing_x.get(), glyph.bearing_y.get()), (0.0, 4.0));
}

/// A ClipBox whose corners, in 26.6 device pixels y up, span `left..right` by `bottom..top` whole pixels.
fn clip_box_px(left: i64, bottom: i64, right: i64, top: i64) -> FT_ClipBox_ {
    let corner = |x_px: i64, y_px: i64| ::freetype::FT_Vector {
        x: ::freetype::FT_Pos::from_font_units((x_px * 64) as _),
        y: ::freetype::FT_Pos::from_font_units((y_px * 64) as _),
    };
    FT_ClipBox_ {
        bottom_left: corner(left, bottom),
        top_left: corner(left, top),
        top_right: corner(right, top),
        bottom_right: corner(right, bottom),
    }
}

/// A COLRv1 glyph without a ClipBox rasterizes unclipped through the production entry point when its paint is
/// bounded: the bitmap is exactly its ink, with the ink's bearings and colour.
#[test]
fn a_bounded_glyph_without_a_clip_box_renders_unclipped() {
    let glyph = rasterize_colr(opaque_square(-3.0, -2.0, 1.0, 3.0), 1.0, 1.0, None)
        .expect("a bounded glyph without a ClipBox renders");
    assert_eq!((glyph.width, glyph.height), (4, 5));
    assert_eq!((glyph.bearing_x.get(), glyph.bearing_y.get()), (-3.0, 2.0));
    assert!(glyph.has_color);
    let (pixels, rest) = glyph.data.as_chunks::<4>();
    assert!(
        rest.is_empty() && pixels.iter().all(|pixel| *pixel == [255, 0, 0, 255]),
        "every pixel is opaque red"
    );
}

/// Without a ClipBox nothing bounds a graph Cairo cannot bound, so there is no finite rendering: it fails, and
/// the error names the missing ClipBox rather than only the extents.
#[test]
fn an_unbounded_glyph_without_a_clip_box_fails_with_a_named_reason() {
    let error = rasterize_colr(unbounded_in_composite(), 1.0, 1.0, None)
        .expect_err("an unbounded graph without a ClipBox has no finite rendering")
        .to_string();
    assert!(error.contains("has no ClipBox to bound"), "{error}");
    assert!(error.contains("invalid color glyph extents"), "{error}");
}

/// The production entry point applies a ClipBox when the glyph has one: the unbounded composite renders inside
/// it, so a path that dropped the box would fail here.
#[test]
fn a_glyph_with_a_clip_box_is_clipped_through_the_production_path() {
    let clip_box = clip_box_px(0, -10, 10, 0);
    let glyph = rasterize_colr(unbounded_in_composite(), 1.0, 1.0, Some(&clip_box))
        .expect("a ClipBox bounds the composite");
    assert_eq!((glyph.width, glyph.height), (10, 10));
    assert_eq!(alpha_at(&glyph, 4, 4), 255);
}

/// FreeType's status 0 is "no ClipBox": nothing is read, so no uninitialized box is used; status 1 reads it.
#[test]
fn a_zero_clip_box_status_is_absent_and_reads_nothing() {
    let reads = std::cell::Cell::new(0_u32);
    let read = || {
        reads.set(reads.get() + 1);
        clip_box_px(1, -4, 7, -1)
    };
    assert!(crate::ftwrap::clip_box_from_status(0, read).is_none());
    assert_eq!(reads.get(), 0, "status 0 reads no box");
    let present = crate::ftwrap::clip_box_from_status(1, read).expect("status 1 is a ClipBox");
    assert_eq!(reads.get(), 1);
    assert_eq!(clip_box_corners_px(&present), clip_box_corners_px(&clip_box_px(1, -4, 7, -1)));
}

/// Why `rasterizer` and `ftwrap`, the sources of rasterizer/freetype.rs and ftwrap.rs, do not carry a missing
/// ClipBox to the rasterizer as None, or empty: rasterize_outlines must hand the face's optional ClipBox straight
/// to `rasterize_colr` and never turn its absence into an error, and the face must map FreeType's status through
/// `clip_box_from_status` without failing. Both are read as code only and must be defined exactly once.
fn clip_box_wiring_problems(rasterizer: &str, ftwrap: &str) -> Vec<String> {
    let mut problems = Vec::new();
    match item_body(rasterizer, "fn rasterize_outlines(") {
        Ok(outlines) => {
            if !outlines.contains("let clip_box = face.get_color_glyph_clip_box(glyph_pos);") {
                problems.push("rasterize_outlines does not read the optional ClipBox".to_owned());
            }
            if !outlines.contains("rasterize_colr(walker.ops, 1.0, -1.0, clip_box.as_ref())") {
                problems
                    .push("rasterize_outlines does not pass the optional ClipBox on".to_owned());
            }
            if outlines.contains("get_color_glyph_clip_box(glyph_pos)?") {
                problems
                    .push("rasterize_outlines turns a missing ClipBox into an error".to_owned());
            }
        }
        Err(problem) => problems.push(problem),
    }
    match item_body(ftwrap, "pub fn get_color_glyph_clip_box(") {
        Ok(query) => {
            if !query.contains("clip_box_from_status(status,") {
                problems.push("get_color_glyph_clip_box does not map the status".to_owned());
            }
            if query.contains("bail!") || query.contains("anyhow::Result") {
                problems.push("get_color_glyph_clip_box can fail".to_owned());
            }
        }
        Err(problem) => problems.push(problem),
    }
    problems
}

/// The production decision "no ClipBox → None": the real rasterizer and face carry a missing ClipBox to
/// `rasterize_colr` as None, read as code only so no comment, literal or second copy can stand in.
#[test]
fn a_missing_clip_box_reaches_the_rasterizer_as_none() {
    assert_eq!(
        clip_box_wiring_problems(include_str!("freetype.rs"), include_str!("../ftwrap.rs")),
        Vec::<String>::new()
    );
}

/// A rasterize_outlines body that reads the ClipBox, correct (`good`) or turning absence into an error (`bad`).
fn outlines_source(good: bool) -> String {
    let read = if good {
        "let clip_box = face.get_color_glyph_clip_box(glyph_pos);"
    } else {
        "let clip_box = Some(face.get_color_glyph_clip_box(glyph_pos)?);"
    };
    format!(
        "impl FreeTypeRasterizer {{\n    fn rasterize_outlines(&self) -> anyhow::Result<RasterizedGlyph> {{\n        \
         {read}\n        rasterize_colr(walker.ops, 1.0, -1.0, clip_box.as_ref())\n    }}\n}}\n"
    )
}

/// A get_color_glyph_clip_box body, mapping the status (`good`) or failing on it (`bad`).
fn query_source(good: bool) -> String {
    let (returns, body) = if good {
        ("Option<FT_ClipBox_>", "clip_box_from_status(status, || unsafe { result.assume_init() })")
    } else {
        ("anyhow::Result<FT_ClipBox_>", "if status == 0 { bail!(\"no ClipBox\") }")
    };
    format!(
        "impl Face {{\n    pub fn get_color_glyph_clip_box(&mut self, glyph_index: FT_UInt) -> {returns} {{\n        \
         {body}\n    }}\n}}\n"
    )
}

/// No decoy satisfies the ClipBox pin. With a broken rasterizer or face, a correct copy in an unused macro or a
/// `#[cfg(any())]` impl, before or after it, makes the source ambiguous, and one inside a nested block comment, a
/// hashed `r`, `br` or `cr` raw string or a C string is masked away; either way the pin still fails. With the
/// correct source, the masked decoys leave it passing, so the masker hides them without hiding real code.
#[test]
fn a_macro_cfg_comment_or_raw_string_decoy_never_satisfies_the_clip_box_pin() {
    for target in ["rasterizer", "ftwrap"] {
        let item = |good: bool| {
            if target == "rasterizer" {
                outlines_source(good)
            } else {
                query_source(good)
            }
        };
        let good_item = item(true);
        let inner = good_item
            .strip_prefix("impl FreeTypeRasterizer {\n")
            .or_else(|| good_item.strip_prefix("impl Face {\n"))
            .and_then(|rest| rest.strip_suffix("}\n"))
            .expect("the item is one impl");
        let code_decoys = [
            ("macro", format!("macro_rules! decoy {{\n    () => {{\n{inner}    }};\n}}\n")),
            ("cfg impl", format!("#[cfg(any())]\n{good_item}")),
        ];
        let masked_decoys = [
            ("nested comment", format!("/* outer /* inner */\n{good_item}*/\n")),
            ("r raw string", format!("const RAW: &str = r#\"say \"\n{good_item}\"#;\n")),
            ("br raw string", format!("const BYTES: &[u8] = br##\"say \"#\n{good_item}\"##;\n")),
            (
                "cr raw string",
                format!("const CRAW: &std::ffi::CStr = cr#\"say \"\n{good_item}\"#;\n"),
            ),
            ("C string", format!("const CSTR: &std::ffi::CStr = c\"\n{good_item}\";\n")),
        ];
        let sources = |real: &str, decoy: &str| {
            [("before", format!("{decoy}{real}")), ("after", format!("{real}{decoy}"))]
        };
        let pin = |source: &str| {
            if target == "rasterizer" {
                clip_box_wiring_problems(source, &query_source(true))
            } else {
                clip_box_wiring_problems(&outlines_source(true), source)
            }
        };
        assert_eq!(pin(&good_item), Vec::<String>::new(), "{target}: the correct source passes");
        assert!(!pin(&item(false)).is_empty(), "{target}: the broken source fails");
        for (label, decoy) in &code_decoys {
            for (place, source) in sources(&item(false), decoy) {
                let problems = pin(&source);
                assert!(
                    problems.iter().any(|problem| problem.starts_with("ambiguous source")),
                    "{target} {label} {place}: {problems:?}"
                );
            }
        }
        for (label, decoy) in &masked_decoys {
            for (place, source) in sources(&item(false), decoy) {
                assert!(
                    !pin(&source).is_empty(),
                    "{target} {label} {place}: the decoy satisfied the pin"
                );
            }
            for (place, source) in sources(&good_item, decoy) {
                assert_eq!(
                    pin(&source),
                    Vec::<String>::new(),
                    "{target} {label} {place}: the decoy was code"
                );
            }
        }
    }
}

/// The COLRv1 paint fixture built by scripts/generate-colr-fixture.py: 2048 units per em, one colour glyph per
/// walker conversion, every glyph with a ClipBox.
const COLR_FIXTURE: &[u8] = include_bytes!("../../test-fonts/colr-paint-fixture.ttf");
const FIXTURE_UNITS_PER_EM: f64 = 2048.0;
/// Fixture glyph ids, in the generator's glyph order.
const GLYPH_LINEAR: u32 = 4;
const GLYPH_RADIAL: u32 = 5;
const GLYPH_SWEEP: u32 = 6;
const GLYPH_SCALED: u32 = 7;
const GLYPH_ROTATED: u32 = 8;
const GLYPH_SKEWED: u32 = 9;
const GLYPH_MOVED: u32 = 10;
const GLYPH_CLIPPED: u32 = 11;

/// The fixture as a parsed font, with synthetic italic when `italic`.
fn fixture_font(italic: bool) -> ParsedFont {
    let handle = crate::locator::FontDataHandle {
        source: crate::locator::FontDataSource::BuiltIn {
            name: "colr-paint-fixture",
            data: COLR_FIXTURE,
        },
        index: 0,
        variation: 0,
        origin: crate::locator::FontOrigin::BuiltIn,
        coverage: None,
    };
    let mut parsed = ParsedFont::from_locator(&handle).expect("the fixture parses");
    parsed.synthesize_italic = italic;
    parsed
}

/// `glyph_id` of the fixture rasterized by the FreeType COLRv1 walker at `size_px` (dpi 72, so ppem = size),
/// through the same `rasterize_outlines` call `rasterize_glyph` makes when FreeType is the COLR rasterizer.
fn fixture_colr_glyph(glyph_id: u32, size_px: f64, italic: bool) -> RasterizedGlyph {
    let rasterizer =
        FreeTypeRasterizer::from_locator(&fixture_font(italic), DisplayPixelGeometry::RGB).unwrap();
    rasterizer.face.borrow_mut().set_font_size(size_px, 72).unwrap();
    let (load_flags, _) = ftwrap::compute_load_flags_from_config(
        rasterizer.freetype_load_flags,
        rasterizer.freetype_load_target,
        rasterizer.freetype_render_target,
        Some(72),
    );
    rasterizer
        .rasterize_outlines(glyph_id, load_flags | FT_LOAD_NO_HINTING as i32)
        .expect("the fixture glyph rasterizes")
}

/// The pixel of `glyph` covering the device point (`x_px`, `y_px`), y up from the baseline, with that pixel's
/// centre in the same frame; None outside the bitmap. Row 0 is the bitmap's top, at `bearing_y`.
fn pixel_at(glyph: &RasterizedGlyph, x_px: f64, y_px: f64) -> Option<([u8; 4], (f64, f64))> {
    let column = (x_px - glyph.bearing_x.get()).floor();
    let row = (glyph.bearing_y.get() - y_px).floor();
    // When: the point lies outside the bitmap, there is no pixel to read.
    if column < 0.0 || row < 0.0 || column >= glyph.width as f64 || row >= glyph.height as f64 {
        return None;
    }
    let at = (row as usize * glyph.width + column as usize) * 4;
    let rgba = glyph.data[at..at + 4].try_into().unwrap();
    let centre = (glyph.bearing_x.get() + column + 0.5, glyph.bearing_y.get() - row - 0.5);
    Some((rgba, centre))
}

/// The left and right ink edges of the bitmap row whose centre is nearest `y_px`, in device pixels: each edge
/// is the first or last fully covered column, moved out by the coverage of the partial pixels beyond it.
fn row_ink_edges(glyph: &RasterizedGlyph, y_px: f64) -> (f64, f64) {
    let row = (glyph.bearing_y.get() - y_px).floor() as usize;
    let alpha = |column: usize| f64::from(glyph.data[(row * glyph.width + column) * 4 + 3]) / 255.0;
    let full: Vec<usize> =
        (0..glyph.width).filter(|column| alpha(*column) >= 254.0 / 255.0).collect();
    let (&first, &last) = (full.first().expect("a covered row"), full.last().unwrap());
    let left = first as f64 - (0..first).map(alpha).sum::<f64>();
    let right = (last + 1) as f64 + (last + 1..glyph.width).map(alpha).sum::<f64>();
    (glyph.bearing_x.get() + left, glyph.bearing_x.get() + right)
}

/// Gradient anchors and contours share one font-unit space under the included root transform. The fixture's
/// linear gradient runs red at y = 0 to blue at y = 1600 font units over a 1600-unit square, so at every size,
/// plain or italic, a pixel's colour is the line's value at its own height: anchors scaled by ppem/upem, red
/// at the bottom (y up). Sizes on both sides of 16 px, where the old 1/64 mapping happened to be exact.
#[test]
fn colr_linear_gradient_anchors_scale_with_the_contour_at_every_size() {
    for size_px in [13.0, 26.0] {
        for italic in [false, true] {
            let glyph = fixture_colr_glyph(GLYPH_LINEAR, size_px, italic);
            let scale = size_px / FIXTURE_UNITS_PER_EM;
            for font_y in [400.0, 1200.0] {
                // Mid-square, moved right by the italic shear of that height.
                let font_x = 800.0 + if italic { FAKE_ITALIC_SKEW * font_y } else { 0.0 };
                let Some((rgba, (_, centre_y))) = pixel_at(&glyph, font_x * scale, font_y * scale)
                else {
                    panic!("{size_px}px italic={italic}: no pixel at font ({font_x}, {font_y})");
                };
                let offset = (centre_y / scale / 1600.0).clamp(0.0, 1.0);
                let expected = [255.0 * (1.0 - offset), 0.0, 255.0 * offset];
                assert_eq!(
                    rgba[3], 255,
                    "{size_px}px italic={italic} y={font_y}: inside the square"
                );
                for (channel, want) in expected.iter().enumerate() {
                    assert!(
                        (f64::from(rgba[channel]) - want).abs() <= 8.0,
                        "{size_px}px italic={italic} y={font_y}: {rgba:?}, expected {expected:?} at offset {offset:.3}"
                    );
                }
            }
            let (bottom, _) = pixel_at(&glyph, 800.0 * scale, 100.0 * scale).unwrap();
            assert!(bottom[0] > bottom[2], "{size_px}px italic={italic}: red at the bottom, y up");
        }
    }
}

/// Synthetic italic shears a COLR contour exactly once: the left edge of the fixture's square sits at
/// FAKE_ITALIC_SKEW times the height, as the face transform puts it, not twice that.
#[test]
fn colr_synthetic_italic_shears_contours_once() {
    for size_px in [13.0, 26.0] {
        let scale = size_px / FIXTURE_UNITS_PER_EM;
        let glyph = fixture_colr_glyph(GLYPH_LINEAR, size_px, true);
        let plain = fixture_colr_glyph(GLYPH_LINEAR, size_px, false);
        for font_y in [500.0, 1100.0] {
            let (_, (_, centre_y)) = pixel_at(&glyph, 1.0, font_y * scale).unwrap();
            let (left, _) = row_ink_edges(&glyph, centre_y);
            let (plain_left, _) = row_ink_edges(&plain, centre_y);
            let want = FAKE_ITALIC_SKEW * centre_y;
            assert!(
                (left - want).abs() <= 0.25,
                "{size_px}px at y={centre_y:.2}: left edge {left:.3}, one shear puts it at {want:.3}"
            );
            assert!(plain_left.abs() <= 0.1, "{size_px}px plain left edge {plain_left:.3}");
        }
    }
}

/// The device-space ClipBox clip is unchanged by the coordinate fix: the fixture's clipped square ends at its
/// ClipBox's right side, 800 font units, plain, and under italic at that side sheared once by the face
/// transform, as FreeType reports the box.
#[test]
fn colr_clip_box_still_bounds_the_contour_in_device_space() {
    for size_px in [13.0, 26.0] {
        let scale = size_px / FIXTURE_UNITS_PER_EM;
        for italic in [false, true] {
            let glyph = fixture_colr_glyph(GLYPH_CLIPPED, size_px, italic);
            let (_, (_, centre_y)) = pixel_at(&glyph, 1.0, 800.0 * scale).unwrap();
            let (_, right) = row_ink_edges(&glyph, centre_y);
            let want = 800.0 * scale + if italic { FAKE_ITALIC_SKEW * centre_y } else { 0.0 };
            assert!(
                (right - want).abs() <= 0.25,
                "{size_px}px italic={italic}: right edge {right:.3}, ClipBox side at {want:.3}"
            );
        }
    }
}

/// The paint ops the production walker records for the fixture's `glyph_id` at `size_px`: the same root
/// paint, palette and walk `rasterize_outlines` performs, before any Cairo replay.
fn fixture_walker_ops(glyph_id: u32, size_px: f64) -> Vec<PaintOp> {
    let rasterizer =
        FreeTypeRasterizer::from_locator(&fixture_font(false), DisplayPixelGeometry::RGB).unwrap();
    let mut face = rasterizer.face.borrow_mut();
    face.set_font_size(size_px, 72).unwrap();
    let paint = face
        .get_color_glyph_paint(glyph_id, FT_Color_Root_Transform::FT_COLOR_INCLUDE_ROOT_TRANSFORM)
        .unwrap();
    face.get_palette_data().unwrap();
    face.select_palette(0).unwrap();
    let mut walker = Walker { load_flags: FT_LOAD_NO_HINTING as i32, face: &mut face, ops: vec![] };
    walker.walk_paint(paint, 0).unwrap();
    walker.ops
}

/// The fixture's `glyph_id` painted by HarfBuzz's COLR painter, the reference the FreeType walker must agree
/// with, at `size_px` and dpi 72.
fn fixture_harfbuzz_glyph(glyph_id: u32, size_px: f64) -> RasterizedGlyph {
    HarfbuzzRasterizer::from_locator(&fixture_font(false))
        .unwrap()
        .rasterize_glyph(glyph_id, size_px, 72)
        .unwrap()
}

/// Asserts `rgba`'s colour channels are within `tolerance` of `expected`, naming `context` on failure.
fn assert_rgb_near(rgba: [u8; 4], expected: [f64; 3], tolerance: f64, context: &str) {
    for (channel, want) in expected.iter().enumerate() {
        assert!(
            (f64::from(rgba[channel]) - want).abs() <= tolerance,
            "{context}: {rgba:?}, expected {expected:?}"
        );
    }
}

/// FreeType hands a radial gradient's radii as 16.16 font units, like its centres: the walker records the
/// fixture's r0 = 200 and r1 = 800 as font units, not their raw 16.16 bits.
#[test]
fn colr_radial_radii_are_read_as_font_units() {
    let ops = fixture_walker_ops(GLYPH_RADIAL, 13.0);
    let radii = ops
        .iter()
        .find_map(|op| match op {
            PaintOp::PaintRadialGradient { start_radius, end_radius, .. } => {
                Some((*start_radius, *end_radius))
            }
            _ => None,
        })
        .expect("the fixture's radial glyph records a radial gradient");
    assert_eq!(radii, (200.0, 800.0));
}

/// The fixture's radial gradient (centre 800, 800; r0 = 200; r1 = 800; red to blue) renders its analytic ramp at
/// 13 and 26 px: red inside r0, the mixed colour at each pixel's own distance, matching HarfBuzz.
#[test]
fn colr_radial_gradient_renders_its_ramp_at_every_size() {
    for size_px in [13.0, 26.0] {
        let scale = size_px / FIXTURE_UNITS_PER_EM;
        let glyph = fixture_colr_glyph(GLYPH_RADIAL, size_px, false);
        let reference = fixture_harfbuzz_glyph(GLYPH_RADIAL, size_px);
        for (font_x, font_y) in [(800.0, 800.0), (1300.0, 800.0), (800.0, 1400.0)] {
            let context = format!("{size_px}px at font ({font_x}, {font_y})");
            let Some((rgba, (centre_x, centre_y))) =
                pixel_at(&glyph, font_x * scale, font_y * scale)
            else {
                panic!("{context}: no pixel");
            };
            let distance = (centre_x / scale - 800.0).hypot(centre_y / scale - 800.0);
            let offset = ((distance - 200.0) / 600.0).clamp(0.0, 1.0);
            assert_rgb_near(rgba, [255.0 * (1.0 - offset), 0.0, 255.0 * offset], 8.0, &context);
            let Some((reference_rgba, _)) = pixel_at(&reference, centre_x, centre_y) else {
                panic!("{context}: no HarfBuzz pixel");
            };
            let reference_rgb = reference_rgba.map(f64::from);
            assert_rgb_near(
                rgba,
                [reference_rgb[0], reference_rgb[1], reference_rgb[2]],
                12.0,
                &context,
            );
        }
    }
}

/// Whether `glyph` has visible ink at the device point (`x_px`, `y_px`), y up; a point outside the bitmap has none.
fn has_ink_at(glyph: &RasterizedGlyph, x_px: f64, y_px: f64) -> bool {
    pixel_at(glyph, x_px, y_px).is_some_and(|(rgba, _)| rgba[3] > 0)
}

/// A PaintTransform's translation lands on its own axes: the fixture's `moved` glyph translates by dx = 800,
/// dy = 200 font units, and the walker records exactly that as the matrix's (x0, y0).
#[test]
fn colr_affine_translation_keeps_its_axes() {
    let translations: Vec<(f64, f64)> = fixture_walker_ops(GLYPH_MOVED, 13.0)
        .iter()
        .filter_map(|op| match op {
            PaintOp::PushTransform(matrix) => Some((matrix.x0(), matrix.y0())),
            _ => None,
        })
        .filter(|(x0, y0)| *x0 != 0.0 || *y0 != 0.0)
        .collect();
    assert_eq!(translations, [(800.0, 200.0)]);
}

/// The fixture's `moved` glyph renders its 400-unit red square translated by (800, 200), at 13 and 26 px: ink at
/// that square's centre, none where swapped axes would put it, as HarfBuzz paints it.
#[test]
fn colr_translated_paint_lands_on_its_axes_in_pixels() {
    for size_px in [13.0, 26.0] {
        let scale = size_px / FIXTURE_UNITS_PER_EM;
        let glyph = fixture_colr_glyph(GLYPH_MOVED, size_px, false);
        let reference = fixture_harfbuzz_glyph(GLYPH_MOVED, size_px);
        let (right_x, right_y) = (1000.0 * scale, 400.0 * scale);
        let (swapped_x, swapped_y) = (400.0 * scale, 1000.0 * scale);
        let Some((rgba, _)) = pixel_at(&glyph, right_x, right_y) else {
            panic!("{size_px}px: no pixel at the translated square's centre");
        };
        assert_rgb_near(rgba, [255.0, 0.0, 0.0], 8.0, &format!("{size_px}px translated square"));
        assert!(
            !has_ink_at(&glyph, swapped_x, swapped_y),
            "{size_px}px: ink where swapped axes put it"
        );
        assert!(
            has_ink_at(&reference, right_x, right_y),
            "{size_px}px: HarfBuzz paints the same square"
        );
        assert!(
            !has_ink_at(&reference, swapped_x, swapped_y),
            "{size_px}px: HarfBuzz leaves the swap empty"
        );
    }
}

/// The fixture's pivoted glyphs, each with its pivot in font units: scale and rotate around (400, 1200) and skew
/// around (0, 800), pivots whose x and y differ.
const PIVOTED_GLYPHS: [(&str, u32, (f64, f64)); 3] = [
    ("scaled", GLYPH_SCALED, (400.0, 1200.0)),
    ("rotated", GLYPH_ROTATED, (400.0, 1200.0)),
    ("skewed", GLYPH_SKEWED, (0.0, 800.0)),
];

/// Scale, rotate and skew around a centre move to that centre's own x and y and back: the walker records the
/// pure translations (cx, cy) and (-cx, -cy) around each operation, never cx for both axes.
#[test]
fn colr_scale_rotate_and_skew_pivot_on_their_own_centre() {
    for (name, glyph_id, (centre_x, centre_y)) in PIVOTED_GLYPHS {
        let translations: Vec<(f64, f64)> = fixture_walker_ops(glyph_id, 13.0)
            .iter()
            .filter_map(|op| match op {
                PaintOp::PushTransform(matrix)
                    if (matrix.xx(), matrix.yx(), matrix.xy(), matrix.yy())
                        == (1.0, 0.0, 0.0, 1.0) =>
                {
                    Some((matrix.x0(), matrix.y0()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(translations, [(centre_x, centre_y), (-centre_x, -centre_y)], "{name}");
    }
}

/// Each pivoted glyph renders where its own pivot puts it, at 13 and 26 px, as HarfBuzz paints it: red ink at a
/// point only the true pivot covers, and none at a point only a pivot of (cx, cx) would cover.
#[test]
fn colr_pivoted_paint_lands_where_its_centre_puts_it() {
    // Per glyph: a point only the true pivot covers, then one only (cx, cx) covers, in font units.
    let probes = [
        (GLYPH_SCALED, (600.0, 1250.0), (600.0, 350.0)),
        (GLYPH_ROTATED, (-400.0, 1600.0), (-400.0, 400.0)),
        (GLYPH_SKEWED, (1700.0, 200.0), (-700.0, 1450.0)),
    ];
    for size_px in [13.0, 26.0] {
        let scale = size_px / FIXTURE_UNITS_PER_EM;
        for (glyph_id, (inside_x, inside_y), (outside_x, outside_y)) in probes {
            let context = format!("{size_px}px glyph {glyph_id}");
            let glyph = fixture_colr_glyph(glyph_id, size_px, false);
            let reference = fixture_harfbuzz_glyph(glyph_id, size_px);
            let Some((rgba, _)) = pixel_at(&glyph, inside_x * scale, inside_y * scale) else {
                panic!("{context}: no pixel where the true pivot puts the square");
            };
            assert_rgb_near(rgba, [255.0, 0.0, 0.0], 8.0, &context);
            assert!(
                has_ink_at(&reference, inside_x * scale, inside_y * scale),
                "{context}: HarfBuzz ink"
            );
            assert!(
                !has_ink_at(&glyph, outside_x * scale, outside_y * scale),
                "{context}: ink where a pivot of (cx, cx) puts the square"
            );
            assert!(
                !has_ink_at(&reference, outside_x * scale, outside_y * scale),
                "{context}: HarfBuzz leaves the wrong pivot's square empty"
            );
        }
    }
}

/// FreeType gives a sweep angle as the stored F2DOT14 value, which the format biases by minus one half-turn; the
/// shared sweep renderer takes HarfBuzz's convention, radians of (angle + 1) * pi. The fixture's full turn,
/// 0 to 360 degrees, is stored as -1 to 1 and reaches the renderer as 0 to 2 pi.
#[test]
fn colr_sweep_angles_use_the_harfbuzz_convention() {
    let angles = fixture_walker_ops(GLYPH_SWEEP, 13.0)
        .iter()
        .find_map(|op| match op {
            PaintOp::PaintSweepGradient { start_angle, end_angle, .. } => {
                Some((*start_angle, *end_angle))
            }
            _ => None,
        })
        .expect("the fixture's sweep glyph records a sweep gradient");
    let pi = std::f32::consts::PI;
    assert!(angles.0.abs() < 1e-5 && (angles.1 - 2.0 * pi).abs() < 1e-5, "{angles:?}");
}

/// The fixture's sweep paints four hard bands (red, green, blue, yellow) over a full turn. At 13 and 26 px each
/// quadrant, sampled mid-band, shows the band HarfBuzz paints there, and the four quadrants are four bands.
#[test]
fn colr_sweep_gradient_quadrants_match_harfbuzz() {
    for size_px in [13.0, 26.0] {
        let scale = size_px / FIXTURE_UNITS_PER_EM;
        let glyph = fixture_colr_glyph(GLYPH_SWEEP, size_px, false);
        let reference = fixture_harfbuzz_glyph(GLYPH_SWEEP, size_px);
        let mut bands = Vec::new();
        for (offset_x, offset_y) in
            [(450.0, 450.0), (-450.0, 450.0), (-450.0, -450.0), (450.0, -450.0)]
        {
            let (x_px, y_px) = ((800.0 + offset_x) * scale, (800.0 + offset_y) * scale);
            let context = format!("{size_px}px quadrant ({offset_x}, {offset_y})");
            let (Some((rgba, _)), Some((reference_rgba, _))) =
                (pixel_at(&glyph, x_px, y_px), pixel_at(&reference, x_px, y_px))
            else {
                panic!("{context}: no pixel");
            };
            let reference_rgb = reference_rgba.map(f64::from);
            assert_rgb_near(
                rgba,
                [reference_rgb[0], reference_rgb[1], reference_rgb[2]],
                24.0,
                &context,
            );
            bands.push(reference_rgba.map(|channel| channel > 127));
        }
        bands.sort();
        bands.dedup();
        assert_eq!(bands.len(), 4, "{size_px}px: the four quadrants are four bands");
    }
}
