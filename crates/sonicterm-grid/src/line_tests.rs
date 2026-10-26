//! Behavior tests for [`Line`] cluster/flat storage.
//!
//! Coverage map:
//! * cluster ⇄ flat range iteration and random access parity,
//! * forward / backward (`DoubleEndedIterator`) walks and `Hash` parity
//!   across storage forms,
//! * smart same-cell no-op vs changed-cell degrade to Flat,
//! * resize / truncate / compression storage transitions,
//! * wide / wide-continuation / extras / hyperlink metadata fidelity through
//!   compression and degradation.

use super::line_test_support::LINE_SOURCE as LINE_CHECKOUT;
use super::line_test_support::*;
use super::*;
use sonicterm_types::cell::{CellFlags, Color};
use sonicterm_types::HyperlinkId;

// --- helpers --------------------------------------------------------------

fn hash_of(line: &Line) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn resize_clipped_wide_tail_uses_fill_without_flattening_or_resurrection() {
    // Shrinking through a wide pair removes the orphan in either storage form without reflow.
    let lead = Cell::plain('中', Color::Default, Color::Default, CellFlags::WIDE);
    let continuation = Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT);
    let fill = Cell::plain(' ', Color::Default, Color::Indexed(3), CellFlags::empty());
    let (flat, clustered) = flat_and_cluster(&[
        (plain_cell('a'), 1),
        (plain_cell('b'), 1),
        (lead, 1),
        (continuation, 1),
    ]);
    for mut line in [flat, clustered] {
        let was_clustered = line.is_clustered();
        line.resize(3, fill.clone());
        assert_eq!(line.get(2), Some(&fill));
        assert_eq!(line.is_clustered(), was_clustered);
        line.resize(4, fill.clone());
        assert_eq!(line.get(2), Some(&fill));
        assert_eq!(line.get(3), Some(&fill));
        line.resize(4, blank());
        assert_eq!(line.get(2), Some(&fill));
    }
}

#[test]
fn resize_preserves_complete_wide_pairs_and_compact_blank_rows() {
    // Intact pairs survive adjacent changes; single-column clipping and zero-length shrink remain valid.
    let lead = Cell::plain('中', Color::Default, Color::Default, CellFlags::WIDE);
    let continuation = Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT);
    let (flat, clustered) =
        flat_and_cluster(&[(lead.clone(), 1), (continuation.clone(), 1), (blank(), 2)]);
    for mut line in [flat, clustered] {
        line.resize(3, blank());
        line.resize(2, blank());
        assert_eq!(line.get(0), Some(&lead));
        assert_eq!(line.get(1), Some(&continuation));
        line.resize(1, blank());
        assert_eq!(line.get(0), Some(&blank()));
        line.resize(2, blank());
        assert_eq!(line.get(0), Some(&blank()));
        line.resize(0, blank());
        assert!(line.is_empty());
    }
    let mut blanks = Line::from_clusters(vec![Cluster { cell: blank(), count: 4000 }]);
    blanks.resize(3999, blank());
    assert!(blanks.is_clustered());
    assert!(blanks.approx_capacity_byte_size() < 1000);
}

#[test]
fn clipped_wide_repair_coalesces_fill_and_preserves_neighbor_attributes() {
    // Only the clipped edge loses its metadata; equal fill coalesces rather than splitting compact runs.
    let fill = ch_bold(' ');
    let lead = rich('中');
    let continuation = Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT);
    let mut line = Line::from_clusters(vec![
        Cluster { cell: fill.clone(), count: 8 },
        Cluster { cell: lead, count: 1 },
        Cluster { cell: continuation, count: 1 },
    ]);
    line.resize(9, fill.clone());
    assert_eq!(line.storage(), &LineStorage::Cluster(vec![Cluster { cell: fill, count: 9 }]));
    assert_eq!(line.fat_attribute_bytes(), 0);
}

// --- cluster/flat range iteration + random access parity ------------------

#[test]
fn hash_parity_between_flat_and_equal_cluster() {
    let (flat, clustered) = flat_and_cluster(&[(blank(), 10), (plain_cell('x'), 2), (blank(), 8)]);
    assert_eq!(
        hash_of(&flat),
        hash_of(&clustered),
        "cluster and content-equal flat line must hash identically"
    );
}

#[test]
fn hash_differs_on_different_content() {
    let original = Line::from_flat(vec![plain_cell('a'), plain_cell('b'), plain_cell('c')]);
    let changed = Line::from_flat(vec![plain_cell('a'), plain_cell('z'), plain_cell('c')]);
    assert_ne!(hash_of(&original), hash_of(&changed));
}

