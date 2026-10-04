//! Pins highlight recoloring over the main glyph list: the row-pruned scan recolors exactly
//! what the full scan does, visits only rows whose ink meets the target plus every glyph
//! outside the recorded rows, and draws the same pixels.

use super::*;
use crate::quad::px_to_ndc;

/// Foreground every generated glyph starts with.
const INK: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Color the recolor writes; distinct from `INK` so a recolored glyph is identifiable.
const MARK: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// A small deterministic generator so every frame is reproducible from its seed.
struct Lcg(u64);

impl Lcg {
    /// The next 32 pseudo-random bits.
    fn next_u32(&mut self) -> u32 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    /// A value in `0.0..1.0`.
    fn unit(&mut self) -> f32 {
        self.next_u32() as f32 / (u32::MAX as f32 + 1.0)
    }

    /// A value in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        self.next_u32() as usize % bound.max(1)
    }
}

/// One glyph covering `(x, y, w, h)` surface pixels.
fn glyph_px(x: f32, y: f32, w: f32, h: f32, surface: (f32, f32)) -> GlyphInstance {
    GlyphInstance {
        rect: px_to_ndc(x, y, w, h, surface.0, surface.1),
        uv: [0.0; 4],
        color: INK,
        flags: [0.0; 4],
    }
}

/// A generated frame: the main glyph list, its recorded rows, and the cell metrics.
struct Frame {
    glyphs: Vec<GlyphInstance>,
    rows: Vec<RowGlyphSpan>,
    cell_w: f32,
    pitch: f32,
}

/// Build a frame whose rows advance by `pitch_factor` of the cell height, as a compressed
/// `line_height` does, while glyph ink keeps its full height and can overhang several rows.
/// Rows mix wide glyphs, zero-advance combining marks, side overhang and replays of an
/// earlier row's glyphs (a cache hit); chrome glyphs sit between rows and after them.
fn generated_frame(seed: u64, pitch_factor: f32, surface: (f32, f32)) -> Frame {
    let mut rng = Lcg(seed);
    let cell_w = 9.0;
    let cell_h = 18.0;
    let pitch = (cell_h * pitch_factor).max(0.18);
    let mut glyphs = Vec::new();
    let mut rows = Vec::new();
    let mut stored: Vec<Vec<GlyphInstance>> = Vec::new();
    for row in 0..12 {
        if rng.below(5) == 0 {
            // Chrome glyph between rows: part of the non-row complement.
            glyphs.push(glyph_px(rng.unit() * 200.0, rng.unit() * 200.0, 7.0, 12.0, surface));
        }
        let top = 4.0 + row as f32 * pitch;
        let start = glyphs.len();
        if !stored.is_empty() && rng.below(4) == 0 {
            // Cache-hit replay: the same glyph instances an earlier frame stored for this row.
            let replay = stored[rng.below(stored.len())].clone();
            glyphs.extend(replay);
        } else {
            let mut col = 0.0_f32;
            while col < 20.0 {
                let wide = rng.below(6) == 0;
                let span = if wide { 2.0 } else { 1.0 };
                let overhang_x = rng.unit() * 4.0 - 1.0;
                let ink_h = cell_h * (0.4 + rng.unit() * 1.6);
                let ink_top = top + cell_h - ink_h + rng.unit() * 3.0;
                let x = 4.0 + col * cell_w - overhang_x.min(0.0);
                glyphs.push(glyph_px(x, ink_top, cell_w * span + overhang_x.abs(), ink_h, surface));
                if rng.below(5) == 0 {
                    // Combining mark: zero advance, drawn over the previous glyph.
                    glyphs.push(glyph_px(x + 2.0, ink_top - 3.0, 4.0, 4.0, surface));
                }
                col += span;
            }
        }
        stored.push(glyphs[start..].to_vec());
        rows.push(RowGlyphSpan::new(&glyphs, start..glyphs.len(), surface.0, surface.1));
    }
    // Tab titles appended after the rows.
    for title in 0..3 {
        glyphs.push(glyph_px(10.0 + title as f32 * 30.0, 1.0, 8.0, 10.0, surface));
    }
    Frame { glyphs, rows, cell_w, pitch }
}

