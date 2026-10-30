//! Pins how placeholder runs become image slices: the exact and placement-0 lookups, the
//! no-fallback rule for a named placement, and the surface rect and uv of a run.

use std::sync::Arc;

use sonicterm_render_model::boundary::grid::grid::{Color, Grid};
use sonicterm_render_model::kitty_placeholder::{diacritic_value, PLACEHOLDER};
use sonicterm_render_model::{InlineImage, KittyShow, KittyTag};

use super::*;

/// The diacritic for `value`, found by searching the table's inverse.
fn mark(value: u32) -> char {
    (0x0300..=0x1D244)
        .filter_map(char::from_u32)
        .find(|candidate| diacritic_value(*candidate) == Some(value))
        .expect("every small value has a diacritic")
}

/// A grid whose row 0 holds `cols` placeholder cells for image row `image_row`, naming `image_id`
/// in the foreground and, when nonzero, `placement_id` in the underline colour.
fn placeholder_grid(image_id: u32, placement_id: u32, image_row: u32, cols: u16) -> Grid {
    let mut grid = Grid::new(8, 2);
    let fg = Color::Rgb((image_id >> 16) as u8, (image_id >> 8) as u8, image_id as u8);
    for col in 0..cols {
        let cell = &mut grid.row_mut(0)[usize::from(col)];
        cell.ch = PLACEHOLDER;
        cell.fg = fg;
        cell.set_extras(Some(
            format!("{}{}", mark(image_row), mark(u32::from(col))).into_boxed_str(),
        ));
        if placement_id != 0 {
            // When: placement_id is nonzero, the underline colour carries it.
            cell.set_underline_color(Some(Color::Indexed(placement_id as u8)));
        }
    }
    grid
}

/// A 40x20 px image entry tagged as a virtual placement of `image_id`/`placement_id` in a 4x2 box.
fn virtual_image(id: u64, image_id: u32, placement_id: u32) -> InlineImage {
    InlineImage {
        id,
        row: 0,
        col: 0,
        width: 40,
        height: 20,
        bgra: Arc::from(vec![0u8; 40 * 20 * 4]),
        kitty: Some(KittyTag {
            image_id,
            placement_id,
            show: KittyShow::Virtual { cols: 4, rows: 2 },
            source_px: (40, 20),
        }),
    }
}

/// The slices of `grid`'s first row for `images`, with 10x10 px cells at origin (100, 50).
fn slices<'img>(
    grid: &Grid,
    images: &'img [InlineImage],
) -> Vec<(&'img InlineImage, PlaceholderSlice)> {
    slices_for_rows(std::iter::once((0, grid.row(0))), images, (100.0, 50.0), (10.0, 10.0))
}

#[test]
fn a_placeholder_row_maps_to_its_slice_of_the_fitted_image() {
    // A 40x20 image fills a 4x2 box of 10 px cells exactly; image row 1 is its bottom half.
    let grid = placeholder_grid(7, 0, 1, 4);
    let images = [virtual_image(1, 7, 0)];
    let found = slices(&grid, &images);
    assert_eq!(found.len(), 1, "four consecutive cells form one run");
    let (image, slice) = found[0];
    assert_eq!(image.id, 1);
    assert_eq!(slice.dest, PaneRect::new(100.0, 50.0, 40.0, 10.0));
    assert_eq!(slice.source_uv, [0.0, 0.5, 1.0, 1.0]);
}

#[test]
fn placement_zero_uses_any_virtual_placement_of_the_image() {
    // A cell without an underline colour names no placement, so the image's placement serves.
    let grid = placeholder_grid(7, 0, 0, 2);
    let images = [virtual_image(1, 7, 3)];
    assert_eq!(slices(&grid, &images).len(), 1);
}

#[test]
fn a_named_placement_never_falls_back_to_another() {
    // A cell naming placement 5 draws nothing when only placement 3 exists.
    let grid = placeholder_grid(7, 5, 0, 2);
    let images = [virtual_image(1, 7, 3)];
    assert!(slices(&grid, &images).is_empty());
    let named = [virtual_image(1, 7, 3), virtual_image(2, 7, 5)];
    assert_eq!(slices(&grid, &named)[0].0.id, 2, "the exact placement is chosen");
}

#[test]
fn uploads_anchored_and_deleted_images_draw_no_placeholder() {
    // Only virtual placements are drawn through placeholders; anything else, or nothing, draws blank.
    let grid = placeholder_grid(7, 0, 0, 2);
    let mut upload = virtual_image(1, 7, 0);
    upload.kitty = Some(KittyTag {
        image_id: 7,
        placement_id: 0,
        show: KittyShow::Upload,
        source_px: (40, 20),
    });
    let mut anchored = virtual_image(2, 7, 0);
    anchored.kitty = Some(KittyTag {
        image_id: 7,
        placement_id: 0,
        show: KittyShow::Anchored,
        source_px: (40, 20),
    });
    assert!(slices(&grid, &[upload, anchored]).is_empty());
    assert!(slices(&grid, &[]).is_empty());
    assert!(!has_virtual_placement(&[]));
}

