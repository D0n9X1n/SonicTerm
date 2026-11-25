use super::*;

/// A buffer reads back zero at every length, holds exactly what was asked for, and keeps what is
/// written through it; on macOS a large one is a mapping and a small one stays on the heap.
#[test]
fn a_buffer_is_zeroed_writable_and_sized_as_asked() {
    for len in [16, 4096, 1024 * 1024, 16 * 1024 * 1024] {
        let mut pixels = AtlasPixels::zeroed(len);
        assert_eq!((pixels.len(), pixels.capacity()), (len, len), "{len}");
        assert!(pixels.iter().all(|byte| *byte == 0), "{len} reads zero");
        pixels[len - 1] = 7;
        pixels[0] = 9;
        assert_eq!((pixels[0], pixels[len - 1]), (9, 7), "{len} keeps writes");
        assert_eq!(pixels.is_mapped(), cfg!(target_os = "macos") && len >= 1024 * 1024, "{len}");
    }
}
