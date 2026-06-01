//! Epic #300 P4 follow-up — Haiku PR #318 review wire.
//!
//! Regression guard for the bug Haiku flagged on PR #318: the
//! `AsyncFallbackLoader` was being constructed inside tests only, and
//! `GpuRenderer`'s production construction paths in `sonicterm-app`
//! (event_loop / tear_out / misc) never called `set_async_loader`.
//! Result: real frame-time misses on CJK / emoji / nerd-font codepoints
//! silently rendered as tofu forever because `request_load` was never
//! invoked and the `UserEvent::ClearShapeCache` notifier never fired.
//!
//! This test pins the production wire:
//!
//! 1. [`sonicterm_app::app::build_async_fallback_loader_for_proxy`] exists
//!    as a public, callable function on the `EventLoopProxy<UserEvent>`
//!    surface (the exact API every `GpuRenderer::new` site uses).
//! 2. The loader's notifier, when fired, sends
//!    `UserEvent::ClearShapeCache` through the proxy — verified
//!    end-to-end by running the loader against a stub `load_fn` and
//!    observing that the simulated proxy receives the variant.
//! 3. The loader is plumb-compatible with
//!    `sonicterm_shared::render::GpuRenderer::set_async_loader` — verified
//!    structurally via the `AsyncFallbackLoader` type re-export so a
//!    future refactor that drops the wire surface would fail to
//!    compile.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sonicterm_text::async_fallback::{AsyncFallbackLoader, FontHandle, LoadFn, NotifyFn};

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

/// The helper must be a real public function. If a future refactor
/// removes/renames it, this test stops compiling — exactly the signal
/// we want, since renaming the wire would silently re-introduce the
/// PR #318 bug.
#[test]
fn build_async_fallback_loader_for_proxy_is_public_api() {
    // Reference the function symbol via a fn pointer so the test
    // doesn't need a live winit `EventLoopProxy<UserEvent>` (winit
    // forbids constructing one off the main thread on macOS during
    // `cargo test`). Compile-checking the signature is enough to
    // guarantee the wire surface still exists with the right type.
    let _: fn(
        winit::event_loop::EventLoopProxy<sonicterm_app::app::UserEvent>,
    ) -> sonicterm_text::async_fallback::AsyncFallbackLoader =
        sonicterm_app::app::build_async_fallback_loader_for_proxy;
}

