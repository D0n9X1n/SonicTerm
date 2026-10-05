//! Content-keyed per-row glyph cache.
//!
//! The renderer walks every emitted row, groups its cells into style runs and shapes each run
//! through the font stack and the glyph atlas. Shaping is the expensive part of a frame, and most
//! rows a frame emits were shaped before: a scroll moves every row one slot, a selection drag or
//! a focus change redraws rows whose text is unchanged.
//!
//! **Key.** An entry is keyed by `(pane, content key)`. The content key hashes every cell of the
//! row in order, the column count, the style revision, the cell size, the baseline, the raster
//! size, whether the software presenter draws the frame, and the active hover fragment of the
//! row. It folds no position: not the absolute row, the viewport slot, the pane origin, the
//! surface extent or the selection. A row that moves keeps its key, so the cache hits wherever
//! it is drawn. The hash is SipHash with keys drawn once per cache from a [`RandomState`], so a
//! colliding row cannot be built offline; a 64-bit collision is still possible, and is the one
//! case this probabilistic cache can replay a wrong row. Key 0 is reserved for an empty slot.
//!
//! **Records.** A cached row holds [`RowGlyph`] records positioned in the row's cell grid, not
//! on the surface: atlas region, colour, raster and shaping offsets kept apart, raster size, the
//! columns the glyph spans and a packed kind. The renderer projects them onto the surface at the
//! row's current slot and origin, on a hit and on a miss alike, so both paths draw the same
//! bytes. Underlines are column runs and tofu boxes are column records, projected the same way.
//!
//! **Validity.** Each entry records the atlas content identity its UVs belong to, and a lookup
//! with another identity misses. A row containing software-presenter block glyphs is accepted
//! only when the caller's validator confirms each block still rasterizes at its stored size at
//! the current position. Only complete rows are admitted: the renderer withholds a row in which
//! any glyph was refused by the atlas, any block drew nothing, or any run failed to shape.
//!
//! **Pin, stage, commit.** Each assembly pass calls [`RowGlyphCache::begin_frame`] once with the
//! panes it draws, then per pane [`RowGlyphCache::pin`] with the keys of every row it will emit,
//! before its first admission; eviction never removes a pinned key. Each emitted slot's key is
//! staged with [`RowGlyphCache::stage_slot`] and becomes the slot's committed key only through
//! [`RowGlyphCache::commit_slots`] when the frame presents; any other outcome calls
//! [`RowGlyphCache::discard_staged`]. Committed keys are pinned on later passes, so a row still
//! on screen keeps its entry.
//!
//! **Bounds.** A pane holds at most `4 × rows` entries and `4 × rows × cols` cells' worth of
//! payload ([`row_payload_limit`]); an admission that would exceed either evicts this pane's
//! unpinned entries, oldest first, down to three quarters of each. Payload across every pane stays within the payload budget (448 MiB in
//! production) by refusing admission, and the tables, slot and pin vectors stay within the
//! tracking budget (64 MiB) by leaving a pane untracked: it draws, and caches nothing. Both hold
//! after every public mutation, so the cache's reported storage never exceeds their sum.
//! A pane not drawn in a pass is released at its next `begin_frame`.

use sonicterm_types::{retained_hash_table_bytes, Cell, Color, ResourceAmount, UnderlineStyle};
use std::borrow::Borrow;
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};

/// sRGB-encoded RGBA of a tofu box, converted to the renderer's colour type at projection.
pub type TofuColor = [u8; 4];

/// Opaque per-pane identifier; every entry belongs to exactly one pane.
pub type PaneId = u64;

/// Production bound on the payload every pane of one renderer's cache holds together.
pub const DEFAULT_PAYLOAD_BUDGET_BYTES: usize = 448 * 1024 * 1024;

/// Production bound on the cache's tables, slot vectors and pin lists together.
pub const DEFAULT_TRACKING_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// Entries a pane may hold, per visible row; its payload quota is the same multiple of a row.
pub const QUOTA_ROWS_FACTOR: usize = 4;

