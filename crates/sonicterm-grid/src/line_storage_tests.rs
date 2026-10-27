use super::*;
use crate::line::line_test_support::*;
use sonicterm_types::{CellFlags, Color};

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
