//! A warm chrome-cache hit allocates no glyphs.
//!
//! The renderer keeps tab titles and the search overlay's runs as prepared runs and draws them
//! with `layout_view`. After one warm draw, drawing a kept title or run must allocate exactly what
//! laying out an already-held run allocates (the output vectors), and nothing for glyphs or text:
//! the cache lookup itself allocates nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_engine::FontStack;
use sonicterm_gpu::chrome_cache_seam::{ChromeCacheSeam, SeamDraw};
use sonicterm_gpu::chrome_text::{layout_prepared, ChromeAttrs, ChromeShapedRun};
use sonicterm_gpu::color::ChromeColor;
use sonicterm_text::glyph_atlas::GlyphAtlas;
use sonicterm_types::GlyphRasterVariant;

/// Allocations made while a counting thread had counting on.
static COUNTED_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Whether this thread's allocations are counted; other test threads never are.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct Counting;

// SAFETY: every operation forwards its exact pointer, layout and size to `System`; the
// bookkeeping is a const-initialized thread-local read and an atomic add, which never allocate.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` is forwarded unchanged after allocation-free bookkeeping.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: `layout` is the valid layout received from the allocator caller.
        unsafe { System.alloc(layout) }
    }

    // SAFETY: `ptr` and `layout` are forwarded unchanged.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` are the matching pair received from the allocator caller.
        unsafe { System.dealloc(ptr, layout) }
    }

    // SAFETY: `ptr`, `layout` and `new_size` are forwarded unchanged after bookkeeping.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        // SAFETY: all arguments are the exact valid values received from the allocator caller.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Count one allocation when this thread is counting. `try_with` tolerates thread teardown.
fn note_allocation() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        COUNTED_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Allocations `work` makes on this thread.
fn allocations_of<Output>(work: impl FnOnce() -> Output) -> (Output, usize) {
    let before = COUNTED_ALLOCATIONS.load(Ordering::Relaxed);
    COUNTING.with(|counting| counting.set(true));
    let output = work();
    COUNTING.with(|counting| counting.set(false));
    (output, COUNTED_ALLOCATIONS.load(Ordering::Relaxed) - before)
}

/// The packaged Rec Mono stack, independent of host fonts.
fn packaged_stack() -> FontStack {
    let fonts = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    FontStack::try_new_with_font_dirs_for_test(
        &[("Rec Mono St.Helens", false)],
        vec![fonts],
        15.0,
        72,
        1.0,
    )
    .expect("the packaged fonts build a stack")
}

const SCREEN: (f32, f32) = (1200.0, 200.0);
const ORIGIN: (f32, f32) = (10.0, 40.0);

#[test]
fn warm_hits_allocate_no_glyphs() {
    // Each warm cache draw allocates exactly what laying out an already-held run of the same
    // text allocates, so the lookup, the key compare and the kept run add no allocation.
    let stack = packaged_stack();
    let mut raster = stack.clone();
    for (text, variant, is_title) in [
        ("shell ~/src", GlyphRasterVariant::TabTitle, true),
        ("search: needle 1/3", GlyphRasterVariant::Normal, false),
    ] {
        let mut seam = ChromeCacheSeam::new();
        let mut atlas = GlyphAtlas::new(1024, 1024);
        let draw = |seam: &mut ChromeCacheSeam, atlas: &mut GlyphAtlas, raster: &mut FontStack| {
            let target = SeamDraw {
                raster,
                atlas,
                color: ChromeColor::WHITE,
                origin: ORIGIN,
                screen: SCREEN,
            };
            if is_title {
                seam.draw_prepared_title(&stack, 0, text, 15.0, 1000.0, target)
            } else {
                seam.draw_chrome_run(&stack, text, 15.0, target)
            }
        };
        // The warm draw shapes, keeps the run and fills the atlas.
        let _ = draw(&mut seam, &mut atlas, &mut raster);
        let held = ChromeShapedRun::shape(&stack, text, ChromeAttrs::default(), 15.0, 15.0)
            .expect("the packaged font shapes the sample");
        let lay_out_held = |atlas: &mut GlyphAtlas, raster: &mut FontStack| {
            layout_prepared(&held, raster, atlas, ChromeColor::WHITE, ORIGIN, SCREEN, None, variant)
        };
        let (held_layout, held_allocations) =
            allocations_of(|| lay_out_held(&mut atlas, &mut raster));
        let (hit_layout, hit_allocations) =
            allocations_of(|| draw(&mut seam, &mut atlas, &mut raster));
        assert!(!hit_layout.glyphs.is_empty(), "{text:?}: the warm hit draws glyphs");
        assert_eq!(hit_layout.glyphs.len(), held_layout.glyphs.len(), "{text:?}: the same glyphs");
        assert_eq!(
            hit_allocations, held_allocations,
            "{text:?}: a warm hit allocates only the layout's output vectors"
        );
    }
}
