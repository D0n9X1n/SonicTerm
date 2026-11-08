//! Checks that a borrowed-pixel image insert copies the image once, into the atlas, by counting
//! the bytes the heap hands out during it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_text::glyph_atlas::{GlyphAtlas, RasterTile};
use sonicterm_types::GlyphKey;

static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: Operations forward exact pointers, layouts, and sizes to `System`; atomic bookkeeping allocates nothing and cannot re-enter.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` is forwarded unchanged after allocation-free bookkeeping.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is the valid layout received from the allocator caller.
        unsafe { System.alloc(layout) }
    }

    // SAFETY: `ptr` and `layout` are the matching pair received from the allocator caller.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` are forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }

    // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged after allocation-free bookkeeping.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATED_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        // SAFETY: all arguments are the exact valid values received from the allocator caller.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes the heap handed out while `work` ran, frees not subtracted.
fn allocated_during(work: impl FnOnce()) -> usize {
    let before = ALLOCATED_BYTES.load(Ordering::Relaxed);
    work();
    ALLOCATED_BYTES.load(Ordering::Relaxed) - before
}

/// The renderer's key for image `id`.
fn image_key(id: u32) -> GlyphKey {
    GlyphKey {
        ch: '\u{fffc}',
        font_slot: 0xFE,
        weight_bold: false,
        italic: false,
        glyph_id: id,
        raster_variant: sonicterm_types::glyph_key::GlyphRasterVariant::Normal,
    }
}

const SIDE_PX: u32 = 1024;
const IMAGE_BYTES: usize = (SIDE_PX * SIDE_PX * 4) as usize;

#[test]
fn a_first_image_insert_does_not_copy_the_image_on_the_heap() {
    // The borrowed insert copies the image's pixels straight into the atlas, allocating only
    // bookkeeping (the entry and its dirty rect), far below one image; the owned-tile path,
    // counted the same way, allocates the whole image for its tile first.
    let pixels = vec![0x5a_u8; IMAGE_BYTES];
    let mut borrowed = GlyphAtlas::new(2048, 2048);
    let borrowed_bytes = allocated_during(|| {
        let info = borrowed.get_or_insert_color_pixels_without_eviction(
            image_key(1),
            SIDE_PX,
            SIDE_PX,
            &pixels,
        );
        assert!(info.is_some(), "the image fits");
    });
    assert!(
        borrowed_bytes < IMAGE_BYTES / 16,
        "the borrowed insert allocated {borrowed_bytes} bytes for a {IMAGE_BYTES}-byte image"
    );

    let mut owned = GlyphAtlas::new(2048, 2048);
    let owned_bytes = allocated_during(|| {
        let info =
            owned.get_or_insert_lazy_without_eviction(image_key(1), SIDE_PX, SIDE_PX, || {
                RasterTile {
                    width: SIDE_PX,
                    height: SIDE_PX,
                    offset_x: 0,
                    offset_y: 0,
                    advance: SIDE_PX as f32,
                    coverage: pixels.clone(),
                    is_color: true,
                    is_subpixel: false,
                }
            });
        assert!(info.is_some(), "the image fits");
    });
    assert!(
        owned_bytes >= IMAGE_BYTES,
        "the counter sees the owned path's {owned_bytes}-byte copy"
    );
    assert!(borrowed.pixels() == owned.pixels(), "both paths leave the same atlas pixels");
}
