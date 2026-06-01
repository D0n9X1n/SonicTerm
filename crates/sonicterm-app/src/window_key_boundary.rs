//! Platform boundary: translate winit-flavored input identifiers
//! (`winit::window::WindowId`, `winit::keyboard::ModifiersState`) into the
//! winit-free [`WindowKey`] / [`ModKey`] types from `sonicterm-types`.
//!
//! These translations live here (not in `sonicterm-types`) so the types
//! crate stays free of any windowing dependency. Platform shells consult
//! a [`WindowKeyRegistry`] to assign a stable monotonically-increasing
//! `u64` to every distinct `winit::WindowId` on first sight.
//!
//! Introduced at M6a-expand-1 as the boundary layer for the reducer-bound
//! refactor that lands in M6a-expand-2.

use std::collections::HashMap;

use sonicterm_types::{ModKey, WindowKey};
use winit::{keyboard::ModifiersState, window::WindowId};

/// Translate a winit [`ModifiersState`] into the platform-agnostic
/// [`ModKey`] bitflags.
#[inline]
pub fn mod_key_from_winit(m: ModifiersState) -> ModKey {
    let mut out = ModKey::empty();
    if m.shift_key() {
        out |= ModKey::SHIFT;
    }
    if m.control_key() {
        out |= ModKey::CTRL;
    }
    if m.alt_key() {
        out |= ModKey::ALT;
    }
    if m.super_key() {
        out |= ModKey::SUPER;
    }
    out
}

/// Inverse of [`mod_key_from_winit`] — primarily useful in tests where a
/// platform-agnostic [`ModKey`] needs to be re-injected into a winit
/// event path.
#[inline]
pub fn winit_from_mod_key(m: ModKey) -> ModifiersState {
    let mut out = ModifiersState::empty();
    if m.contains(ModKey::SHIFT) {
        out |= ModifiersState::SHIFT;
    }
    if m.contains(ModKey::CTRL) {
        out |= ModifiersState::CONTROL;
    }
    if m.contains(ModKey::ALT) {
        out |= ModifiersState::ALT;
    }
    if m.contains(ModKey::SUPER) {
        out |= ModifiersState::SUPER;
    }
    out
}

/// Monotonic translator from `winit::WindowId` → [`WindowKey`].
///
/// Each new `WindowId` gets the next sequential `u64`. Lookups for an
/// already-seen `WindowId` return the previously-assigned key. The
/// registry is intentionally tiny (no removal) — platform shells track
/// the inverse mapping (`HashMap<WindowKey, Arc<Window>>`) separately so
/// that closing a window does not invalidate the key.
#[derive(Debug, Default)]
pub struct WindowKeyRegistry {
    next: u64,
    map: HashMap<WindowId, WindowKey>,
}

impl WindowKeyRegistry {
    /// Create an empty registry. The first key minted will be
    /// [`WindowKey`]`(1)`.
    pub fn new() -> Self {
        Self { next: 1, map: HashMap::new() }
    }

    /// Look up the existing [`WindowKey`] for a `winit::WindowId`, or
    /// allocate a new monotonically-increasing key on first sight.
    pub fn intern(&mut self, id: WindowId) -> WindowKey {
        if let Some(k) = self.map.get(&id) {
            return *k;
        }
        let key = WindowKey::new(self.next);
        self.next += 1;
        self.map.insert(id, key);
        key
    }

    /// Look up the [`WindowKey`] for a `winit::WindowId` without
    /// allocating. Returns `None` if `id` was never interned.
    pub fn get(&self, id: WindowId) -> Option<WindowKey> {
        self.map.get(&id).copied()
    }

    /// Number of distinct `winit::WindowId`s seen.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if no `winit::WindowId` has been interned yet.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // winit 0.30 provides `From<u64> for WindowId` which we use to
    // synthesize stable test ids.

    #[test]
    fn winit_to_mod_key_translation() {
        let cases: &[(ModifiersState, ModKey)] = &[
            (ModifiersState::empty(), ModKey::empty()),
            (ModifiersState::SHIFT, ModKey::SHIFT),
            (ModifiersState::CONTROL, ModKey::CTRL),
            (ModifiersState::ALT, ModKey::ALT),
            (ModifiersState::SUPER, ModKey::SUPER),
            (ModifiersState::SHIFT | ModifiersState::CONTROL, ModKey::SHIFT | ModKey::CTRL),
            (
                ModifiersState::SHIFT
                    | ModifiersState::CONTROL
                    | ModifiersState::ALT
                    | ModifiersState::SUPER,
                ModKey::SHIFT | ModKey::CTRL | ModKey::ALT | ModKey::SUPER,
            ),
        ];
        for (w, k) in cases {
            assert_eq!(mod_key_from_winit(*w), *k, "winit {w:?} → mod_key");
            assert_eq!(winit_from_mod_key(*k), *w, "mod_key {k:?} → winit");
        }
    }

    #[test]
    fn winit_to_window_key_translation() {
        let mut reg = WindowKeyRegistry::new();
        let a = WindowId::from(1001u64);
        let b = WindowId::from(2002u64);
        let ka1 = reg.intern(a);
        let kb = reg.intern(b);
        let ka2 = reg.intern(a);
        assert_eq!(ka1, ka2, "same winit id maps to same WindowKey");
        assert_ne!(ka1, kb, "distinct winit ids map to distinct WindowKeys");
        assert_eq!(ka1, WindowKey::new(1));
        assert_eq!(kb, WindowKey::new(2));
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.get(a), Some(ka1));
        assert_eq!(reg.get(WindowId::from(9999u64)), None);
    }

    #[test]
    fn registry_is_empty_on_construction() {
        let r = WindowKeyRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }
}
