use super::*;

#[test]
fn v120_grid_aggregate_retention_has_one_governor() {
    // The baseline invariant this sentinel was placed for: one figure covers
    // everything a grid retains, and it is the same figure the grid enforces
    // its own budget against. Two accounting paths that can disagree is the
    // failure this exists to prevent.
    let mut grid = Grid::new(80, 24);
    let empty = grid.retained_amount();
    assert_eq!(empty.items, 24, "a fresh grid retains its visible rows");

    grid.set_scrollback_limit(500);
    for row in 0..600 {
        for column in 0..40 {
            grid.put_char(
                char::from(b'a' + (column % 26) as u8),
                Color::Default,
                Color::Default,
                CellFlags::empty(),
            );
            let _ = row;
        }
        grid.linefeed();
    }

    let filled = grid.retained_amount();
    assert!(filled.bytes > empty.bytes, "scrollback growth is reflected in retained bytes");
    assert!(filled.items > empty.items, "scrollback growth is reflected in retained rows");

    // The reported figure is the enforced figure: it never exceeds the budget
    // the grid clamps itself to.
    let budget_bytes = MAX_GRID_CELLS as usize * std::mem::size_of::<Cell>();
    assert!(
        filled.bytes <= budget_bytes,
        "retained {} bytes exceeds the {} byte budget the grid enforces",
        filled.bytes,
        budget_bytes
    );

    // Alternate-screen storage is counted in the same aggregate rather than
    // escaping it.
    grid.enter_alt_screen();
    for _ in 0..24 {
        for _ in 0..40 {
            grid.put_char('z', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.linefeed();
    }
    let with_alt = grid.retained_amount();
    assert!(
        with_alt.items > filled.items,
        "alternate-screen rows join the aggregate rather than escaping it"
    );
    assert!(
        with_alt.bytes <= budget_bytes,
        "the aggregate still respects the enforced budget with an alternate screen"
    );

    // Leaving the alternate screen returns to the primary aggregate.
    grid.leave_alt_screen();
    let restored = grid.retained_amount();
    assert!(restored.items <= with_alt.items, "leaving the alternate screen releases its rows");
}

#[test]
fn retained_amount_tracks_trimming_back_down() {
    // Retention has to fall when history is trimmed, or a governor would hold
    // a charge for storage that was already given back.
    let mut grid = Grid::new(40, 10);
    grid.set_scrollback_limit(200);
    for _ in 0..250 {
        for _ in 0..20 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.linefeed();
    }
    let grown = grid.retained_amount();

    grid.set_scrollback_limit(10);
    let trimmed = grid.retained_amount();

    assert!(
        trimmed.items < grown.items,
        "trimming history must reduce retained rows: {} -> {}",
        grown.items,
        trimmed.items
    );
}

// ---------------------------------------------------------------------------
// Rare-attribute box accounting
//
// `Cell` keeps hyperlink ids, grapheme extras, and non-default underline
// metadata behind `Option<Box<FatAttributes>>`. Row capacity accounting
// multiplies by `size_of::<Cell>()`, which counts the pointer slot and not the
// allocation behind it.
// ---------------------------------------------------------------------------

/// Linked cells must move the reported figure.
///
/// Before this was counted, filling a screen with OSC 8 links moved
/// `retained_amount().bytes` by **exactly zero** while allocating 76,800 bytes.
#[test]
fn linking_cells_moves_the_retained_figure() {
    let mut grid = Grid::new(80, 24);
    let plain = grid.retained_amount().bytes;

    let mut linked = 0usize;
    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char_linked(
                'x',
                Color::Default,
                Color::Default,
                CellFlags::empty(),
                Some(HyperlinkId(1)),
            );
            linked += 1;
        }
    }

    let after = grid.retained_amount().bytes;
    let delta = after.saturating_sub(plain);
    let fat = std::mem::size_of::<sonicterm_types::FatAttributes>();

    assert!(
        delta > 0,
        "linking {linked} cells allocated {} bytes and moved the reported figure by 0",
        linked * fat
    );
    // Cluster storage may collapse identical runs, so the figure is bounded
    // above by one box per cell rather than equal to it. Assert the shape,
    // not an exact count that storage form would make brittle.
    assert!(
        delta <= linked * fat,
        "reported {delta} bytes for at most {} of boxes — over-counting suggests \
         logical columns are being counted instead of stored cells",
        linked * fat
    );
}

/// Grapheme extras allocate the same box with no hyperlink involved.
///
/// Worth pinning separately: the excuse for excluding this figure was that the
/// hyperlink registry meters it. The registry meters URI *strings*. A cell
/// carrying only combining marks has no registry entry at all, so nothing else
/// in the process accounts for its box.
#[test]
fn grapheme_extras_are_counted_though_no_hyperlink_exists() {
    let mut grid = Grid::new(80, 24);
    let plain = grid.retained_amount().bytes;

    // A base character followed by combining marks lands in `extras`.
    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char('e', Color::Default, Color::Default, CellFlags::empty());
            grid.put_char('\u{0301}', Color::Default, Color::Default, CellFlags::empty());
        }
    }

    let after = grid.retained_amount().bytes;
    assert!(
        after > plain,
        "combining marks allocate a rare-attribute box with no hyperlink and no \
         registry entry; nothing else in the process meters it"
    );
}

