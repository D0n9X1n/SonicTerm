use super::*;

impl Grid {
    /// Bytes retained by cell storage across primary and alternate screens.
    ///
    /// Counts reserved capacity rather than live length, because capacity is
    /// what the allocator is holding. The same figure drives
    /// [`Self::enforce_retained_capacity_budget`], so what the grid enforces
    /// and what it can shrink cannot drift apart.
    ///
    /// This is *not* the whole of what the grid retains — see
    /// [`Self::fat_attribute_bytes`] for the per-cell boxes it excludes, and
    /// [`Self::retained_amount`] for the total a governor is charged.
    fn retained_cell_bytes(&self) -> usize {
        let primary_bytes = self
            .visible
            .iter()
            .chain(self.scrollback.iter())
            .map(Line::approx_capacity_byte_size)
            .sum::<usize>();
        let saved_primary_bytes = self.alt_screen.as_ref().map_or(0, |primary| {
            primary
                .visible
                .iter()
                .chain(primary.scrollback.iter())
                .map(Line::approx_capacity_byte_size)
                .sum::<usize>()
        });
        primary_bytes.saturating_add(saved_primary_bytes)
    }

    /// Heap bytes held by cells' rare-attribute boxes across every screen.
    ///
    /// [`Self::retained_cell_bytes`] multiplies row capacity by
    /// `size_of::<Cell>()`, which counts the 8-byte `Option<Box<…>>` slot but
    /// not the 40-byte allocation behind it. A row of linked cells therefore
    /// under-reports by more than it reports: measured at **1.67×** on a
    /// populated scrollback of OSC 8 output — 1.85 MiB reported against 3.09
    /// MiB actually held.
    ///
    /// Kept separate from `retained_cell_bytes` because the two have different
    /// costs and different callers. That function is O(1) per row and runs on
    /// alt-screen and resize transitions; this one walks every stored cell,
    /// measured at 48 µs against 22 ns on a 1050-row grid — 2098×. Acceptable
    /// on the retention sampling timer, not on a transition the user drives by
    /// launching `vim`.
    fn fat_attribute_bytes(&self) -> usize {
        let primary = self
            .visible
            .iter()
            .chain(self.scrollback.iter())
            .map(Line::fat_attribute_bytes)
            .sum::<usize>();
        let saved = self.alt_screen.as_ref().map_or(0, |screen| {
            screen
                .visible
                .iter()
                .chain(screen.scrollback.iter())
                .map(Line::fat_attribute_bytes)
                .sum::<usize>()
        });
        primary.saturating_add(saved)
    }

    /// Bytes held by the row containers themselves, rather than by cells.
    ///
    /// A `VecDeque<Line>` reserves slots for its `Line` headers independently
    /// of the cell storage each header points at. `dirty_rows` reserves one flag
    /// per visible row, and `row_content_seq` one `u64` change stamp. None is
    /// counted by cell accounting, and nothing else in the process counts them.
    ///
    /// The saved primary behind an alternate screen is a whole `Box<Grid>`: its
    /// deque spines, its own dirty bitset, its prompt ring, and the `Grid`
    /// struct itself. Counting only its rows leaves the rest uncounted, which
    /// measured as a 232-byte shortfall while an alternate screen was active —
    /// fixed rather than proportional, and counted for the same reason the rest
    /// is.
    ///
    /// Measured at 33 KiB against 4.9 MiB of cells on a 200x50 grid with full
    /// scrollback — 0.68%. Counted anyway: "small" was also the answer for the
    /// rare-attribute boxes before they were measured at 1.67x the figure that
    /// was being reported. An unmeasured term is not a small term, it is an
    /// unknown one.
    pub(super) fn container_bytes(&self) -> usize {
        let line = std::mem::size_of::<Line>();
        let prompt = std::mem::size_of::<PromptRegion>();
        let primary = self.visible.capacity().saturating_add(self.scrollback.capacity()) * line;
        let saved = self.alt_screen.as_ref().map_or(0, |screen| {
            let rows =
                screen.visible.capacity().saturating_add(screen.scrollback.capacity()) * line;
            let dirty = screen.dirty_rows.capacity() * std::mem::size_of::<bool>()
                + screen.dirty_stamps.capacity() * std::mem::size_of::<u64>();
            let content_stamps = screen.row_content_seq.capacity() * std::mem::size_of::<u64>();
            let prompts = screen.prompts.capacity() * prompt;
            // The `Box<Grid>` holding it all.
            rows.saturating_add(dirty)
                .saturating_add(content_stamps)
                .saturating_add(prompts)
                .saturating_add(std::mem::size_of::<Grid>())
        });
        let dirty = self.dirty_rows.capacity() * std::mem::size_of::<bool>()
            + self.dirty_stamps.capacity() * std::mem::size_of::<u64>();
        let content_stamps = self.row_content_seq.capacity() * std::mem::size_of::<u64>();
        primary.saturating_add(saved).saturating_add(dirty).saturating_add(content_stamps)
    }

