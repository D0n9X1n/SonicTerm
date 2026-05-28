//! Browser-style tab model.

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

impl TabId {
    pub fn next() -> Self {
        Self(NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    /// Path or scheme-like icon hint ("github", "chrome", "bilibili", ...).
    /// The render layer maps this to a glyph/asset.
    pub icon_hint: Option<String>,
}

impl Tab {
    pub fn new(title: impl Into<String>) -> Self {
        Self { id: TabId::next(), title: title.into(), icon_hint: None }
    }
}

#[derive(Debug, Default)]
pub struct TabBar {
    tabs: Vec<Tab>,
    active: usize,
}

impl TabBar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Replace the title of the tab with `id`. No-op if not found.
    pub fn set_title(&mut self, id: TabId, title: impl Into<String>) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
            t.title = title.into();
        }
    }

    /// Replace the title of the currently-active tab. No-op if empty.
    pub fn set_active_title(&mut self, title: impl Into<String>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.title = title.into();
        }
    }

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
            let Some(body) = strip_index_prefix(&tab.title) else { continue };
            let new_prefix = format!("#{}", i + 1);
            let mut s = String::with_capacity(new_prefix.len() + body.len());
            s.push_str(&new_prefix);
            s.push_str(body);
            tab.title = s;
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
            // Pre-fix, this only clamped on overflow, which silently
            // shifted focus to the wrong tab when closing any inactive
            // tab to the LEFT of the active one (e.g. close tab #0 with
            // tab #1 active → vec shrinks so old-tab-#2 becomes the new
            // tab #1, but `active` stayed at 1 — user lost their place).
            if pos < self.active {
                self.active -= 1;
            }
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
            self.recompute_all_titles();
        }
    }

    pub fn activate(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
        }
    }

    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = if self.active == 0 { self.tabs.len() - 1 } else { self.active - 1 };
        }
    }

    /// Reorder the tab at `from` to position `to` (used by drag-reorder).
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        if self.active == from {
            self.active = to;
        }
        self.recompute_all_titles();
    }

    /// Pop a tab out of this bar — used to seed a new window when the user
    /// drags a tab off the bar.
    pub fn detach(&mut self, id: TabId) -> Option<Tab> {
        let pos = self.tabs.iter().position(|t| t.id == id)?;
        let tab = self.tabs.remove(pos);
        if self.active >= self.tabs.len() && !self.tabs.is_empty() {
            self.active = self.tabs.len() - 1;
        }
        self.recompute_all_titles();
        Some(tab)
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
        return None;
    }
    Some(&rest[digits_end..])
}