/// Targets for one frame: random cells, plus rectangles cut from real glyphs so that a
/// fifth, just under a fifth and just over a fifth of the glyph's area lies inside.
fn targets(frame: &Frame, rng: &mut Lcg, surface: (f32, f32)) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::new();
    for _ in 0..16 {
        let x = rng.unit() * 190.0;
        let y = rng.unit() * (frame.pitch * 12.0 + 20.0);
        out.push((x, y, frame.cell_w * (1.0 + rng.below(4) as f32), frame.pitch.max(1.0)));
    }
    for _ in 0..8 {
        let glyph = frame.glyphs[rng.below(frame.glyphs.len())];
        let (x, y, w, h) = glyph_rect_px(&glyph, surface.0, surface.1);
        for fraction in [0.2_f32, 0.19, 0.21] {
            out.push((x, y, w, h * fraction));
            out.push((x, y + h * (1.0 - fraction), w, h * fraction));
        }
    }
    out
}

/// True when the glyph carries the recolor mark.
fn marked(glyph: &GlyphInstance) -> bool {
    glyph.color.map(f32::to_bits) == MARK.map(f32::to_bits)
}

/// Differential: across generated frames, uncompressed and compressed pitch, the row-pruned
/// recolor marks exactly the glyphs the full scan marks, and never examines more glyphs.
#[test]
fn row_pruned_recolor_matches_the_full_scan() {
    let surface = (220.0, 260.0);
    let (mut recolored, mut kept, mut multi_row_ink) = (0usize, 0usize, false);
    for seed in 0..48_u64 {
        for pitch_factor in [1.0_f32, 0.5, 0.01] {
            let frame = generated_frame(seed, pitch_factor, surface);
            let mut rng = Lcg(seed ^ 0x5eed);
            multi_row_ink |= frame
                .rows
                .iter()
                .any(|row| row.ink_px.is_some_and(|ink| ink[3] - ink[1] > 3.0 * frame.pitch));
            for (x, y, w, h) in targets(&frame, &mut rng, surface) {
                let mut full = frame.glyphs.clone();
                recolor_cursor_glyphs(&mut full, x, y, w, h, surface.0, surface.1, MARK);
                let mut pruned = frame.glyphs.clone();
                let RecolorOutcome { visited, .. } = recolor_cursor_glyphs_in(
                    &mut pruned,
                    &frame.rows,
                    x,
                    y,
                    w,
                    h,
                    surface.0,
                    surface.1,
                    MARK,
                );
                for (index, (expected, actual)) in full.iter().zip(&pruned).enumerate() {
                    assert_eq!(
                        marked(expected),
                        marked(actual),
                        "seed {seed} pitch {pitch_factor} target {:?} glyph {index}",
                        (x, y, w, h)
                    );
                }
                assert!(visited <= frame.glyphs.len());
                recolored += full.iter().filter(|glyph| marked(glyph)).count();
                kept += full.iter().filter(|glyph| !marked(glyph)).count();
            }
        }
    }
    assert!(recolored > 0 && kept > 0, "targets must both hit and miss glyphs");
    assert!(multi_row_ink, "compressed pitch must produce ink spanning several rows");
}

