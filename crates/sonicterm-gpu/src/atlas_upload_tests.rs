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
