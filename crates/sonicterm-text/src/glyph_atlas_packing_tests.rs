//! Shelf-fit classification requires complete placement and the specified headroom.

use super::*;

/// The fit rule takes the smallest candidate that places every tile and leaves a quarter free.
#[test]
fn fit_outcome_needs_every_tile_placed_and_a_quarter_of_the_height_free() {
    // Three 129-wide tiles need 387 px of shelf: 256 cannot hold the row, 512 holds it at 100 px.
    assert_eq!(fit_outcome_of_tiles(&[[129, 100]; 3]), FitOutcome::Fits(512));
    // Four 500×100 tiles place at 512 one per shelf, using 400 px, above 75% of 512 (384), so the
    // smallest qualifying start is 1024, where two shelves use 200 px.
    assert_eq!(fit_outcome_of_tiles(&[[500, 100]; 4]), FitOutcome::Fits(1024));
    // One 64×1600 tile places only at 2048 and there uses 78% of the height.
    assert_eq!(fit_outcome_of_tiles(&[[64, 1600]]), FitOutcome::FitsWithoutHeadroom);
    // Two 1100×1100 tiles need two shelves totalling 2200 px, more than 2048.
    assert_eq!(fit_outcome_of_tiles(&[[1100, 1100]; 2]), FitOutcome::DoesNotFit);
    assert_eq!(fit_outcome_of_tiles(&[]), FitOutcome::Fits(256), "an empty set fits the floor");
    for (outcome, label) in [
        (FitOutcome::Fits(512), "512"),
        (FitOutcome::FitsWithoutHeadroom, "no_headroom"),
        (FitOutcome::DoesNotFit, "does_not_fit"),
        (FitOutcome::Evicted, "evicted"),
    ] {
        assert_eq!(outcome.label(), label);
    }
}