/// Pinned keys a pane can need at once, per visible row: its committed slots and this pass's.
pub const PIN_ROWS_FACTOR: usize = 2;

/// Outer-table capacity kept when the drawn pane set contracts; above it a table filled to under
/// a quarter of its capacity is shrunk.
const PANE_TABLE_KEEP_CAPACITY: usize = 16;

/// Bytes one cell of a row may add to a cached row: one glyph, one underline run, one tofu box
/// and one missing character. A row above `cols` times this is never admitted.
pub const ROW_PAYLOAD_PER_CELL_BYTES: usize = std::mem::size_of::<RowGlyph>()
    + std::mem::size_of::<UnderlineRun>()
    + std::mem::size_of::<RowTofu>()
    + std::mem::size_of::<char>();

/// The largest payload one admitted row of `cols` columns may hold.
#[must_use]
pub fn row_payload_limit(cols: u16) -> usize {
    usize::from(cols).saturating_mul(ROW_PAYLOAD_PER_CELL_BYTES)
}

/// The most entries a pane of `rows` visible rows may hold.
#[must_use]
pub fn pane_entry_quota(rows: u16) -> usize {
    usize::from(rows).saturating_mul(QUOTA_ROWS_FACTOR)
}

/// The most payload a pane of `rows` by `cols` cells may hold.
#[must_use]
pub fn pane_payload_quota(rows: u16, cols: u16) -> usize {
    pane_entry_quota(rows).saturating_mul(row_payload_limit(cols))
}

/// Cell-decoration run for an underlined span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnderlineRun {
    /// Inclusive start column.
    pub start_col: u16,
    /// Inclusive end column.
    pub end_col: u16,
    /// Underline stroke style.
    pub style: UnderlineStyle,
    /// Effective underline colour. This is either the explicit SGR 58 colour
    /// or the cell foreground for default underline colour.
    pub color: Color,
}

/// How a [`RowGlyph`] is projected onto the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RowGlyphKind {
    /// An ASCII fast-path glyph: cell origin plus raster offset, nothing else.
    Natural = 0,
    /// A character-fallback glyph: natural, then shaping offset and optional marker fit.
    Fallback = 1,
    /// A shaped glyph: natural, then shaping offset and optional marker fit.
    Shaped = 2,
    /// A block-sprite glyph filling its cells; its rectangle comes from the column edges.
    Block = 3,
}

impl RowGlyphKind {
    /// Every kind, in encoding order.
    pub const ALL: [Self; 4] = [Self::Natural, Self::Fallback, Self::Shaped, Self::Block];
}

/// The decoded form of [`RowGlyph::kind_and_bits`].
///
/// Layout: bits 0-1 kind, bit 2 colour tile, bit 3 subpixel coverage, bit 4 marker-fit
/// eligibility, bit 5 wide lead cell, bit 6 cluster extras, bits 16-31 cluster cell count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowGlyphBits {
    /// Projection kind.
    pub kind: RowGlyphKind,
    /// The tile holds premultiplied colour pixels.
    pub is_color: bool,
    /// The tile holds subpixel coverage.
    pub is_subpixel: bool,
    /// Projection fits the glyph inside its cell, as standalone status markers are fitted.
    pub marker_fit_eligible: bool,
    /// The lead cell is a wide cell.
    pub is_wide: bool,
    /// The lead cell carries combining extras.
    pub has_extras: bool,
    /// Cells the shaped cluster covers.
    pub cluster_cells: u16,
}

const KIND_MASK: u32 = 0b11;
const COLOR_BIT: u32 = 1 << 2;
const SUBPIXEL_BIT: u32 = 1 << 3;
const MARKER_FIT_BIT: u32 = 1 << 4;
const WIDE_BIT: u32 = 1 << 5;
const EXTRAS_BIT: u32 = 1 << 6;
const CLUSTER_SHIFT: u32 = 16;

