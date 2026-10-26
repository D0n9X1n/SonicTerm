//! Where a glyph's ink starts above the baseline, predicted from its own outline: the face's design
//! extent, the requested strike and the coverage at which the renderer's blend first shows.

/// How far a channel must move from the background to count as ink, as the native layout tests read
/// a frame.
pub const INK_DIFFERENCE: u8 = 80;

/// A top row whose outline coverage lies within this of the ink threshold may land on either side
/// once a curved top is anti-aliased: Cascadia Mono's round 2 covers 0.26 of its top row at
/// 21.75 px where its outline predicts 0.29. Such a top is reported, not predicted.
pub const DECISION_MARGIN: f32 = 0.08;

/// A glyph's vertical design extent, in font units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphExtent {
    /// The face's units per em.
    pub units_per_em: u16,
    /// The lowest outline point.
    pub y_min: i16,
    /// The highest outline point.
    pub y_max: i16,
}

/// The ink a glyph's outline puts above the baseline at one strike.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutlineTop {
    /// Rows above the baseline that show ink, counting the partial top row only when it shows.
    pub rows_above_baseline: u32,
    /// The fraction of the top row the outline covers.
    pub top_row_coverage: f32,
}

/// `codepoint`'s extent in a TrueType face, read from `head`, a format 4 `cmap`, `loca` and the
/// `glyf` header, so it is the design outline a grid-fit-disabled rasterizer draws. A variable face
/// gives its default instance. `None` for a collection, a CFF face, an unmapped codepoint or an
/// empty glyph.
#[must_use]
pub fn glyph_extent(font: &[u8], codepoint: char) -> Option<GlyphExtent> {
    let head = table(font, b"head")?;
    let units_per_em = read_u16(head, 18)?;
    let long_offsets = read_u16(head, 50)? == 1;
    let glyph = usize::from(glyph_index(table(font, b"cmap")?, codepoint)?);
    let loca = table(font, b"loca")?;
    let (start, end) = if long_offsets {
        // `loca` holds 32-bit byte offsets.
        (read_u32(loca, glyph * 4)?, read_u32(loca, glyph * 4 + 4)?)
    } else {
        // When: `long_offsets` is false, `loca` holds 16-bit offsets in units of two bytes.
        let short = |index: usize| read_u16(loca, index * 2).map(|half| u32::from(half) * 2);
        (short(glyph)?, short(glyph + 1)?)
    };
    let (start, end) = (usize::try_from(start).ok()?, usize::try_from(end).ok()?);
    if end <= start {
        // When: `end <= start`, the glyph has no outline, so it has no extent.
        return None;
    }
    let header = table(font, b"glyf")?.get(start..end)?;
    Some(GlyphExtent { units_per_em, y_min: read_i16(header, 4)?, y_max: read_i16(header, 8)? })
}

/// The smallest coverage at which the renderer's blend of `foreground` over `background` (both
/// sRGB) moves some channel more than [`INK_DIFFERENCE`]; 1.0 when no coverage does.
#[must_use]
pub fn ink_threshold(foreground: [u8; 3], background: [u8; 3]) -> f32 {
    let decode = crate::color::srgb_u8_to_linear_lut();
    let shows = |coverage: f32| {
        let original = [background[2], background[1], background[0], 255];
        let mut pixel = original;
        let source = [
            decode[usize::from(foreground[0])] * coverage,
            decode[usize::from(foreground[1])] * coverage,
            decode[usize::from(foreground[2])] * coverage,
            coverage,
        ];
        crate::color::blend_premul_linear_over_srgb_bgra(&mut pixel, source);
        pixel[..3]
            .iter()
            .zip(&original[..3])
            .any(|(blended, before)| blended.abs_diff(*before) > INK_DIFFERENCE)
    };
    let (mut hidden, mut shown) = (0.0_f32, 1.0_f32);
    for _ in 0..32 {
        let middle = (hidden + shown) / 2.0;
        if shows(middle) {
            // The threshold is at or below `middle`.
            shown = middle;
        } else {
            // When: `middle` does not show, the threshold lies above it.
            hidden = middle;
        }
    }
    shown
}