/// Search with two matches visits only the glyphs of the two rows its matches touch, plus
/// the tab titles outside every row, and the counter records exactly that.
#[test]
fn search_recolor_visits_only_matching_rows_and_the_non_row_glyphs() {
    let surface = (200.0, 200.0);
    let (cell_w, cell_h) = (10.0, 20.0);
    let mut glyphs = Vec::new();
    let mut rows = Vec::new();
    for row in 0..6 {
        let start = glyphs.len();
        for col in 0..10 {
            // Inset by a pixel so neighbouring rows' ink never touches.
            glyphs.push(glyph_px(
                col as f32 * cell_w + 1.0,
                30.0 + row as f32 * cell_h + 1.0,
                cell_w - 2.0,
                cell_h - 2.0,
                surface,
            ));
        }
        rows.push(RowGlyphSpan::new(&glyphs, start..glyphs.len(), surface.0, surface.1));
    }
    for title in 0..3 {
        glyphs.push(glyph_px(title as f32 * 20.0, 2.0, 8.0, 10.0, surface));
    }
    let sink = crate::frame_stats::FrameStatsSink::default();
    {
        let _counting = crate::frame_stats::CollectGuard::enter(Some(&sink));
        for match_row in [2_usize, 4] {
            let y = 30.0 + match_row as f32 * cell_h;
            let RecolorOutcome { visited, .. } = recolor_cursor_glyphs_in(
                &mut glyphs,
                &rows,
                20.0,
                y,
                3.0 * cell_w,
                cell_h,
                surface.0,
                surface.1,
                MARK,
            );
            crate::frame_stats::note_recolor_glyphs_visited(|| visited);
        }
    }
    assert_eq!(sink.snapshot().recolor_glyphs_visited, 2 * (10 + 3));
    assert_eq!(glyphs.iter().filter(|glyph| marked(glyph)).count(), 2 * 3);
}

/// In the renderer's call order (rows, cursor recolor, tab titles appended, search recolor)
/// the cursor call sees no title glyph and search still recolors a matching title glyph.
#[test]
fn recolor_in_real_call_order_reaches_titles_only_after_they_are_appended() {
    let surface = (100.0, 100.0);
    let mut glyphs = Vec::new();
    let mut rows = Vec::new();
    for row in 0..4 {
        let start = glyphs.len();
        for col in 0..5 {
            glyphs.push(glyph_px(col as f32 * 10.0, 40.0 + row as f32 * 10.0, 10.0, 10.0, surface));
        }
        rows.push(RowGlyphSpan::new(&glyphs, start..glyphs.len(), surface.0, surface.1));
    }
    let row_glyphs = glyphs.len();

    let RecolorOutcome { visited: cursor_visits, .. } = recolor_cursor_glyphs_in(
        &mut glyphs,
        &rows,
        0.0,
        0.0,
        surface.0,
        surface.1,
        surface.0,
        surface.1,
        MARK,
    );
    assert_eq!(cursor_visits, row_glyphs, "the cursor call sees only the rows");

    glyphs.push(glyph_px(5.0, 2.0, 8.0, 10.0, surface));
    glyphs.push(glyph_px(30.0, 2.0, 8.0, 10.0, surface));
    let RecolorOutcome { visited: search_visits, .. } = recolor_cursor_glyphs_in(
        &mut glyphs,
        &rows,
        4.0,
        0.0,
        10.0,
        14.0,
        surface.0,
        surface.1,
        [0.0, 1.0, 0.0, 1.0],
    );
    assert_eq!(search_visits, 2, "no row's ink meets the target; both titles are scanned");
    assert_eq!(glyphs[row_glyphs].color, [0.0, 1.0, 0.0, 1.0], "the matching title is recolored");
    assert_eq!(glyphs[row_glyphs + 1].color, INK, "the other title is untouched");
}

/// Rasterizes every key as a solid 4 x 16 coverage tile.
struct SolidTile;

impl sonicterm_text::glyph_atlas::Rasterizer for SolidTile {
    fn rasterize(
        &mut self,
        _key: sonicterm_types::GlyphKey,
    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
        Some(sonicterm_text::glyph_atlas::RasterTile {
            width: 4,
            height: 16,
            offset_x: 0,
            offset_y: 0,
            advance: 4.0,
            coverage: vec![255; 64],
            is_color: false,
            is_subpixel: false,
        })
    }
}