    /// Rows retained across primary and alternate screens.
    ///
    /// One row container is the item unit for grid accounting: a cell is far
    /// too fine to reserve against, and the row is what actually gets
    /// allocated, trimmed, and compacted.
    fn retained_rows(&self) -> usize {
        let primary = self.visible.len().saturating_add(self.scrollback.len());
        let saved = self
            .alt_screen
            .as_ref()
            .map_or(0, |screen| screen.visible.len().saturating_add(screen.scrollback.len()));
        primary.saturating_add(saved)
    }

    /// Storage this grid is holding, for governor accounting and telemetry.
    ///
    /// Bytes cover visible, history, saved-primary and alternate cell storage
    /// including reserved row capacity, the heap allocations behind cells'
    /// rare-attribute boxes, and retained prompt regions. Items count retained
    /// row containers.
    ///
    /// Hyperlink *targets* are deliberately excluded: the registry that owns
    /// the URI strings meters and bounds them separately, so counting them
    /// here would charge them twice. The per-cell `Box<FatAttributes>` is not
    /// registry-owned and is counted — it is grid storage that happens to hold
    /// a link id, and it is also what grapheme extras and non-default
    /// underline metadata allocate, neither of which the registry knows about.
    ///
    /// **Deliberately larger than the figure
    /// `Self::enforce_retained_capacity_budget` acts on.** Enforcement can
    /// only shrink what `shrink_capacity_to_fit` reaches, which is row
    /// capacity; a rare-attribute box is freed by clearing the cell, not by
    /// compacting the row. Reporting the enforceable figure instead would make
    /// the governor's charge smaller than the memory actually held, which is
    /// the direction that lets a process past its ceiling. The two numbers
    /// answer different questions and are allowed to differ.
    #[must_use]
    pub fn retained_amount(&self) -> ResourceAmount {
        let prompt_bytes = self.prompts.capacity() * std::mem::size_of::<PromptRegion>();
        ResourceAmount {
            bytes: self
                .retained_cell_bytes()
                .saturating_add(prompt_bytes)
                .saturating_add(self.fat_attribute_bytes())
                .saturating_add(self.container_bytes()),
            items: self.retained_rows(),
        }
    }

