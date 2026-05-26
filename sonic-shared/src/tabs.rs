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
        id
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
        id
    }

    pub fn close(&mut self, id: TabId) {
        if let Some(pos) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs.remove(pos);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
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
    }

    /// Pop a tab out of this bar — used to seed a new window when the user
    /// drags a tab off the bar.
    pub fn detach(&mut self, id: TabId) -> Option<Tab> {
        let pos = self.tabs.iter().position(|t| t.id == id)?;
        let tab = self.tabs.remove(pos);
        if self.active >= self.tabs.len() && !self.tabs.is_empty() {
            self.active = self.tabs.len() - 1;
        }
        Some(tab)
    }
}