impl RowGlyphBits {
    /// Pack these fields into the record's `kind_and_bits` word.
    #[must_use]
    pub fn pack(self) -> u32 {
        let flag = |set: bool, bit: u32| if set { bit } else { 0 };
        u32::from(self.kind as u8)
            | flag(self.is_color, COLOR_BIT)
            | flag(self.is_subpixel, SUBPIXEL_BIT)
            | flag(self.marker_fit_eligible, MARKER_FIT_BIT)
            | flag(self.is_wide, WIDE_BIT)
            | flag(self.has_extras, EXTRAS_BIT)
            | (u32::from(self.cluster_cells) << CLUSTER_SHIFT)
    }

    /// Decode a `kind_and_bits` word written by [`Self::pack`].
    #[must_use]
    pub fn unpack(packed: u32) -> Self {
        Self {
            kind: RowGlyphKind::ALL[(packed & KIND_MASK) as usize],
            is_color: packed & COLOR_BIT != 0,
            is_subpixel: packed & SUBPIXEL_BIT != 0,
            marker_fit_eligible: packed & MARKER_FIT_BIT != 0,
            is_wide: packed & WIDE_BIT != 0,
            has_extras: packed & EXTRAS_BIT != 0,
            cluster_cells: (packed >> CLUSTER_SHIFT) as u16,
        }
    }
}

/// One cached glyph, positioned in its row's cell grid rather than on the surface.
///
/// Offsets and sizes are raster pixels. Projection adds the row's current cell origin, so the
/// same record draws correctly at any slot, origin and surface size the key does not fold.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowGlyph {
    /// Normalized atlas region `[u0, v0, u1, v1]`.
    pub uv: [f32; 4],
    /// Linear RGBA the coverage is modulated by, as the renderer emits it.
    pub color: [f32; 4],
    /// The atlas tile's raster offset; zero for blocks.
    pub raster_offset: [f32; 2],
    /// The resolved shaping offset (x with its cluster pen, and y); zero for natural and block.
    pub shape_offset: [f32; 2],
    /// The atlas tile's size; for a block, the target size it was rasterized at.
    pub raster_size: [f32; 2],
    /// The column the glyph is anchored to.
    pub lead_col: u16,
    /// For a block, the clamped column after its span; otherwise `lead_col + 1`.
    pub end_col: u16,
    /// The packed [`RowGlyphBits`].
    pub kind_and_bits: u32,
}

const _: () = assert!(std::mem::size_of::<RowGlyph>() == 64);

impl RowGlyph {
    /// The decoded kind and flags.
    #[must_use]
    pub fn bits(&self) -> RowGlyphBits {
        RowGlyphBits::unpack(self.kind_and_bits)
    }
}

/// One missing-glyph box, positioned in its row's cell grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowTofu {
    /// The column the box starts in.
    pub lead_col: u16,
    /// The inset from the cell's left and top edges, in raster pixels.
    pub inset: f32,
    /// The box width, in raster pixels.
    pub width: f32,
    /// The box height, in raster pixels.
    pub height: f32,
    /// The box colour.
    pub color: TofuColor,
}

/// One cached row's position-free render artefacts.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct CachedRow {
    /// Glyph records composing the row.
    pub glyphs: Vec<RowGlyph>,
    /// Underline runs for this row; the row is implied by where it is drawn.
    pub underlines: Vec<UnderlineRun>,
    /// Missing-glyph tofu boxes for this row.
    pub tofu: Vec<RowTofu>,
    /// Codepoints that were missing this row, published for the unicode gate.
    pub missing_chars: Vec<char>,
}

impl CachedRow {
    /// Whether the row holds any block glyph, whose software rasterization can depend on position.
    #[must_use]
    pub fn has_blocks(&self) -> bool {
        self.glyphs.iter().any(|glyph| glyph.bits().kind == RowGlyphKind::Block)
    }
}

/// The payload one cached row retains: every vector's capacity times its element size. One
/// function drives admission, the running sums and the retained report.
#[must_use]
pub fn cached_row_payload_bytes(row: &CachedRow) -> usize {
    row.glyphs
        .capacity()
        .saturating_mul(std::mem::size_of::<RowGlyph>())
        .saturating_add(
            row.underlines.capacity().saturating_mul(std::mem::size_of::<UnderlineRun>()),
        )
        .saturating_add(row.tofu.capacity().saturating_mul(std::mem::size_of::<RowTofu>()))
        .saturating_add(row.missing_chars.capacity().saturating_mul(std::mem::size_of::<char>()))
}