    /// Storage split by the region that holds it.
    ///
    /// [`Self::retained_amount`] returns one figure for the whole grid, which
    /// is what a governor charges but not what an operator can act on: a pane
    /// holding 14 MiB of scrollback and one holding 14 MiB in a saved primary
    /// behind an alternate screen call for different responses, and the total
    /// cannot tell them apart.
    ///
    /// The three parts sum to [`Self::retained_amount`] exactly, so charging
    /// them to separate classes and charging the total are the same charge —
    /// which is the property that lets attribution improve without the
    /// aggregate moving.
    ///
    /// Rare-attribute boxes follow the rows that own them, and live prompt
    /// regions are attributed to `visible`. Container overhead—including the
    /// saved primary's prompt-ring capacity while the alternate screen is
    /// active—is also assigned to `visible` so the three parts sum exactly.
    #[must_use]
    pub fn retained_amount_by_region(&self) -> GridRegionAmounts {
        // Container bytes ride with the visible region rather than being split
        // across the three: the deques are one allocation each, not divisible
        // by which rows they hold, and attributing a shared allocation to the
        // parts that share it would be inventing a split the allocator does
        // not have.
        let visible = ResourceAmount {
            bytes: self.visible.iter().map(Line::approx_capacity_byte_size).sum::<usize>()
                + self.visible.iter().map(Line::fat_attribute_bytes).sum::<usize>()
                + self.prompts.capacity() * std::mem::size_of::<PromptRegion>()
                + self.container_bytes(),
            items: self.visible.len(),
        };
        let history = ResourceAmount {
            bytes: self.scrollback.iter().map(Line::approx_capacity_byte_size).sum::<usize>()
                + self.scrollback.iter().map(Line::fat_attribute_bytes).sum::<usize>(),
            items: self.scrollback.len(),
        };
        let alternate = self.alt_screen.as_ref().map_or_else(ResourceAmount::default, |saved| {
            let rows = saved.visible.iter().chain(saved.scrollback.iter());
            ResourceAmount {
                bytes: rows
                    .clone()
                    .map(Line::approx_capacity_byte_size)
                    .sum::<usize>()
                    .saturating_add(rows.map(Line::fat_attribute_bytes).sum::<usize>()),
                items: saved.visible.len().saturating_add(saved.scrollback.len()),
            }
        });

        GridRegionAmounts { visible, history, alternate }
    }

    pub(super) fn enforce_retained_capacity_budget(&mut self) {
        let visible_bytes = self.visible.iter().map(Line::approx_capacity_byte_size).sum::<usize>();
        let visible_budget_bytes = MAX_VISIBLE_GRID_CELLS as usize * std::mem::size_of::<Cell>();
        if visible_bytes > visible_budget_bytes {
            for row in &mut self.visible {
                row.shrink_capacity_to_fit();
            }
        }
        // Compare the figure this grid *reports*, not a subset of it.
        //
        // Enforcement used `retained_cell_bytes()`, which excludes the
        // rare-attribute boxes and row containers that `retained_amount()` —
        // the figure the governor is charged — includes. Two numbers for one
        // grid, and the smaller one held the budget.
        //
        // Measured against a counting allocator with `scrollback` raised, which
        // a user config reaches directly: 35 MiB held against a 24 MiB budget
        // at one column with hyperlinks, 63 MiB at eighty. Reported tracked
        // held exactly, so the reporting was honest and the comparison was not.
        let budget_bytes = MAX_GRID_CELLS as usize * std::mem::size_of::<Cell>();
        if self.retained_amount().bytes <= budget_bytes && !self.capacity_excess_is_material() {
            // When: `retained_amount().bytes <= budget_bytes` and excess is immaterial, skip compaction.
            return;
        }
        for row in &mut self.visible {
            row.shrink_capacity_to_fit();
        }
        for row in &mut self.scrollback {
            row.shrink_capacity_to_fit();
        }
        // Compaction reclaims capacity slack. When the overage is not slack it
        // reclaims nothing, and the check above would fire on every mutation
        // while the grid stayed over budget.
        //
        // Measured at 80 columns with every cell linked: 63 MiB retained, of
        // which cell capacity is 23 MiB — inside the budget — with **0 KiB of
        // reclaimable slack**. The other 40 MiB is rare-attribute boxes and
        // deque spine, neither of which `shrink_capacity_to_fit` can touch.
        //
        // So the budget has to be enforced against the term that can actually
        // shrink: scrollback rows. `bounded_scrollback_rows` bounds them by
        // cell count alone, which understates a linked row by up to 2.7x, so
        // rows are dropped here until what the grid *reports* fits.
        self.trim_scrollback_to_reported_budget(budget_bytes);

        if let Some(primary) = self.alt_screen.as_mut() {
            for row in &mut primary.visible {
                row.shrink_capacity_to_fit();
            }
            for row in &mut primary.scrollback {
                row.shrink_capacity_to_fit();
            }
        }
    }

