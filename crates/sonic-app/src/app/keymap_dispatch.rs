//! `run_action` keymap dispatcher split out of [`super::core`] for PR 8b.
//!
//! The big `match` over [`sonic_core::keymap::Action`] variants currently
//! still lives inside `App::run_action` in [`super::core`] (search for
//! `pub fn run_action`). It calls into helpers — many on `&mut self`
//! (font size, theme, search, palette, tab/pane management) — that also
//! live in `core.rs`. Splitting the match across modules without
//! introducing free-function adapters for every helper would either:
//!
//! 1. Require making 20+ `App` methods `pub(super)` (a real visibility
//!    blast radius for a refactor PR), or
//! 2. Forward each variant to a small free function in this module that
//!    immediately calls back into `App` — which is pure boilerplate with
//!    no readability win.
//!
//! For PR 8b the module exists as the named home for that future
//! extraction. When the dispatcher does move here, it should land as an
//! additional `impl App { pub fn run_action(...) }` block in this file
//! and `core.rs` should lose the matching definition in the same diff.
