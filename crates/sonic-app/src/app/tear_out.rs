//! Tab tear-out + cross-window drag/merge helpers split out of
//! [`super::core`] for PR 8b.
//!
//! The actual `App::tear_out_tab`, `compute_main_drag_target`,
//! `compute_child_drag_target`, `cursor_inside_any_window`,
//! `build_payload_for_tab`, `try_os_drag_handoff`,
//! `new_tab_from_payload`, `merge_main_into_child`,
//! `merge_child_into_target`, `try_cross_window_merge`,
//! `detach_tab_state`, `attach_tab_state`, `detach_from_child`,
//! `attach_to_child`, `tear_out_would_be_noop`, and `reap_empty_child`
//! methods still live inside the primary `impl App` block in
//! [`super::core`].
//!
//! They form a cohesive subsystem (tab payloads + window topology +
//! drag-session bookkeeping) and are the obvious next candidates for a
//! follow-up extraction PR — once moved here they become a second
//! `impl App { … }` block with `pub(super)` visibility on the
//! supporting helpers. PR 8b keeps the move conservative because
//! splitting these touches the same struct fields that the
//! `window_event` router and `handle_child_window_event` use, and the
//! diff is best evaluated against a green baseline first.
//!
//! See `crates/sonic-app/src/tab_drag.rs` for the underlying drag-
//! session types — those already live outside the giant `app.rs`.
