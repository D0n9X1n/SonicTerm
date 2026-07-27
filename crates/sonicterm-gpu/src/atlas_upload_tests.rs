use super::*;

#[test]
fn coalesces_touching_rows_and_columns() {
    let input = [
        DirtyRect { x: 0, y: 0, w: 2, h: 1 },
        DirtyRect { x: 2, y: 0, w: 2, h: 1 },
        DirtyRect { x: 0, y: 1, w: 4, h: 1 },
        DirtyRect { x: 7, y: 7, w: 1, h: 1 },
    ];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(
        output,
        [DirtyRect { x: 0, y: 0, w: 4, h: 2 }, DirtyRect { x: 7, y: 7, w: 1, h: 1 }]
    );
}

#[test]
fn separate_dirty_regions_remain_separate() {
    let input = [DirtyRect { x: 0, y: 0, w: 1, h: 1 }, DirtyRect { x: 2, y: 0, w: 1, h: 1 }];
    let mut output = Vec::new();

    coalesce_dirty_rects(&input, &mut output);

    assert_eq!(output, input);
}

#[test]
fn copies_tightly_packed_subrect_and_reuses_capacity() {
    let pixels: Vec<u8> = (0..48).collect();
    let rect = DirtyRect { x: 1, y: 0, w: 2, h: 2 };
    let mut scratch = Vec::new();

    copy_rect_into_scratch(&pixels, 4, rect, &mut scratch);
    let capacity = scratch.capacity();

    assert_eq!(scratch, [4, 5, 6, 7, 8, 9, 10, 11, 20, 21, 22, 23, 24, 25, 26, 27]);

    copy_rect_into_scratch(&pixels, 4, DirtyRect { x: 0, y: 0, w: 1, h: 1 }, &mut scratch);
    assert_eq!(scratch, [0, 1, 2, 3]);
    assert_eq!(scratch.capacity(), capacity);
}

/// What the retained staging buffer can hold, measured rather than assumed.
///
/// The test above establishes that capacity survives the call — deliberately,
/// because reallocating per frame on this path would cost more than the memory
/// does. What it does not say is how much memory that is, and the coverage
/// table records this class as `TransientWithinCall`: allocated and released
/// within one call, so a charge would be taken and returned before a sampler
/// could see it. The buffer is a field, it is cleared and never shrunk, and it
/// holds the largest rect it has ever copied for as long as the renderer
/// lives — so the figure is worth having on the record.
///
/// Derived from the atlas constants rather than typed, because a dirty rect
/// cannot exceed the atlas it comes from. A renderer holds two `AtlasUpload`s,
/// one for glyphs and one for images.
#[test]
fn retained_staging_is_bounded_by_one_whole_atlas() {
    use sonicterm_text::glyph_atlas::ATLAS_DIM;

    let whole_atlas = ATLAS_DIM as usize * ATLAS_DIM as usize * BYTES_PER_PIXEL as usize;

    assert_eq!(
        whole_atlas,
        16 * 1024 * 1024,
        "a full-atlas dirty rect stages this much, and it is retained after the copy"
    );

    // Measured on the real function rather than argued from the constants: a
    // copy of a given size leaves at least that much capacity behind.
    let width = 512u32;
    let height = 64u32;
    let pixels = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL as usize];
    let mut scratch = Vec::new();
    copy_rect_into_scratch(
        &pixels,
        width,
        DirtyRect { x: 0, y: 0, w: width, h: height },
        &mut scratch,
    );

    let copied = width as usize * height as usize * BYTES_PER_PIXEL as usize;
    assert!(
        scratch.capacity() >= copied,
        "a {copied}-byte copy left {} bytes of capacity; the retained figure scales with \
         the rect, up to {whole_atlas} for a whole atlas",
        scratch.capacity()
    );
}