/// Clearing a linked cell must return the bytes.
///
/// A figure that only rises is a figure that will eventually read as a leak
/// whether or not one exists.
#[test]
fn clearing_linked_cells_returns_their_bytes() {
    let mut grid = Grid::new(80, 24);
    let plain = grid.retained_amount().bytes;

    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char_linked(
                'x',
                Color::Default,
                Color::Default,
                CellFlags::empty(),
                Some(HyperlinkId(1)),
            );
        }
    }
    let linked = grid.retained_amount().bytes;
    assert!(linked > plain, "precondition: linking raised the figure");

    // Overwrite every cell with plain content.
    grid.cursor = Pos { row: 0, col: 0 };
    for _ in 0..24 {
        for _ in 0..80 {
            grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
        }
    }

    let cleared = grid.retained_amount().bytes;
    assert!(
        cleared < linked,
        "overwriting linked cells with plain ones must return their boxes: \
         {cleared} vs {linked}"
    );
}

// ---------------------------------------------------------------------------
// Adjacent-resize capacity reclamation
//
// Growing a row by one column doubles its `Vec`; shrinking back truncates the
// length and keeps the capacity. Per row that excess is trivial. In aggregate
// over a populated scrollback it is not.
// ---------------------------------------------------------------------------

/// The regions must sum to the total, in every state the grid can be in.
///
/// This is what makes splitting the charge safe: if the parts did not sum to
/// the whole, attributing them to separate classes would silently change how
/// much is charged, not just where. A governor would then be reading a
/// different number than the one every existing test pins.
#[test]
fn region_amounts_sum_to_the_retained_total() {
    let mut grid = Grid::new(80, 24);

    let states: [(&str, fn(&mut Grid)); 5] = [
        ("empty", |_| {}),
        ("visible content", |target| {
            for _ in 0..20 {
                for _ in 0..70 {
                    target.put_char('x', Color::Default, Color::Default, CellFlags::empty());
                }
                target.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
            }
        }),
        ("populated scrollback", |target| {
            for _ in 0..500 {
                for _ in 0..70 {
                    target.put_char('y', Color::Default, Color::Default, CellFlags::empty());
                }
                target.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
            }
        }),
        ("alternate screen active", |target| target.enter_alt_screen()),
        ("back to primary", |target| target.leave_alt_screen()),
    ];

    for (name, step) in states {
        step(&mut grid);
        let regions = grid.retained_amount_by_region();
        let total = grid.retained_amount();
        assert_eq!(
            regions.total(),
            total,
            "regions must sum to the total in state '{name}': \
             visible={:?} history={:?} alternate={:?}",
            regions.visible,
            regions.history,
            regions.alternate
        );
    }
}

/// Entering an alternate screen must move bytes into the alternate region,
/// not merely leave them in history.
///
/// This is the attribution the split exists for: the saved primary is what an
/// operator would want to see separated, because it is memory held for a
/// screen the user is not currently looking at.
#[test]
fn entering_an_alternate_screen_attributes_the_saved_primary_separately() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..500 {
        for _ in 0..70 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }

    let before = grid.retained_amount_by_region();
    assert_eq!(before.alternate, ResourceAmount::default(), "no alternate screen yet");
    assert!(before.history.bytes > 0, "precondition: history holds the scrollback");

    grid.enter_alt_screen();
    let during = grid.retained_amount_by_region();

    assert!(
        during.alternate.bytes > 0,
        "the saved primary must be attributed to the alternate region, not left in history"
    );
    assert!(
        during.history.bytes < before.history.bytes,
        "and history must no longer be carrying it: {} vs {}",
        during.history.bytes,
        before.history.bytes
    );

    grid.leave_alt_screen();
    let after = grid.retained_amount_by_region();
    assert_eq!(after.alternate, ResourceAmount::default(), "leaving must clear the region");
}

