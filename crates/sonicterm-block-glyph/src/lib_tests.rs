//! Crate-root unit tests: the public pixel glue, the shared raster case table, the
//! isolated-texel fixture check, and the reviewed raster digest table.
//!
//! The case table and the `rasterize` helper are shared with `customglyph_tests.rs`,
//! so every test rasterizes through the same entry point and the same size mix.

use crate::customglyph::{BlockKey, SizedBlockKey};
use crate::glue::{BgraPixel, Size};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One raster request in the shape the GPU renderer's block branch sends it:
/// integer cell width and height and underline thickness, each clamped to at
/// least 1, and anti-aliasing on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RasterCase {
    /// Cell width in raster pixels.
    pub(crate) w: isize,
    /// Cell height in raster pixels.
    pub(crate) h: isize,
    /// Underline thickness in raster pixels; light strokes use this width.
    pub(crate) underline: isize,
}

/// Sizes the raster tests draw from. Both parities appear on each axis because
/// coordinate hinting moves only integral midpoints, so odd and even cells take
/// different code paths through the same geometry.
pub(crate) const RASTER_CASES: [RasterCase; 6] = [
    RasterCase { w: 5, h: 9, underline: 1 },
    RasterCase { w: 8, h: 16, underline: 1 },
    RasterCase { w: 15, h: 31, underline: 2 },
    RasterCase { w: 16, h: 32, underline: 2 },
    RasterCase { w: 30, h: 40, underline: 2 },
    RasterCase { w: 45, h: 60, underline: 3 },
];

/// The alpha plane of one rasterized tile, the only channel the renderer keeps.
#[derive(Clone, Debug)]
pub(crate) struct AlphaRaster {
    /// Tile width in texels.
    pub(crate) w: usize,
    /// Tile height in texels.
    pub(crate) h: usize,
    /// Row-major alpha, `w * h` bytes.
    pub(crate) alpha: Vec<u8>,
}

impl AlphaRaster {
    /// Alpha of the texel at column `x`, row `y`.
    pub(crate) fn at(&self, x: usize, y: usize) -> u8 {
        self.alpha[y * self.w + x]
    }

    /// Sum of every texel's alpha.
    pub(crate) fn sum(&self) -> u64 {
        self.alpha.iter().map(|&a| u64::from(a)).sum()
    }

    /// Alpha of column `x`, top to bottom.
    pub(crate) fn column(&self, x: usize) -> Vec<u8> {
        (0..self.h).map(|y| self.at(x, y)).collect()
    }

    /// Alpha of row `y`, left to right.
    pub(crate) fn row(&self, y: usize) -> Vec<u8> {
        self.alpha[y * self.w..(y + 1) * self.w].to_vec()
    }

