//! Browser-style tab model.

use std::{
    borrow::Cow,
    hash::{Hash, Hasher},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;

static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

impl TabId {
    /// Allocate the next process-unique tab identifier.
    // Ordering: NEXT_TAB_ID uses Relaxed; ids only need uniqueness, which the
    // atomic increment alone gives, not publication of other memory.
    pub fn next() -> Self {
        Self(NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CommandStatus {
    #[default]
    Idle,
    Running(Instant),
    Done {
        exit: Option<u8>,
        until: Instant,
    },
}

impl CommandStatus {
    /// Return the next strictly future badge transition without changing this status.
    ///
    /// Inactive running commands first show their badge at six whole seconds;
    /// active running commands have no badge deadline. Done badges expire at
    /// `until` regardless of activity. Due/past or unrepresentable deadlines
    /// return `None`, so a caller never re-arms an already elapsed transition.
    #[must_use]
    pub fn next_visual_deadline(&self, now: Instant, is_active: bool) -> Option<Instant> {
        let deadline = match self {
            Self::Running(started) if !is_active => started.checked_add(Duration::from_secs(6)),
            Self::Done { until, .. } => Some(*until),
            Self::Idle | Self::Running(_) => None,
        };
        deadline.filter(|deadline| *deadline > now)
    }

    /// Short status glyph to draw on the tab, or `None` for no badge: an
    /// ellipsis once an inactive tab's command has run past five seconds, then
    /// a tick for exit `0` and a cross for any other or unrecorded exit, each
    /// shown only until its `until` deadline passes.
    pub fn badge(self, now: Instant, is_active: bool) -> Option<&'static str> {
        match self {
            Self::Running(started) if !is_active && now.duration_since(started).as_secs() > 5 => {
                Some("…")
            }
            Self::Done { exit: Some(0), until } if now < until => Some("✓"),
            Self::Done { exit: _, until } if now < until => Some("✗"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub auto_title: String,
    pub custom_title: Option<String>,
    pub custom_color: Option<String>,
    /// Whether this tab's current Windows foreground process requires a privilege warning.
    pub foreground_privileged: bool,
    pub command: CommandStatus,
    /// Path or scheme-like icon hint ("github", "chrome", "bilibili", ...).
    /// The render layer maps this to a glyph/asset.
    pub icon_hint: Option<String>,
    /// Latest successful measurement of this tab's drawn content.
    measured: Option<ContentMeasure>,
    /// Measurement the tab bar lays out with. It trails `measured` while the
    /// bar is held, so a title change never moves a tab under the pointer.
    laid_out: Option<ContentMeasure>,
}

impl Tab {
    /// Build a tab with a freshly allocated id whose automatic and effective
    /// titles both start as `title`, carrying no user overrides.
    pub fn new(title: impl Into<String>) -> Self {
        let title = title.into();
        Self {
            id: TabId::next(),
            title: title.clone(),
            auto_title: title,
            custom_title: None,
            custom_color: None,
            foreground_privileged: false,
            command: CommandStatus::default(),
            icon_hint: None,
            measured: None,
            laid_out: None,
        }
    }

    /// Drawn width of this tab's content, in raster pixels, that the tab bar
    /// lays out with, or `None` before the renderer has measured it.
    #[must_use]
    pub fn content_width_px(&self) -> Option<f32> {
        self.laid_out.map(|measure| measure.width_px)
    }

    fn refresh_effective_title(&mut self) {
        self.title = self
            .custom_title
            .as_ref()
            .map(|custom| title_with_replaced_body(&self.auto_title, custom))
            .unwrap_or_else(|| self.auto_title.clone());
    }

    fn set_auto_title(&mut self, title: String) {
        self.auto_title = title;
        self.refresh_effective_title();
    }

    fn set_custom_title(&mut self, body: Option<String>) {
        self.custom_title = body;
        self.refresh_effective_title();
    }
}

/// What one tab draws in its title area: the command-status badge, the title
/// and the privilege marker. Measuring and drawing both build this value, so a
/// stored width always belongs to the text that is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabContent<'a> {
    /// Command-status badge (`…`, `✓` or `✗`) while it is shown.
    pub badge: Option<&'static str>,
    /// The effective title, including its `#N` index and process icon.
    pub title: &'a str,
    /// Whether the privilege marker is drawn before the text.
    pub privileged: bool,
}

impl<'a> TabContent<'a> {
    /// Describe what `tab` draws at `now`, given whether it is the active tab
    /// and whether the whole SonicTerm process runs elevated.
    #[must_use]
    pub fn of(tab: &'a Tab, now: Instant, is_active: bool, process_privileged: bool) -> Self {
        Self {
            badge: tab.command.clone().badge(now, is_active),
            title: &tab.title,
            privileged: process_privileged || tab.foreground_privileged,
        }
    }

    /// The text drawn after the privilege marker: `"{badge} {title}"` while a
    /// badge is shown, otherwise the title alone.
    #[must_use]
    pub fn display_text(&self) -> Cow<'a, str> {
        match self.badge {
            Some(badge) => Cow::Owned(format!("{badge} {}", self.title)),
            None => Cow::Borrowed(self.title),
        }
    }

    /// Identity of this content, so an unchanged tab is not measured again.
    #[must_use]
    pub fn key(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// One measurement of a tab's drawn content.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ContentMeasure {
    /// Drawn width in raster pixels.
    width_px: f32,
    /// [`TabContent::key`] of the measured content.
    content_key: u64,
    /// Identity of the font and scale the content was measured with.
    font_key: u64,
    /// Fallback epoch the content was measured in; a newer epoch may resolve a placeholder.
    fallback_epoch: u64,
}

/// What one [`TabBar::refresh_content_widths`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentWidthRefresh {
    /// Tabs whose content was shaped this pass.
    pub measured: usize,
    /// Tabs whose laid-out width changed this pass.
    pub applied: usize,
    /// Tabs whose newer measurement waits for the bar to be released.
    pub held: usize,
}

/// Each tab's laid-out width and the width limits the bar was laid out with,
/// captured before a redraw measures the bar, so a redraw whose frame does not
/// present can restore the geometry still on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct LaidOutWidths {
    widths: Vec<(TabId, Option<ContentMeasure>)>,
    limits: Option<(f32, f32)>,
}

#[derive(Debug, Default, Clone)]
pub struct TabBar {
    tabs: Vec<Tab>,
    active: usize,
    /// How many times the active tab has changed identity.
    activation: u64,
    /// Instant of the last `refresh_content_widths` pass. The renderer
    /// judges command badges at this instant, so it draws the text it measured.
    content_measured_at: Option<Instant>,
    /// `(tab_min_width, tab_max_width)` in logical pixels, as the last
    /// `refresh_content_widths` pass laid the bar out with them; `None` until then.
    laid_out_limits: Option<(f32, f32)>,
}

impl TabBar {
    /// An empty tab bar, with the active index resting at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of tabs currently in the bar.
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Whether the bar holds no tabs at all.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The tabs in left-to-right bar order.
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Index of the tab the user is currently viewing.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The tab the user is currently viewing, or `None` when the active index
    /// addresses no tab, as on an empty bar.
    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Replace the automatic title of the tab with `id`. No-op if not found.
    pub fn set_title(&mut self, id: TabId, title: impl Into<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.set_auto_title(title.into());
        }
    }

    /// Replace the automatic title of the currently-active tab. No-op if empty.
    pub fn set_active_title(&mut self, title: impl Into<String>) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.set_auto_title(title.into());
        }
    }

    /// The editable body of the active tab's title: the user's custom title
    /// when one is set, otherwise the displayed title with its `#N` index and
    /// any leading icon token stripped. `None` when no tab is active.
    pub fn active_title_body(&self) -> Option<String> {
        let tab = self.tabs.get(self.active)?;
        Some(tab.custom_title.clone().unwrap_or_else(|| title_body(&tab.title).to_string()))
    }

    /// Set or clear the active tab's custom title body. Whitespace-only input
    /// clears the override, so the tab falls back to its automatic title.
    pub fn set_active_custom_title(&mut self, body: impl Into<String>) {
        let Some(id) = self.tabs.get(self.active).map(|tab| tab.id) else {
            // When: self.active addresses no tab, so there is nothing to
            // retitle and the request is dropped.
            return;
        };
        self.set_custom_title(id, body);
    }

    /// Set or clear the custom title body of the tab carrying `id`, whether or
    /// not it is active. Whitespace-only input clears the override, so the tab
    /// falls back to its automatic title. Returns `false`, changing nothing,
    /// when no tab carries `id`.
    pub fn set_custom_title(&mut self, id: TabId, body: impl Into<String>) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            // When: no tab carries `id` because it closed or moved, report the edit as unapplied.
            return false;
        };
        let body = body.into();
        if body.trim().is_empty() {
            // When: body.trim() leaves nothing, read as "drop my override"
            // rather than as a request for a blank title.
            tab.set_custom_title(None);
            return true;
        }
        tab.set_custom_title(Some(body));
        true
    }

    /// Update one tab's foreground-process privilege warning state.
    ///
    /// Returns whether the value changed, or `false` when `index` is absent.
    pub fn set_foreground_privileged(&mut self, index: usize, privileged: bool) -> bool {
        let Some(tab) = self.tabs.get_mut(index) else {
            // When: `index` addresses no tab, no foreground state can be updated.
            return false;
        };
        if tab.foreground_privileged == privileged {
            // When: `tab.foreground_privileged == privileged`, the cached warning is already current.
            return false;
        }
        tab.foreground_privileged = privileged;
        true
    }

    /// Update the active tab's foreground-process privilege warning state.
    ///
    /// Returns whether the value changed, so callers can distinguish a fresh
    /// observation from an unchanged cached one without inspecting tab fields.
    pub fn set_active_foreground_privileged(&mut self, privileged: bool) -> bool {
        self.set_foreground_privileged(self.active, privileged)
    }

    /// Give the active tab an explicit color, replacing any previous one.
    pub fn set_active_custom_color(&mut self, color: impl Into<String>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            // When: self.active addresses no tab, so there is nothing to
            // color and the request is discarded.
            return;
        };
        tab.custom_color = Some(color.into());
    }

    /// Drop the active tab's explicit color override.
    pub fn clear_active_custom_color(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            // When: self.active addresses no tab, so no override exists to
            // clear and the call does nothing.
            return;
        };
        tab.custom_color = None;
    }

    /// Give the tab carrying `id` an explicit color, whether or not it is
    /// active. Returns `false`, changing nothing, when no tab carries `id`.
    pub fn set_custom_color(&mut self, id: TabId, color: impl Into<String>) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            // When: no tab carries `id` because it closed or moved, leave every color unchanged.
            return false;
        };
        tab.custom_color = Some(color.into());
        true
    }

    /// Drop the explicit color override of the tab carrying `id`. Returns
    /// `false`, changing nothing, when no tab carries `id`.
    pub fn clear_custom_color(&mut self, id: TabId) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            // When: no tab carries `id` because it closed or moved, there is no override to drop.
            return false;
        };
        tab.custom_color = None;
        true
    }

    /// The active tab's explicit color override, or `None` when it has none
    /// or no tab is active.
    pub fn active_custom_color(&self) -> Option<&str> {
        self.tabs.get(self.active)?.custom_color.as_deref()
    }

    /// Record the command status of the tab at `index`. No-op when `index` is
    /// out of range.
    pub fn set_command_status(&mut self, index: usize, status: CommandStatus) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.command = status;
        }
    }

    /// Return every tab whose `Done` deadline has passed by `now` to `Idle`,
    /// so stale success/failure badges stop being drawn.
    pub fn clear_expired_command_badges(&mut self, now: Instant) {
        for tab in &mut self.tabs {
            if matches!(tab.command, CommandStatus::Done { until, .. } if now >= until) {
                tab.command = CommandStatus::Idle;
            }
        }
    }

    /// Re-measure the tabs whose drawn content, font or scale changed, and
    /// store the widths the tab bar lays out with, together with the active
    /// `tab_min_width` and `tab_max_width`, which are never held.
    ///
    /// `measure` returns the drawn width of one tab's content in raster pixels,
    /// or `None` when it cannot shape the text; that tab keeps its last good
    /// width and is measured again on the next pass. `font_key` identifies the
    /// font and scale. While `hold` is set, a changed title, badge or privilege
    /// marker is measured but not laid out, so no tab moves under the pointer;
    /// a font or scale change, and a tab with no width yet, lay out at once.
    pub fn refresh_content_widths(
        &mut self,
        now: Instant,
        process_privileged: bool,
        font_key: u64,
        hold: bool,
        measure: impl FnMut(&TabContent<'_>) -> Option<f32>,
    ) -> ContentWidthRefresh {
        self.refresh_content_widths_at_epoch(now, process_privileged, font_key, 0, hold, measure)
    }

    /// [`Self::refresh_content_widths`] in fallback epoch `fallback_epoch`.
    ///
    /// A width measured in another epoch may hold a placeholder's advance, so it is measured
    /// again. Under `hold` the new width is held like a changed title; only a font or scale
    /// change lays out at once.
    pub fn refresh_content_widths_at_epoch(
        &mut self,
        now: Instant,
        process_privileged: bool,
        font_key: u64,
        fallback_epoch: u64,
        hold: bool,
        mut measure: impl FnMut(&TabContent<'_>) -> Option<f32>,
    ) -> ContentWidthRefresh {
        self.content_measured_at = Some(now);
        // The active limits lay the bar out with the widths below; a limit reload moves every
        // tab, so it is never held.
        self.laid_out_limits =
            Some((crate::tabbar_view::min_tab_width(), crate::tabbar_view::max_tab_width()));
        let active = self.active;
        let mut refresh = ContentWidthRefresh::default();
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let content = TabContent::of(tab, now, index == active, process_privileged);
            let content_key = content.key();
            let latest = if let Some(current) = tab.measured.filter(|stored| {
                stored.content_key == content_key
                    && stored.font_key == font_key
                    && stored.fallback_epoch == fallback_epoch
            }) {
                current
            } else {
                // When: no `current` measurement matches because the content, font or
                // fallback epoch changed, so the tab is shaped again before it can be laid out.
                let Some(width_px) = measure(&content) else {
                    // When: `measure` cannot shape the text, keep the last good width and
                    // measure again on the next pass.
                    continue;
                };
                refresh.measured += 1;
                let measured = ContentMeasure { width_px, content_key, font_key, fallback_epoch };
                tab.measured = Some(measured);
                measured
            };
            if tab.laid_out == Some(latest) {
                // When: `laid_out` already equals `latest`, the drawn width is current.
                continue;
            }
            let font_changed = tab.laid_out.is_none_or(|laid_out| laid_out.font_key != font_key);
            if hold && !font_changed {
                // When: `hold` is set and the font key did not change, keep the laid-out
                // width so a title change never moves a tab under the pointer.
                refresh.held += 1;
                continue;
            }
            tab.laid_out = Some(latest);
            refresh.applied += 1;
        }
        refresh
    }

    /// Whether any tab holds a newer measurement than the one it lays out
    /// with; the first frame that does not hold the bar applies it.
    #[must_use]
    pub fn has_held_content_widths(&self) -> bool {
        self.tabs.iter().any(|tab| tab.measured != tab.laid_out)
    }

    /// Each tab's laid-out width and the bar's laid-out width limits, for a
    /// redraw to restore with [`Self::restore_laid_out_widths`] when its frame
    /// does not present.
    #[must_use]
    pub fn laid_out_widths(&self) -> LaidOutWidths {
        LaidOutWidths {
            widths: self.tabs.iter().map(|tab| (tab.id, tab.laid_out)).collect(),
            limits: self.laid_out_limits,
        }
    }

    /// Restore the laid-out widths and width limits `widths` captured, so
    /// hit-testing matches the bar still on screen. A tab `widths` does not name
    /// keeps its width, and every tab keeps its newest measurement for the next
    /// pass to apply.
    pub fn restore_laid_out_widths(&mut self, widths: LaidOutWidths) {
        for (tab_id, laid_out) in widths.widths {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                tab.laid_out = laid_out;
            }
        }
        self.laid_out_limits = widths.limits;
    }

    /// `(tab_min_width, tab_max_width)` in logical pixels as the bar was last
    /// laid out with them, or `None` before its first measurement pass.
    #[must_use]
    pub fn laid_out_limits(&self) -> Option<(f32, f32)> {
        self.laid_out_limits
    }

    /// Instant of the last width measurement, or `None` before the first one.
    /// Drawing judges command badges at this instant, so a badge that appears
    /// or expires between measuring and drawing cannot outgrow its stored width.
    #[must_use]
    pub fn content_measured_at(&self) -> Option<Instant> {
        self.content_measured_at
    }

    /// How many times the active tab has changed identity.
    ///
    /// A pointer drag records this at its press, so it can tell a later tab
    /// switch, including one that came back to the same tab, from no switch.
    pub fn activation(&self) -> u64 {
        self.activation
    }

    /// Id of the active tab, if any.
    fn active_id(&self) -> Option<TabId> {
        self.active().map(|tab| tab.id)
    }

    /// Count an activation when the active tab is no longer `before`.
    fn note_activation(&mut self, before: Option<TabId>) {
        if self.active_id() != before {
            self.activation = self.activation.wrapping_add(1);
        }
    }

    /// Append `tab`, make it the active tab, renumber every `#N` prefix, and
    /// return the pushed tab's id.
    pub fn push(&mut self, tab: Tab) -> TabId {
        let before = self.active_id();
        let id = tab.id;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.note_activation(before);
        self.recompute_all_titles();
        id
    }

    /// Rewrite the `#N ` prefix of every tab's title so it matches the
    /// tab's current 1-based position in the bar. The body (icon + cwd)
    /// is preserved verbatim. This must be called after any operation
    /// that changes the tab list shape (close / insert / reorder /
    /// detach / drag-merge) so that INACTIVE tabs don't keep a stale
    /// `#N` from their previous slot — only the active tab is rebuilt
    /// from scratch each frame in the render loop.
    pub fn recompute_all_titles(&mut self) {
        for (tab_index, tab) in self.tabs.iter_mut().enumerate() {
            // Only rewrite tabs that already carry a `#N ` prefix —
            // leave raw user/system titles ("A", "Welcome", …) alone.
            let Some(body) = strip_index_prefix(&tab.auto_title) else {
                // When: strip_index_prefix finds no numeric head, so the title
                // is not position-numbered and keeps its text verbatim.
                continue;
            };
            let new_prefix = format!("#{}", tab_index + 1);
            let mut retitled = String::with_capacity(new_prefix.len() + body.len());
            retitled.push_str(&new_prefix);
            retitled.push_str(body);
            tab.set_auto_title(retitled);
        }
    }

    /// Insert `tab` at `index`, clamping to `[0, len]`. The newly-inserted
    /// tab becomes the active tab. Used by the cross-window drag-merge
    /// flow to drop a torn tab into the destination bar at the slot the
    /// user released over.
    pub fn insert(&mut self, index: usize, tab: Tab) -> TabId {
        let before = self.active_id();
        let idx = index.min(self.tabs.len());
        let id = tab.id;
        self.tabs.insert(idx, tab);
        self.active = idx;
        self.note_activation(before);
        self.recompute_all_titles();
        id
    }

    /// Remove the tab carrying `id`, keep the user on the nearest sensible
    /// neighbour, and renumber the remaining `#N` prefixes. No-op when no tab
    /// has that id.
    pub fn close(&mut self, id: TabId) {
        let before = self.active_id();
        if let Some(pos) = self.tabs.iter().position(|candidate| candidate.id == id) {
            self.tabs.remove(pos);
            // Three cases for adjusting `active` after removing `pos`:
            //  - pos < active: every index above `pos` shifts down by 1,
            //    so the originally-active tab is now at `active - 1`.
            //  - pos == active: the active tab itself was just closed.
            //    Stay at the same numeric index (which now points at the
            //    next tab to the right). Clamp below if it was the last
            //    tab in the vec.
            //  - pos > active: the active tab kept its index — no change.
            //
            // Clamping alone is not enough: it only corrects an overflowing
            // index, so closing an inactive tab to the LEFT of the active one
            // would silently move focus (close tab #0 with tab #1 active → the
            // vec shrinks so old tab #2 becomes tab #1, but `active` stays at
            // 1 and the user loses their place). The `pos < active` decrement
            // is what keeps the same tab selected.
            if pos < self.active {
                self.active -= 1;
            }
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
            self.recompute_all_titles();
        }
        self.note_activation(before);
    }

    /// Make the tab at `index` the active one. No-op when `index` is out of
    /// range, so the current selection survives a stale request.
    pub fn activate(&mut self, index: usize) {
        let before = self.active_id();
        if index < self.tabs.len() {
            self.active = index;
        }
        self.note_activation(before);
    }

    /// Move the selection one tab to the right, wrapping from the last tab
    /// round to the first. No-op on an empty bar.
    pub fn next(&mut self) {
        let before = self.active_id();
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
        self.note_activation(before);
    }

    /// Move the selection one tab to the left, wrapping from the first tab
    /// round to the last. No-op on an empty bar.
    pub fn prev(&mut self) {
        let before = self.active_id();
        if !self.tabs.is_empty() {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                // When: active is non-zero, so stepping back stays inside the
                // bar and lands on the neighbour to its left.
                self.active - 1
            };
        }
        self.note_activation(before);
    }

    /// Reorder the tab at `from` to position `to` (used by drag-reorder).
    ///
    /// Re-anchors `self.active` so that the *same* `Tab` instance remains
    /// active after the move. Handling only the `from == active` case is not
    /// enough: dragging a *non-active* tab past the active slot shifts the
    /// active `Tab` to a new index, and leaving `self.active` pinned would
    /// make the tab bar highlight and the rendered pane disagree (user sees
    /// tab `#1` selected but the pane shows tab `#2`'s grid).
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            // When: from or to falls outside the bar, or both name one slot, so
            // there is no move to make and the drag is ignored.
            return;
        }
        let moved = self.tabs.remove(from);
        self.tabs.insert(to, moved);
        self.active = if self.active == from {
            // The active tab itself was dragged → follow it.
            to
        } else if from < self.active && to >= self.active {
            // When: a tab left of active moved to its right or onto it, so the
            // active tab slides one slot left and keeps the same Tab selected.
            self.active - 1
        } else if from > self.active && to <= self.active {
            // When: a tab right of active moved to its left or onto it, so the
            // active tab slides one slot right and keeps the same Tab selected.
            self.active + 1
        } else {
            // When: from and to sit on one side of active, so the move leaves
            // the active tab's index unaffected.
            self.active
        };
        self.recompute_all_titles();
    }

    /// Pop a tab out of this bar — used to seed a new window when the user
    /// drags a tab off the bar.
    pub fn detach(&mut self, id: TabId) -> Option<Tab> {
        let before = self.active_id();
        let pos = self.tabs.iter().position(|candidate| candidate.id == id)?;
        let tab = self.tabs.remove(pos);
        if pos < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        self.note_activation(before);
        self.recompute_all_titles();
        Some(tab)
    }
}