/// Everything besides the cells that decides a row's records.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowKeyInputs {
    /// Theme, palette and font revision.
    pub style_rev: u64,
    /// Cell width in raster pixels.
    pub cell_w: f32,
    /// Cell height in raster pixels.
    pub cell_h: f32,
    /// Baseline offset inside a cell, in raster pixels.
    pub baseline_y_in_cell: f32,
    /// Font raster size.
    pub raster_px: f32,
    /// The Windows software presenter draws the frame, which changes block rasterization.
    pub software_presenter: bool,
    /// The row's active (recoloring) hover fragment as inclusive start and end columns.
    pub hover_span: Option<(u16, u16)>,
}

/// One cached row with its validity data.
#[derive(Debug)]
struct CacheEntry {
    /// Atlas content identity the row's UVs belong to.
    atlas_identity: u64,
    /// The row holds block glyphs, so a lookup asks the caller to validate them.
    has_blocks: bool,
    /// Frame clock of the last accepted hit or insertion.
    last_used: u64,
    /// [`cached_row_payload_bytes`] of `row`, measured at admission.
    payload_bytes: usize,
    row: CachedRow,
}

/// One tracked pane: its entries, slot keys and pin list, reserved at creation.
#[derive(Debug)]
struct PaneRows {
    entries: HashMap<u64, CacheEntry>,
    /// Per slot, the key of the row the presented frame drew; 0 for none.
    committed: Vec<u64>,
    /// Per slot, the key this pass emitted; 0 for none.
    staged: Vec<u64>,
    /// Sorted keys eviction must keep this pass.
    pinned: Vec<u64>,
    rows: u16,
    cols: u16,
    payload_bytes: usize,
}

impl PaneRows {
    fn new(rows: u16, cols: u16) -> Self {
        let row_count = usize::from(rows);
        Self {
            entries: HashMap::with_capacity(pane_entry_quota(rows)),
            committed: vec![0; row_count],
            staged: vec![0; row_count],
            pinned: Vec::with_capacity(row_count.saturating_mul(PIN_ROWS_FACTOR)),
            rows,
            cols,
            payload_bytes: 0,
        }
    }

    /// Tables and vectors this pane holds besides payload, at their actual capacities.
    fn tracking_bytes(&self) -> usize {
        let slots = self
            .committed
            .capacity()
            .saturating_add(self.staged.capacity())
            .saturating_add(self.pinned.capacity());
        retained_hash_table_bytes::<u64, CacheEntry>(self.entries.capacity())
            .saturating_add(slots.saturating_mul(std::mem::size_of::<u64>()))
    }

    fn is_pinned(&self, key: u64) -> bool {
        self.pinned.binary_search(&key).is_ok()
    }