/// A placeholder slice samples only its part of the image's atlas tile: the bottom half of a
/// 40x20 image drawn at 1:1 maps to the lower half of the tile's v range, clipped to the pane.
#[test]
fn a_slice_samples_its_part_of_the_atlas_tile() {
    let image = virtual_image(1, 7, 0);
    let mut atlas = GlyphAtlas::new(64, 64);
    let mut instances = Vec::new();
    let placement = InlineImagePlacement {
        image: &image,
        origin_x: 0.0,
        origin_y: 0.0,
        content_clip: PaneRect::new(0.0, 0.0, 30.0, 100.0),
        painter_order: 0,
        slice: Some(PlaceholderSlice {
            dest: PaneRect::new(0.0, 10.0, 40.0, 10.0),
            source_uv: [0.0, 0.5, 1.0, 1.0],
        }),
    };
    let skipped = emit_inline_image_instances(
        &mut atlas,
        &mut instances,
        &[placement],
        10.0,
        10.0,
        100.0,
        100.0,
    );
    assert_eq!(skipped, 0);
    let instance = instances[0];
    assert_eq!(instance.rect_px, [0.0, 10.0, 30.0, 10.0], "clipped to the pane's 30 px width");
    let [u0, v0, u1, v1] = instance.sample_uv;
    let expect = [u0, v0 + (v1 - v0) * 0.5, u0 + (u1 - u0) * 0.75, v1];
    for (got, want) in instance.uv.iter().zip(expect) {
        assert!((got - want).abs() < 1e-6, "uv {:?} != {expect:?}", instance.uv);
    }
}

/// A pane whose only image is a virtual placement still yields a visible placement where its
/// placeholders are, so the atlas promotion decision sees renderable media on that frame.
#[test]
fn a_placeholder_only_pane_has_renderable_media() {
    let grid = placeholder_grid(7, 0, 0, 4);
    let images = [virtual_image(1, 7, 0)];
    let mut placements = Vec::new();
    push_pane_placements(
        &mut placements,
        &images,
        std::iter::once((0, grid.row(0))),
        (0.0, 0.0),
        PaneRect::new(0.0, 0.0, 200.0, 100.0),
        (10.0, 10.0),
    );
    assert_eq!(placements.len(), 1, "drawn only through its placeholder run, never at its anchor");
    let renderable = placements
        .iter()
        .any(|placement| placement.visible_rect(10.0, 10.0, 200.0, 100.0).is_some());
    assert!(renderable);
    assert!(image_atlas_promotion_required(&GlyphAtlas::new(1, 1), renderable));
}

/// A pane without virtual placements adds only its anchored images and never reads its rows.
#[test]
fn a_pane_without_virtual_placements_is_not_scanned() {
    let images = [InlineImage { kitty: None, ..virtual_image(1, 7, 0) }];
    let mut placements = Vec::new();
    let rows = std::iter::from_fn(
        || -> Option<(u16, &sonicterm_render_model::boundary::grid::line::Line)> {
            panic!("rows must not be read for a pane without virtual placements")
        },
    );
    push_pane_placements(
        &mut placements,
        &images,
        rows,
        (0.0, 0.0),
        PaneRect::new(0.0, 0.0, 9.0, 9.0),
        (1.0, 1.0),
    );
    assert_eq!(placements.len(), 1);
    assert!(placements[0].slice.is_none());
}

/// A change to a placement alone — its box, or its deletion — changes the frame's media identity.
#[test]
fn placement_only_changes_change_the_media_hash() {
    let base = [virtual_image(1, 7, 0)];
    let hash_of = |images: &[InlineImage]| inline_media_hash(std::iter::once((3, images)));
    let mut resized = base.clone();
    resized[0].kitty =
        Some(KittyTag { show: KittyShow::Virtual { cols: 8, rows: 2 }, ..base[0].kitty.unwrap() });
    let mut uploaded = base.clone();
    uploaded[0].kitty = Some(KittyTag { show: KittyShow::Upload, ..base[0].kitty.unwrap() });
    assert_eq!(hash_of(&base), hash_of(&base.clone()));
    assert_ne!(hash_of(&base), hash_of(&resized));
    assert_ne!(hash_of(&base), hash_of(&uploaded));
}

/// Without `c`/`r`, the box comes from the transmitted size: an image downscaled for retention
/// still covers its natural cells, and is drawn 1:1 from the box's corner, not enlarged.
#[test]
fn natural_size_uses_the_transmitted_size() {
    let grid = placeholder_grid(7, 0, 0, 4);
    let mut image = virtual_image(1, 7, 0);
    image.kitty = Some(KittyTag {
        image_id: 7,
        placement_id: 0,
        show: KittyShow::Virtual { cols: 0, rows: 0 },
        source_px: (80, 40),
    });
    let images = [image];
    let found = slices(&grid, &images);
    assert_eq!(
        found[0].1.dest,
        PaneRect::new(100.0, 50.0, 40.0, 10.0),
        "row 0 of an 8x4-cell box, first 4 cells"
    );
    assert_eq!(found[0].1.source_uv, [0.0, 0.0, 0.5, 0.25]);
}