/// Rebuild a tab title by keeping `template`'s `#N` index and any short
/// symbolic icon token that follows it, while `body` replaces the rest. When
/// `template` carries no such index prefix, `body` becomes the whole title.
pub fn title_with_replaced_body(template: &str, body: &str) -> String {
    let trimmed = template.trim();
    let Some(rest) = trimmed.strip_prefix('#') else {
        // When: trimmed opens with no '#', so there is no index to preserve and
        // body stands alone as the title.
        return body.to_string();
    };
    let Some(space) = rest.find(' ') else {
        // When: rest holds no separator, so the template is a bare index with
        // no body to keep and body replaces the whole title.
        return body.to_string();
    };
    let index = &trimmed[..space + 1];
    let after_index = trimmed[space + 1..].trim_start();
    let mut parts = after_index.splitn(2, ' ');
    let first = parts.next().unwrap_or_default();
    let rest = parts.next();
    let keep_icon = rest.is_some()
        && first.chars().count() <= 2
        && !first.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '/' || character == '~'
        });
    if keep_icon {
        format!("{index} {first} {body}")
    } else {
        // When: keep_icon is false, so first was ordinary title text rather
        // than an icon and only the index survives alongside body.
        format!("{index} {body}")
    }
}

/// Pixel slack allowed when a measured title is compared with its rect, so
/// sub-pixel layout rounding never cuts a title that fits.
pub const TITLE_FIT_TOLERANCE_PX: f32 = 0.5;

