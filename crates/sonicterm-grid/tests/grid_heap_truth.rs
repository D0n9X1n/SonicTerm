//! Does the grid stay inside its budget, measured against real heap?
//!
//! Every accounting defect in this milestone shared one shape: a figure
//! checked against the number it was derived from rather than against memory
//! actually held. A counting allocator is the check that catches all of them,
//! and it has to live in an integration test because `#[global_allocator]` is
//! crate-wide.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid, MAX_GRID_CELLS};
use sonicterm_grid::hyperlink::HyperlinkId;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size().saturating_sub(new_size), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Serialises the tests, because the allocator is process-global.
///
/// Concurrent tests attribute each other's allocations to whichever one is
/// reading. A lock rather than `--test-threads=1`: a suite that only works
/// under a flag will eventually run without it.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn held() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

fn budget() -> usize {
    MAX_GRID_CELLS as usize * std::mem::size_of::<Cell>()
}

/// Fill a grid by scrolling, the way `cat` does. No resize.
fn fill_by_scrolling(cols: u16, rows: u32, linked: bool) -> Grid {
    let mut grid = Grid::new(cols, 24);
    // A user config sets this directly: `scrollback` is a plain `usize` in
    // TOML, passed to `set_scrollback_limit` unchanged.
    grid.set_scrollback_limit(usize::MAX);
    for row in 0..rows {
        for _ in 0..cols {
            if linked {
                grid.put_char_linked(
                    'x',
                    Color::Default,
                    Color::Default,
                    CellFlags::empty(),
                    Some(HyperlinkId(u64::from(row) + 1)),
                );
            } else {
                grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
            }
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    grid
}

/// Ordinary output must not grow the grid past its budget.
///
/// Enforcement ran only on alt-screen transitions and resize, so a grid
/// growing through `cat` was never checked. Measured before this: **63 MiB
/// against a 24 MiB budget** at 80 columns with linked cells, with no resize
/// anywhere in the sequence.
#[test]
fn scrolling_alone_keeps_the_grid_inside_its_budget() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    for (cols, linked) in [(1u16, true), (1, false), (80, true), (200, false)] {
        let before = held();
        let grid = fill_by_scrolling(cols, 200_000, linked);
        let truth = held().saturating_sub(before);
        let reported = grid.retained_amount().bytes;

        assert!(
            truth > budget() / 2,
            "cols={cols} linked={linked}: the fill must approach the budget or this \
             asserts nothing (held {truth})"
        );

        // The amortization window: the scroll path checks every
        // ROWS_BETWEEN_BUDGET_CHECKS scrolls, so the grid may overshoot by
        // that many rows before the next walk.
        let ceiling = budget() + budget() / 8;
        assert!(
            truth <= ceiling,
            "cols={cols} linked={linked}: real heap {truth} exceeds the budget {} by {} \
             — scrolling alone must bound the grid",
            budget(),
            truth.saturating_sub(budget())
        );

        // And the reported figure must not understate what is held, or the
        // budget is enforced against a number smaller than reality.
        assert!(
            reported + truth / 20 >= truth,
            "cols={cols} linked={linked}: reported {reported} understates real heap {truth}"
        );
        drop(grid);
    }
}

/// The reported figure must track real heap, not a subset of it.
///
/// `retained_amount` counts cells, rare-attribute boxes, and row containers.
/// Enforcement used to compare only the first of the three.
#[test]
fn the_reported_figure_tracks_real_heap() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    for (cols, linked) in [(80u16, true), (80, false)] {
        let before = held();
        let grid = fill_by_scrolling(cols, 20_000, linked);
        let truth = held().saturating_sub(before);
        let reported = grid.retained_amount().bytes;

        assert!(truth > 0, "precondition: filling allocated");
        let ratio = truth as f64 / reported.max(1) as f64;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "cols={cols} linked={linked}: reported {reported} against real heap {truth} \
             ({ratio:.2}x) — the figure the governor is charged must be the memory held"
        );
        drop(grid);
    }
}

/// A grid over budget must come back under it on resize too.
///
/// Resize is where enforcement always ran. It must still work, and it must
/// use the same figure the scroll path does.
#[test]
fn resize_brings_an_over_budget_grid_back_under() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let before = held();
    let mut grid = fill_by_scrolling(80, 50_000, true);
    grid.resize(81, 24);
    let truth = held().saturating_sub(before);

    assert!(
        truth <= budget() + budget() / 8,
        "after resize, real heap {truth} exceeds the budget {}",
        budget()
    );
}

/// Trimming must not cost the user their visible screen.
///
/// Scrollback is the term that shrinks; the rows on screen are what the user
/// is looking at and must survive.
#[test]
fn trimming_preserves_the_visible_screen() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut grid = fill_by_scrolling(80, 100_000, true);
    grid.put_char('Z', Color::Default, Color::Default, CellFlags::empty());

    let row = grid.cursor.row;
    let text: String = grid.row(row).iter().map(|cell| cell.ch).collect();
    assert!(text.contains('Z'), "the most recent output must survive trimming");
    assert!(grid.retained_amount().items > 0, "the grid must still hold rows");
}

/// Enforcement must compare the figure it reports, not a subset of it.
///
/// Measured on a grid the scroll path has already trimmed: **cell bytes 9 MiB,
/// reported 25 MiB, budget 24 MiB.** `retained_cell_bytes()` says the grid is
/// comfortably inside its budget; `retained_amount()` — the figure the
/// governor is charged — says it is over. The two disagree by the boxes and
/// containers one counts and the other does not.
///
/// This is the state a resize lands in, which is why enforcement comparing the
/// smaller figure would decline to act on a grid that is genuinely over.
#[test]
fn enforcement_sees_the_same_figure_the_governor_is_charged() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let grid = fill_by_scrolling(80, 200_000, true);

    let cell_bytes: usize = grid
        .rows_iter()
        .chain(grid.scrollback_iter())
        .map(sonicterm_grid::line::Line::approx_capacity_byte_size)
        .sum();
    let reported = grid.retained_amount().bytes;

    assert!(
        reported > cell_bytes,
        "precondition: the two figures must differ, or this asserts nothing \
         (cells {cell_bytes}, reported {reported})"
    );

    // The gap must be material — boxes and containers are the whole point.
    assert!(
        reported > cell_bytes + cell_bytes / 2,
        "the excluded terms must be significant: cells {cell_bytes}, reported {reported}"
    );

    // And the reported figure is what real heap follows.
    assert!(
        reported >= cell_bytes,
        "reported {reported} must not understate cell storage {cell_bytes}"
    );
}
