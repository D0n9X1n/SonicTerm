use super::*;

fn image(id: u64, bytes: usize) -> InlineImage {
    InlineImage { id, row: 0, col: 0, width: 1, height: 1, bgra: Arc::from(vec![0; bytes]) }
}

#[test]
fn decode_dimensions_reject_image_bombs() {
    assert!(inline_image_decode_dimensions_allowed(2048, 2048));
    assert!(!inline_image_decode_dimensions_allowed(2049, 1));
    assert!(!inline_image_decode_dimensions_allowed(1, 2049));
    assert!(!inline_image_decode_dimensions_allowed(u32::MAX, u32::MAX));
}

#[test]
fn retained_inline_images_respect_count_and_byte_budgets() {
    let chunk = MAX_RETAINED_INLINE_IMAGE_BYTES / 2;
    let mut images = vec![image(1, chunk), image(2, chunk), image(3, chunk)];

    trim_inline_images(&mut images);

    assert_eq!(images.iter().map(|image| image.id).collect::<Vec<_>>(), vec![2, 3]);
    assert!(
        images.iter().map(|image| image.bgra.len()).sum::<usize>()
            <= MAX_RETAINED_INLINE_IMAGE_BYTES
    );
}