/// Composite `glyphs` on black through the software presenter and return every pixel.
fn composite(
    atlas: &sonicterm_text::glyph_atlas::GlyphAtlas,
    glyphs: &[GlyphInstance],
    width: u32,
    height: u32,
) -> Vec<[u8; 4]> {
    let mut frame = crate::software_frame::SoftwareFrame::new(width, height, [0.0, 0.0, 0.0, 1.0])
        .expect("valid software frame");
    frame.draw_layers(atlas, atlas, &[], &[], glyphs, &[], &[]);
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| frame.pixel_bgra_at(x, y).expect("pixel in bounds"))
        .collect()
}

/// Pixel parity: a 16 px glyph from an earlier row reaching into the matched row is drawn
/// identically by the full scan and the row-pruned scan, at normal and compressed pitch.
#[test]
fn tall_glyph_over_a_match_draws_identical_pixels() {
    const WIDTH: u32 = 24;
    const HEIGHT: u32 = 48;
    let surface = (WIDTH as f32, HEIGHT as f32);
    let mut atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(4, 16);
    let tile = atlas
        .get_or_insert(sonicterm_types::GlyphKey::new('T', false, false), &mut SolidTile)
        .expect("solid tile inserts");
    for pitch in [8.0_f32, 4.0] {
        let mut glyphs = Vec::new();
        let mut rows = Vec::new();
        for row in 0..5 {
            let start = glyphs.len();
            let mut glyph =
                glyph_px(row as f32 * 4.0, 2.0 + row as f32 * pitch, 4.0, 16.0, surface);
            glyph.uv = tile.uv;
            glyphs.push(glyph);
            rows.push(RowGlyphSpan::new(&glyphs, start..glyphs.len(), surface.0, surface.1));
        }
        let match_row = 4.0;
        let target = (0.0, 2.0 + match_row * pitch, surface.0, pitch);
        let mut full = glyphs.clone();
        recolor_cursor_glyphs(
            &mut full, target.0, target.1, target.2, target.3, surface.0, surface.1, MARK,
        );
        let mut pruned = glyphs.clone();
        recolor_cursor_glyphs_in(
            &mut pruned,
            &rows,
            target.0,
            target.1,
            target.2,
            target.3,
            surface.0,
            surface.1,
            MARK,
        );

        assert!(marked(&full[3]), "pitch {pitch}: the tall glyph of the row above is recolored");
        assert_eq!(
            composite(&atlas, &full, WIDTH, HEIGHT),
            composite(&atlas, &pruned, WIDTH, HEIGHT),
            "pitch {pitch}"
        );
    }
}

#[test]
fn a_selection_or_caret_recolors_the_tofu_quads_it_covers() {
    // Field tofu outlines are quads drawn after the selection and caret; each quad that overlaps
    // the target takes the target's foreground, and one outside it keeps its own color.
    let surface = (200.0, 100.0);
    let original = [0.5, 0.5, 0.5, 0.55];
    let mut quads = vec![
        QuadInstance {
            rect: px_to_ndc(10.0, 10.0, 8.0, 1.0, surface.0, surface.1),
            color: original,
            ..Default::default()
        },
        QuadInstance {
            rect: px_to_ndc(60.0, 10.0, 8.0, 1.0, surface.0, surface.1),
            color: original,
            ..Default::default()
        },
    ];
    let foreground = [1.0, 0.0, 0.0, 1.0];
    recolor_cursor_quads(&mut quads, (8.0, 5.0, 20.0, 20.0), surface.0, surface.1, foreground);

    assert_eq!(quads[0].color, foreground, "the covered edge takes the foreground");
    assert_eq!(quads[1].color, original, "an edge outside the target keeps its color");
}

