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

use super::*;
use sonicterm_types::cell::{CellFlags, Color};
use sonicterm_types::HyperlinkId;

// --- helpers --------------------------------------------------------------

fn blank() -> Cell {
    Cell::default()
}

fn plain_cell(character: char) -> Cell {
    Cell::plain(character, Color::Default, Color::Default, CellFlags::empty())
}

fn ch_bold(character: char) -> Cell {
    Cell::plain(character, Color::Default, Color::Default, CellFlags::BOLD)
}

/// A cell carrying every "fat"/flag channel we want to prove survives
/// storage transitions: wide flag, truecolor fg, indexed bg, a hyperlink id,
/// and trailing zero-width extras.
fn rich(character: char) -> Cell {
    let mut cell =
        Cell::plain(character, Color::Rgb(10, 20, 30), Color::Indexed(4), CellFlags::WIDE);
    cell.set_hyperlink(Some(HyperlinkId(7)));
    cell.set_extras(Some("\u{0301}".to_string().into_boxed_str())); // combining acute
    cell
}

fn hash_of(line: &Line) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

/// Build a Flat and a content-equal Cluster line from the same cell run list.
fn flat_and_cluster(runs: &[(Cell, usize)]) -> (Line, Line) {
    let mut flat_cells = Vec::new();
    let mut clusters = Vec::new();
    for (cell, count) in runs {
        for _ in 0..*count {
            flat_cells.push(cell.clone());
        }
        clusters.push(Cluster { cell: cell.clone(), count: *count });
    }
    (Line::from_flat(flat_cells), Line::from_clusters(clusters))
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
fn iteration_and_random_access_agree_across_forms() {
    let (flat, clustered) = flat_and_cluster(&[
        (blank(), 10),
        (plain_cell('x'), 1),
        (blank(), 5),
        (plain_cell('x'), 2),
        (blank(), 50),
    ]);
    assert!(!flat.is_clustered());
    assert!(clustered.is_clustered());
    assert_eq!(flat.len(), clustered.len());

    let flat_iter: Vec<_> = flat.iter().cloned().collect();
    let clust_iter: Vec<_> = clustered.iter_storage().cloned().collect();
    assert_eq!(flat_iter, clust_iter);

    for column in 0..flat.len() {
        assert_eq!(flat.get(column), clustered.get(column), "mismatch at {column}");
    }
}

#[test]
fn get_range_matches_across_forms_and_clamps() {
    let (flat, clustered) =
        flat_and_cluster(&[(plain_cell('a'), 3), (plain_cell('b'), 4), (plain_cell('c'), 3)]);

    // Interior window straddling cluster boundaries.
    let flat_cells: Vec<_> = flat.get_range(2, 8).cloned().collect();
    let cluster_cells: Vec<_> = clustered.get_range(2, 8).cloned().collect();
    assert_eq!(flat_cells, cluster_cells);
    assert_eq!(
        flat_cells,
        vec![
            plain_cell('a'),
            plain_cell('b'),
            plain_cell('b'),
            plain_cell('b'),
            plain_cell('b'),
            plain_cell('c')
        ]
    );

    // end clamps to len(); start==end and reversed ranges are empty.
    assert_eq!(clustered.get_range(8, 999).count(), 2);
    assert_eq!(clustered.get_range(4, 4).count(), 0);
    assert_eq!(clustered.get_range(6, 2).count(), 0);
    assert_eq!(flat.get_range(6, 2).count(), 0);
}

#[test]
fn get_range_window_inside_single_cluster() {
    let clustered = Line::from_clusters(vec![Cluster { cell: plain_cell('z'), count: 20 }]);
    let got: Vec<_> = clustered.get_range(5, 9).cloned().collect();
    assert_eq!(got, vec![plain_cell('z'); 4]);
}

#[test]
fn storage_get_range_owned_matches_reference_range() {
    // `LineStorage::get_range` yields owned cells over a u16 range; prove it
    // agrees with the reference-yielding `Line::get_range`.
    let (_flat, clustered) = flat_and_cluster(&[(plain_cell('a'), 3), (plain_cell('b'), 5)]);
    let owned: Vec<Cell> = clustered.storage().get_range(2, 6).collect();
    let borrowed: Vec<Cell> = clustered.get_range(2, 6).cloned().collect();
    assert_eq!(owned, borrowed);
    assert_eq!(owned, vec![plain_cell('a'), plain_cell('b'), plain_cell('b'), plain_cell('b')]);
}

// --- forward / backward iterator + hash parity ----------------------------

#[test]
fn reverse_iteration_agrees_across_forms() {
    let (flat, clustered) =
        flat_and_cluster(&[(plain_cell('a'), 3), (plain_cell('b'), 1), (plain_cell('c'), 4)]);
    let fwd: Vec<_> = flat.iter().cloned().collect();

    let flat_rev: Vec<_> = flat.iter().rev().cloned().collect();
    let clust_rev: Vec<_> = clustered.iter().rev().cloned().collect();
    assert_eq!(flat_rev, clust_rev);

    let mut expect_rev = fwd.clone();
    expect_rev.reverse();
    assert_eq!(clust_rev, expect_rev);
}

#[test]
fn double_ended_meet_in_middle_on_cluster() {
    // Alternate popping from front and back; the two ends must meet without
    // yielding a cell twice or skipping one.
    let clustered = Line::from_clusters(vec![
        Cluster { cell: plain_cell('a'), count: 2 },
        Cluster { cell: plain_cell('b'), count: 3 },
        Cluster { cell: plain_cell('c'), count: 2 },
    ]);
    let mut it = clustered.iter();
    let mut front = Vec::new();
    let mut back = Vec::new();
    while let Some(front_cell) = it.next() {
        front.push(front_cell.clone());
        if let Some(back_cell) = it.next_back() {
            back.push(back_cell.clone());
        }
    }
    back.reverse();
    front.extend(back);
    assert_eq!(front, clustered.to_vec());
    assert_eq!(front.len(), 7);
}

#[test]
fn exact_size_hint_tracks_remaining() {
    let clustered = Line::from_clusters(vec![
        Cluster { cell: plain_cell('a'), count: 4 },
        Cluster { cell: plain_cell('b'), count: 2 },
    ]);
    let mut it = clustered.iter();
    assert_eq!(it.len(), 6);
    assert_eq!(it.size_hint(), (6, Some(6)));
    it.next();
    it.next_back();
    assert_eq!(it.len(), 4);
    assert_eq!(it.size_hint(), (4, Some(4)));
}

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

fn linked_cell(id: u64) -> Cell {
    let mut cell = Cell::plain('x', Color::Default, Color::Default, CellFlags::empty());
    cell.set_hyperlink(Some(sonicterm_types::HyperlinkId(id)));
    cell
}

/// Cluster form must charge one box per *stored* cell, not per logical column.
///
/// A run of N identical linked cells collapses to one `Cluster` holding one
/// `Cell`, hence one `Box<FatAttributes>`. Charging the run length would
/// inflate a long link span — a whole line inside one OSC 8 span — by up to
/// its column count.
///
/// Tested against `LineStorage` directly because cluster form is rare when
/// driving a `Grid` through `put_char`: measured at 3 of 203 rows, which is
/// too few for a grid-level assertion to discriminate. This is the level the
/// distinction exists at.
#[test]
fn cluster_storage_charges_one_box_per_stored_cell() {
    let fat = std::mem::size_of::<sonicterm_types::FatAttributes>();

    // One run of 80 identical linked cells.
    let flat: Vec<Cell> = (0..80).map(|_| linked_cell(1)).collect();
    let clustered = LineStorage::cluster_from_flat(&flat);

    assert!(clustered.is_cluster(), "precondition: identical cells must collapse to one run");
    assert_eq!(clustered.len(), 80, "precondition: the logical length is unchanged");

    assert_eq!(
        clustered.fat_attribute_bytes(),
        fat,
        "one collapsed run holds one boxed attribute set, so it must be charged once — \
         charging its 80-column length would over-report by 80x"
    );
}

/// Flat form charges every linked cell, because every one has its own box.
#[test]
fn flat_storage_charges_every_linked_cell() {
    let fat = std::mem::size_of::<sonicterm_types::FatAttributes>();

    // Distinct link ids defeat run collapsing, so each cell keeps its own box.
    let flat: Vec<Cell> = (0..80).map(|index| linked_cell(index + 1)).collect();
    let storage = LineStorage::Flat(flat);

    assert_eq!(
        storage.fat_attribute_bytes(),
        80 * fat,
        "each distinct linked cell holds its own box and must be charged"
    );
}

/// The two forms must agree on identical content.
///
/// Converting between them frees no memory and allocates none, so a figure
/// that moved across the conversion would make compaction look like a leak or
/// a saving that never happened.
#[test]
fn converting_between_storage_forms_does_not_move_the_figure() {
    // Distinct ids: nothing collapses, so both forms hold the same boxes.
    let flat: Vec<Cell> = (0..40).map(|index| linked_cell(index + 1)).collect();

    let as_flat = LineStorage::Flat(flat.clone());
    let mut as_cluster = LineStorage::cluster_from_flat(&flat);

    assert_eq!(
        as_flat.fat_attribute_bytes(),
        as_cluster.fat_attribute_bytes(),
        "identical content must report identically whichever form holds it"
    );

    as_cluster.to_flat();
    assert_eq!(
        as_cluster.fat_attribute_bytes(),
        as_flat.fat_attribute_bytes(),
        "converting back must not move the figure either"
    );
}

/// Plain cells cost nothing.
///
/// The whole point of the box is that the overwhelming majority of cells leave
/// it `None`. If plain content charged for it, every grid would over-report.
#[test]
fn plain_cells_charge_nothing() {
    let flat: Vec<Cell> = (0..80)
        .map(|_| Cell::plain(' ', Color::Default, Color::Default, CellFlags::empty()))
        .collect();
    assert_eq!(LineStorage::Flat(flat).fat_attribute_bytes(), 0);
}

/// Grapheme extras are a second allocation and must be charged as one.
///
/// `FatAttributes::extras` is an `Option<Box<str>>`. `size_of::<FatAttributes>()`
/// describes the fat pointer, never the string behind it, so a figure built
/// only from the box size reports a cell of combining marks identically to a
/// cell with none. Ordinary output reaches this: accented text and ZWJ emoji
/// both land here.
#[test]
fn extras_payload_is_charged_beyond_the_box() {
    let fat = std::mem::size_of::<sonicterm_types::FatAttributes>();

    let mut with_extras = plain_cell('a');
    // Four combining acute accents: 2 bytes UTF-8 each.
    with_extras.set_extras(Some(String::from("\u{0301}\u{0301}\u{0301}\u{0301}").into_boxed_str()));
    let extras_len = with_extras.extras().map_or(0, str::len);
    assert_eq!(extras_len, 8, "precondition: the payload is the bytes behind the pointer");

    let storage = LineStorage::Flat(vec![with_extras]);
    assert_eq!(
        storage.fat_attribute_bytes(),
        fat + extras_len,
        "the box and the string it points at are two allocations and must both be charged"
    );
}

/// A longer payload must cost more than a shorter one.
///
/// The assertion the box-only figure cannot make: it reports both at exactly
/// `size_of::<FatAttributes>()`, so a grid of emoji and a grid of plain linked
/// cells become indistinguishable.
#[test]
fn a_longer_extras_payload_costs_more() {
    let mut short = plain_cell('a');
    short.set_extras(Some(String::from("\u{0301}").into_boxed_str()));
    let mut long = plain_cell('a');
    long.set_extras(Some("\u{0301}".repeat(16).into_boxed_str()));

    let short_bytes = LineStorage::Flat(vec![short]).fat_attribute_bytes();
    let long_bytes = LineStorage::Flat(vec![long]).fat_attribute_bytes();

    assert!(
        long_bytes > short_bytes,
        "a 32-byte payload must report above a 2-byte one (short {short_bytes}, long {long_bytes})"
    );
    assert_eq!(
        long_bytes - short_bytes,
        30,
        "the difference must be the payload difference, not a constant"
    );
}

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

/// `truncate` and `resize` on a trimmed row read back equal to the same operation on its flat
/// original, for every length, both fills and a wide pair straddling every cut, and always leave a
/// valid `Trimmed` or `Flat` row.
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
    let source = include_str!("line.rs").replace("\r\n", "\n");
    assert!(!source.contains("compact_if_beneficial"));
    assert!(!source.contains("not yet wired"));
}
