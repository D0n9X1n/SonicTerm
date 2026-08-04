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

// SAFETY: Operations forward exact pointers, layouts, and sizes to `System`; atomic bookkeeping allocates nothing and cannot re-enter.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` must be valid; the atomic byte update is allocation-free before forwarding it unchanged.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is the exact valid layout received under `GlobalAlloc::alloc`.
        unsafe { System.alloc(layout) }
    }
    // SAFETY: `ptr` and its original `layout` must match; allocation-free bookkeeping cannot re-enter deallocation.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` and original `layout` are forwarded unchanged from the valid deallocation call.
        unsafe { System.dealloc(ptr, layout) }
    }
    // SAFETY: `ptr`, original `layout`, and `new_size` must be valid; atomic bookkeeping allocates nothing and cannot re-enter.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size().saturating_sub(new_size), Ordering::Relaxed);
        // SAFETY: `ptr`, original `layout`, and `new_size` are forwarded unchanged under `GlobalAlloc::realloc`.
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

/// Fill a grid with text carrying grapheme extras, the way accented or ZWJ
/// emoji output does.
///
/// Each lead character is followed by `marks` zero-width combining marks. Those
/// do not advance the cursor; they are appended to the lead cell's extras
/// string, one heap allocation per cell that the cell's own size does not
/// describe.
fn fill_with_extras(cols: u16, rows: u32, marks: usize) -> Grid {
    let mut grid = Grid::new(cols, 24);
    grid.set_scrollback_limit(usize::MAX);
    for _ in 0..rows {
        for _ in 0..cols {
            grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
            for _ in 0..marks {
                // U+0301 COMBINING ACUTE ACCENT: zero width, 2 bytes UTF-8.
                grid.put_char('\u{0301}', Color::Default, Color::Default, CellFlags::empty());
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

/// Grapheme extras must be counted, not assumed small.
///
/// `FatAttributes::extras` is an `Option<Box<str>>` holding a cell's trailing
/// zero-width codepoints. The figure was built from `size_of::<FatAttributes>()`
/// alone, which describes the pointer and not the string behind it, so a grid
/// of accented text reported the same as a grid of plain linked cells.
///
/// Measured before the payload was counted, at 80 columns with a full
/// scrollback: 40,682,664 held against 20,455,464 reported — **1.99x**. The
/// uncounted term was the larger one, exactly as it was for the rare-attribute
/// box before it.
///
/// Reached by ordinary output. Combining marks and ZWJ emoji both land here; no
/// resize, no configuration change, and no alternate screen is involved.
#[test]
fn grapheme_extras_are_counted_against_real_heap() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    for marks in [1usize, 8, 32] {
        let before = held();
        let grid = fill_with_extras(80, 4_000, marks);
        let truth = held().saturating_sub(before);
        let reported = grid.retained_amount().bytes;

        assert!(truth > 0, "precondition: filling must allocate");

        // Understating is the direction that matters: the governor is charged
        // the reported figure and the grid enforces its budget against it, so
        // an undercount both admits past the cap and declines to trim.
        assert!(
            reported + truth / 50 >= truth,
            "marks={marks}: reported {reported} understates real heap {truth} by {} ({:.2}x)",
            truth.saturating_sub(reported),
            truth as f64 / reported.max(1) as f64
        );
        assert!(
            reported <= truth + truth / 20 + 4096,
            "marks={marks}: reported {reported} overstates real heap {truth}"
        );
        drop(grid);
    }
}

/// A grid of extras-bearing text must stop at the budget, in real memory.
///
/// The case the ratio assertion above cannot make on its own: enforcement
/// compares `retained_amount()`, so while the payload went uncounted the grid
/// judged itself compliant and kept accepting rows. Measured before the fix:
/// **52,958,568 bytes held against a 25,165,824 budget — 210%** — while the
/// grid reported 26,673,384 and considered itself in band.
#[test]
fn extras_bearing_output_stops_at_the_budget() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let before = held();
    let grid = fill_with_extras(80, 60_000, 32);
    let truth = held().saturating_sub(before);

    assert!(
        truth > budget() / 2,
        "precondition: the fill must approach the budget or this asserts nothing (held {truth})"
    );

    // The same amortization window the plain-scrolling test allows: the scroll
    // path checks every ROWS_BETWEEN_BUDGET_CHECKS scrolls.
    let ceiling = budget() + budget() / 8;
    assert!(
        truth <= ceiling,
        "real heap {truth} exceeds the budget {} by {} — output carrying combining marks \
         must be bounded like any other",
        budget(),
        truth.saturating_sub(budget())
    );
    drop(grid);
}

/// Allowance for allocations the test harness itself makes inside a
/// measurement window.
///
/// [`MEASURE`] serialises this file's measurement code, but the harness runs
/// its own per-test bookkeeping on sibling threads that are blocked on the
/// lock, and those allocations still land in whichever window is open.
/// Measured at up to 5,716 bytes in a nine-test parallel run, against zero
/// when the same measurement runs alone.
///
/// So an exact equality here would be asserting on the harness, not the grid.
/// Terms smaller than this floor are pinned in unit tests instead, where no
/// allocator is involved and the figure can be checked exactly.
const HARNESS_NOISE_BYTES: usize = 64 * 1024;

/// While an alternate screen is active, both screens must be counted.
///
/// The saved primary is a whole `Box<Grid>`: deque spines, its own dirty
/// bitset, its prompt ring, and the struct itself.
///
/// This measures the proportional question — is the saved primary counted at
/// all, or does entering an alternate screen make 25 MB of history vanish from
/// the figure while it stays in memory. The fixed-size container term is below
/// the noise floor here and is pinned exactly in a unit test.
#[test]
fn an_active_alternate_screen_counts_both_screens() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let before = held();
    let mut grid = fill_by_scrolling(80, 5_000, true);
    let primary_reported = grid.retained_amount().bytes;

    grid.enter_alt_screen();
    let truth = held().saturating_sub(before);
    let reported = grid.retained_amount().bytes;

    assert!(
        primary_reported > budget() / 8,
        "precondition: the saved primary must be substantial (held {primary_reported})"
    );
    assert!(
        reported + HARNESS_NOISE_BYTES >= truth,
        "alt active: reported {reported} understates real heap {truth} by {} — the saved \
         primary is memory held while the alternate screen shows",
        truth.saturating_sub(reported)
    );

    // The saved primary must be visible in the figure, not silently dropped.
    let regions = grid.retained_amount_by_region();
    assert!(
        regions.alternate.bytes > budget() / 8,
        "the saved primary must be charged while the alternate screen is active \
         (alternate {})",
        regions.alternate.bytes
    );
    assert_eq!(
        regions.total().bytes,
        reported,
        "the three regions must sum to the total the governor is charged"
    );

    // Writing into the alternate screen must stay counted too.
    for _ in 0..200 {
        for _ in 0..80 {
            grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    let truth = held().saturating_sub(before);
    assert!(
        grid.retained_amount().bytes + HARNESS_NOISE_BYTES >= truth,
        "after writing to the alternate screen, reported {} understates real heap {truth}",
        grid.retained_amount().bytes
    );

    grid.leave_alt_screen();
    let truth = held().saturating_sub(before);
    assert!(
        grid.retained_amount().bytes + HARNESS_NOISE_BYTES >= truth,
        "after leaving, reported {} understates real heap {truth}",
        grid.retained_amount().bytes
    );
    drop(grid);
}

/// Trimming history must return memory, not just reduce the figure.
///
/// A reported drop with no matching heap drop is the defect this measures for:
/// it would mean rows were forgotten while their capacity stayed held, and the
/// governor would be told memory came back that never did.
#[test]
fn shrinking_the_scrollback_limit_returns_real_memory() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let before = held();
    let mut grid = fill_by_scrolling(80, 20_000, true);
    let truth_before = held().saturating_sub(before);
    let reported_before = grid.retained_amount().bytes;

    assert!(
        truth_before > budget() / 2,
        "precondition: the grid must hold enough for trimming to be visible ({truth_before})"
    );

    grid.set_scrollback_limit(100);
    let truth_after = held().saturating_sub(before);
    let reported_after = grid.retained_amount().bytes;

    let reported_drop = reported_before.saturating_sub(reported_after);
    let truth_drop = truth_before.saturating_sub(truth_after);

    assert!(
        reported_drop > reported_before / 2,
        "precondition: the reported figure must fall materially (dropped {reported_drop})"
    );

    // The heap must follow the figure down. Allowing the reported drop to
    // exceed the real one by a twentieth covers container slack that is
    // legitimately retained.
    assert!(
        truth_drop + reported_drop / 20 >= reported_drop,
        "reported fell by {reported_drop} but real heap only fell by {truth_drop} — \
         a reported drop with no memory returned is capacity retained, not released"
    );
    assert!(
        reported_after + HARNESS_NOISE_BYTES >= truth_after,
        "after trimming, reported {reported_after} understates real heap {truth_after}"
    );
    drop(grid);
}