/// Automatic-wrap provenance is logical row identity, while fresh rows start unwrapped.
#[test]
fn soft_wrap_provenance_participates_in_equality_and_hash() {
    let plain = Line::from_flat(vec![plain_cell('a'), plain_cell('b')]);
    let mut wrapped = plain.clone();
    assert!(!plain.soft_wrapped_from_previous());

    assert!(wrapped.set_soft_wrapped_from_previous(true));

    assert_ne!(plain, wrapped);
    assert_ne!(hash_of(&plain), hash_of(&wrapped));
}

/// Packing provenance into the sequence word keeps the row-container memory contract unchanged.
#[test]
fn soft_wrap_provenance_does_not_enlarge_line() {
    assert_eq!(
        std::mem::size_of::<Line>(),
        std::mem::size_of::<LineStorage>() + std::mem::size_of::<u64>()
    );
}

// --- smart same-cell no-op vs changed-cell degrade ------------------------

fn clustered_uniform(cell: Cell, len: usize) -> Line {
    let mut line = Line::from_flat(vec![cell; len]);
    assert!(line.try_compress(), "uniform line must compress");
    assert!(line.is_clustered());
    line
}

#[test]
fn set_same_cell_is_noop_and_stays_cluster() {
    let mut line = clustered_uniform(blank(), 80);
    assert!(line.set(10, blank()), "in-range write returns true");
    assert!(line.is_clustered(), "same-cell write must NOT degrade");
    assert_eq!(line.cluster_representative(), Some(blank()));
}

#[test]
fn set_different_char_degrades_to_flat() {
    let mut line = clustered_uniform(blank(), 80);
    assert!(line.set(5, plain_cell('X')));
    assert!(!line.is_clustered(), "char mismatch must degrade");
    assert_eq!(line.get(5), Some(&plain_cell('X')));
    assert_eq!(line.get(4), Some(&blank()));
    assert_eq!(line.get(6), Some(&blank()));
    assert_eq!(line.len(), 80);
}

#[test]
fn set_different_attrs_degrades_to_flat() {
    let mut line = clustered_uniform(plain_cell(' '), 40);
    assert!(line.set(10, ch_bold(' ')));
    assert!(!line.is_clustered(), "attr mismatch must degrade");
    assert_eq!(line.get(10).map(|cell| cell.flags), Some(CellFlags::BOLD));
    assert_eq!(line.len(), 40);
}

#[test]
fn set_out_of_range_returns_false_and_preserves_storage() {
    let mut line = clustered_uniform(blank(), 8);
    assert!(!line.set(100, plain_cell('Z')));
    assert!(line.is_clustered(), "rejected write must not degrade");
}

#[test]
fn multi_cluster_set_has_no_representative_and_degrades() {
    // A line with >1 cluster has no single representative, so even a write
    // equal to the target cell degrades (the smart path only covers uniform).
    let mut line = Line::from_clusters(vec![
        Cluster { cell: plain_cell('a'), count: 4 },
        Cluster { cell: plain_cell('b'), count: 4 },
    ]);
    assert_eq!(line.cluster_representative(), None);
    assert!(line.set(0, plain_cell('a')));
    assert!(!line.is_clustered());
    assert_eq!(line.get(0), Some(&plain_cell('a')));
    assert_eq!(line.get(4), Some(&plain_cell('b')));
}

#[test]
fn fill_range_matching_stays_cluster_mismatch_degrades() {
    let mut keep = clustered_uniform(blank(), 80);
    keep.fill_range(10, 50, blank());
    assert!(keep.is_clustered(), "matching fill must NOT degrade");

    let mut degrade = clustered_uniform(blank(), 80);
    degrade.fill_range(10, 50, ch_bold(' '));
    assert!(!degrade.is_clustered());
    for column in 0..10 {
        assert_eq!(degrade.get(column), Some(&blank()), "prefix {column}");
    }
    for column in 10..50 {
        assert_eq!(
            degrade.get(column).map(|cell| cell.flags),
            Some(CellFlags::BOLD),
            "filled {column}"
        );
    }
    for column in 50..80 {
        assert_eq!(degrade.get(column), Some(&blank()), "suffix {column}");
    }
}

#[test]
fn fill_range_empty_or_reversed_is_noop() {
    let mut line = clustered_uniform(blank(), 40);
    line.fill_range(5, 5, plain_cell('X'));
    line.fill_range(10, 3, plain_cell('X'));
    assert!(line.is_clustered(), "no-op fills must not degrade");
    assert_eq!(line.len(), 40);
}