    /// Drop the oldest scrollback until the reported figure fits the budget.
    ///
    /// The last resort in [`Self::enforce_retained_capacity_budget`], and the
    /// only remedy that works when the overage is not capacity slack.
    ///
    /// Scrollback is the right term to cut: the visible screen is what the user
    /// is looking at, the saved primary belongs to a program that will exit,
    /// and history is the one part that is both large and already understood to
    /// be finite. `bounded_scrollback_rows` already bounds it — by cell count,
    /// which understates a row carrying rare-attribute boxes by up to 2.7x.
    ///
    /// Drops in blocks rather than one row at a time: each check walks every
    /// row to recompute the figure, so per-row trimming would be quadratic in a
    /// large scrollback.
    pub(super) fn trim_scrollback_to_reported_budget(&mut self, budget_bytes: usize) {
        // A grid with no history cannot trim, and one already inside the budget
        // has nothing to do.
        if self.scrollback.is_empty() || self.retained_amount().bytes <= budget_bytes {
            // When: `self.scrollback.is_empty()` or retained bytes fit `budget_bytes`, skip trimming.
            return;
        }

        const TRIM_BLOCK_ROWS: usize = 64;
        while !self.scrollback.is_empty() && self.retained_amount().bytes > budget_bytes {
            let removed = TRIM_BLOCK_ROWS.min(self.scrollback.len());
            drop(self.drain_scrollback_prefix(removed));
            self.budget_trimmed_rows = self.budget_trimmed_rows.saturating_add(removed as u64);
        }
        compact_scrollback_capacity(&mut self.scrollback);
    }

    /// `true` when rows are holding materially more capacity than they use.
    ///
    /// The absolute ceiling above is not reached by ordinary sessions — a grid
    /// at 80×24 with full scrollback sits at roughly 0.4% of it — so on its own
    /// it never releases anything. What it misses is a *relative* excess.
    ///
    /// The mechanism, measured at the primitive: `Line::resize(81)` on an
    /// 80-cell row allocates capacity **160**, `Vec`'s amortized doubling, and
    /// `Line::resize(80)` leaves it at 160. Eighty wasted cells per row. Across
    /// a 1024-row scrollback that is **1.875 MiB retained by a ±1 column window
    /// drag** — about 8% of a pane's entire grid ceiling, from a gesture the
    /// user would not describe as doing anything. Twenty panes make it 37.5 MiB.
    ///
    /// Per row the excess is invisible: eighty spare cells is far below any
    /// sensible per-row threshold, which is why `shrink_vec_if_excessive` does
    /// not catch it. It is material only in aggregate — the same composition
    /// shape as the rest of this work, locally correct decisions summing to a
    /// wrong one.
    ///
    /// Deliberately hysteretic. The criterion is that *adjacent* resizing must
    /// avoid allocation churn, and a user dragging a window edge produces a
    /// continuous stream of ±1 resizes. Shrinking on every one would compact
    /// every row per frame of the drag. Requiring the excess to clear both a
    /// proportion of live bytes and an absolute floor lets a drag settle into
    /// steady state — measured at one change across eight adjacent steps —
    /// while a drag that ends materially smaller still returns its memory.
    fn capacity_excess_is_material(&self) -> bool {
        let live = self
            .visible
            .iter()
            .chain(self.scrollback.iter())
            .map(Line::approx_byte_size)
            .sum::<usize>();
        let reserved = self.retained_cell_bytes();
        let excess = reserved.saturating_sub(live);

        excess >= CAPACITY_EXCESS_FLOOR_BYTES && excess > live / CAPACITY_EXCESS_DIVISOR
    }
}

#[cfg(test)]
#[path = "grid_retention_tests.rs"]
mod grid_retention_tests;
