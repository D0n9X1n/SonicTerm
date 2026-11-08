//! A window's pane membership: which panes it holds. Reads, and changes to a pane's own state, are
//! open to the app; a pane joins only through [`PaneMap::attach`] (or a new window's
//! [`PaneMap::founding`] set) and leaves only through [`PaneMap::detach`] or
//! [`PaneMap::detach_all`], which hand back a [`DetachedPane`] that must be retired through
//! `App::retire_pane` or attached to a window. A pane can therefore never be dropped silently or
//! held by two maps.

use std::collections::HashMap;

use super::PaneState;

/// The panes one window holds, by id.
#[doc(hidden)]
#[derive(Default)]
pub struct PaneMap(HashMap<u64, PaneState>);

impl std::ops::Deref for PaneMap {
    type Target = HashMap<u64, PaneState>;

    /// Every read of the map: `get`, `contains_key`, `keys`, `values`, `iter`, `len`, indexing.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PaneMap {
    /// A new window's first panes, before the window joins the registry, which then checks that
    /// none belongs to another window.
    pub(in crate::app) fn founding(panes: HashMap<u64, PaneState>) -> Self {
        Self(panes)
    }

    /// The pane `id`'s state, mutably; the pane stays in the window.
    pub(in crate::app) fn get_mut(&mut self, id: &u64) -> Option<&mut PaneState> {
        self.0.get_mut(id)
    }

    /// Every pane's state, mutably; none can join or leave through this.
    pub(in crate::app) fn values_mut(&mut self) -> impl Iterator<Item = &mut PaneState> {
        self.0.values_mut()
    }

    /// Every (id, pane) pair, with the pane mutable.
    pub(in crate::app) fn iter_mut(&mut self) -> impl Iterator<Item = (&u64, &mut PaneState)> {
        self.0.iter_mut()
    }

    /// Room for `additional` more panes, so a later attach cannot allocate.
    pub(in crate::app) fn reserve(&mut self, additional: usize) {
        self.0.reserve(additional);
    }

    /// Add `pane` to this window. A pane whose id the window already holds is handed back
    /// unchanged rather than replacing the held one, whose PTY would otherwise be dropped unretired.
    pub(in crate::app) fn attach(&mut self, mut pane: DetachedPane) -> Result<(), DetachedPane> {
        if self.0.contains_key(&pane.id) {
            // When: the window `contains_key` the id already, the caller keeps custody.
            return Err(pane);
        }
        let state = pane.state.take().expect("a detached pane holds its state until consumed");
        self.0.insert(pane.id, *state);
        Ok(())
    }

    /// Add a newly spawned `state` as pane `id`, whose id came from the process-wide counter and so
    /// is held by no window. Panics if a window somehow holds it already.
    pub(in crate::app) fn attach_new(&mut self, id: u64, state: PaneState) {
        if let Err(pane) = self.attach(DetachedPane::unattached(id, state)) {
            // A fresh id collided: the held pane stays, and this one is dropped as the panic unwinds.
            panic!("fresh pane id {} is already held", pane.id());
        }
    }

    /// Take pane `id` out of this window, for retirement or a move.
    pub(in crate::app) fn detach(&mut self, id: u64) -> Option<DetachedPane> {
        self.0.remove(&id).map(|state| DetachedPane::new(id, state))
    }

    /// Take every pane out, for a window that is going away; in no particular order.
    pub(in crate::app) fn detach_all(&mut self) -> Vec<DetachedPane> {
        self.0.drain().map(|(id, state)| DetachedPane::new(id, state)).collect()
    }

    /// Add `pane` as `id` directly, for a test fixture building a window's panes.
    #[cfg(test)]
    pub(in crate::app) fn insert_for_test(
        &mut self,
        id: u64,
        pane: PaneState,
    ) -> Option<PaneState> {
        self.0.insert(id, pane)
    }

    /// Remove pane `id` directly, for a test that inspects or drops it.
    #[cfg(test)]
    pub(in crate::app) fn remove_for_test(&mut self, id: &u64) -> Option<PaneState> {
        self.0.remove(id)
    }
}

impl<'map> IntoIterator for &'map PaneMap {
    type Item = (&'map u64, &'map PaneState);
    type IntoIter = std::collections::hash_map::Iter<'map, u64, PaneState>;

