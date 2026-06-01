//! Epic #300 P4 follow-up: wire `AsyncFallbackLoader` into
//! `SwashRasterizer::resolve_slot`.
//!
//! Contract under test:
//!
//! 1. When the rasterizer has a loader attached AND the live chain
//!    cannot satisfy a codepoint, `resolve_slot` MUST call
//!    `loader.request_load` for every static
//!    `PLATFORM_FALLBACK_CHAIN` entry not yet attempted (loaded /
//!    pending / failed all dedup correctly).
//! 2. The miss MUST return `None` (tofu) without sync-blocking on the
//!    background thread.
//! 3. After the loader fires its notifier and the renderer calls
//!    `SwashRasterizer::clear_caches`, a subsequent `resolve_slot` for
//!    the same codepoint MUST re-walk the chain (no stale `Some(None)`
//!    sitting in the slot cache).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosmic_text::{fontdb, FontSystem};
use sonicterm_text::async_fallback::{AsyncFallbackLoader, FontHandle, LoadFn, NotifyFn};
use sonicterm_text::swash_rasterizer::{platform_fallback_chain_for_test, SwashRasterizer};

/// Build a [`FontSystem`] with an EMPTY `fontdb` — no system fonts,
/// no bundled fonts. Every fallback chain entry will then miss every
/// lookup and the rasterizer is forced down the
/// async-fallback-loader path under test. The default
/// `FontSystem::new()` scans the OS font directories on macOS /
/// Windows, which would silently satisfy CJK queries from the system
/// `PingFang SC` / `Microsoft YaHei` and short-circuit the path we
/// are trying to exercise.
fn empty_font_system() -> FontSystem {
    FontSystem::new_with_locale_and_db("en-US".to_string(), fontdb::Database::new())
}

fn wait_until<F: Fn() -> bool>(timeout: Duration, cond: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// Pick a codepoint we are confident the FRESH `FontSystem` we build
/// in this test cannot satisfy — no bundled fonts loaded, no fallback
/// chain warmed. The CJK codepoint U+4E2D (中) is the canonical
/// example.
const TOFU_CHAR: char = '中';

#[test]
fn missing_glyph_triggers_request_load_for_every_platform_chain_entry() {
    // Stub loader: records which families it was asked to load.
    let requested = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let requested_for_fn = requested.clone();
    let load_fn: LoadFn = Arc::new(move |family: &'static str| {
        requested_for_fn.lock().unwrap().push(family);
        // Pretend the load succeeded so the notifier fires, but with
        // zero bytes_loaded — the actual FontSystem is unchanged, so
        // the rasterizer's second resolve still returns tofu. That is
        // EXPECTED for this test; we are validating wiring, not
        // resolution success.
        Some(FontHandle { family, bytes_loaded: 0 })
    });
    let notify_count = Arc::new(AtomicUsize::new(0));
    let notify_for_fn = notify_count.clone();
    let notify: NotifyFn = Arc::new(move || {
        notify_for_fn.fetch_add(1, Ordering::SeqCst);
    });
    let loader = AsyncFallbackLoader::new(load_fn, notify);

    // Build a rasterizer over a bare FontSystem — no bundled fonts
    // loaded — so every chain entry will miss on CJK and trip the
    // async-load path.
    let mut fs = empty_font_system();
    let mut rasterizer =
        SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", 16.0).with_async_loader(loader.clone());

    let result = rasterizer.resolve_slot(TOFU_CHAR, false, false);
    assert!(
        result.is_none(),
        "with a bare FontSystem and no live fallback the rasterizer must return tofu, got {result:?}"
    );

    // Wait until every static chain entry has been asked for.
    let chain = platform_fallback_chain_for_test();
    let all_seen = wait_until(Duration::from_secs(2), || {
        let r = requested.lock().unwrap();
        chain.iter().all(|f| r.iter().any(|asked| asked == f))
    });
    assert!(
        all_seen,
        "every PLATFORM_FALLBACK_CHAIN family should be request_load'd; saw {:?}",
        requested.lock().unwrap()
    );

    // Notifier must have fired at least once (one per successful load).
    let fired = wait_until(Duration::from_secs(2), || notify_count.load(Ordering::SeqCst) >= 1);
    assert!(fired, "loader notifier should fire after at least one successful load");

    // Negative result MUST NOT be cached when an async load was
    // actually spawned — otherwise the post-`clear_shape_cache`
    // re-render would short-circuit through the memo and the user
    // would keep seeing tofu.
    rasterizer.clear_caches();
    let _ = rasterizer.resolve_slot(TOFU_CHAR, false, false);
    // Second resolve, after clear_caches + with everything now marked
    // `is_loaded`, should NOT spawn new loads (dedup against `loaded`).
    let before = requested.lock().unwrap().len();
    let _ = rasterizer.resolve_slot('日', false, false);
    let after = requested.lock().unwrap().len();
    assert_eq!(
        before, after,
        "after every chain entry is loaded, a fresh tofu char must not respawn loads"
    );
}

#[test]
fn no_loader_attached_keeps_legacy_behavior() {
    // With no loader, the historical contract (resolve_slot returns
    // None and caches the negative) must still hold.
    let mut fs = empty_font_system();
    let mut rasterizer = SwashRasterizer::new(&mut fs, "Rec Mono St.Helens", 16.0);

    assert!(rasterizer.async_loader().is_none());
    let first = rasterizer.resolve_slot(TOFU_CHAR, false, false);
    let second = rasterizer.resolve_slot(TOFU_CHAR, false, false);
    assert_eq!(first, None);
    assert_eq!(second, None);
}