#[test]
fn a_field_draws_its_marks_before_its_tofu_and_recolors_what_they_cover() {
    // Search and palette fields compose through `paint_field_marks`: each mark's block comes first,
    // the glyph and tofu outline it covers take its foreground, and the tofu is pushed after every
    // block, so a selected or caret-covered placeholder stays visible.
    let surface = (200.0, 100.0);
    let original = [0.5, 0.5, 0.5, 0.55];
    let tofu = vec![QuadInstance {
        rect: px_to_ndc(10.0, 10.0, 8.0, 1.0, surface.0, surface.1),
        color: original,
        ..Default::default()
    }];
    let mut glyphs = vec![glyph_px(30.0, 10.0, 8.0, 12.0, surface)];
    let selection = FieldMark {
        rect: (8.0, 5.0, 40.0, 20.0),
        background: [0.0, 0.0, 1.0, 1.0],
        foreground: [1.0, 1.0, 1.0, 1.0],
    };
    let mut quads = Vec::new();
    paint_field_marks(&mut quads, &mut glyphs, tofu, &[selection], surface.0, surface.1);

    assert_eq!(quads.len(), 2, "one block and one tofu edge");
    assert_eq!(quads[0].color, [0.0, 0.0, 1.0, 1.0], "the selection block is drawn first");
    assert_eq!(quads[1].color, [1.0, 1.0, 1.0, 1.0], "the tofu edge is drawn after it, recolored");
    assert_eq!(glyphs[0].color, [1.0, 1.0, 1.0, 1.0], "the covered glyph is recolored");
}

/// A power-of-two surface, so pixel rectangles survive the NDC round trip exactly.
const EXACT_SURFACE: (f32, f32) = (512.0, 512.0);

/// The whole-list and row-pruned recolor of `glyphs` under `target`, each with its record.
fn recolor_both(
    glyphs: &[GlyphInstance],
    rows: &[RowGlyphSpan],
    target: (f32, f32, f32, f32),
) -> ((Vec<GlyphInstance>, RecolorRecord), (Vec<GlyphInstance>, RecolorRecord)) {
    let (left, top, width, height) = target;
    let (sw, sh) = EXACT_SURFACE;
    let mut full = glyphs.to_vec();
    let full_record = recolor_cursor_glyphs(&mut full, left, top, width, height, sw, sh, MARK);
    let mut pruned = glyphs.to_vec();
    let RecolorOutcome { record: pruned_record, .. } =
        recolor_cursor_glyphs_in(&mut pruned, rows, left, top, width, height, sw, sh, MARK);
    ((full, full_record), (pruned, pruned_record))
}

/// A recolored glyph taller than the cursor row reports its own pixel rectangle: the cursor at
/// (50,100,10,12) covers 30% of a glyph at (50,72,10,40), so the whole glyph is recolored and the
/// bounds are the glyph's, reaching y=72 above the cursor row. Both recolor entry points agree.
#[test]
fn recolor_reports_the_bounds_of_a_tall_recolored_glyph() {
    let tall = glyph_px(50.0, 72.0, 10.0, 40.0, EXACT_SURFACE);
    let far = glyph_px(200.0, 300.0, 10.0, 12.0, EXACT_SURFACE);
    let glyphs = vec![tall, far];
    let rows = vec![RowGlyphSpan::new(&glyphs, 0..2, EXACT_SURFACE.0, EXACT_SURFACE.1)];
    let ((full, full_record), (pruned, pruned_record)) =
        recolor_both(&glyphs, &rows, (50.0, 100.0, 10.0, 12.0));
    let expected = RecolorBounds::Rect(PixelRect { x: 50, y: 72, w: 10, h: 40 });
    assert_eq!(full_record.bounds, expected);
    assert_eq!(pruned_record, full_record);
    assert!(marked(&full[0]) && marked(&pruned[0]));
    assert!(!marked(&full[1]) && !marked(&pruned[1]));
}

/// A glyph under less than a fifth of the cursor is not recolored, so the call reports no
/// bounds: 19% of the (50,72,10,40) glyph lies inside the cursor rectangle.
#[test]
fn recolor_below_a_fifth_reports_empty_bounds() {
    let glyphs = vec![glyph_px(50.0, 72.0, 10.0, 40.0, EXACT_SURFACE)];
    let rows = vec![RowGlyphSpan::new(&glyphs, 0..1, EXACT_SURFACE.0, EXACT_SURFACE.1)];
    // The cursor's top at 104.4 leaves 7.6 of the glyph's 40 rows inside it: 19% of its area.
    let ((full, full_record), (_, pruned_record)) =
        recolor_both(&glyphs, &rows, (50.0, 104.4, 10.0, 12.0));
    assert!(!marked(&full[0]));
    assert_eq!(full_record, RecolorRecord::default());
    assert_eq!(full_record.bounds, RecolorBounds::Empty);
    assert_eq!(pruned_record, full_record);
}