// --- resize / truncate / compression transitions --------------------------

#[test]
fn resize_grow_matching_fill_stays_single_cluster() {
    let mut line = Line::from_clusters(vec![Cluster { cell: blank(), count: 80 }]);
    line.resize(100, blank());
    assert!(line.is_clustered());
    assert_eq!(line.len(), 100);
    match line.storage() {
        LineStorage::Cluster(clusters) => {
            assert_eq!(clusters.len(), 1, "matching fill merges into one cluster");
            assert_eq!(clusters[0].count, 100);
        }
        _ => panic!("expected cluster"),
    }
}

#[test]
fn resize_grow_mismatched_fill_appends_second_cluster() {
    let mut red = blank();
    red.bg = Color::Indexed(1);
    let mut line = Line::from_clusters(vec![Cluster { cell: red.clone(), count: 80 }]);
    line.resize(100, blank());
    assert!(line.is_clustered(), "stays clustered as multi-cluster");
    match line.storage() {
        LineStorage::Cluster(clusters) => {
            assert_eq!(clusters.len(), 2);
            assert_eq!(clusters[0], Cluster { cell: red, count: 80 });
            assert_eq!(clusters[1], Cluster { cell: blank(), count: 20 });
        }
        _ => panic!("expected cluster"),
    }
}

#[test]
fn resize_shrink_delegates_to_truncate() {
    let mut line = Line::from_flat(vec![
        plain_cell('a'),
        plain_cell('b'),
        plain_cell('c'),
        plain_cell('d'),
        plain_cell('e'),
    ]);
    line.resize(2, blank());
    assert_eq!(line.len(), 2);
    assert_eq!(line.to_vec(), vec![plain_cell('a'), plain_cell('b')]);
}

#[test]
fn truncate_within_single_cluster_preserves_form() {
    let mut line = Line::from_clusters(vec![Cluster { cell: blank(), count: 80 }]);
    line.truncate(50);
    assert!(line.is_clustered());
    match line.storage() {
        LineStorage::Cluster(clusters) => {
            assert_eq!(clusters.len(), 1);
            assert_eq!(clusters[0].count, 50);
        }
        _ => panic!("expected cluster"),
    }
}

#[test]
fn truncate_across_clusters_and_to_zero() {
    let mut red = blank();
    red.bg = Color::Indexed(1);
    let mut line = Line::from_clusters(vec![
        Cluster { cell: red.clone(), count: 30 },
        Cluster { cell: blank(), count: 50 },
    ]);
    line.truncate(40); // lands inside the second cluster
    match line.storage() {
        LineStorage::Cluster(clusters) => {
            assert_eq!(clusters.len(), 2);
            assert_eq!(clusters[0].count, 30);
            assert_eq!(clusters[1].count, 10);
        }
        _ => panic!("expected cluster"),
    }
    line.truncate(20); // drops the second cluster entirely
    match line.storage() {
        LineStorage::Cluster(clusters) => {
            assert_eq!(clusters.len(), 1);
            assert_eq!(clusters[0], Cluster { cell: red, count: 20 });
        }
        _ => panic!("expected cluster"),
    }
    line.truncate(0); // empties to Flat
    assert!(line.is_empty());
    assert!(!line.is_clustered());
}

#[test]
fn truncate_noop_when_new_len_ge_current() {
    let mut line = Line::from_flat(vec![plain_cell('x'), plain_cell('y')]);
    line.truncate(10);
    assert_eq!(line.len(), 2);
}

#[test]
fn try_compress_requires_full_uniformity() {
    let mut uniform = Line::from_flat(vec![blank(); 10]);
    assert!(uniform.try_compress());
    assert!(uniform.is_clustered());

    // Non-uniform stays flat.
    let mut mixed = Line::from_flat(vec![blank(), plain_cell('x'), blank()]);
    assert!(!mixed.try_compress());
    assert!(!mixed.is_clustered());

    // Already cluster and empty flat are both no-ops.
    assert!(!uniform.try_compress());
    let mut empty = Line::from_flat(Vec::new());
    assert!(!empty.try_compress());
}

#[test]
fn iter_mut_and_as_vec_mut_force_flat() {
    let mut line = clustered_uniform(plain_cell('a'), 3);
    for slot in line.iter_mut() {
        slot.ch = 'Z';
    }
    assert!(!line.is_clustered(), "iter_mut degrades to Flat");
    assert_eq!(line.to_vec(), vec![plain_cell('Z'), plain_cell('Z'), plain_cell('Z')]);

    let mut line2 = clustered_uniform(plain_cell('a'), 4);
    line2.as_vec_mut().push(plain_cell('b'));
    assert!(!line2.is_clustered());
    assert_eq!(line2.len(), 5);
}