/// Row containers are counted, not only the cells they point at.
///
/// A `VecDeque<Line>` reserves `Line` headers independently of the cell
/// storage each points at, and nothing else in the process counts them. The
/// figure is small — 0.68% on a full 200×50 grid — and is counted for the same
/// reason the rare-attribute boxes are: "small" was the answer there too,
/// before measurement made it 1.67× the reported total.
#[test]
fn row_containers_are_counted_in_the_retained_figure() {
    let mut grid = Grid::new(80, 24);
    let empty = grid.retained_amount().bytes;

    // Grow the scrollback so the deque reserves materially more slots.
    for _ in 0..2_000 {
        for _ in 0..70 {
            grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    let full = grid.retained_amount().bytes;

    let line = std::mem::size_of::<Line>();
    let container = (grid.visible.capacity() + grid.scrollback.capacity()) * line
        + grid.dirty_rows.capacity() * std::mem::size_of::<bool>()
        + grid.dirty_stamps.capacity() * std::mem::size_of::<u64>()
        + grid.row_content_seq.capacity() * std::mem::size_of::<u64>();
    assert!(container > 0, "precondition: the deques reserved slots");

    let cells: usize =
        grid.rows_iter().chain(grid.scrollback_iter()).map(Line::approx_capacity_byte_size).sum();

    assert!(
        full > cells,
        "the reported figure must exceed cell storage alone; containers and boxes are \
         grid-owned and counted by nothing else"
    );
    assert!(full > empty, "filling the grid must move the figure");
}

/// Adding the container term must not break the region split.
///
/// The three regions sum to the total, and that property is what lets them be
/// charged to separate classes. A term added to the total and not to a region
/// would silently change how much is charged.
#[test]
fn container_bytes_keep_the_region_split_exact() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..300 {
        for _ in 0..70 {
            grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    grid.enter_alt_screen();

    assert_eq!(
        grid.retained_amount_by_region().total(),
        grid.retained_amount(),
        "regions must still sum to the total with containers counted"
    );
}

/// The saved primary's own containers are counted, not just its rows.
///
/// Entering an alternate screen boxes the whole primary `Grid`: its two deque
/// spines, dirty bitset, row-content stamps, prompt ring, and the struct itself.
/// Counting only the rows leaves the rest held but unreported.
///
/// Pinned here rather than in the counting-allocator suite because the term is
/// fixed-size — measured at 232 bytes — and the allocations the test harness
/// makes inside a measurement window are larger than that. At this level no
/// allocator is involved and the figure is exact.
#[test]
fn entering_an_alternate_screen_counts_the_saved_primarys_containers() {
    let mut grid = Grid::new(80, 24);
    for _ in 0..300 {
        for _ in 0..70 {
            grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
        }
        grid.put_char('\n', Color::Default, Color::Default, CellFlags::empty());
    }
    grid.enter_alt_screen();

    let saved = grid.alt_screen.as_ref().expect("the alternate screen holds the saved primary");
    let saved_rows = saved.visible.capacity().saturating_add(saved.scrollback.capacity())
        * std::mem::size_of::<Line>();
    let saved_dirty = saved.dirty_rows.capacity() * std::mem::size_of::<bool>()
        + saved.dirty_stamps.capacity() * std::mem::size_of::<u64>();
    let saved_content_stamps = saved.row_content_seq.capacity() * std::mem::size_of::<u64>();
    let saved_prompts = saved.prompts.capacity() * std::mem::size_of::<PromptRegion>();
    let expected_saved = saved_rows
        + saved_dirty
        + saved_content_stamps
        + saved_prompts
        + std::mem::size_of::<Grid>();

    let live_rows = grid.visible.capacity().saturating_add(grid.scrollback.capacity())
        * std::mem::size_of::<Line>();
    let live_dirty = grid.dirty_rows.capacity() * std::mem::size_of::<bool>()
        + grid.dirty_stamps.capacity() * std::mem::size_of::<u64>();
    let live_content_stamps = grid.row_content_seq.capacity() * std::mem::size_of::<u64>();

    assert!(
        expected_saved > saved_rows,
        "precondition: the non-row terms must be non-zero or this asserts nothing"
    );
    assert_eq!(
        grid.container_bytes(),
        live_rows + live_dirty + live_content_stamps + expected_saved,
        "the saved primary's spines, dirty bitset, content stamps, prompt ring and struct are \
         all memory held while the alternate screen shows, and all must be counted"
    );
}

/// Grapheme extras move the reported figure.
///
/// A cell's trailing zero-width codepoints live in a `Box<str>` behind the
/// rare-attribute box. A figure built from `size_of::<FatAttributes>()` alone
/// reports a screen of accented text identically to a screen of plain text,
/// while the accented one holds a separate allocation per cell.
#[test]
fn grapheme_extras_move_the_reported_figure() {
    fn screen_with_marks(marks: usize) -> usize {
        let mut grid = Grid::new(80, 24);
        for _ in 0..24 {
            for _ in 0..80 {
                grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
                for _ in 0..marks {
                    // U+0301 COMBINING ACUTE ACCENT: zero width, 2 bytes UTF-8.
                    grid.put_char('\u{0301}', Color::Default, Color::Default, CellFlags::empty());
                }
            }
        }
        grid.retained_amount().bytes
    }

    let plain = screen_with_marks(0);
    let light = screen_with_marks(8);
    let heavy = screen_with_marks(24);

    assert!(
        light > plain,
        "a screen of accented text holds an allocation per cell that plain text does not, \
         so it must report above it (plain {plain}, accented {light})"
    );
    assert!(
        heavy > light,
        "the figure must scale with the payload rather than being a per-cell constant \
         ({light} at 8 marks, {heavy} at 24)"
    );
}
