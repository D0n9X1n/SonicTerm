//! Browser-style tab model.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

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
    pub command: CommandStatus,
    /// Path or scheme-like icon hint ("github", "chrome", "bilibili", ...).
    /// The render layer maps this to a glyph/asset.
    pub icon_hint: Option<String>,
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
            command: CommandStatus::default(),
            icon_hint: None,
        }
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

#[derive(Debug, Default)]
pub struct TabBar {
    tabs: Vec<Tab>,
    active: usize,
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
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.set_auto_title(title.into());
        }
    }

    /// Replace the automatic title of the currently-active tab. No-op if empty.
    pub fn set_active_title(&mut self, title: impl Into<String>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.set_auto_title(title.into());
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
        let Some(tab) = self.tabs.get_mut(self.active) else {
            // When: self.active addresses no tab, so there is nothing to
            // retitle and the request is dropped.
            return;
        };
        let body = body.into();
        if body.trim().is_empty() {
            // When: body.trim() leaves nothing, read as "drop my override"
            // rather than as a request for a blank title.
            tab.set_custom_title(None);
            return;
        }
        tab.set_custom_title(Some(body));
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

    /// The active tab's explicit color override, or `None` when it has none
    /// or no tab is active.
    pub fn active_custom_color(&self) -> Option<&str> {
        self.tabs.get(self.active)?.custom_color.as_deref()
    }

    /// Record the command status of the tab at `index`. No-op when `index` is
    /// out of range.
    pub fn set_command_status(&mut self, index: usize, status: CommandStatus) {
        if let Some(t) = self.tabs.get_mut(index) {
            t.command = status;
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

    /// Append `tab`, make it the active tab, renumber every `#N` prefix, and
    /// return the pushed tab's id.
    pub fn push(&mut self, tab: Tab) -> TabId {
        let id = tab.id;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
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
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            // Only rewrite tabs that already carry a `#N ` prefix —
            // leave raw user/system titles ("A", "Welcome", …) alone.
            let Some(body) = strip_index_prefix(&tab.auto_title) else {
                // When: strip_index_prefix finds no numeric head, so the title
                // is not position-numbered and keeps its text verbatim.
                continue;
            };
            let new_prefix = format!("#{}", i + 1);
            let mut s = String::with_capacity(new_prefix.len() + body.len());
            s.push_str(&new_prefix);
            s.push_str(body);
            tab.set_auto_title(s);
        }
    }

    /// Insert `tab` at `index`, clamping to `[0, len]`. The newly-inserted
    /// tab becomes the active tab. Used by the cross-window drag-merge
    /// flow to drop a torn tab into the destination bar at the slot the
    /// user released over.
    pub fn insert(&mut self, index: usize, tab: Tab) -> TabId {
        let idx = index.min(self.tabs.len());
        let id = tab.id;
        self.tabs.insert(idx, tab);
        self.active = idx;
        self.recompute_all_titles();
        id
    }

    /// Remove the tab carrying `id`, keep the user on the nearest sensible
    /// neighbour, and renumber the remaining `#N` prefixes. No-op when no tab
    /// has that id.
    pub fn close(&mut self, id: TabId) {
        if let Some(pos) = self.tabs.iter().position(|t| t.id == id) {
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
    }

    /// Make the tab at `index` the active one. No-op when `index` is out of
    /// range, so the current selection survives a stale request.
    pub fn activate(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    /// Move the selection one tab to the right, wrapping from the last tab
    /// round to the first. No-op on an empty bar.
    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    /// Move the selection one tab to the left, wrapping from the first tab
    /// round to the last. No-op on an empty bar.
    pub fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = if self.active == 0 {
                self.tabs.len() - 1
            } else {
                // When: active is non-zero, so stepping back stays inside the
                // bar and lands on the neighbour to its left.
                self.active - 1
            };
        }
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
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
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
        let pos = self.tabs.iter().position(|t| t.id == id)?;
        let tab = self.tabs.remove(pos);
        if pos < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
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
        && !first.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '/' || ch == '~');
    if keep_icon {
        format!("{index} {first} {body}")
    } else {
        // When: keep_icon is false, so first was ordinary title text rather
        // than an icon and only the index survives alongside body.
        format!("{index} {body}")
    }
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
        && !first.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '/' || ch == '~');
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
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
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