// --- metadata fidelity: wide / wide-cont / extras / hyperlink -------------

#[test]
fn rich_metadata_survives_compression_and_access() {
    // A uniform run of fully-decorated cells compresses; every access form
    // must reproduce the wide flag, colors, hyperlink id, and extras.
    let mut line = Line::from_flat(vec![rich('A'); 20]);
    assert!(line.try_compress(), "uniform rich run compresses");
    assert!(line.is_clustered());

    let via_get = line.get(0).expect("cell 0");
    assert_eq!(via_get, &rich('A'));
    assert!(via_get.flags.contains(CellFlags::WIDE));
    assert_eq!(via_get.hyperlink(), Some(HyperlinkId(7)));
    assert_eq!(via_get.extras(), Some("\u{0301}"));
    assert_eq!(via_get.fg, Color::Rgb(10, 20, 30));
    assert_eq!(via_get.bg, Color::Indexed(4));

    // Every iterated cell is byte-identical to the representative.
    assert!(line.iter().all(|cell| cell == &rich('A')));
    // Range access through the cluster preserves the fat channels too.
    assert!(line.get_range(3, 7).all(|cell| cell.hyperlink() == Some(HyperlinkId(7))));
}

#[test]
fn metadata_survives_degradation_from_cluster() {
    let mut line = Line::from_flat(vec![rich('A'); 10]);
    assert!(line.try_compress());
    // Punch a different cell in the middle: storage degrades, but the
    // untouched neighbours keep their full metadata.
    assert!(line.set(5, plain_cell('x')));
    assert!(!line.is_clustered());
    assert_eq!(line.get(5), Some(&plain_cell('x')));
    for column in (0..10).filter(|&column| column != 5) {
        let cell = line.get(column).expect("neighbour present");
        assert_eq!(cell, &rich('A'), "neighbour {column} lost metadata");
        assert_eq!(cell.extras(), Some("\u{0301}"));
    }
}

#[test]
fn wide_lead_and_continuation_pair_roundtrip() {
    // Model a wide glyph as lead (WIDE) + continuation (WIDE_CONT): the pair
    // must survive flat storage, cluster access, and hash parity.
    let lead = Cell::plain('中', Color::Default, Color::Default, CellFlags::WIDE);
    let cont = Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT);
    let (flat, clustered) = flat_and_cluster(&[(lead.clone(), 1), (cont.clone(), 1), (blank(), 6)]);

    assert_eq!(flat.get(0), clustered.get(0));
    assert!(clustered.get(0).unwrap().flags.contains(CellFlags::WIDE));
    assert!(clustered.get(1).unwrap().flags.contains(CellFlags::WIDE_CONT));
    assert_eq!(flat.to_vec(), clustered.to_vec());
    assert_eq!(hash_of(&flat), hash_of(&clustered));
}

// --- basic constructors / index surface -----------------------------------

#[test]
fn index_ops_and_len_reflect_storage() {
    let mut line = Line::from_flat(vec![plain_cell('a'), plain_cell('b'), plain_cell('c')]);
    assert_eq!(line[1], plain_cell('b'));
    line[1] = plain_cell('Z');
    assert_eq!(line[1], plain_cell('Z'));
    assert!(!line.is_clustered(), "IndexMut degrades to Flat");

    let filled = Line::flat_filled(5, blank());
    assert_eq!(filled.len(), 5);
    assert!(!filled.is_empty());
    assert!(Line::from_flat(Vec::new()).is_empty());
}

// ---------------------------------------------------------------------------
// Rare-attribute box accounting
// ---------------------------------------------------------------------------

