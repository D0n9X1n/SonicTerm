//! sonic-ui — pure-data UI state and layout for Sonic Terminal.
//!
//! Extracted from `sonic-shared` in PR-5 of the workspace refactor
//! (issue #121). This crate carries only *state* and *layout* — no
//! winit, no wgpu, no glyphon. The render layer in `sonic-shared` /
//! `sonic-gpu` consumes these types to produce draw commands.
//!
//! `sonic-shared` re-exports every module here, so existing imports
//! of the form `use sonic_shared::tabs::TabBar;` continue to work.
//!
//! ## Deviations from the PR-5 plan
//!
//! - `menu.rs` stays in `sonic-shared` (transitively depends on
//!   `winit` via `menubar_bridge`).
//! - `sonic-grid` is added as a third dep (alongside `sonic-types`
//!   and `sonic-cfg`) because `selection` and `search` need
//!   `Grid`/`Cell`/`CellFlags`. `sonic-grid` is itself pure data
//!   with no GPU/windowing deps, so the "pure UI state" invariant
//!   is preserved.
//! - `ime.rs` is moved here too (the plan didn't list it explicitly,
//!   but it has zero external deps and `overlays.rs` requires it).

// TODO: add per-item docs and switch to #![deny(missing_docs)] in a follow-up PR.
#![allow(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod broadcast;
pub mod cheatsheet;
pub mod command_label;
pub mod command_palette;
pub mod copy_mode;
pub mod cursor;
pub mod i18n;
pub mod icon;
pub mod ime;
pub mod overlays;
pub mod pane;
pub mod prefs;
pub mod search;
pub mod selection;
pub mod tab_title;
pub mod tabbar_view;
pub mod tabs;
pub mod ui_tokens;