/// Where a glyph with a non-finite rectangle draws cannot be bounded, so it is never recolored
/// and the call reports `Unbounded` wherever it sits in the list, even beside a recolored glyph.
/// The row-pruned scan must still see it: a row holding it is scanned, never skipped by ink.
#[test]
fn a_non_finite_glyph_makes_the_recolor_unbounded() {
    let broken = GlyphInstance { rect: [f32::NAN; 4], uv: [0.0; 4], color: INK, flags: [0.0; 4] };
    let under = glyph_px(50.0, 100.0, 10.0, 12.0, EXACT_SURFACE);
    let far = glyph_px(200.0, 300.0, 10.0, 12.0, EXACT_SURFACE);
    for glyphs in [vec![broken, under], vec![under, broken]] {
        let rows = vec![RowGlyphSpan::new(&glyphs, 0..2, EXACT_SURFACE.0, EXACT_SURFACE.1)];
        let ((full, full_record), (pruned, pruned_record)) =
            recolor_both(&glyphs, &rows, (50.0, 100.0, 10.0, 12.0));
        assert_eq!(full_record.bounds, RecolorBounds::Unbounded);
        assert_eq!(pruned_record.bounds, RecolorBounds::Unbounded);
        for list in [&full, &pruned] {
            let broken_index = usize::from(list[0].rect[0].is_finite());
            assert!(!marked(&list[broken_index]), "a non-finite glyph is never recolored");
            assert!(marked(&list[1 - broken_index]), "the finite glyph under the cursor is");
        }
    }
    // The broken glyph shares a row with a far glyph whose finite ink misses the cursor: the
    // row is still scanned, so the pruned scan reports `Unbounded` as the full scan does.
    let glyphs = vec![far, broken];
    let rows = vec![RowGlyphSpan::new(&glyphs, 0..2, EXACT_SURFACE.0, EXACT_SURFACE.1)];
    let ((_, full_record), (_, pruned_record)) =
        recolor_both(&glyphs, &rows, (50.0, 100.0, 10.0, 12.0));
    assert_eq!(full_record.bounds, RecolorBounds::Unbounded);
    assert_eq!(pruned_record.bounds, RecolorBounds::Unbounded);
}

/// A non-finite cursor rectangle recolors nothing and reports `Unbounded`, never `Empty`.
#[test]
fn a_non_finite_cursor_makes_the_recolor_unbounded() {
    let glyphs = vec![glyph_px(50.0, 100.0, 10.0, 12.0, EXACT_SURFACE)];
    let rows = vec![RowGlyphSpan::new(&glyphs, 0..1, EXACT_SURFACE.0, EXACT_SURFACE.1)];
    let ((full, full_record), (pruned, pruned_record)) =
        recolor_both(&glyphs, &rows, (f32::NAN, 100.0, 10.0, 12.0));
    assert_eq!(full_record.bounds, RecolorBounds::Unbounded);
    assert_eq!(pruned_record.bounds, RecolorBounds::Unbounded);
    assert!(!marked(&full[0]) && !marked(&pruned[0]));
}

/// The record's hash identifies what was recolored, not only where: the same rectangle with
/// different atlas coordinates hashes differently, and an identical frame hashes the same.
#[test]
fn the_recolor_hash_follows_the_recolored_atlas_coordinates() {
    let glyph = glyph_px(50.0, 100.0, 10.0, 12.0, EXACT_SURFACE);
    let moved_uv = GlyphInstance { uv: [0.25, 0.0, 0.25, 0.25], ..glyph };
    let record_of = |instance: GlyphInstance| {
        let glyphs = vec![instance];
        let rows = vec![RowGlyphSpan::new(&glyphs, 0..1, EXACT_SURFACE.0, EXACT_SURFACE.1)];
        let ((_, full_record), (_, pruned_record)) =
            recolor_both(&glyphs, &rows, (50.0, 100.0, 10.0, 12.0));
        assert_eq!(full_record, pruned_record);
        full_record
    };
    let first = record_of(glyph);
    assert_eq!(first, record_of(glyph), "an identical frame reports an identical record");
    let moved = record_of(moved_uv);
    assert_eq!(first.bounds, moved.bounds);
    assert_ne!(first.hash, moved.hash);
}