/// The mechanism behind adjacent-resize capacity retention, pinned where it
/// happens.
///
/// `Vec::resize` growing past capacity does not allocate the amount asked for
/// — it doubles. Growing an 80-cell row to 81 allocates 160, and shrinking
/// back to 80 truncates the length while keeping all 160.
///
/// Pinned at this level because the aggregate effect is easy to mis-attribute.
/// While investigating it I offered three wrong explanations — resize
/// direction, working-set size, and construction path — each of which fitted
/// the grid-level numbers and none of which was the cause. Reading it here
/// takes one assertion.
#[test]
fn growing_a_row_by_one_cell_doubles_its_capacity_and_shrinking_keeps_it() {
    let mut line = Line::from_flat(vec![Cell::default(); 80]);
    let cells = |line: &Line| line.approx_capacity_byte_size() / std::mem::size_of::<Cell>();

    assert_eq!(cells(&line), 80, "a row built at an exact size reserves exactly that");

    line.resize(81, Cell::default());
    assert!(
        cells(&line) > 81,
        "growing past capacity doubles rather than allocating what was asked for; \
         this is the allocation the aggregate pass exists to reclaim"
    );

    let grown = cells(&line);
    line.resize(80, Cell::default());
    assert_eq!(
        cells(&line),
        grown,
        "shrinking truncates the length and keeps the capacity — 80 wasted cells per \
         row, which across a full scrollback is 1.875 MiB"
    );
}

// --- trimmed history rows -------------------------------------------------

/// A blank cell on a coloured background: an erased tail under background-colour erase.
fn coloured_fill() -> Cell {
    Cell::plain(' ', Color::Default, Color::Indexed(4), CellFlags::empty())
}

fn wide_lead() -> Cell {
    Cell::plain('中', Color::Default, Color::Default, CellFlags::WIDE)
}

fn wide_continuation() -> Cell {
    Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT)
}

fn text_cells(text: &str) -> Vec<Cell> {
    text.chars().map(plain_cell).collect()
}

/// `cells` padded with `fill` to `width` columns.
fn padded(mut cells: Vec<Cell>, fill: Cell, width: usize) -> Vec<Cell> {
    cells.resize(width, fill);
    cells
}

/// The trim fixtures: each 200-column row, and whether `try_trim` must trim it.
fn trim_rows() -> Vec<(&'static str, Vec<Cell>, bool)> {
    let mut linked = plain_cell('l');
    linked.set_hyperlink(Some(HyperlinkId(9)));
    let mut combining = plain_cell('e');
    combining.set_extras(Some("\u{0301}".to_string().into_boxed_str()));
    let pairs: Vec<Cell> = (0..20).flat_map(|_| [wide_lead(), wide_continuation()]).collect();
    vec![
        (
            "text",
            padded(text_cells("forty characters of ordinary shell output"), blank(), 200),
            true,
        ),
        (
            "wide and combining",
            padded(
                vec![wide_lead(), wide_continuation(), combining, plain_cell('x')],
                blank(),
                200,
            ),
            true,
        ),
        ("wide pairs", padded(pairs, blank(), 200), true),
        ("hyperlinked", padded(vec![linked; 40], blank(), 200), true),
        ("coloured erased tail", padded(text_cells("prompt$ "), coloured_fill(), 200), true),
        ("all blank", vec![blank(); 200], true),
        (
            "all content",
            (0..200).map(|index| plain_cell(char::from(b'a' + (index % 26) as u8))).collect(),
            false,
        ),
    ]
}

/// A trimmed row is valid: at least two fill columns, a fill that is not half of a wide pair, and a
/// stored prefix whose last cell differs from the fill.
fn assert_valid_storage(line: &Line, context: &str) {
    if let LineStorage::Trimmed { cells, len } = line.storage() {
        let stored = cells.len() - 1;
        let fill = &cells[stored];
        assert!(len - stored >= 2, "{context}: {} fill columns", len - stored);
        assert!(
            !fill.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT),
            "{context}: wide fill"
        );
        if stored > 0 {
            assert_ne!(&cells[stored - 1], fill, "{context}: the prefix ends in a fill cell");
        }
    }
}

