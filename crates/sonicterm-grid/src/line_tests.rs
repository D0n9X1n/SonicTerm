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

fn ch(c: char) -> Cell {
    Cell::plain(c, Color::Default, Color::Default, CellFlags::empty())
}

fn ch_bold(c: char) -> Cell {
    Cell::plain(c, Color::Default, Color::Default, CellFlags::BOLD)
}

/// A cell carrying every "fat"/flag channel we want to prove survives
/// storage transitions: wide flag, truecolor fg, indexed bg, a hyperlink id,
/// and trailing zero-width extras.
fn rich(c: char) -> Cell {
    let mut cell = Cell::plain(c, Color::Rgb(10, 20, 30), Color::Indexed(4), CellFlags::WIDE);
    cell.set_hyperlink(Some(HyperlinkId(7)));
    cell.set_extras(Some("\u{0301}".to_string().into_boxed_str())); // combining acute
    cell
}

fn hash_of(line: &Line) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut h);
    h.finish()
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

// --- cluster/flat range iteration + random access parity ------------------

#[test]
fn iteration_and_random_access_agree_across_forms() {
    let (flat, clustered) =
        flat_and_cluster(&[(blank(), 10), (ch('x'), 1), (blank(), 5), (ch('x'), 2), (blank(), 50)]);
    assert!(!flat.is_clustered());
    assert!(clustered.is_clustered());
    assert_eq!(flat.len(), clustered.len());

    let flat_iter: Vec<_> = flat.iter().cloned().collect();
    let clust_iter: Vec<_> = clustered.iter_storage().cloned().collect();
    assert_eq!(flat_iter, clust_iter);

    for i in 0..flat.len() {
        assert_eq!(flat.get(i), clustered.get(i), "mismatch at {i}");
    }
}

#[test]
fn get_range_matches_across_forms_and_clamps() {
    let (flat, clustered) = flat_and_cluster(&[(ch('a'), 3), (ch('b'), 4), (ch('c'), 3)]);

    // Interior window straddling cluster boundaries.
    let f: Vec<_> = flat.get_range(2, 8).cloned().collect();
    let c: Vec<_> = clustered.get_range(2, 8).cloned().collect();
    assert_eq!(f, c);
    assert_eq!(f, vec![ch('a'), ch('b'), ch('b'), ch('b'), ch('b'), ch('c')]);

    // end clamps to len(); start==end and reversed ranges are empty.
    assert_eq!(clustered.get_range(8, 999).count(), 2);
    assert_eq!(clustered.get_range(4, 4).count(), 0);
    assert_eq!(clustered.get_range(6, 2).count(), 0);
    assert_eq!(flat.get_range(6, 2).count(), 0);
}

#[test]
fn get_range_window_inside_single_cluster() {
    let clustered = Line::from_clusters(vec![Cluster { cell: ch('z'), count: 20 }]);
    let got: Vec<_> = clustered.get_range(5, 9).cloned().collect();
    assert_eq!(got, vec![ch('z'); 4]);
}

#[test]
fn storage_get_range_owned_matches_reference_range() {
    // `LineStorage::get_range` yields owned cells over a u16 range; prove it
    // agrees with the reference-yielding `Line::get_range`.
    let (_flat, clustered) = flat_and_cluster(&[(ch('a'), 3), (ch('b'), 5)]);
    let owned: Vec<Cell> = clustered.storage().get_range(2, 6).collect();
    let borrowed: Vec<Cell> = clustered.get_range(2, 6).cloned().collect();
    assert_eq!(owned, borrowed);
    assert_eq!(owned, vec![ch('a'), ch('b'), ch('b'), ch('b')]);
}

// --- forward / backward iterator + hash parity ----------------------------

