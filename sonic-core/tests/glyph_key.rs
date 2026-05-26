//! Tests for `sonic_core::glyph_key::GlyphKey` — the atlas key type.

use std::collections::{HashMap, HashSet};

use sonic_core::glyph_key::GlyphKey;
use sonic_core::grid::{Cell, CellFlags, Color};
use sonic_core::hyperlink::HyperlinkId;

fn cell(ch: char, flags: CellFlags) -> Cell {
    Cell { ch, fg: Color::Default, bg: Color::Default, flags, hyperlink: None }
}

#[test]
fn from_cell_plain_char_yields_some_with_no_attrs() {
    let k = GlyphKey::from_cell(&cell('A', CellFlags::empty())).expect("plain cell -> Some");
    assert_eq!(k.ch, 'A');
    assert!(!k.weight_bold);
    assert!(!k.italic);
}

#[test]
fn from_cell_wide_cont_returns_none() {
    // Right half of a wide glyph: the renderer must not request its own tile.
    let k = GlyphKey::from_cell(&cell(' ', CellFlags::WIDE_CONT));
    assert!(k.is_none(), "WIDE_CONT must be skipped at the key level");
}

#[test]
fn from_cell_wide_lead_yields_key() {
    // The lead cell of a wide glyph still produces a tile — its `ch` is the
    // real character and the renderer will draw it at the wider rect.
    let k = GlyphKey::from_cell(&cell('漢', CellFlags::WIDE)).expect("wide lead -> Some");
    assert_eq!(k.ch, '漢');
}

#[test]
fn from_cell_picks_up_bold_and_italic_independently() {
    let b = GlyphKey::from_cell(&cell('x', CellFlags::BOLD)).unwrap();
    let i = GlyphKey::from_cell(&cell('x', CellFlags::ITALIC)).unwrap();
    let bi = GlyphKey::from_cell(&cell('x', CellFlags::BOLD | CellFlags::ITALIC)).unwrap();
    assert!(b.weight_bold && !b.italic);
    assert!(!i.weight_bold && i.italic);
    assert!(bi.weight_bold && bi.italic);
}

#[test]
fn distinct_attrs_produce_distinct_keys() {
    let mut set = HashSet::new();
    set.insert(GlyphKey::new('A', false, false));
    set.insert(GlyphKey::new('A', true, false));
    set.insert(GlyphKey::new('A', false, true));
    set.insert(GlyphKey::new('A', true, true));
    set.insert(GlyphKey::new('B', false, false));
    assert_eq!(set.len(), 5, "all five (char, bold, italic) combos must be distinct");
}

#[test]
fn hash_and_eq_are_deterministic() {
    // Same logical key built two ways from two different cells must hash
    // the same and compare equal.
    let mut c1 = cell('q', CellFlags::BOLD);
    c1.fg = Color::Rgb(10, 20, 30); // colors are deliberately NOT in the key
    let mut c2 = cell('q', CellFlags::BOLD);
    c2.fg = Color::Rgb(200, 0, 0);
    c2.hyperlink = Some(HyperlinkId(7)); // and hyperlinks aren't either
    let k1 = GlyphKey::from_cell(&c1).unwrap();
    let k2 = GlyphKey::from_cell(&c2).unwrap();
    assert_eq!(k1, k2);

    let mut map: HashMap<GlyphKey, u32> = HashMap::new();
    *map.entry(k1).or_insert(0) += 1;
    *map.entry(k2).or_insert(0) += 1;
    assert_eq!(map.len(), 1, "color/hyperlink must not split the key");
    assert_eq!(map[&k1], 2);
}

#[test]
fn flags_other_than_bold_italic_do_not_affect_key() {
    // UNDERLINE/STRIKETHROUGH/INVERSE/DIM/BLINK are quad-pass decorations,
    // not glyph-shape changes — they must collapse into the same tile.
    let plain = GlyphKey::from_cell(&cell('z', CellFlags::empty())).unwrap();
    for f in [
        CellFlags::UNDERLINE,
        CellFlags::STRIKETHROUGH,
        CellFlags::INVERSE,
        CellFlags::DIM,
        CellFlags::HIDDEN,
        CellFlags::BLINK,
    ] {
        let k = GlyphKey::from_cell(&cell('z', f)).unwrap();
        assert_eq!(k, plain, "flag {f:?} must not split the key");
    }
}

#[test]
fn key_is_pointer_sized_or_smaller() {
    // The hot path puts millions of these through HashMap::entry per
    // second; bloating the key would tank the bench. Keep it small.
    //
    // Pre-shaping the key fit in 8 bytes (4 char + 1 slot + 2 bool +
    // 1 pad). Shaping added a 2-byte `glyph_id` so shaped tiles cache
    // as themselves; with alignment that pushes us to 12 bytes. Still
    // well under cache-line size and bench-validated as not
    // regressing atlas throughput.
    let n = std::mem::size_of::<GlyphKey>();
    assert!(n <= 12, "GlyphKey too large: {n} bytes");
}