    /// When `added` more entries or `incoming` more bytes would exceed either quota, evict
    /// unpinned entries other than `keep`, oldest first by `(last_used, key)`, until they fit
    /// within three quarters of each quota or nothing evictable remains; below the quotas
    /// nothing is evicted. Survivors are drained and reinserted rather than removed in
    /// place: draining resets every slot, while removal leaves deleted markers that can make a
    /// later insertion grow the table. Returns the payload bytes freed.
    fn evict_toward_target(&mut self, keep: u64, added: usize, incoming: usize) -> usize {
        let (entry_quota, payload_quota) =
            (pane_entry_quota(self.rows), pane_payload_quota(self.rows, self.cols));
        let (mut count, mut bytes) = (self.entries.len(), self.payload_bytes);
        if count + added <= entry_quota && bytes.saturating_add(incoming) <= payload_quota {
            // When: `count + added` and `bytes + incoming` fit both quotas, nothing is evicted.
            return 0;
        }
        let (entry_target, payload_target) = (entry_quota * 3 / 4, payload_quota * 3 / 4);
        let fits = |count: usize, bytes: usize| {
            count + added <= entry_target && bytes.saturating_add(incoming) <= payload_target
        };
        let mut victims: Vec<(u64, u64, usize)> = self
            .entries
            .iter()
            .filter(|(key, _)| **key != keep && !self.is_pinned(**key))
            .map(|(key, entry)| (entry.last_used, *key, entry.payload_bytes))
            .collect();
        victims.sort_unstable();
        let mut evicted: Vec<u64> = Vec::new();
        for (_, key, size) in victims {
            if fits(count, bytes) {
                // When: `fits(count, bytes)`, the targets are met and older entries stay cached.
                break;
            }
            evicted.push(key);
            count -= 1;
            bytes -= size;
        }
        if evicted.is_empty() {
            // When: `evicted` is empty, every entry is pinned or is the key being replaced.
            return 0;
        }
        evicted.sort_unstable();
        let kept: Vec<(u64, CacheEntry)> =
            self.entries.drain().filter(|(key, _)| evicted.binary_search(key).is_err()).collect();
        for (key, entry) in kept {
            self.entries.insert(key, entry);
        }
        let freed = self.payload_bytes - bytes;
        self.payload_bytes = bytes;
        freed
    }
}

/// Per-pane, content-keyed row glyph cache with pinned slots and two enforced budgets.
#[derive(Debug)]
pub struct RowGlyphCache {
    /// Keys the content hash; drawn once per cache.
    hasher: RandomState,
    /// Ticked once per assembly pass by [`Self::begin_frame`].
    frame_clock: u64,
    payload_budget: usize,
    tracking_budget: usize,
    /// Running sum of every entry's payload.
    payload_bytes: usize,
    panes: HashMap<PaneId, PaneRows>,
}

impl Default for RowGlyphCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RowGlyphCache {
    /// An empty cache with the production budgets.
    #[must_use]
    pub fn new() -> Self {
        Self::with_budgets(DEFAULT_PAYLOAD_BUDGET_BYTES, DEFAULT_TRACKING_BUDGET_BYTES)
    }

    /// An empty cache whose payload and tracking storage are bounded by the given budgets.
    #[must_use]
    pub fn with_budgets(payload_budget: usize, tracking_budget: usize) -> Self {
        Self {
            hasher: RandomState::new(),
            frame_clock: 0,
            payload_budget,
            tracking_budget,
            payload_bytes: 0,
            panes: HashMap::new(),
        }
    }

    /// The content key of a row of `cols` cells drawn with `inputs`; never 0.
    #[must_use]
    pub fn content_key<I, C>(&self, cells: I, cols: u16, inputs: &RowKeyInputs) -> u64
    where
        I: IntoIterator<Item = C>,
        C: Borrow<Cell>,
    {
        let mut hasher = self.hasher.build_hasher();
        for cell in cells {
            cell.borrow().hash(&mut hasher);
        }
        cols.hash(&mut hasher);
        inputs.style_rev.hash(&mut hasher);
        inputs.cell_w.to_bits().hash(&mut hasher);
        inputs.cell_h.to_bits().hash(&mut hasher);
        inputs.baseline_y_in_cell.to_bits().hash(&mut hasher);
        inputs.raster_px.to_bits().hash(&mut hasher);
        inputs.software_presenter.hash(&mut hasher);
        if let Some((start_col, end_col)) = inputs.hover_span {
            // A recoloring hover fragment changes the fragment's glyph colours.
            0x55_524C_u64.hash(&mut hasher);
            start_col.hash(&mut hasher);
            end_col.hash(&mut hasher);
        }
        hasher.finish().max(1)
    }

