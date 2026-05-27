//! Redraw / vsync-gate helpers split out of [`super::core`] for PR 8b.
//!
//! ## CLAUDE.md §4 land-mine — `try_lock`, not `lock`, on the parser
//!
//! The render path takes `parser.try_lock()`, NEVER `parser.lock()`.
//! Earlier `lock()` deadlocked the macOS main thread under shell-startup
//! output bursts (the PTY-reader thread was holding the parser mutex while
//! pushing bytes through vte, and the render thread blocked behind it; the
//! winit runloop then stopped pumping `WindowEvent::RedrawRequested` and
//! the window froze permanently).
//!
//! Treat `try_lock` on the parser as a load-bearing invariant. If you ever
//! see `parser.lock()` on a render-thread code path, that's the bug — drop
//! the frame instead and rely on the next vsync coalesce.
//!
//! ## CLAUDE.md §4 land-mine — no unconditional heartbeat redraw
//!
//! `window_event` MUST NOT end with an unconditional `request_redraw` call
//! ("redraw the window after every event, just in case"). That creates a
//! feedback loop where redraw → new event → redraw, pegging the CPU.
//! Real triggers (pty bytes, mouse drag, key event, resize, IME) already
//! cover every visible-state change. If a future PR introduces an
//! "everything I forgot" redraw at the end of an event handler, that's
//! the regression — find the specific missing trigger instead.
//!
//! The actual try_lock site is inside the render closure spawned from
//! [`super::core::App::spawn_pane`] (search the file for `try_lock` to
//! locate it). For PR 8b the helper remains in `core.rs` to keep the diff
//! tight; future PRs can extract the body here without changing semantics
//! as long as the two invariants above are preserved.
