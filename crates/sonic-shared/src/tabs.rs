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

    pub fn push(&mut self, tab: Tab) -> TabId {
        let id = tab.id;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_activate() {
        let mut bar = TabBar::new();
        let a = bar.push(Tab::new("A"));
        let _b = bar.push(Tab::new("B"));
        assert_eq!(bar.len(), 2);
        assert_eq!(bar.active().unwrap().title, "B");
        bar.activate(0);
        assert_eq!(bar.active().unwrap().id, a);
    }

    #[test]
    fn close_shifts_active() {
        let mut bar = TabBar::new();
        bar.push(Tab::new("A"));
        let b = bar.push(Tab::new("B"));
        bar.close(b);
        assert_eq!(bar.active().unwrap().title, "A");
    }

    #[test]
    fn reorder_moves_tabs() {
        let mut bar = TabBar::new();
        let a = bar.push(Tab::new("A"));
        let _b = bar.push(Tab::new("B"));
        let _c = bar.push(Tab::new("C"));
        bar.reorder(0, 2);
        assert_eq!(bar.tabs()[2].id, a);
    }

    #[test]
    fn detach_removes_and_returns_tab() {
        let mut bar = TabBar::new();
        let a = bar.push(Tab::new("A"));
        let b = bar.push(Tab::new("B"));
        let taken = bar.detach(a).expect("detached");
        assert_eq!(taken.id, a);
        assert_eq!(bar.len(), 1);
        assert_eq!(bar.active().unwrap().id, b);
    }

    #[test]
    fn next_and_prev_wrap_around() {
        let mut bar = TabBar::new();
        bar.push(Tab::new("A"));
        bar.push(Tab::new("B"));
        bar.push(Tab::new("C"));
        bar.activate(2);
        bar.next(); // wraps to 0
        assert_eq!(bar.active_index(), 0);
        bar.prev(); // wraps back to 2
        assert_eq!(bar.active_index(), 2);
    }

    #[test]
    fn close_last_tab_leaves_empty_bar() {
        let mut bar = TabBar::new();
        let a = bar.push(Tab::new("A"));
        bar.close(a);
        assert!(bar.is_empty());
        assert!(bar.active().is_none());
    }

    #[test]
    fn tab_ids_are_unique() {
        let a = Tab::new("x");
        let b = Tab::new("y");
        assert_ne!(a.id, b.id);
    }
}