/// The ink `extent` puts above the baseline at `strike_px` when coverage at or above `threshold`
/// shows: every whole row up to its top, and the partial top row only when it shows.
#[must_use]
pub fn predicted_ink_top(extent: GlyphExtent, strike_px: f32, threshold: f32) -> OutlineTop {
    let height_px = f32::from(extent.y_max) * strike_px / f32::from(extent.units_per_em);
    let whole_rows = height_px.floor();
    let top_row_coverage = height_px - whole_rows;
    let partial_shows = top_row_coverage >= threshold;
    OutlineTop {
        rows_above_baseline: whole_rows.max(0.0) as u32 + u32::from(partial_shows),
        top_row_coverage,
    }
}

/// The table `tag` of an sfnt font, or `None` when it is absent or a collection header.
fn table<'font>(font: &'font [u8], tag: &[u8; 4]) -> Option<&'font [u8]> {
    // A collection starts with `ttcf` and holds several faces, so it has no single table directory.
    (font.get(0..4)? != b"ttcf").then_some(())?;
    let table_count = usize::from(read_u16(font, 4)?);
    (0..table_count).find_map(|index| {
        let record = 12 + index * 16;
        (font.get(record..record + 4)? == tag).then_some(())?;
        let offset = usize::try_from(read_u32(font, record + 8)?).ok()?;
        let length = usize::try_from(read_u32(font, record + 12)?).ok()?;
        font.get(offset..offset.checked_add(length)?)
    })
}

/// `codepoint`'s glyph through the first Unicode format 4 subtable of `cmap`.
fn glyph_index(cmap: &[u8], codepoint: char) -> Option<u16> {
    let code = u16::try_from(u32::from(codepoint)).ok()?;
    let subtable_count = usize::from(read_u16(cmap, 2)?);
    let subtable = (0..subtable_count).find_map(|index| {
        let record = 4 + index * 8;
        let platform = read_u16(cmap, record)?;
        let encoding = read_u16(cmap, record + 2)?;
        let offset = usize::try_from(read_u32(cmap, record + 4)?).ok()?;
        let unicode = platform == 0 || (platform == 3 && encoding == 1);
        (unicode && read_u16(cmap, offset)? == 4).then_some(offset)
    })?;
    let segment_count = usize::from(read_u16(cmap, subtable + 6)? / 2);
    let end_codes = subtable + 14;
    let start_codes = end_codes + segment_count * 2 + 2;
    let deltas = start_codes + segment_count * 2;
    let range_offsets = deltas + segment_count * 2;
    let segment = (0..segment_count)
        .find(|segment| read_u16(cmap, end_codes + segment * 2).is_some_and(|end| end >= code))?;
    let start = read_u16(cmap, start_codes + segment * 2)?;
    let delta = read_u16(cmap, deltas + segment * 2)?;
    let range_offset_at = range_offsets + segment * 2;
    let range_offset = usize::from(read_u16(cmap, range_offset_at)?);
    let glyph = match (code >= start, range_offset) {
        (false, _) => 0,
        (true, 0) => code.wrapping_add(delta),
        (true, _) => {
            let at = range_offset_at + range_offset + usize::from(code - start) * 2;
            match read_u16(cmap, at)? {
                0 => 0,
                indexed => indexed.wrapping_add(delta),
            }
        }
    };
    (glyph != 0).then_some(glyph)
}

/// The big-endian `u16` at `offset`.
fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let field = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes([field[0], field[1]]))
}

/// The big-endian `i16` at `offset`.
fn read_i16(bytes: &[u8], offset: usize) -> Option<i16> {
    read_u16(bytes, offset).map(|value| i16::from_be_bytes(value.to_be_bytes()))
}

/// The big-endian `u32` at `offset`.
fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let field = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes([field[0], field[1], field[2], field[3]]))
}

#[cfg(test)]
#[path = "outline_ink_tests.rs"]
mod outline_ink_tests;