/// A tab title fitted into the width its tab draws it in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FittedTitle {
    /// The drawn text: the whole title, or a grapheme prefix followed by `…`.
    pub text: String,
    /// Drawn width of `text` in raster pixels.
    pub width_px: f32,
    /// Whether `text` was cut to fit.
    pub cut: bool,
}

/// Fit `text` into `available_px` by measured width.
///
/// `advances` are the shaped pen advances of `text` as `(cluster byte offset,
/// advance)` pairs in left-to-right order, and `ellipsis_px` is the measured
/// width of `…`. Text that fits is returned whole. Otherwise the longest
/// prefix that ends on a grapheme boundary and fits beside `…` is kept, so a
/// CJK character, an emoji sequence or a combining mark is never split, and a
/// leading `#N` index, process icon and command badge survive whenever they
/// fit beside `…`. When even `…` does not fit, nothing is drawn.
#[must_use]
pub fn fit_title_to_width(
    text: &str,
    advances: &[(usize, f32)],
    ellipsis_px: f32,
    available_px: f32,
) -> FittedTitle {
    let whole_px: f32 = advances.iter().map(|(_, advance)| advance).sum();
    if whole_px <= available_px + TITLE_FIT_TOLERANCE_PX {
        // When: `whole_px` fits `available_px`, draw the whole title without an ellipsis.
        return FittedTitle { text: text.to_string(), width_px: whole_px, cut: false };
    }
    let budget_px = available_px + TITLE_FIT_TOLERANCE_PX - ellipsis_px;
    if budget_px < 0.0 {
        // When: `budget_px` is negative, not even the ellipsis fits, so nothing is drawn.
        return FittedTitle { text: String::new(), width_px: 0.0, cut: true };
    }
    let mut kept_end = 0;
    let mut kept_px = 0.0;
    let mut prefix_px = 0.0;
    let mut cluster = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        let end = start + grapheme.len();
        while cluster < advances.len() && advances[cluster].0 < end {
            prefix_px += advances[cluster].1;
            cluster += 1;
        }
        if prefix_px > budget_px {
            // When: `prefix_px` passes `budget_px`, this grapheme no longer fits beside the
            // ellipsis, so the cut lands on its start.
            break;
        }
        kept_end = end;
        kept_px = prefix_px;
    }
    let mut kept = String::with_capacity(kept_end + '…'.len_utf8());
    kept.push_str(&text[..kept_end]);
    kept.push('…');
    FittedTitle { text: kept, width_px: kept_px + ellipsis_px, cut: true }
}