/// Every read of a trimmed row matches its flat original: `get` at every column and past the end,
/// `iter` both ways, every range, `Hash` and `PartialEq`. A row that cannot be trimmed stays `Flat`.
#[test]
fn trimmed_rows_read_back_identical_to_their_flat_original() {
    for (name, cells, trims) in trim_rows() {
        let original = Line::from_flat(cells.clone());
        let mut line = original.clone();
        let released = line.try_trim();
        assert_eq!(line.is_trimmed(), trims, "{name}");
        assert_eq!(released.is_some(), trims, "{name}");
        assert!(trims || line.storage().is_flat(), "{name}");
        assert_valid_storage(&line, name);
        assert_eq!(line.len(), cells.len(), "{name}");
        for (column, cell) in cells.iter().enumerate() {
            assert_eq!(line.get(column), Some(cell), "{name} column {column}");
            assert_eq!(&line[column], cell, "{name} column {column}");
        }
        assert_eq!(line.get(cells.len()), None, "{name}");
        assert!(line.iter().eq(cells.iter()), "{name}");
        assert!(line.iter().rev().eq(cells.iter().rev()), "{name}");
        assert_eq!(line.iter().len(), cells.len(), "{name}");
        for start in [0_usize, 1, 3, 39, 40, 41, 150, 198, 199, 200] {
            for end in [0_usize, 2, 40, 41, 42, 100, 199, 200, 230] {
                let expected: Vec<&Cell> =
                    cells.iter().skip(start).take(end.saturating_sub(start)).collect();
                assert!(
                    line.get_range(start, end).eq(expected.iter().copied()),
                    "{name} {start}..{end}"
                );
                assert!(
                    line.get_range(start, end).rev().eq(expected.iter().rev().copied()),
                    "{name} {start}..{end} reversed"
                );
                let storage: Vec<Cell> =
                    line.storage().get_range(start as u16, end as u16).collect();
                assert_eq!(
                    storage,
                    expected.into_iter().cloned().collect::<Vec<_>>(),
                    "{name} storage {start}..{end}"
                );
            }
        }
        let storage_cells: Vec<Cell> = line.storage().iter().collect();
        assert_eq!(storage_cells, cells, "{name}");
        assert_eq!(line.storage().get(cells.len() - 1).as_ref(), cells.last(), "{name}");
        assert_eq!(line, original, "{name}");
        assert_eq!(hash_of(&line), hash_of(&original), "{name}");
        assert_eq!(format!("{line:?}"), format!("{original:?}"), "{name}");
    }
}

/// `try_trim` refuses a fill that is half of a wide pair, a one-column fill tail, a saving under a
/// quarter of the row, and a saving under 256 bytes; the 25% boundary itself trims.
#[test]
fn trim_refusals_leave_the_row_flat() {
    let refused = [
        ("wide lead fill", padded(text_cells("text"), wide_lead(), 200)),
        ("wide continuation fill", padded(text_cells("text"), wide_continuation(), 200)),
        ("one fill column", padded(text_cells(&"x".repeat(199)), blank(), 200)),
        ("saving under a quarter", padded(text_cells(&"x".repeat(150)), blank(), 200)),
        ("saving under 256 bytes", padded(text_cells("ab"), blank(), 12)),
    ];
    for (name, cells) in refused {
        let mut line = Line::from_flat(cells);
        assert!(line.try_trim().is_none(), "{name}");
        assert!(line.storage().is_flat(), "{name}");
    }
    // 149 content cells leave 51 fill columns: a 1,200-byte saving, exactly a quarter of 4,800.
    let mut boundary = Line::from_flat(padded(text_cells(&"x".repeat(149)), blank(), 200));
    assert!(boundary.try_trim().is_some());
    assert!(boundary.is_trimmed());
}

/// `truncate` and `resize` on each trim fixture read back equal to the same operation on its flat
/// original, for every length and both fills, and always leave a valid `Trimmed` or `Flat` row. The
/// fixtures' wide pairs sit at even columns below 40 only; every other cut through a wide pair is
/// covered by `every_cut_through_a_wide_pair_matches_the_flat_original`.
#[test]
fn trimmed_truncate_and_resize_match_their_flat_original() {
    let other_fill = Cell::plain(' ', Color::Default, Color::Indexed(1), CellFlags::empty());
    for (name, cells, trims) in trim_rows() {
        if !trims {
            continue;
        }
        let flat = Line::from_flat(cells.clone());
        let mut trimmed = flat.clone();
        trimmed.try_trim();
        let LineStorage::Trimmed { cells: stored_cells, len } = trimmed.storage() else {
            panic!("{name} trims");
        };
        let stored = stored_cells.len() - 1;
        let fill = stored_cells[stored].clone();
        for new_len in 0..=len + 3 {
            let context = format!("{name} truncate({new_len})");
            let (mut cut, mut expected) = (trimmed.clone(), flat.clone());
            cut.truncate(new_len);
            expected.truncate(new_len);
            assert_eq!(cut, expected, "{context}");
            assert_valid_storage(&cut, &context);
            if new_len == 0 {
                assert!(
                    matches!(cut.storage(), LineStorage::Flat(cells) if cells.is_empty()),
                    "{context}"
                );
            }
            if new_len == stored + 1 {
                assert!(cut.storage().is_flat(), "{context}");
            }
            for padding in [fill.clone(), other_fill.clone()] {
                let context = format!("{name} resize({new_len}, {:?})", padding.bg);
                let (mut sized, mut expected) = (trimmed.clone(), flat.clone());
                sized.resize(new_len, padding.clone());
                expected.resize(new_len, padding.clone());
                assert_eq!(sized, expected, "{context}");
                assert!(sized.iter().eq(expected.iter()), "{context}");
                assert_valid_storage(&sized, &context);
                if new_len == 0 {
                    assert!(
                        matches!(sized.storage(), LineStorage::Flat(cells) if cells.is_empty()),
                        "{context}"
                    );
                }
                if new_len == stored + 1 {
                    assert!(sized.storage().is_flat(), "{context}");
                }
            }
        }
    }
}

