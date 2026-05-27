//! Event-loop helpers split out of [`super::core`] for PR 8b.
//!
//! ## CLAUDE.md §4 land-mine — VT-thread coalesce ≥16 ms
//!
//! The PTY-reader thread MUST coalesce redraw requests to at least 16 ms
//! intervals. Without coalescing, a shell-startup output burst posts
//! thousands of `request_redraw` events to the macOS main thread within
//! a single frame, and the OS marks the app "not responding" (the
//! beachball appears and the window stops accepting input).
//!
//! The canonical implementation lives in [`super::core::App::spawn_pane`] —
//! search for `Duration::from_millis(16)`. This module exists so future
//! coalesce / vsync-gate helpers (about_to_wait scheduling, frame-period
//! WaitUntil arms, ResumeTimeReached fan-in) can land here without
//! polluting `core.rs` further. For PR 8b the helper functions remain in
//! [`super::core`] to keep the diff bounded — splitting the actual
//! `about_to_wait` body across modules requires also splitting the
//! `ApplicationHandler<UserEvent>` impl, which is a larger change than
//! the spec asks for.
//!
//! When future PRs extract real helpers here, KEEP THE 16 ms FLOOR and
//! reference this comment.