    /// Start one assembly pass drawing `drawn` panes, each `(pane, rows, cols)`.
    ///
    /// Ticks the recency clock, releases panes not drawn and panes whose size changed, clears
    /// every pane's staged keys and pin list, and tracks each new pane when its reserved storage
    /// fits the tracking budget; a pane that does not fit stays untracked this pass.
    pub fn begin_frame(&mut self, drawn: &[(PaneId, u16, u16)]) {
        self.frame_clock = self.frame_clock.wrapping_add(1);
        let mut released = 0usize;
        self.panes.retain(|pane_id, pane| {
            let keep = drawn.iter().any(|(drawn_id, rows, cols)| {
                drawn_id == pane_id && *rows == pane.rows && *cols == pane.cols
            });
            if !keep {
                // A pane not drawn at this size has its storage released now.
                released += pane.payload_bytes;
            }
            keep
        });
        self.payload_bytes -= released;
        self.shrink_pane_table();
        for pane in self.panes.values_mut() {
            pane.staged.fill(0);
            pane.pinned.clear();
        }
        for &(pane_id, rows, cols) in drawn {
            if rows == 0 || cols == 0 || self.panes.contains_key(&pane_id) {
                // When: `rows` or `cols` is 0 nothing is cached, and a pane in `panes` is already tracked.
                continue;
            }
            self.track_pane(pane_id, rows, cols);
        }
        self.debug_assert_budgets();
    }

    /// Track `pane_id`, charged at the capacities its reservation and the outer table's growth
    /// actually produced; released at once, leaving nothing retained, when that is over budget.
    fn track_pane(&mut self, pane_id: PaneId, rows: u16, cols: u16) -> bool {
        let outer_before = self.panes.capacity();
        self.panes.insert(pane_id, PaneRows::new(rows, cols));
        if self.tracking_bytes() <= self.tracking_budget {
            // When: `tracking_bytes` fits `tracking_budget`, the reservation is kept.
            return true;
        }
        self.panes.remove(&pane_id);
        if self.panes.capacity() > outer_before {
            // Inserting grew the outer table, so it returns to its previous size.
            self.panes.shrink_to(outer_before);
        }
        false
    }

    fn shrink_pane_table(&mut self) {
        let (len, capacity) = (self.panes.len(), self.panes.capacity());
        if capacity > PANE_TABLE_KEEP_CAPACITY && len < capacity / 4 {
            // The drawn pane set shrank well below the table's allocation.
            self.panes.shrink_to((2 * len).max(PANE_TABLE_KEEP_CAPACITY));
        }
    }