/// 200 columns: letters in `0..content` with a wide pair at `lead` and `lead + 1`, then blanks.
fn row_with_wide_pair(content: usize, lead: usize) -> Vec<Cell> {
    let mut cells: Vec<Cell> =
        (0..content).map(|column| plain_cell(char::from(b'a' + (column % 26) as u8))).collect();
    cells[lead] = wide_lead();
    cells[lead + 1] = wide_continuation();
    padded(cells, blank(), 200)
}

/// For every cut a trimmed row can take through a wide pair, at both column parities, both inside
/// the stored prefix (letters to column 147) and at the prefix boundary (the pair is the prefix's
/// last two cells): truncating or resizing the trimmed row there reads back equal to its flat
/// original with either fill, the resize repairs the clipped lead to the fill, and the result is a
/// valid row. Lengths around the cut and the prefix end are checked too.
#[test]
fn every_cut_through_a_wide_pair_matches_the_flat_original() {
    let other_fill = Cell::plain(' ', Color::Default, Color::Indexed(1), CellFlags::empty());
    let mut rows = Vec::new();
    for cut in 1..=147 {
        rows.push(("inside the prefix", cut, row_with_wide_pair(148, cut - 1)));
    }
    for cut in 1..=148 {
        rows.push(("at the prefix boundary", cut, row_with_wide_pair(cut + 1, cut - 1)));
    }
    let mut cuts_by_parity = [0usize; 2];
    let mut boundary_cuts = Vec::new();
    for (place, cut, cells) in rows {
        let flat = Line::from_flat(cells);
        let mut trimmed = flat.clone();
        assert!(trimmed.try_trim().is_some(), "{place} cut {cut}: the row trims");
        let LineStorage::Trimmed { cells: stored_cells, .. } = trimmed.storage() else {
            panic!("{place} cut {cut}: trimmed");
        };
        let stored = stored_cells.len() - 1;
        let mut lengths = vec![cut - 1, cut, cut + 1, stored - 1, stored, stored + 1, stored + 2];
        lengths.sort_unstable();
        lengths.dedup();
        for new_len in lengths {
            let context = format!("{place} cut {cut} truncate({new_len})");
            let (mut cut_row, mut expected) = (trimmed.clone(), flat.clone());
            cut_row.truncate(new_len);
            expected.truncate(new_len);
            assert_eq!(cut_row, expected, "{context}");
            assert_valid_storage(&cut_row, &context);
            for padding in [blank(), other_fill.clone()] {
                let context = format!("{place} cut {cut} resize({new_len}, {:?})", padding.bg);
                let (mut sized, mut expected) = (trimmed.clone(), flat.clone());
                sized.resize(new_len, padding.clone());
                expected.resize(new_len, padding.clone());
                assert_eq!(sized, expected, "{context}");
                assert_valid_storage(&sized, &context);
                if new_len == cut {
                    // The kept lead lost its continuation, so the flat original repairs it.
                    assert_eq!(
                        expected[cut - 1],
                        padding,
                        "{context}: the clipped lead is repaired"
                    );
                }
            }
        }
        cuts_by_parity[cut % 2] += 1;
        if place == "at the prefix boundary" {
            assert_eq!(stored, cut + 1, "{place} cut {cut}: the pair ends the prefix");
            boundary_cuts.push(cut);
        }
    }
    assert!(cuts_by_parity.iter().all(|count| *count > 100), "{cuts_by_parity:?}");
    assert_eq!(boundary_cuts, (1..=148).collect::<Vec<_>>());
}

