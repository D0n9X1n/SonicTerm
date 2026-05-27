//! `App` module — the winit `ApplicationHandler` plus everything wired off it
//! (PTY/parser/grid per pane, renderer, tabs, panes, IME, drag, prefs).
//!
//! Split (refactor PR 8b) of the original monolithic `app.rs` (~4150 LOC).
//! Sub-modules expose `impl App` blocks rather than free state — `App` itself
//! is defined in [`core`], with its fields kept `pub(super)` where sibling
//! modules need them.
//!
//! Module map:
//!
//! - [`core`]: `App` struct, constructors, `run*` entry points,
//!   `ApplicationHandler` impl, the small helpers that don't logically
//!   belong anywhere else.
//! - [`event_loop`]: VT-thread 16 ms redraw coalescing helpers
//!   (CLAUDE.md §4 land-mine: coalesce ≥16 ms or macOS marks the app
//!   unresponsive).
//! - [`input`]: keyboard / mouse / IME encoding helpers (re-exported via
//!   [`core`]).
//! - [`keymap_dispatch`]: `run_action` and its supporting helpers.
//! - [`redraw`]: try_lock-based render gate (CLAUDE.md §4 land-mine:
//!   parser is `try_lock`'d on the render thread, never `lock`'d, to
//!   avoid the macOS main-thread deadlock under shell-startup output
//!   bursts).
//! - [`tear_out`]: tab tear-out + cross-window merge logic.
//!
//! All `impl App` blocks live within the same crate, so visibility stays
//! `pub(super)` and no `pub` API leaks beyond what existed pre-split.

mod core;
mod event_loop;
mod input;
mod keymap_dispatch;
mod redraw;
mod tear_out;

pub use self::core::*;
pub use self::input::{encode_logical, key_name, KeyName};
