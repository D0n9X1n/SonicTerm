//! Cross-platform SonicTerm application orchestration.
//!
//! This crate owns the winit application loop, window/tab/pane lifecycle,
//! PTY and parser wiring, input dispatch, redraw scheduling, config watching,
//! overlays, tab transfer, OS-drag bridges, menubar abstractions, and the
//! platform-shell builders consumed by the macOS and Windows binaries.

// TODO: add per-item docs and switch to #![deny(missing_docs)].
#![allow(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod menu;
pub mod menubar_bridge;
pub mod open_script_bridge;
pub mod os_drag;
pub mod os_drag_bridge;
pub mod shell;
pub mod tab_drag;
pub mod tab_thumbnail;
pub mod window_key_boundary;

pub use app::{ConfigNormalizer, KeymapLoader, ThemeLoader};

/// Privilege observed for the SonicTerm process at native startup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProcessPrivilege {
    /// The process was observed without elevated operating-system privilege.
    #[default]
    Unprivileged,
    /// The process was observed with an elevated token or effective user ID zero.
    Privileged,
}

impl ProcessPrivilege {
    /// Whether tab chrome should show the process-level privilege warning.
    #[must_use]
    pub const fn is_privileged(self) -> bool {
        matches!(self, Self::Privileged)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
