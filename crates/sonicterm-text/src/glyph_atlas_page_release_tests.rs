use super::*;

/// Only whole pages strictly inside the buffer are marked: the start rounds up and the end rounds
/// down, so no byte outside the buffer's allocation is ever advised.
#[test]
fn only_whole_pages_inside_the_buffer_are_marked() {
    let page = 16_384;
    assert_eq!(whole_pages(0, 4 * page, page), Some((0, 4 * page)));
    assert_eq!(whole_pages(100, 4 * page, page), Some((page, 3 * page)));
    assert_eq!(whole_pages(page + 1, page, page), None, "less than one whole page");
    assert_eq!(whole_pages(usize::MAX - 10, 100, page), None, "an overflowing range");
}

/// Releasing a buffer drops it and, on macOS, advises its pages without disturbing the allocator:
/// a buffer allocated after the release is usable and zeroed as allocated.
#[test]
fn a_released_buffer_leaves_the_allocator_usable() {
    release_pixel_pages(vec![7u8; 16 * 1024 * 1024]);
    let next = vec![0u8; 16 * 1024 * 1024];
    assert!(next.iter().all(|byte| *byte == 0));
}
