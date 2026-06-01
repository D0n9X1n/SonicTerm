//! Platform-agnostic window identifier.
//!
//! `WindowKey` is a winit-free stand-in for `winit::window::WindowId`
//! used by platform-agnostic crates (e.g. `sonicterm-app-core`,
//! `sonicterm-types`). Platform shells maintain a
//! `HashMap<winit::WindowId, WindowKey>` registry, assigning
//! monotonically-increasing `u64` ids on first sight.
//!
//! Introduced at M6a-expand-1 as part of the type-relocation inventory
//! that lets the reducer (landing in M6a-expand-2) live in a
//! windowing-agnostic crate.

use std::fmt;

/// Opaque, platform-agnostic window identifier.
///
/// Equality / hash are derived from the wrapped `u64`. A registry in
/// the platform shell (`sonicterm-app`) translates winit
/// `WindowId` values into stable `WindowKey`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WindowKey(pub u64);

impl WindowKey {
    /// Construct a `WindowKey` from a raw `u64`.
    #[inline]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Raw underlying id.
    #[inline]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for WindowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WindowKey({})", self.0)
    }
}

impl From<u64> for WindowKey {
    #[inline]
    fn from(id: u64) -> Self {
        Self(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn window_key_hashmap_works() {
        let mut map: HashMap<WindowKey, &'static str> = HashMap::new();
        let a = WindowKey::new(1);
        let b = WindowKey::new(2);
        let a2 = WindowKey::from(1u64);
        map.insert(a, "alpha");
        map.insert(b, "beta");
        assert_eq!(map.get(&a2), Some(&"alpha"));
        assert_eq!(map.get(&b), Some(&"beta"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn window_key_display() {
        assert_eq!(WindowKey::new(42).to_string(), "WindowKey(42)");
    }

    #[test]
    fn window_key_raw_round_trip() {
        let k = WindowKey::new(7);
        assert_eq!(k.raw(), 7);
        assert_eq!(WindowKey::from(7u64), k);
    }

    #[test]
    fn window_key_copy_eq() {
        let a = WindowKey::new(3);
        let b = a; // Copy
        assert_eq!(a, b);
        assert_eq!(a, WindowKey::new(3));
        assert_ne!(a, WindowKey::new(4));
    }
}