    /// Pin `pane_id`'s committed slot keys and `keys`, the rows this pass will emit, before any
    /// admission; eviction keeps every pinned key. Zero keys are ignored, and at most one key per
    /// visible row is taken from `keys`.
    pub fn pin(&mut self, pane_id: PaneId, keys: &[u64]) {
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            // When: the pane is untracked, nothing it emits can be admitted, so nothing is pinned.
            return;
        };
        let limit = usize::from(pane.rows);
        pane.pinned.clear();
        pane.pinned.extend(pane.committed.iter().copied().filter(|key| *key != 0));
        pane.pinned.extend(keys.iter().copied().filter(|key| *key != 0).take(limit));
        pane.pinned.sort_unstable();
        pane.pinned.dedup();
    }

    /// Look up `pane_id`'s row with content `key` built against `atlas_identity`. A row holding
    /// block glyphs is accepted only when `validate_blocks` accepts it. An accepted hit is the
    /// entry's most recent use.
    pub fn get(
        &mut self,
        pane_id: PaneId,
        key: u64,
        atlas_identity: u64,
        validate_blocks: impl FnOnce(&CachedRow) -> bool,
    ) -> Option<&CachedRow> {
        let clock = self.frame_clock;
        let entry = self.panes.get_mut(&pane_id)?.entries.get_mut(&key)?;
        if entry.atlas_identity != atlas_identity {
            // When: `atlas_identity` differs, the row's UVs may name other glyphs.
            return None;
        }
        if entry.has_blocks && !validate_blocks(&entry.row) {
            // When: a block would rasterize at another size here, the row is shaped again.
            return None;
        }
        entry.last_used = clock;
        Some(&entry.row)
    }

    /// Admit a complete row for `pane_id` under `key`, replacing any entry with that key.
    ///
    /// Returns whether it was admitted. A row above [`row_payload_limit`] is refused; otherwise
    /// this pane's unpinned entries are evicted until its quotas hold, and the row is refused
    /// when the renderer-wide payload budget would be exceeded. A refused row is a miss next
    /// pass, never a wrong pixel.
    pub fn insert(
        &mut self,
        pane_id: PaneId,
        key: u64,
        atlas_identity: u64,
        mut row: CachedRow,
    ) -> bool {
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            // When: the pane is untracked this pass, its rows are drawn but not cached.
            return false;
        };
        row.glyphs.shrink_to_fit();
        row.underlines.shrink_to_fit();
        row.tofu.shrink_to_fit();
        row.missing_chars.shrink_to_fit();
        let incoming = cached_row_payload_bytes(&row);
        if incoming > row_payload_limit(pane.cols) {
            // When: `incoming` exceeds `row_payload_limit`, the quota proof would not hold.
            return false;
        }
        let existing = pane.entries.get(&key).map(|entry| entry.payload_bytes);
        let added = usize::from(existing.is_none());
        // Replacement is decided on `total - old + new`: the old payload leaves with the new.
        let old = existing.unwrap_or(0);
        pane.payload_bytes -= old;
        let freed = pane.evict_toward_target(key, added, incoming);
        let pane_after = pane.payload_bytes;
        pane.payload_bytes += old;
        self.payload_bytes -= freed;
        let total_after = self.payload_bytes - old;
        let pane = self.panes.get_mut(&pane_id).expect("tracked above");
        if pane.entries.len() + added > pane_entry_quota(pane.rows)
            || pane_after.saturating_add(incoming) > pane_payload_quota(pane.rows, pane.cols)
            || total_after.saturating_add(incoming) > self.payload_budget
        {
            // When: an entry quota, `pane_payload_quota` or `payload_budget` would be passed, refuse.
            self.debug_assert_budgets();
            return false;
        }
        let capacity_before = pane.entries.capacity();
        let entry = CacheEntry {
            atlas_identity,
            has_blocks: row.has_blocks(),
            last_used: self.frame_clock,
            payload_bytes: incoming,
            row,
        };
        if let Some(slot) = pane.entries.get_mut(&key) {
            // A cached key is replaced in place, leaving no deleted marker.
            *slot = entry;
        } else {
            // When: the key is new, the reserved table has room by the entry quota.
            pane.entries.insert(key, entry);
        }
        pane.payload_bytes = pane_after + incoming;
        self.payload_bytes = total_after + incoming;
        if pane.entries.capacity() != capacity_before {
            // The table grew after its reservation, an invariant broke; stop tracking the pane.
            debug_assert!(false, "a pane's row table grew after its reservation");
            self.untrack(pane_id);
        }
        self.debug_assert_budgets();
        true
    }

    /// Record that this pass emitted the row with `key` at `slot` of `pane_id`.
    pub fn stage_slot(&mut self, pane_id: PaneId, slot: u16, key: u64) {
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            if let Some(staged) = pane.staged.get_mut(usize::from(slot)) {
                *staged = key;
            }
        }
    }

    /// The pass presented: each slot it emitted now shows its staged key. Slots it did not emit
    /// keep their committed key, as their pixels stay on screen.
    pub fn commit_slots(&mut self) {
        for pane in self.panes.values_mut() {
            for (committed, staged) in pane.committed.iter_mut().zip(pane.staged.iter_mut()) {
                if *staged != 0 {
                    // The slot was emitted this pass, so its staged key replaces the old one.
                    *committed = std::mem::take(staged);
                }
            }
        }
    }

    /// The pass did not present: forget its staged keys and keep the committed ones.
    pub fn discard_staged(&mut self) {
        for pane in self.panes.values_mut() {
            pane.staged.fill(0);
        }
    }

    /// Drop every cached row. Called on font, theme, scale, resize and atlas rebuild events,
    /// anything that invalidates UVs or colours across the whole grid. Panes stay tracked;
    /// committed keys that now name no entry pin nothing.
    pub fn invalidate_all(&mut self) {
        for pane in self.panes.values_mut() {
            pane.entries.clear();
            pane.payload_bytes = 0;
        }
        self.payload_bytes = 0;
        self.debug_assert_budgets();
    }

    /// Release one pane: its rows, its committed and staged slots and its pin list.
    pub fn invalidate_pane(&mut self, pane_id: PaneId) {
        self.untrack(pane_id);
        self.shrink_pane_table();
        self.debug_assert_budgets();
    }

    fn untrack(&mut self, pane_id: PaneId) {
        if let Some(pane) = self.panes.remove(&pane_id) {
            self.payload_bytes -= pane.payload_bytes;
        }
    }

    /// Retained payload plus tracking storage, and the number of cached rows.
    #[must_use]
    pub fn retained_amount(&self) -> ResourceAmount {
        ResourceAmount {
            bytes: self.payload_bytes.saturating_add(self.tracking_bytes()),
            items: self.len(),
        }
    }

    /// Payload every cached row retains, by [`cached_row_payload_bytes`].
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }

    /// Tables, slot vectors and pin lists, at their actual capacities.
    #[must_use]
    pub fn tracking_bytes(&self) -> usize {
        self.panes.values().fold(
            retained_hash_table_bytes::<PaneId, PaneRows>(self.panes.capacity()),
            |total, pane| total.saturating_add(pane.tracking_bytes()),
        )
    }

    /// The renderer-wide payload bound.
    #[must_use]
    pub fn payload_budget(&self) -> usize {
        self.payload_budget
    }

    /// The renderer-wide tracking bound.
    #[must_use]
    pub fn tracking_budget(&self) -> usize {
        self.tracking_budget
    }

    /// Number of cached rows across every pane.
    #[must_use]
    pub fn len(&self) -> usize {
        self.panes.values().map(|pane| pane.entries.len()).sum()
    }

    /// True when no rows are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether `pane_id` is tracked this pass; an untracked pane draws but caches nothing.
    #[must_use]
    pub fn is_tracked(&self, pane_id: PaneId) -> bool {
        self.panes.contains_key(&pane_id)
    }

    /// Rows cached for `pane_id`, or `None` when it is untracked.
    #[must_use]
    pub fn pane_len(&self, pane_id: PaneId) -> Option<usize> {
        self.panes.get(&pane_id).map(|pane| pane.entries.len())
    }

    /// Payload cached for `pane_id`, or `None` when it is untracked.
    #[must_use]
    pub fn pane_payload_bytes(&self, pane_id: PaneId) -> Option<usize> {
        self.panes.get(&pane_id).map(|pane| pane.payload_bytes)
    }

    /// Whether `pane_id` caches a row under `key`, whatever its atlas identity.
    #[must_use]
    pub fn contains(&self, pane_id: PaneId, key: u64) -> bool {
        self.panes.get(&pane_id).is_some_and(|pane| pane.entries.contains_key(&key))
    }

    /// The committed key of `slot` of `pane_id`; 0 when the slot shows no committed row.
    #[must_use]
    pub fn committed_slot(&self, pane_id: PaneId, slot: u16) -> Option<u64> {
        self.panes.get(&pane_id)?.committed.get(usize::from(slot)).copied()
    }

    /// The key this pass staged for `slot` of `pane_id`; 0 when none.
    #[must_use]
    pub fn staged_slot(&self, pane_id: PaneId, slot: u16) -> Option<u64> {
        self.panes.get(&pane_id)?.staged.get(usize::from(slot)).copied()
    }

    /// Payload recomputed from every entry, for checking the running sums.
    #[must_use]
    pub fn folded_payload_bytes(&self) -> usize {
        self.panes
            .values()
            .flat_map(|pane| pane.entries.values())
            .map(|entry| cached_row_payload_bytes(&entry.row))
            .sum()
    }

    fn debug_assert_budgets(&self) {
        debug_assert!(self.payload_bytes <= self.payload_budget, "payload over its budget");
        debug_assert!(self.tracking_bytes() <= self.tracking_budget, "tracking over its budget");
    }
}

#[cfg(test)]
#[path = "row_glyph_cache_tests.rs"]
mod row_glyph_cache_tests;
