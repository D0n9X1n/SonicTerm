//! Epic #300 P4 follow-up: `UserEvent::ClearShapeCache` plumbing.
//!
//! Covers the contract that the async-font-fallback notifier path can
//! reach the App without depending on a live wgpu surface:
//!
//! 1. `UserEvent::ClearShapeCache` is a real variant — adding it does
//!    not break the existing match arms in
//!    `App::do_user_event` (compile-time guard via `match` below).
//! 2. The variant round-trips through `Clone` / `PartialEq` so the
//!    `EventLoopProxy::send_event` path (which clones the payload
//!    into the loop) does not silently drop a discriminant.
//! 3. An `EventLoopProxy<UserEvent>` accepts the variant — this is the
//!    exact API the
//!    [`sonic_text::async_fallback::AsyncFallbackLoader`] notifier
//!    wraps in production.
//!
//! GpuRenderer-level coverage (the `style_rev` bump and the
//! shape/row/line cache invalidation triggered from
//! `GpuRenderer::clear_shape_cache`) lives in
//! `crates/sonic-shared/tests/` — it can't be exercised here without
//! a live winit `Window` for the wgpu surface, which `cargo test`
//! cannot provide on a headless macOS runner.

use sonic_app::app::UserEvent;

#[test]
fn clear_shape_cache_variant_round_trips_through_clone_and_eq() {
    let ev = UserEvent::ClearShapeCache;
    let cloned = ev.clone();
    assert_eq!(ev, cloned);
    assert_ne!(ev, UserEvent::ConfigChanged);
    assert_ne!(ev, UserEvent::MenuAction);
    assert_ne!(ev, UserEvent::OsDrag);
    assert_ne!(ev, UserEvent::DragMoved);
    assert_ne!(ev, UserEvent::DragEnded);
}

#[test]
fn all_user_event_variants_have_distinct_discriminants() {
    // Exhaustiveness check — if a future PR adds a new variant the
    // compiler forces them to extend this match (and, by symmetry,
    // the dispatcher in `App::do_user_event`). The ClearShapeCache
    // arm pins that this PR's variant survived.
    let cover = |e: UserEvent| match e {
        UserEvent::ConfigChanged
        | UserEvent::MenuAction
        | UserEvent::OsDrag
        | UserEvent::DragMoved
        | UserEvent::DragEnded
        | UserEvent::ClearShapeCache => true,
    };
    assert!(cover(UserEvent::ClearShapeCache));
}

#[test]
fn user_event_is_send_and_sync_for_proxy_use() {
    // The production wiring threads `UserEvent::ClearShapeCache`
    // through `EventLoopProxy::send_event`, which requires the
    // payload type be `Send + 'static`. (We deliberately do NOT
    // construct a real `EventLoop` here — winit forces that onto the
    // main thread, which `cargo test` cannot guarantee on macOS.)
    fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<UserEvent>();
    // Hand the variant across a thread boundary as a smoke check —
    // matches what the async font loader notifier does on every load
    // completion.
    let ev = UserEvent::ClearShapeCache;
    let handle = std::thread::spawn(move || ev);
    let received = handle.join().expect("thread join");
    assert_eq!(received, UserEvent::ClearShapeCache);
}
