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

#![deny(missing_docs)]

mod app_state;
mod intent;

pub use app_state::{AppState, AppStateBuilder};
pub use intent::{AppIntent, RedrawReason};

// M6a-expand-1 type-relocation inventory (prep for M6a-expand-2).
//
// These re-exports give the reducer (landing in M6a-expand-2) a stable
// windowing-agnostic surface. Backend shells (`sonicterm-app`,
// `sonicterm-mac`, `sonicterm-windows`) own the boundary translation
// from `winit::WindowId` / `winit::keyboard::ModifiersState` into these
// types — see `sonicterm_app::window_key_boundary`.
pub use sonicterm_types::{
    Action, BroadcastScope, Cell, CellFlags, Color, Direction, FatAttributes, GlyphKey,
    HyperlinkId, ModKey, Pos, ScrollAction, WindowKey,
};