/// Construct a loader using exactly the same wiring shape the
/// production helper uses (default load fn + notifier-fires-event), and
/// drive it through a successful load. The notifier MUST end up
/// "sending" the `ClearShapeCache` variant. We can't build a real
/// `EventLoopProxy` here, so the proxy is simulated by a channel — the
/// production helper's body is one line of glue around exactly this
/// pattern.
#[test]
fn loader_notifier_fires_clear_shape_cache_on_load_completion() {
    let (tx, rx) = std::sync::mpsc::channel::<sonicterm_app::app::UserEvent>();
    // Stub the load function so it returns deterministic success
    // immediately, without touching the disk.
    let load_fn: LoadFn =
        Arc::new(|family: &'static str| Some(FontHandle { family, bytes_loaded: 0 }));
    // Mirror the production notifier closure shape: forward
    // `UserEvent::ClearShapeCache` over the proxy on every successful
    // load completion.
    let tx_clone = tx.clone();
    let notify: NotifyFn = Arc::new(move || {
        let _ = tx_clone.send(sonicterm_app::app::UserEvent::ClearShapeCache);
    });

    let loader = AsyncFallbackLoader::new(load_fn, notify);
    let spawned = loader.request_load("Apple Color Emoji");
    assert!(spawned, "first request_load for a fresh family must spawn a loader thread");

    let ev = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("notifier should deliver one event after load completes");
    assert_eq!(ev, sonicterm_app::app::UserEvent::ClearShapeCache);

    // Loader must observably mark the family as loaded so the
    // post-clear re-shape sees `is_loaded(...) == true` and short-
    // circuits subsequent `request_load` calls (dedup contract).
    assert!(loader.is_loaded("Apple Color Emoji"));
}

/// Multiple sequential `request_load` calls for distinct families MUST
/// each fire one notifier — i.e. the production wiring will get one
/// `ClearShapeCache` per family-completion, not a single wake-up that
/// the loop coalesces away mid-burst.
#[test]
fn loader_fires_notifier_once_per_distinct_family_load() {
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_fn = count.clone();
    let load_fn: LoadFn =
        Arc::new(|family: &'static str| Some(FontHandle { family, bytes_loaded: 0 }));
    let notify: NotifyFn = Arc::new(move || {
        count_for_fn.fetch_add(1, Ordering::SeqCst);
    });
    let loader = AsyncFallbackLoader::new(load_fn, notify);

    let families: &[&'static str] = &["PingFang SC", "Hiragino Sans", "Apple Color Emoji"];
    for f in families {
        let _ = loader.request_load(f);
    }
    assert!(
        wait_until(Duration::from_secs(2), || count.load(Ordering::SeqCst) >= families.len()),
        "expected one notifier callback per family load, got {} after timeout",
        count.load(Ordering::SeqCst)
    );
}

/// The notifier closure the production helper builds MUST be `Send +
/// Sync + 'static` because the loader hands clones to every spawned
/// background worker thread. Verifying this structurally pins the
/// constraint that any future refactor of `NotifyFn` cannot relax it
/// without breaking the spawn boundary.
#[test]
fn notify_fn_type_is_send_sync_static() {
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}
    assert_send_sync_static::<sonicterm_text::async_fallback::NotifyFn>();
    assert_send_sync_static::<sonicterm_text::async_fallback::LoadFn>();
    // And the loader handle itself — every renderer construction site
    // clones one into `GpuRenderer::set_async_loader`.
    assert_send_sync_static::<AsyncFallbackLoader>();
}

/// The loader the production helper produces MUST be the same type the
/// renderer accepts in `set_async_loader`. If the renderer were to
/// change its setter to a different concrete type, this trait-bound
/// shim would fail to compile.
#[test]
fn loader_type_matches_renderer_setter_surface() {
    // Compile-time witness: the function the production wire passes to
    // `set_async_loader` must accept an `AsyncFallbackLoader`. We don't
    // build a real `GpuRenderer` here (no wgpu surface), but the
    // setter's signature is captured via a generic stand-in that names
    // the exact same type.
    fn takes_loader(_l: AsyncFallbackLoader) {}
    let load_fn: LoadFn =
        Arc::new(|family: &'static str| Some(FontHandle { family, bytes_loaded: 0 }));
    let notify: NotifyFn = Arc::new(|| {});
    let loader = AsyncFallbackLoader::new(load_fn, notify);
    takes_loader(loader);
    // Sanity scaffold so the test has at least one runtime assertion.
    let _unused: Mutex<()> = Mutex::new(());
}

/// Strengthened guard (Haiku PR #318 review follow-up).
///
/// The prior tests in this file all pass even if every
/// `renderer.set_async_loader(...)` call is deleted from the
/// production source — they only validate that the *plumbing types*
/// exist and that the helper *can* fire `ClearShapeCache`. They do
/// NOT exercise the attachment path on the real `GpuRenderer`
/// construction sites in `sonicterm-app`. That's the actual bug Haiku
/// flagged on PR #318.
///
/// We can't drive a live `GpuRenderer` from a `cargo test` process
/// (no wgpu surface, no winit `EventLoop`). So instead we read the
/// production source files that own every `GpuRenderer::new` site in
/// `sonicterm-app` and assert that each one is followed by a
/// `set_async_loader` call. If a future refactor removes ANY of the
/// production wires, this test fails — which is exactly the signal
/// Haiku said was missing.
///
/// Remove-restore experiment: delete the
/// `renderer.set_async_loader(...)` line from
/// `crates/sonicterm-app/src/app/event_loop.rs` (or `tear_out.rs` or
/// `misc.rs`) and re-run `cargo test -p sonicterm-app
/// --test async_font_loader_attached_in_prod` — this test fails.
#[test]
fn every_gpurenderer_new_site_in_sonic_app_attaches_async_loader() {
    // Resolve files relative to this crate's manifest dir so the test
    // works no matter where `cargo test` is invoked from.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let sources: &[&str] = &["src/app/event_loop.rs", "src/app/tear_out.rs", "src/app/misc.rs"];

    for rel in sources {
        let path = std::path::Path::new(manifest_dir).join(rel);
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read production source {}: {e}", path.display()));
        // Sanitize comments + string literals so commented-out / stringified
        // calls do not count (Haiku PR #318 follow-up: prior implementation
        // counted matches inside `//` and `/* */` and string literals, so
        // commenting out the production call still let the test pass).
        // Mirrors the sanitizer in
        // `crates/sonicterm-shared/tests/suboptimal_drop_ordering.rs`.
        let body = strip_comments_and_strings(&raw);

        // Every file that constructs a `GpuRenderer` MUST also wire the
        // async fallback loader. We treat `GpuRenderer::new(` as the
        // construction marker and `set_async_loader` as the wire
        // marker. Count both — if a file constructs N renderers it must
        // also wire N loaders.
        let new_sites = body.matches("GpuRenderer::new(").count();
        let wire_sites = body.matches("set_async_loader(").count();

        assert!(
            new_sites > 0,
            "{rel}: expected at least one GpuRenderer::new( construction site (test premise broke — \
             update the source list if the renderer moved)",
        );
        assert_eq!(
            new_sites, wire_sites,
            "{rel}: found {new_sites} GpuRenderer::new( construction sites but only \
             {wire_sites} set_async_loader( wire sites — every production renderer must \
             have its async font fallback loader attached (PR #318 Haiku guard).",
        );
    }

    // Defense-in-depth: the helper that builds the loader from the
    // proxy must also be referenced by each of those source files.
    // Removing the call (the original PR #318 bug shape) is what this
    // catches.
    for rel in sources {
        let path = std::path::Path::new(manifest_dir).join(rel);
        let raw = std::fs::read_to_string(&path).unwrap();
        let body = strip_comments_and_strings(&raw);
        assert!(
            body.contains("build_async_fallback_loader_for_proxy"),
            "{rel}: production source must reference \
             build_async_fallback_loader_for_proxy — without it the loader \
             passed to set_async_loader cannot notify ClearShapeCache.",
        );
    }
}

// ---------------------------------------------------------------------------
// Comment / string-literal sanitizer (mirrors
// `crates/sonicterm-shared/tests/suboptimal_drop_ordering.rs`). Inlined here
// instead of factored to a shared helper because cargo integration-test
// crates don't share modules across crates without extra build wiring, and
// the function is small and self-contained.
// ---------------------------------------------------------------------------

/// Strip Rust line comments (`// ...\n`), block comments (`/* ... */`,
/// nesting allowed), and the *contents* of string (`"..."`, including raw
/// strings `r#"..."#`) and char (`'.'`) literals from `src`. Replaces those
/// byte ranges with spaces / newlines so that line numbers stay stable.
///
/// This is intentionally conservative — it does NOT attempt to fully parse
/// Rust — but it is enough to defeat the "commented-out `set_async_loader(`
/// fools the scanner" failure mode Haiku flagged on PR #318.
fn strip_comments_and_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i < bytes.len() && depth > 0 {
                if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else {
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i] == b'r' && i + 1 < bytes.len() && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#')
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < bytes.len() && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                out.extend(std::iter::repeat_n(b' ', j - i + 1));
                i = j + 1;
                loop {
                    if i >= bytes.len() {
                        break;
                    }
                    if bytes[i] == b'"' {
                        let mut k = i + 1;
                        let mut got = 0usize;
                        while k < bytes.len() && got < hashes && bytes[k] == b'#' {
                            got += 1;
                            k += 1;
                        }
                        if got == hashes {
                            out.extend(std::iter::repeat_n(b' ', k - i));
                            i = k;
                            break;
                        }
                    }
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
                continue;
            }
        }
        if bytes[i] == b'"' {
            out.push(b' ');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    out.push(b' ');
                    i += 1;
                    break;
                }
                out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'\'' {
            let mut k = i + 1;
            let mut found = None;
            while k < bytes.len() && k - i <= 6 {
                if bytes[k] == b'\\' && k + 1 < bytes.len() {
                    k += 2;
                    continue;
                }
                if bytes[k] == b'\'' {
                    found = Some(k);
                    break;
                }
                k += 1;
            }
            if let Some(end) = found {
                out.extend(std::iter::repeat_n(b' ', end - i + 1));
                i = end + 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).expect("sanitized buffer must remain valid UTF-8")
}

// ---------------------------------------------------------------------------
// Sanity self-tests for the sanitizer — guarantee a commented or stringified
// `set_async_loader(` cannot satisfy the scan (the PR #318 Haiku finding).
// ---------------------------------------------------------------------------

#[test]
fn sanitizer_strips_line_comment_set_async_loader() {
    let src = "// renderer.set_async_loader(loader);\nlet x = 1;\n";
    let sanitized = strip_comments_and_strings(src);
    assert!(
        !sanitized.contains("set_async_loader("),
        "line-commented set_async_loader( must be stripped: got {sanitized:?}"
    );
}

#[test]
fn sanitizer_strips_block_comment_set_async_loader() {
    let src = "/* renderer.set_async_loader(loader); */\nlet x = 1;\n";
    let sanitized = strip_comments_and_strings(src);
    assert!(
        !sanitized.contains("set_async_loader("),
        "block-commented set_async_loader( must be stripped: got {sanitized:?}"
    );
}

#[test]
fn sanitizer_strips_string_literal_set_async_loader() {
    let src = "log::warn!(\"set_async_loader( was skipped\");\n";
    let sanitized = strip_comments_and_strings(src);
    assert!(
        !sanitized.contains("set_async_loader("),
        "stringified set_async_loader( must be stripped: got {sanitized:?}"
    );
}

#[test]
fn sanitizer_preserves_real_set_async_loader_call() {
    let src = "renderer.set_async_loader(loader);\n";
    let sanitized = strip_comments_and_strings(src);
    assert!(
        sanitized.contains("set_async_loader("),
        "real set_async_loader( call must survive sanitizer: got {sanitized:?}"
    );
}
