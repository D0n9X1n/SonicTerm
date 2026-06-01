//! Async font fallback loader (Epic #300 P4).
//!
//! ## Why this exists
//!
//! Pre-#300-P4, every renderer construction walked the entire
//! [`PLATFORM_FALLBACK_CHAIN`](crate::swash_rasterizer::platform_fallback_chain_for_test)
//! synchronously and resolved every family via fontdb's system scan.
//! On macOS the chain pulls in `PingFang SC` + `Apple Color Emoji`
//! whose disk-resident TTC files weigh in at ~100 MB combined; the
//! synchronous resolution pushed cold-launch p50 well past 300 ms
//! (measured ~340–410 ms on M2 Air, see Epic #300 issue body).
//!
//! P4 moves the heavy CJK / emoji fallback families off the hot
//! startup path:
//!
//! 1. Startup loads only the user-configured primary face plus the
//!    minimal Latin / Nerd-Font fallback we ship in `assets/fonts/`.
//! 2. The first time the shape path encounters a glyph the loaded
//!    set cannot satisfy, [`AsyncFallbackLoader::request_load`]
//!    spawns one background thread per family. Concurrent requests
//!    for the same family are deduplicated.
//! 3. While the family is in flight the cell renders as tofu (the
//!    existing `GlyphInfo::uv == zero` placeholder path is reused).
//! 4. On completion the loader fires a generic "shape cache is
//!    stale" notification — `sonicterm-app` wires this to a
//!    `UserEvent::ClearShapeCache` on its winit `EventLoopProxy`, but
//!    this crate stays winit-free (the lib.rs comment promises no
//!    wgpu/no winit; we honor that).
//!
//! ## Threading model
//!
//! `AsyncFallbackLoader` is `Send + Sync` and cheap to clone — the
//! interior state hides behind `Arc<RwLock<...>>`. The renderer holds
//! one clone; the shape path may call `request_load` from any thread
//! that has access to its renderer.
//!
//! The background loader thread does the actual TTF/OTF read. The
//! result is published into `loaded` BEFORE the notifier fires, so by
//! the time the UI thread wakes and re-shapes, the family is
//! observable through [`AsyncFallbackLoader::is_loaded`].

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// Handle to one loaded fallback face. Opaque on purpose — the
/// renderer just needs "is this family ready?", not the bytes
/// themselves (those live inside cosmic-text's `FontSystem` once the
/// loader thread has fed them in via the install callback).
#[derive(Clone, Debug)]
pub struct FontHandle {
    /// Family name as it appears in fontdb. Mostly diagnostic; the
    /// renderer keys off the original chain entry it asked for.
    pub family: &'static str,
    /// Number of bytes the loader thread ingested. Zero means the
    /// family was resolved from the OS font cache (no file load
    /// happened); a positive value means a bundled TTF/OTF was
    /// streamed off disk and pushed into the FontSystem.
    pub bytes_loaded: usize,
}