fn title_body(title: &str) -> &str {
    let trimmed = title.trim();
    let Some(rest) = trimmed.strip_prefix('#') else {
        // When: trimmed opens with no '#', so nothing was prefixed and the
        // whole title already is the body.
        return trimmed;
    };
    let Some(space) = rest.find(' ') else {
        // When: rest holds no separator, so no body was appended after the
        // index and trimmed is returned unchanged.
        return trimmed;
    };
    let after_index = trimmed[space + 1..].trim_start();
    let mut parts = after_index.splitn(2, ' ');
    let first = parts.next().unwrap_or_default();
    let rest = parts.next();
    let has_icon = rest.is_some()
        && first.chars().count() <= 2
        && !first.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '/' || character == '~'
        });
    if has_icon {
        rest.unwrap_or_default().trim_start()
    } else {
        // When: has_icon is false, so no leading glyph was split off and the
        // whole after_index span is the body.
        after_index
    }
}

/// Strip a leading `#<digits>` index prefix (if any) from a tab title,
/// returning the remaining body. Used by `recompute_all_titles` so a tab
/// can be re-prefixed with its current position without doubling up the
/// `#N`. The new wezterm-parity format places the icon directly after
/// the digits with no space (`#1{icon} body`), so we strip only the
/// `#<digits>` portion; any space (legacy bare-title fallback) is left
/// in the body verbatim.
fn strip_index_prefix(title: &str) -> Option<&str> {
    let rest = title.strip_prefix('#')?;
    let digits_end = rest.find(|character: char| !character.is_ascii_digit()).unwrap_or(rest.len());
    if digits_end == 0 {
        // When: digits_end is zero, so no digit follows the '#' and the title
        // carries no position number to strip.
        return None;
    }
    Some(&rest[digits_end..])
}

#[cfg(test)]
#[path = "tabs_tests.rs"]
mod tabs_tests;