/// A write into a trimmed row expands it to `Flat` in its own buffer and lands, while the soft-wrap
/// flag and the erased tail's background survive; every mutating path expands.
#[test]
fn edits_expand_a_trimmed_row() {
    let cells = padded(text_cells("prompt$ "), coloured_fill(), 200);
    let trimmed = || {
        let mut line = Line::from_flat(cells.clone());
        line.set_soft_wrapped_from_previous(true);
        assert!(line.try_trim().is_some());
        line
    };
    let mut line = trimmed();
    assert!(line.set(150, plain_cell('z')));
    let LineStorage::Flat(expanded) = line.storage() else { panic!("set expands") };
    assert_eq!(expanded.capacity(), 200, "one exact reserve, no regrowth");
    assert_eq!(line[150].ch, 'z');
    assert_eq!(line[149].bg, Color::Indexed(4));
    assert_eq!(line[199].bg, Color::Indexed(4));
    assert!(line.soft_wrapped_from_previous());
    assert_eq!(line.len(), 200);
    let mut via_vec = trimmed();
    via_vec.as_vec_mut()[3] = plain_cell('q');
    assert!(via_vec.storage().is_flat());
    let mut via_iter = trimmed();
    via_iter.iter_mut().for_each(|cell| cell.ch = 'w');
    assert!(via_iter.iter().all(|cell| cell.ch == 'w'));
    let mut via_fill = trimmed();
    via_fill.fill_range(0, 5, plain_cell('f'));
    assert_eq!(via_fill[4].ch, 'f');
    let mut via_ensure = trimmed();
    via_ensure.ensure_flat();
    assert!(via_ensure.storage().is_flat());
    let mut wrapped_original = Line::from_flat(cells.clone());
    wrapped_original.set_soft_wrapped_from_previous(true);
    assert_eq!(via_ensure, wrapped_original);
    let mut via_index = trimmed();
    via_index[180] = plain_cell('i');
    assert_eq!(via_index[180].ch, 'i');
}

/// A trimmed row's capacity bytes are its stored cells, its rare-attribute fill is counted once,
/// and `Line` stays 40 bytes, its size before the trimmed variant was added.
#[test]
fn trimmed_accounting_counts_stored_cells_and_one_fill() {
    let mut linked_fill = blank();
    linked_fill.set_hyperlink(Some(HyperlinkId(3)));
    let mut line = Line::from_flat(padded(text_cells(&"t".repeat(40)), linked_fill, 200));
    let fat = std::mem::size_of::<FatAttributes>();
    assert_eq!(line.fat_attribute_bytes(), 160 * fat);
    assert!(line.try_trim().is_some());
    let LineStorage::Trimmed { cells, .. } = line.storage() else { panic!("trims") };
    assert_eq!(line.approx_capacity_byte_size(), cells.capacity() * std::mem::size_of::<Cell>());
    assert_eq!(cells.capacity(), 41);
    assert_eq!(line.fat_attribute_bytes(), fat);
    assert_eq!(std::mem::size_of::<Line>(), 40);
}

/// A uniform row compresses to one `Cluster`, which is smaller than any trimmed form, and trimming
/// leaves it alone.
#[test]
fn a_uniform_row_stays_one_cluster() {
    let mut line = Line::from_flat(vec![coloured_fill(); 200]);
    assert!(line.try_compress());
    assert!(line.try_trim().is_none());
    assert_eq!(
        line.storage(),
        &LineStorage::Cluster(vec![Cluster { cell: coloured_fill(), count: 200 }])
    );
}

/// The half-built compaction helper is gone and the module doc no longer calls the storage unwired.
#[test]
fn line_source_has_no_compaction_helper_or_stale_wiring_note() {
    let source = LINE_CHECKOUT.replace("\r\n", "\n");
    assert!(!source.contains("compact_if_beneficial"));
    assert!(!source.contains("not yet wired"));
}

/// The fast fill comparison agrees with `Cell::eq` for plain cells differing in each field and for
/// cells with and without a rare-attribute box, so the eject scans trim exactly as before.
#[test]
fn same_as_fill_agrees_with_cell_equality() {
    let plain_cell = Cell::plain(' ', Color::Default, Color::Default, CellFlags::empty());
    let mut linked = plain_cell.clone();
    linked.set_hyperlink(Some(HyperlinkId(3)));
    let mut other_link = plain_cell.clone();
    other_link.set_hyperlink(Some(HyperlinkId(4)));
    let candidates = [
        plain_cell.clone(),
        Cell::plain('x', Color::Default, Color::Default, CellFlags::empty()),
        Cell::plain(' ', Color::Indexed(2), Color::Default, CellFlags::empty()),
        Cell::plain(' ', Color::Default, Color::Rgb(1, 2, 3), CellFlags::empty()),
        Cell::plain(' ', Color::Default, Color::Default, CellFlags::BOLD),
        linked,
        other_link,
    ];
    for cell in &candidates {
        for fill in &candidates {
            assert_eq!(same_as_fill(cell, fill), cell == fill, "{cell:?} vs {fill:?}");
        }
    }
}
