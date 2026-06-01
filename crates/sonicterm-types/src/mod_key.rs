//! Platform-agnostic modifier-key bitflags.
//!
//! `ModKey` is a winit-free replacement for `winit::keyboard::ModifiersState`
//! used by platform-agnostic crates (`sonicterm-app-core`, future
//! `sonicterm-reducer`). The conversion from `ModifiersState` lives
//! at the platform boundary in `sonicterm-app` so this crate stays
//! winit-free.
//!
//! Introduced at M6a-expand-1.

use bitflags::bitflags;

bitflags! {
    /// Set of modifier keys (Shift / Ctrl / Alt / Super) held during an
    /// input event. Backed by a `u32` so it derives `Copy + Eq + Hash`,
    /// suitable for `HashMap` keys (e.g. keymap chord tables).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
    pub struct ModKey: u32 {
        /// Shift key (left or right).
        const SHIFT = 1 << 0;
        /// Control key (left or right).
        const CTRL  = 1 << 1;
        /// Alt / Option key (left or right).
        const ALT   = 1 << 2;
        /// Super / Command / Windows key (left or right).
        const SUPER = 1 << 3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn mod_key_bitflags_round_trip() {
        let m = ModKey::SHIFT | ModKey::CTRL;
        assert!(m.contains(ModKey::SHIFT));
        assert!(m.contains(ModKey::CTRL));
        assert!(!m.contains(ModKey::ALT));
        assert!(!m.contains(ModKey::SUPER));
        assert_eq!(m.bits(), 0b0011);
        assert_eq!(ModKey::from_bits_truncate(0b1111), ModKey::all());
    }

    #[test]
    fn mod_key_hashmap_works() {
        let mut map: HashMap<ModKey, &'static str> = HashMap::new();
        map.insert(ModKey::SUPER | ModKey::SHIFT, "super-shift");
        map.insert(ModKey::CTRL, "ctrl");
        let k = ModKey::SUPER | ModKey::SHIFT;
        assert_eq!(map.get(&k), Some(&"super-shift"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn mod_key_default_is_empty() {
        assert_eq!(ModKey::default(), ModKey::empty());
    }
}