#[test]
fn reverse_iteration_agrees_across_forms() {
    let (flat, clustered) = flat_and_cluster(&[(ch('a'), 3), (ch('b'), 1), (ch('c'), 4)]);
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
        Cluster { cell: ch('a'), count: 2 },
        Cluster { cell: ch('b'), count: 3 },
        Cluster { cell: ch('c'), count: 2 },
    ]);
    let mut it = clustered.iter();
    let mut front = Vec::new();
    let mut back = Vec::new();
    while let Some(c) = it.next() {
        front.push(c.clone());
        if let Some(c) = it.next_back() {
            back.push(c.clone());
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
        Cluster { cell: ch('a'), count: 4 },
        Cluster { cell: ch('b'), count: 2 },
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
    let (flat, clustered) = flat_and_cluster(&[(blank(), 10), (ch('x'), 2), (blank(), 8)]);
    assert_eq!(
        hash_of(&flat),
        hash_of(&clustered),
        "cluster and content-equal flat line must hash identically"
    );
}

#[test]
fn hash_differs_on_different_content() {
    let a = Line::from_flat(vec![ch('a'), ch('b'), ch('c')]);
    let b = Line::from_flat(vec![ch('a'), ch('z'), ch('c')]);
    assert_ne!(hash_of(&a), hash_of(&b));
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
    assert!(line.set(5, ch('X')));
    assert!(!line.is_clustered(), "char mismatch must degrade");
    assert_eq!(line.get(5), Some(&ch('X')));
    assert_eq!(line.get(4), Some(&blank()));
    assert_eq!(line.get(6), Some(&blank()));
    assert_eq!(line.len(), 80);
}

#[test]
fn set_different_attrs_degrades_to_flat() {
    let mut line = clustered_uniform(ch(' '), 40);
    assert!(line.set(10, ch_bold(' ')));
    assert!(!line.is_clustered(), "attr mismatch must degrade");
    assert_eq!(line.get(10).map(|c| c.flags), Some(CellFlags::BOLD));
    assert_eq!(line.len(), 40);
}

#[test]
fn set_out_of_range_returns_false_and_preserves_storage() {
    let mut line = clustered_uniform(blank(), 8);
    assert!(!line.set(100, ch('Z')));
    assert!(line.is_clustered(), "rejected write must not degrade");
}

#[test]
fn multi_cluster_set_has_no_representative_and_degrades() {
    // A line with >1 cluster has no single representative, so even a write
    // equal to the target cell degrades (the smart path only covers uniform).
    let mut line = Line::from_clusters(vec![
        Cluster { cell: ch('a'), count: 4 },
        Cluster { cell: ch('b'), count: 4 },
    ]);
    assert_eq!(line.cluster_representative(), None);
    assert!(line.set(0, ch('a')));
    assert!(!line.is_clustered());
    assert_eq!(line.get(0), Some(&ch('a')));
    assert_eq!(line.get(4), Some(&ch('b')));
}

#[test]
fn fill_range_matching_stays_cluster_mismatch_degrades() {
    let mut keep = clustered_uniform(blank(), 80);
    keep.fill_range(10, 50, blank());
    assert!(keep.is_clustered(), "matching fill must NOT degrade");

    let mut degrade = clustered_uniform(blank(), 80);
    degrade.fill_range(10, 50, ch_bold(' '));
    assert!(!degrade.is_clustered());
    for i in 0..10 {
        assert_eq!(degrade.get(i), Some(&blank()), "prefix {i}");
    }
    for i in 10..50 {
        assert_eq!(degrade.get(i).map(|c| c.flags), Some(CellFlags::BOLD), "filled {i}");
    }
    for i in 50..80 {
        assert_eq!(degrade.get(i), Some(&blank()), "suffix {i}");
    }
}

#[test]
fn fill_range_empty_or_reversed_is_noop() {
    let mut line = clustered_uniform(blank(), 40);
    line.fill_range(5, 5, ch('X'));
    line.fill_range(10, 3, ch('X'));
    assert!(line.is_clustered(), "no-op fills must not degrade");
    assert_eq!(line.len(), 40);
}

// --- resize / truncate / compression transitions --------------------------

#[test]
fn resize_grow_matching_fill_stays_single_cluster() {
    let mut l = Line::from_clusters(vec![Cluster { cell: blank(), count: 80 }]);
    l.resize(100, blank());
    assert!(l.is_clustered());
    assert_eq!(l.len(), 100);
    match l.storage() {
        LineStorage::Cluster(cs) => {
            assert_eq!(cs.len(), 1, "matching fill merges into one cluster");
            assert_eq!(cs[0].count, 100);
        }
        _ => panic!("expected cluster"),
    }
}

#[test]
fn resize_grow_mismatched_fill_appends_second_cluster() {
    let mut red = blank();
    red.bg = Color::Indexed(1);
    let mut l = Line::from_clusters(vec![Cluster { cell: red.clone(), count: 80 }]);
    l.resize(100, blank());
    assert!(l.is_clustered(), "stays clustered as multi-cluster");
    match l.storage() {
        LineStorage::Cluster(cs) => {
            assert_eq!(cs.len(), 2);
            assert_eq!(cs[0], Cluster { cell: red, count: 80 });
            assert_eq!(cs[1], Cluster { cell: blank(), count: 20 });
        }
        _ => panic!("expected cluster"),
    }
}

#[test]
fn resize_shrink_delegates_to_truncate() {
    let mut l = Line::from_flat(vec![ch('a'), ch('b'), ch('c'), ch('d'), ch('e')]);
    l.resize(2, blank());
    assert_eq!(l.len(), 2);
    assert_eq!(l.to_vec(), vec![ch('a'), ch('b')]);
}

#[test]
fn truncate_within_single_cluster_preserves_form() {
    let mut l = Line::from_clusters(vec![Cluster { cell: blank(), count: 80 }]);
    l.truncate(50);
    assert!(l.is_clustered());
    match l.storage() {
        LineStorage::Cluster(cs) => {
            assert_eq!(cs.len(), 1);
            assert_eq!(cs[0].count, 50);
        }
        _ => panic!("expected cluster"),
    }
}

#[test]
fn truncate_across_clusters_and_to_zero() {
    let mut red = blank();
    red.bg = Color::Indexed(1);
    let mut l = Line::from_clusters(vec![
        Cluster { cell: red.clone(), count: 30 },
        Cluster { cell: blank(), count: 50 },
    ]);
    l.truncate(40); // lands inside the second cluster
    match l.storage() {
        LineStorage::Cluster(cs) => {
            assert_eq!(cs.len(), 2);
            assert_eq!(cs[0].count, 30);
            assert_eq!(cs[1].count, 10);
        }
        _ => panic!("expected cluster"),
    }
    l.truncate(20); // drops the second cluster entirely
    match l.storage() {
        LineStorage::Cluster(cs) => {
            assert_eq!(cs.len(), 1);
            assert_eq!(cs[0], Cluster { cell: red, count: 20 });
        }
        _ => panic!("expected cluster"),
    }
    l.truncate(0); // empties to Flat
    assert!(l.is_empty());
    assert!(!l.is_clustered());
}

#[test]
fn truncate_noop_when_new_len_ge_current() {
    let mut l = Line::from_flat(vec![ch('x'), ch('y')]);
    l.truncate(10);
    assert_eq!(l.len(), 2);
}

#[test]
fn compact_if_beneficial_only_when_saving_is_large() {
    // 200 identical cells: cluster is dramatically smaller -> compacts.
    let mut uniform = Line::from_flat(vec![blank(); 200]);
    assert!(uniform.compact_if_beneficial());
    assert!(uniform.is_clustered());
    assert_eq!(uniform.len(), 200);

    // Already clustered -> no-op.
    assert!(!uniform.compact_if_beneficial());

    // Two alternating cells: cluster count ~= flat count, saving < 2x -> stays flat.
    let mut alternating = Line::from_flat((0..40).map(|i| ch((b'a' + (i % 2)) as char)).collect());
    assert!(!alternating.compact_if_beneficial());
    assert!(!alternating.is_clustered());

    // Empty flat -> no-op.
    let mut empty = Line::from_flat(Vec::new());
    assert!(!empty.compact_if_beneficial());
}

#[test]
fn try_compress_requires_full_uniformity() {
    let mut uniform = Line::from_flat(vec![blank(); 10]);
    assert!(uniform.try_compress());
    assert!(uniform.is_clustered());

    // Non-uniform stays flat.
    let mut mixed = Line::from_flat(vec![blank(), ch('x'), blank()]);
    assert!(!mixed.try_compress());
    assert!(!mixed.is_clustered());

    // Already cluster and empty flat are both no-ops.
    assert!(!uniform.try_compress());
    let mut empty = Line::from_flat(Vec::new());
    assert!(!empty.try_compress());
}

#[test]
fn iter_mut_and_as_vec_mut_force_flat() {
    let mut line = clustered_uniform(ch('a'), 3);
    for slot in line.iter_mut() {
        slot.ch = 'Z';
    }
    assert!(!line.is_clustered(), "iter_mut degrades to Flat");
    assert_eq!(line.to_vec(), vec![ch('Z'), ch('Z'), ch('Z')]);

    let mut line2 = clustered_uniform(ch('a'), 4);
    line2.as_vec_mut().push(ch('b'));
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
    assert!(line.iter().all(|c| c == &rich('A')));
    // Range access through the cluster preserves the fat channels too.
    assert!(line.get_range(3, 7).all(|c| c.hyperlink() == Some(HyperlinkId(7))));
}

#[test]
fn metadata_survives_degradation_from_cluster() {
    let mut line = Line::from_flat(vec![rich('A'); 10]);
    assert!(line.try_compress());
    // Punch a different cell in the middle: storage degrades, but the
    // untouched neighbours keep their full metadata.
    assert!(line.set(5, ch('x')));
    assert!(!line.is_clustered());
    assert_eq!(line.get(5), Some(&ch('x')));
    for i in (0..10).filter(|&i| i != 5) {
        let c = line.get(i).expect("neighbour present");
        assert_eq!(c, &rich('A'), "neighbour {i} lost metadata");
        assert_eq!(c.extras(), Some("\u{0301}"));
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
    let mut line = Line::from_flat(vec![ch('a'), ch('b'), ch('c')]);
    assert_eq!(line[1], ch('b'));
    line[1] = ch('Z');
    assert_eq!(line[1], ch('Z'));
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
    let flat: Vec<Cell> = (0..80).map(|i| linked_cell(i + 1)).collect();
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
    let flat: Vec<Cell> = (0..40).map(|i| linked_cell(i + 1)).collect();

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

    let mut with_extras = ch('a');
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
    let mut short = ch('a');
    short.set_extras(Some(String::from("\u{0301}").into_boxed_str()));
    let mut long = ch('a');
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
