//! Platform-agnostic modifier-key bitflags.
//!
//! `ModKey` is a winit-free replacement for `winit::keyboard::ModifiersState`
//! used by platform-agnostic crates such as `sonicterm-app-core`. Conversion
//! from `ModifiersState` lives at the `sonicterm-app` platform boundary so this
//! crate remains independent of winit.

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
#[path = "mod_key_tests.rs"]
mod mod_key_tests;