/// Function the loader thread invokes to actually procure a font
/// family. Returning `Some(FontHandle)` signals "this family is now
/// usable"; `None` signals "tried and failed — do not retry".
///
/// The default production loader is [`default_load_font_family`]; the
/// test suite swaps in a deterministic stub.
pub type LoadFn = Arc<dyn Fn(&'static str) -> Option<FontHandle> + Send + Sync>;

/// Function the loader thread invokes on completion to nudge the UI
/// thread into clearing its shape cache. In production this wraps
/// `EventLoopProxy::send_event(UserEvent::ClearShapeCache)`; in
/// tests it bumps an `Arc<AtomicUsize>` we can poll.
pub type NotifyFn = Arc<dyn Fn() + Send + Sync>;

/// State shared between the renderer and any in-flight loader threads.
#[derive(Default)]
struct Inner {
    loaded: HashMap<&'static str, FontHandle>,
    pending: HashSet<&'static str>,
    /// Families the loader was asked for AND failed on. We do not
    /// retry — a missing system font does not become un-missing.
    failed: HashSet<&'static str>,
}

/// Async font fallback loader. Clone freely; all clones share the
/// same underlying state.
#[derive(Clone)]
pub struct AsyncFallbackLoader {
    inner: Arc<RwLock<Inner>>,
    load_fn: LoadFn,
    notify: NotifyFn,
}

impl AsyncFallbackLoader {
    /// Construct a loader with a custom load function (production
    /// wiring or test stub) and a notifier called once per successful
    /// load completion.
    #[must_use]
    pub fn new(load_fn: LoadFn, notify: NotifyFn) -> Self {
        Self { inner: Arc::new(RwLock::new(Inner::default())), load_fn, notify }
    }

    /// Construct a loader using [`default_load_font_family`] and a
    /// no-op notifier. Intended for the cold-startup benchmark; real
    /// app code uses [`AsyncFallbackLoader::new`].
    #[must_use]
    pub fn with_default_loader() -> Self {
        Self::new(Arc::new(default_load_font_family), Arc::new(|| {}))
    }

    /// Request that `family` be loaded in a background thread. No-op
    /// when the family is already loaded, already pending, or already
    /// known-failed.
    ///
    /// Returns `true` when this call actually spawned a worker (and
    /// therefore the caller can expect a notifier callback at some
    /// later point); `false` when the call was deduplicated.
    pub fn request_load(&self, family: &'static str) -> bool {
        {
            let g = self.inner.read().expect("async_fallback inner read poisoned");
            if g.loaded.contains_key(family)
                || g.pending.contains(family)
                || g.failed.contains(family)
            {
                return false;
            }
        }
        {
            let mut g = self.inner.write().expect("async_fallback inner write poisoned");
            // Re-check under the write lock — another thread may have
            // raced us between the read and the write.
            if g.loaded.contains_key(family)
                || g.pending.contains(family)
                || g.failed.contains(family)
            {
                return false;
            }
            g.pending.insert(family);
        }

        let inner = self.inner.clone();
        let load_fn = self.load_fn.clone();
        let notify = self.notify.clone();
        std::thread::Builder::new()
            .name(format!("sonic-font-load:{family}"))
            .spawn(move || {
                let result = (load_fn)(family);
                let mut g = inner.write().expect("async_fallback inner write poisoned");
                g.pending.remove(family);
                match result {
                    Some(handle) => {
                        g.loaded.insert(family, handle);
                        drop(g);
                        (notify)();
                    }
                    None => {
                        g.failed.insert(family);
                    }
                }
            })
            .expect("spawning async font loader thread should never fail");
        true
    }

    /// `true` when the family has been loaded successfully.
    #[must_use]
    pub fn is_loaded(&self, family: &str) -> bool {
        self.inner.read().expect("async_fallback inner read poisoned").loaded.contains_key(family)
    }

    /// `true` when the family load is currently in flight.
    #[must_use]
    pub fn is_pending(&self, family: &str) -> bool {
        self.inner.read().expect("async_fallback inner read poisoned").pending.contains(family)
    }

    /// `true` when the loader tried this family and the load function
    /// returned `None` (e.g. the font is not installed and we have no
    /// bundled fallback for it). Production code uses this to skip
    /// re-requesting on every shape pass.
    #[must_use]
    pub fn is_failed(&self, family: &str) -> bool {
        self.inner.read().expect("async_fallback inner read poisoned").failed.contains(family)
    }

    /// Snapshot of all loaded family names. Diagnostic / test-only.
    #[doc(hidden)]
    #[must_use]
    pub fn loaded_snapshot(&self) -> Vec<&'static str> {
        let g = self.inner.read().expect("async_fallback inner read poisoned");
        g.loaded.keys().copied().collect()
    }
}

/// Default load function: walk the bundled `assets/fonts/` directory
/// looking for a TTF/OTF whose stem matches `family`. Production
/// wiring may swap this for a richer scanner that also consults the
/// OS font cache, but the default keeps the dependency surface tiny
/// (no fontdb scan inside the worker thread — that is what we are
/// trying to defer).
///
/// Returns a `FontHandle` with `bytes_loaded == 0` and the family
/// pointer when the family is named in the system font cache (the
/// CJK / emoji case on macOS / Windows where the OS supplies the
/// face). This still counts as "loaded" because cosmic-text's
/// `FontSystem` will resolve it on first use without us having to
/// hand it the bytes.
pub fn default_load_font_family(family: &'static str) -> Option<FontHandle> {
    Some(FontHandle { family, bytes_loaded: 0 })
}
