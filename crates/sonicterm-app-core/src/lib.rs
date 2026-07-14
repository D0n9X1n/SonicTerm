//! Winit-agnostic application core for SonicTerm.
//!
//! This crate is the **state machine** the windowing/UI layer drives. It
//! deliberately does NOT depend on `winit`, `wgpu`, `arboard`, or any
//! other backend — those concerns live in `sonicterm-app`. Pure-data
//! types here can be unit-tested without spinning up a real window.
//!
//! The contract includes 63 `AppIntent` variants, 22 `AppEffect` variants,
//! stable seven-class effect ordering, and bounded `drain_pending` support.
//! Reducer arms cover every intent family. `sonicterm-app` still owns the
//! authoritative live window/tab/pane resources; `AppState` is the pure-data
//! transition and observability model while those resources migrate behind ids.

#![deny(missing_docs)]

mod app_state;
mod effect;
mod intent;
mod reducer;
mod state_machine;
mod supporting;

pub use app_state::{AppState, AppStateBuilder};
pub use effect::{AppEffect, EffectClass, LogLevel};
pub use intent::{AppIntent, RedrawReason, SelectionMode};
pub use state_machine::{AppStateMachine, MAX_CASCADE_DEPTH};
pub use supporting::{
    BroadcastScope, KeyCode, LogicalPos, LogicalSize, MenuItem, MenuModel, MouseButton,
    PaletteChoice, PaneId, PendingDragOutcomeCore, PtyConfig, SplitDir, TabId, WindowRole,
};

// `BroadcastScope` is intentionally NOT re-exported from `sonicterm-types`
// here: the richer supporting variant (including `Custom(Vec<PaneId>)`) is
// the one intent fan-out uses. The action-level variant remains available
// through its direct `sonicterm_types` path.
pub use sonicterm_types::{
    Action, Cell, CellFlags, Color, Direction, FatAttributes, GlyphKey, HyperlinkId, ModKey, Pos,
    ScrollAction, WindowKey,
};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
