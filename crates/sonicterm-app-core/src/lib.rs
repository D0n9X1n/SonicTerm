//! Winit-agnostic application core for SonicTerm.
//!
//! This crate is the **state machine** the windowing/UI layer drives. It
//! deliberately does NOT depend on `winit`, `wgpu`, `arboard`, or any
//! other backend — those concerns live in `sonicterm-app`. Pure-data
//! types here can be unit-tested without spinning up a real window.
//!
//! Introduced at M6a as an ADDITIVE parallel crate. Consumers
//! (sonicterm-mac, sonicterm-windows, sonicterm-app) migrate to it
//! over M6b..d.
//!
//! M6a-expand-2a lands the `AppIntent` / `AppEffect` / `AppStateMachine`
//! contract (63 + 22 variants + 7-class ordering + cascade-bound
//! `drain_pending`). Per-Intent reducer arms are stubbed — the state
//! machine returns an empty Effect batch for every Intent. Routing of
//! `sonicterm-app` paths through the machine ships in 2b/2c.

#![deny(missing_docs)]

mod app_state;
mod effect;
mod intent;
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

// M6a-expand-1 type-relocation inventory (prep for M6a-expand-2).
//
// `BroadcastScope` is intentionally NOT re-exported here — the richer
// `supporting::BroadcastScope` variant (carrying `Custom(Vec<PaneId>)`)
// is the one Intent fan-out uses. The bare-action `sonicterm_types`
// version stays available via direct path.
pub use sonicterm_types::{
    Action, Cell, CellFlags, Color, Direction, FatAttributes, GlyphKey, HyperlinkId, ModKey, Pos,
    ScrollAction, WindowKey,
};