/// The test seams' glyph draws exactly the requested surface rectangle from its own resident
/// tile, with no glyph emitted before it, so a block cursor recolors it as it would a real tall
/// glyph: the cursor at (50,100,10,12) reports the injected (50,72,10,40) as its bounds. Its
/// coordinates sample inside the tile, and on an empty surface nothing is injected.
#[test]
fn a_seam_glyph_draws_its_rectangle_from_its_own_tile() {
    let mut atlas = sonicterm_text::glyph_atlas::GlyphAtlas::new(64, 64);
    let injected =
        seam_glyph(&mut atlas, (50.0, 72.0, 10.0, 40.0), MARK, EXACT_SURFACE.0, EXACT_SURFACE.1)
            .expect("the seam tile fits a real atlas");
    assert_eq!(
        glyph_rect_px(&injected, EXACT_SURFACE.0, EXACT_SURFACE.1),
        (50.0, 72.0, 10.0, 40.0)
    );
    assert_eq!((injected.flags, injected.color), ([0.0; 4], MARK));
    let [u_min, v_min, u_max, v_max] = injected.uv;
    assert!(0.0 <= u_min && u_min < u_max && u_max <= 1.0 && 0.0 <= v_min && v_min < v_max);
    let again = seam_glyph(&mut atlas, (0.0, 0.0, 1.0, 1.0), MARK, 8.0, 8.0).unwrap();
    assert_eq!(again.uv, injected.uv, "one tile serves every seam glyph");
    let glyphs = vec![injected];
    let rows = vec![RowGlyphSpan::new(&glyphs, 0..1, EXACT_SURFACE.0, EXACT_SURFACE.1)];
    let ((_, record), _) = recolor_both(&glyphs, &rows, (50.0, 100.0, 10.0, 12.0));
    assert_eq!(record.bounds, RecolorBounds::Rect(PixelRect { x: 50, y: 72, w: 10, h: 40 }));
    assert!(seam_glyph(&mut atlas, (50.0, 72.0, 10.0, 40.0), MARK, 0.0, 512.0).is_none());
}

/// The ink bounds of emitted chrome glyphs are the outward union of their pixel rectangles, so
/// a tall title glyph reaching above its band is bounded where it really draws; no glyph is
/// `Empty`, and a non-finite glyph makes the bounds `Unbounded`.
#[test]
fn glyph_ink_bounds_union_every_glyph_and_refuse_non_finite_ones() {
    let tall = glyph_px(20.0, 100.0, 10.0, 80.0, EXACT_SURFACE);
    let short = glyph_px(40.0, 142.0, 10.0, 12.0, EXACT_SURFACE);
    assert_eq!(
        glyph_ink_bounds(&[tall, short], EXACT_SURFACE.0, EXACT_SURFACE.1),
        RecolorBounds::Rect(PixelRect { x: 20, y: 100, w: 30, h: 80 })
    );
    assert_eq!(glyph_ink_bounds(&[], EXACT_SURFACE.0, EXACT_SURFACE.1), RecolorBounds::Empty);
    let broken = GlyphInstance { rect: [f32::NAN; 4], uv: [0.0; 4], color: INK, flags: [0.0; 4] };
    assert_eq!(
        glyph_ink_bounds(&[short, broken], EXACT_SURFACE.0, EXACT_SURFACE.1),
        RecolorBounds::Unbounded
    );
    assert_eq!(glyph_ink_bounds(&[short], 0.0, 512.0), RecolorBounds::Unbounded);
}