    /// Inclusive `(x0, y0, x1, y1)` bounds of the texels with nonzero alpha, or
    /// `None` for a blank tile.
    pub(crate) fn ink_bbox(&self) -> Option<(usize, usize, usize, usize)> {
        let mut bbox: Option<(usize, usize, usize, usize)> = None;
        for y in 0..self.h {
            for x in 0..self.w {
                if self.at(x, y) == 0 {
                    continue;
                }
                bbox = Some(match bbox {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        bbox
    }
}

/// Rasterize `block` for `case` through the public entry point and return its
/// alpha plane.
///
/// Panics, naming the key and size, when rasterization fails or the tile breaks
/// the renderer's contract: exact cell dimensions, `w * h * 4` bytes of storage,
/// zero offsets, and an advance equal to the width.
pub(crate) fn rasterize(block: BlockKey, case: RasterCase) -> AlphaRaster {
    let key = SizedBlockKey { block, size: Size::new(case.w, case.h) };
    let tile = crate::block_sprite_with_cell_metrics(key, case.underline, true)
        .unwrap_or_else(|err| panic!("{block:?} at {case:?} failed to rasterize: {err:#}"));
    let (w, h) = (case.w as usize, case.h as usize);
    assert_eq!(
        (tile.width as usize, tile.height as usize),
        (w, h),
        "{block:?} at {case:?} returned the wrong tile size"
    );
    assert_eq!(tile.coverage.len(), w * h * 4, "{block:?} at {case:?} has short storage");
    assert_eq!((tile.offset_x, tile.offset_y), (0, 0), "{block:?} at {case:?} is offset");
    assert_eq!(
        tile.advance.to_bits(),
        (w as f32).to_bits(),
        "{block:?} at {case:?} advances {} instead of its width",
        tile.advance
    );
    let alpha = tile.coverage.chunks_exact(4).map(|px| px[3]).collect();
    AlphaRaster { w, h, alpha }
}

/// FNV-1a 64 over `bytes`; the digest the reviewed raster table records.
pub(crate) fn fnv1a64(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Digest of a raster: its width and height as little-endian `u32`, then its
/// row-major alpha.
pub(crate) fn raster_digest(raster: &AlphaRaster) -> u64 {
    let dims = (raster.w as u32).to_le_bytes().into_iter().chain((raster.h as u32).to_le_bytes());
    fnv1a64(dims.chain(raster.alpha.iter().copied()))
}

#[test]
fn exports_pixel_glue() {
    let px = BgraPixel::rgba(1, 2, 3, 4);
    assert_eq!(px, BgraPixel(3, 2, 1, 4));
    assert_eq!(px.a(), 4);
}

/// The shared case table must stay inside the renderer's input domain: the
/// renderer clamps cell sides and underline thickness to at least 1, and the
/// table must keep both parities on each axis so the hinting paths stay covered.
#[test]
fn raster_cases_match_the_renderer_clamps() {
    for case in RASTER_CASES {
        assert!(case.w >= 1 && case.h >= 1 && case.underline >= 1, "{case:?} is below the clamp");
    }
    for parity in [0, 1] {
        assert!(RASTER_CASES.iter().any(|c| c.w % 2 == parity), "no width of parity {parity}");
        assert!(RASTER_CASES.iter().any(|c| c.h % 2 == parity), "no height of parity {parity}");
    }
}

/// The table's digest is FNV-1a 64; published test vectors pin the constants
/// so a typo in the offset basis or prime cannot silently re-key every row.
#[test]
fn fnv1a64_matches_published_vectors() {
    assert_eq!(fnv1a64(*b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(*b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(*b"foobar"), 0x8594_4171_f739_67e8);
}

/// The renderer keeps only each block tile's alpha and draws it as monochrome
/// coverage tinted by the cell foreground. A fully opaque texel whose four
/// neighbours are all transparent therefore reaches the screen as an isolated
/// foreground-coloured dot with no anti-aliasing falloff, the signature of the
/// stray marks once reported against the terminal.
///
/// Braille, sextant, and octant patterns are the densest small-feature geometry
/// this crate draws, so they are scanned at every case size. Every key must
/// rasterize at the requested size and draw ink, except `Sextant(0b1000_0000)`:
/// sextants read only bits 0 through 5, so that pattern is blank by design and
/// must be fully transparent.
#[test]
fn block_glyph_geometry_does_not_leave_isolated_opaque_texels() {
    let mut keys = Vec::new();
    for bits in [0b0000_0001u8, 0b1000_0000, 0b0101_0101, 0b1111_1111] {
        keys.push(BlockKey::Braille(bits));
        keys.push(BlockKey::Sextant(bits));
        keys.push(BlockKey::Octant(bits));
    }

    let mut isolated = Vec::new();
    for case in RASTER_CASES {
        for &block in &keys {
            let raster = rasterize(block, case);
            if block == BlockKey::Sextant(0b1000_0000) {
                assert!(
                    raster.alpha.iter().all(|&a| a == 0),
                    "{block:?} at {case:?} must be blank: sextants ignore bits 6 and 7"
                );
            } else {
                assert!(raster.sum() > 0, "{block:?} at {case:?} drew no ink");
            }
            for y in 1..raster.h.saturating_sub(1) {
                for x in 1..raster.w.saturating_sub(1) {
                    let neighbours = [
                        raster.at(x - 1, y),
                        raster.at(x + 1, y),
                        raster.at(x, y - 1),
                        raster.at(x, y + 1),
                    ];
                    if raster.at(x, y) == 255 && neighbours.iter().all(|&a| a == 0) {
                        isolated.push((block, case.w, case.h, x, y));
                    }
                }
            }
        }
    }

    assert!(
        isolated.is_empty(),
        "block geometry produced {} fully opaque texel(s) with no inked neighbour, which reach \
         the screen as isolated foreground-coloured dots: {:?}",
        isolated.len(),
        &isolated[..isolated.len().min(8)]
    );
}

/// Environment variable that switches `raster_digests_match_reviewed_table` from
/// comparing to rewriting the reviewed table.
const BLESS_VAR: &str = "SONICTERM_BLESS_BLOCK_GLYPH";

/// The command that regenerates the reviewed table.
const BLESS_COMMAND: &str =
    "SONICTERM_BLESS_BLOCK_GLYPH=1 cargo test -p sonicterm-block-glyph raster_digests";

/// Reviewed digest table, relative to this crate's manifest directory.
const GOLDEN_FILE: &str = "raster-digests.golden.tsv";

/// Comment header of the reviewed table. Bless rewrites the file as this header
/// followed by the generated rows, so the header in the tracked file must match.
const GOLDEN_HEADER: &str = "\
# Reviewed block-glyph raster digests; see crates/sonicterm-block-glyph/CLAUDE.md.
# Regenerate only for a named geometry change:
#   SONICTERM_BLESS_BLOCK_GLYPH=1 cargo test -p sonicterm-block-glyph raster_digests
# Bless rewrites this file and fails; review the diff, then rerun without the variable.
# alpha_sum: sum of every texel's alpha. ink_bbox: inclusive x0,y0,x1,y1 of nonzero alpha.
# fnv1a64: FNV-1a 64 over width and height (u32 LE), then row-major alpha.
# codepoint\tcell_w\tcell_h\tunderline\talpha_sum\tink_bbox\tfnv1a64
";

/// Codepoints in the reviewed table: at least one per `BlockKey` family that
/// `from_char` maps, weighted toward joins, weights, and shapes the invariant
/// tests do not pin texel by texel.
const DIGEST_CODEPOINTS: [char; 37] = [
    // Box drawing: straight light and heavy lines, a dash, corners, a tee, and crosses.
    '\u{2500}',
    '\u{2501}',
    '\u{2502}',
    '\u{2503}',
    '\u{2504}',
    '\u{250C}',
    '\u{250F}',
    '\u{251C}',
    '\u{253C}',
    '\u{254B}',
    // Mixed weights, double lines, an arc, and a diagonal cross.
    '\u{251D}',
    '\u{2550}',
    '\u{256C}',
    '\u{256D}',
    '\u{2573}',
    // Block elements: a half block, the full block, the three shades, and a quadrant.
    '\u{2580}',
    '\u{2588}',
    '\u{2591}',
    '\u{2592}',
    '\u{2593}',
    '\u{259A}',
    // Sextant, octant, and asymmetric and full Braille patterns.
    '\u{1FB0B}',
    '\u{1CD00}',
    '\u{2814}',
    '\u{28FF}',
    // Triangles, cell diagonals, and a legacy-computing polygon.
    '\u{1FB68}',
    '\u{1FBA0}',
    '\u{1FB3C}',
    // Powerline filled and outline arrows, a filled semicircle, and a split triangle.
    '\u{E0B0}',
    '\u{E0B1}',
    '\u{E0B4}',
    '\u{E0B8}',
    // Progress chunks, graph branches, and a spinner segment.
    '\u{EE00}',
    '\u{EE04}',
    '\u{F5D0}',
    '\u{F5EE}',
    '\u{EE06}',
];

/// One generated table row: its identity columns, its value columns, and the
/// raster that produced them.
struct DigestRow {
    key: String,
    value: String,
    raster: AlphaRaster,
}

/// Rasterize every table codepoint at all six case sizes, in table order.
fn generate_digest_rows() -> Vec<DigestRow> {
    let mut rows = Vec::new();
    for c in DIGEST_CODEPOINTS {
        let block = BlockKey::from_char(c)
            .unwrap_or_else(|| panic!("U+{:04X} is in the digest table but not mapped", c as u32));
        for case in RASTER_CASES {
            let raster = rasterize(block, case);
            let bbox = match raster.ink_bbox() {
                Some((x0, y0, x1, y1)) => format!("{x0},{y0},{x1},{y1}"),
                None => "-".to_owned(),
            };
            let key = format!("U+{:04X}\t{}\t{}\t{}", c as u32, case.w, case.h, case.underline);
            let value = format!("{}\t{bbox}\t{:016x}", raster.sum(), raster_digest(&raster));
            rows.push(DigestRow { key, value, raster });
        }
    }
    rows
}

/// Parse the reviewed table's data rows as `(key, value)` pairs, skipping blank
/// and `#` comment lines. Panics on a row without exactly seven columns.
fn parse_reviewed_rows(text: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for line in text.lines().map(|line| line.trim_end_matches('\r')) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 7, "{GOLDEN_FILE}: malformed row {line:?}");
        rows.push((fields[..4].join("\t"), fields[4..].join("\t")));
    }
    rows
}

/// A row key as `U+XXXX WxH/underline`.
fn describe_key(key: &str) -> String {
    let fields: Vec<&str> = key.split('\t').collect();
    format!("{} {}x{}/{}", fields[0], fields[1], fields[2], fields[3])
}

/// A row value as `alpha_sum=.. ink_bbox=.. fnv1a64=..`.
fn describe_value(value: &str) -> String {
    let fields: Vec<&str> = value.split('\t').collect();
    format!("alpha_sum={} ink_bbox={} fnv1a64={}", fields[0], fields[1], fields[2])
}

/// A raster as one line of two-digit hex alpha per texel row.
fn raster_hex(raster: &AlphaRaster) -> String {
    let mut out = String::new();
    for y in 0..raster.h {
        for a in raster.row(y) {
            out.push_str(&format!("{a:02x}"));
        }
        out.push('\n');
    }
    out
}

/// Every table codepoint must rasterize exactly as the reviewed table records:
/// the same alpha sum, ink bounds, and digest, at every case size,
/// with no tolerance, and with the same rows in the same order on every host.
///
/// With `SONICTERM_BLESS_BLOCK_GLYPH=1` the test rewrites the table from the
/// generated rows, prints each changed raster in hex, and then fails, so a bless
/// run can never pass; only a plain rerun against the reviewed file can.
#[test]
fn raster_digests_match_reviewed_table() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_FILE);
    let bless = std::env::var(BLESS_VAR).is_ok_and(|value| value == "1");
    let reviewed_text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) if bless => String::new(),
        Err(err) => {
            panic!("cannot read {}: {err}; regenerate it with {BLESS_COMMAND}", path.display())
        }
    };
    let reviewed = parse_reviewed_rows(&reviewed_text);
    let reviewed_map: BTreeMap<&str, &str> =
        reviewed.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
    let generated = generate_digest_rows();

    let mut problems = Vec::new();
    let mut changed = Vec::new();
    for row in &generated {
        match reviewed_map.get(row.key.as_str()) {
            Some(&old) if old == row.value => {}
            Some(&old) => {
                problems.push(format!(
                    "{}: reviewed {} but rasterized {}",
                    describe_key(&row.key),
                    describe_value(old),
                    describe_value(&row.value)
                ));
                changed.push(row);
            }
            None => {
                problems.push(format!(
                    "{}: missing from the table; rasterized {}",
                    describe_key(&row.key),
                    describe_value(&row.value)
                ));
                changed.push(row);
            }
        }
    }
    let generated_keys: Vec<&str> = generated.iter().map(|row| row.key.as_str()).collect();
    for (key, value) in &reviewed {
        if !generated_keys.contains(&key.as_str()) {
            problems.push(format!(
                "{}: stale row no longer generated ({})",
                describe_key(key),
                describe_value(value)
            ));
        }
    }
    let reviewed_keys: Vec<&str> = reviewed.iter().map(|(key, _)| key.as_str()).collect();
    if problems.is_empty() && reviewed_keys != generated_keys {
        problems.push("rows are duplicated or out of case-table order".to_owned());
    }

    if bless {
        let mut text = GOLDEN_HEADER.to_owned();
        for row in &generated {
            text.push_str(&format!("{}\t{}\n", row.key, row.value));
        }
        std::fs::write(&path, text)
            .unwrap_or_else(|err| panic!("cannot write {}: {err}", path.display()));
        for row in &changed {
            println!(
                "{} {}\n{}",
                describe_key(&row.key),
                describe_value(&row.value),
                raster_hex(&row.raster)
            );
        }
        panic!(
            "{BLESS_VAR}=1 rewrote {} with {} rows ({} changed, {} problems before the rewrite); \
             review the diff and the rasters printed above, then rerun without {BLESS_VAR}",
            path.display(),
            generated.len(),
            changed.len(),
            problems.len()
        );
    }

    assert!(
        problems.is_empty(),
        "{} of {} rasters differ from {}:\n{}\nIf a named geometry change caused this, \
         regenerate the table with {BLESS_COMMAND} and attach the printed rasters for review.",
        problems.len(),
        generated.len(),
        path.display(),
        problems.join("\n")
    );
}

/// The digest generator must include the full codepoint/size product, especially
/// the tiny spinner case with a collapsed inner clear circle. This checks
/// membership without inventing a pixel digest; the separate golden test reviews it.
#[test]
fn raster_digest_cases_have_no_size_exclusions() {
    let rows = generate_digest_rows();
    assert_eq!(rows.len(), DIGEST_CODEPOINTS.len() * RASTER_CASES.len());
    assert_eq!(rows.len(), 222);
    assert_eq!(rows.iter().filter(|row| row.key == "U+EE06\t5\t9\t1").count(), 1);
}