    /// `for (id, pane) in &window.panes`, as on the map.
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'map> IntoIterator for &'map mut PaneMap {
    type Item = (&'map u64, &'map mut PaneState);
    type IntoIter = std::collections::hash_map::IterMut<'map, u64, PaneState>;

    /// `for (id, pane) in &mut window.panes`, as on the map; no pane joins or leaves.
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

/// A pane that left its window (or never joined one) and is owed a home: `App::retire_pane`
/// tears it down, or [`PaneMap::attach`] gives it a window. Dropping it otherwise fails in debug
/// builds, unless the thread is already unwinding.
#[doc(hidden)]
#[must_use = "a detached pane must be retired or attached"]
pub struct DetachedPane {
    /// The pane's id.
    id: u64,
    /// The pane, boxed so a detached pane moves as a pointer; `None` once consumed.
    state: Option<Box<PaneState>>,
}

impl DetachedPane {
    /// Wrap `state` as pane `id`.
    fn new(id: u64, state: PaneState) -> Self {
        Self { id, state: Some(Box::new(state)) }
    }

    /// A pane that never joined a window, such as a new pane whose installation failed.
    pub(in crate::app) fn unattached(id: u64, state: PaneState) -> Self {
        Self::new(id, state)
    }

    /// The pane's id.
    pub(in crate::app) fn id(&self) -> u64 {
        self.id
    }

    /// The pane's state.
    pub(in crate::app) fn pane(&self) -> &PaneState {
        self.state.as_ref().expect("a detached pane holds its state until consumed")
    }

    /// The pane's state, mutably.
    pub(in crate::app) fn pane_mut(&mut self) -> &mut PaneState {
        self.state.as_mut().expect("a detached pane holds its state until consumed")
    }

    /// Hand the state to `App::retire_pane`, the only caller; the structural audit pins that.
    pub(in crate::app) fn into_retired(mut self) -> PaneState {
        *self.state.take().expect("a detached pane holds its state until consumed")
    }

    /// The bare state, for a test that inspects or drops a detached pane on purpose.
    #[cfg(test)]
    pub(in crate::app) fn into_state_for_test(mut self) -> PaneState {
        *self.state.take().expect("a detached pane holds its state until consumed")
    }
}

/// The panes of a tab in transit between windows, each owed a home; ordered by id.
#[doc(hidden)]
#[must_use = "detached panes must be retired or attached"]
#[derive(Default)]
pub struct DetachedPanes(Vec<DetachedPane>);

impl DetachedPanes {
    /// Hold `pane` with the others, keeping them ordered by id.
    pub(in crate::app) fn push(&mut self, pane: DetachedPane) {
        let at = self.0.partition_point(|held| held.id < pane.id);
        self.0.insert(at, pane);
    }

    /// How many panes are held.
    pub(in crate::app) fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether pane `id` is held.
    pub(in crate::app) fn contains_key(&self, id: &u64) -> bool {
        self.0.binary_search_by_key(id, |held| held.id).is_ok()
    }

    /// The held ids, ascending.
    pub(in crate::app) fn keys(&self) -> impl Iterator<Item = &u64> {
        self.0.iter().map(|held| &held.id)
    }

    /// The held panes' state.
    pub(in crate::app) fn values(&self) -> impl Iterator<Item = &PaneState> {
        self.0.iter().map(DetachedPane::pane)
    }

    /// The held panes' state, mutably.
    pub(in crate::app) fn values_mut(&mut self) -> impl Iterator<Item = &mut PaneState> {
        self.0.iter_mut().map(DetachedPane::pane_mut)
    }

    /// Every (id, state) pair, with the state mutable.
    pub(in crate::app) fn iter_mut(&mut self) -> impl Iterator<Item = (u64, &mut PaneState)> {
        self.0.iter_mut().map(|held| (held.id, held.pane_mut()))
    }

    /// Pane `id`'s state, mutably.
    pub(in crate::app) fn get_mut(&mut self, id: &u64) -> Option<&mut PaneState> {
        let at = self.0.binary_search_by_key(id, |held| held.id).ok()?;
        Some(self.0[at].pane_mut())
    }

    /// Pane `id`'s state.
    pub(in crate::app) fn get(&self, id: &u64) -> Option<&PaneState> {
        let at = self.0.binary_search_by_key(id, |held| held.id).ok()?;
        Some(self.0[at].pane())
    }

    /// Hand every pane over, each still owed a home.
    pub(in crate::app) fn into_panes(self) -> Vec<DetachedPane> {
        self.0
    }

    /// Drop a test's detached panes, which own no PTY, on purpose.
    #[cfg(test)]
    pub(in crate::app) fn discard_for_test(self) {
        for pane in self.0 {
            drop(pane.into_state_for_test());
        }
    }
}

impl std::ops::Index<&u64> for DetachedPanes {
    type Output = PaneState;

    /// Pane `id`'s state; panics when it is not held, as map indexing does.
    fn index(&self, id: &u64) -> &PaneState {
        self.get(id).expect("a held pane id")
    }
}

impl PaneMap {
    /// Attach every pane of `set`; ids the window already holds come back unattached.
    pub(in crate::app) fn attach_set(&mut self, set: DetachedPanes) -> DetachedPanes {
        let mut refused = DetachedPanes::default();
        for pane in set.into_panes() {
            if let Err(pane) = self.attach(pane) {
                refused.push(pane);
            }
        }
        refused
    }

    /// A new window's panes, taken from a tab in transit.
    pub(in crate::app) fn from_detached(set: DetachedPanes) -> Self {
        let mut map = Self::default();
        let refused = map.attach_set(set);
        assert_eq!(refused.len(), 0, "a tab in transit holds each pane id once");
        map
    }
}

// Lifecycle: DetachedPane drop asserts retirement or attachment consumed the state, so no pane
// leaves a window unretired; an unwinding thread is exempt.
impl Drop for DetachedPane {
    fn drop(&mut self) {
        debug_assert!(
            self.state.is_none() || std::thread::panicking(),
            "pane {} was detached and dropped without retirement",
            self.id
        );
    }
}

#[cfg(test)]
#[path = "window_registry_panes_tests.rs"]
mod window_registry_panes_tests;
