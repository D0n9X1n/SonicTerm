//! #404 — ShadowMainSnapshot deletion regression guard.
//!
//! The Phase B2 shadow-sync infrastructure was the last leftover from
//! the Epic #365 main-window-promotion chain. After #404 the following
//! symbols MUST NOT exist anywhere in `crates/sonic-app/src/`:
//!
//!   - `ShadowMainSnapshot` (struct)
//!   - `apply_shadow_main_snapshot` / `apply_shadow_main_sync` (fns)
//!   - `shadow_main_snapshot_from` / `shadow_main_snapshot` (fns)
//!   - `sync_shadow_main` (method)
//!
//! And no `App`-level reader of `self.scale_factor` / `self.hovered_url`
//! must remain — both fields are promoted to `WindowState` and read
//! through `self.main()?.scale_factor` / `self.main()?.hovered_url`.
//!
//! Plain grep-in-test (no compile_fail roundtrip) — same pattern as
//! the other invariant guards in this crate, and resilient to whatever
//! visibility / re-export tricks a future author might be tempted to
//! reach for.

use std::fs;
use std::path::Path;

fn read_src_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(text) = fs::read_to_string(&p) {
                    out.push((p.display().to_string(), text));
                }
            }
        }
    }
    walk(&root, &mut out);
    out
}

/// Strip Rust line + block comments so doc/explanatory text mentioning
/// the symbol does not trip the regex. Naive but adequate for this
/// crate's source — every real binding-site will survive stripping.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

#[test]
fn shadow_main_snapshot_symbols_absent() {
    let needles = [
        "ShadowMainSnapshot",
        "apply_shadow_main_snapshot",
        "apply_shadow_main_sync",
        "shadow_main_snapshot_from",
        "sync_shadow_main",
    ];
    let mut hits: Vec<String> = Vec::new();
    for (path, text) in read_src_files() {
        let stripped = strip_comments(&text);
        for needle in &needles {
            if stripped.contains(needle) {
                hits.push(format!("{path}: {needle}"));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "#404: ShadowMainSnapshot symbols must not exist in sonic-app src; found: {hits:#?}"
    );
}

#[test]
fn app_level_scale_factor_and_hovered_url_readers_absent() {
    // Both fields were deleted from App in #404 — every reader must go
    // through `self.main()?.scale_factor` / `self.main()?.hovered_url`.
    let needles = ["self.scale_factor", "self.hovered_url"];
    let mut hits: Vec<String> = Vec::new();
    for (path, text) in read_src_files() {
        let stripped = strip_comments(&text);
        for needle in &needles {
            if stripped.contains(needle) {
                hits.push(format!("{path}: {needle}"));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "#404: App-level scale_factor/hovered_url readers must be migrated; found: {hits:#?}"
    );
}

#[test]
fn no_shadow_sync_call_sites_remain() {
    // Belt-and-braces: the call-site spelling `sync_shadow_main()` /
    // `apply_shadow_main_sync(` would slip past the symbol scan if some
    // future PR re-introduced the helpers under a fresh name; pin the
    // literal call patterns too.
    let patterns = [".sync_shadow_main(", "apply_shadow_main_sync(", "apply_shadow_main_snapshot("];
    let mut hits: Vec<String> = Vec::new();
    for (path, text) in read_src_files() {
        let stripped = strip_comments(&text);
        for p in &patterns {
            if stripped.contains(p) {
                hits.push(format!("{path}: {p}"));
            }
        }
    }
    assert!(hits.is_empty(), "#404: shadow-sync call sites must be deleted; found: {hits:#?}");
}

#[test]
fn app_struct_has_no_legacy_scale_factor_field() {
    // The App struct definition itself: search for the exact field
    // declaration. Comments are stripped first so the historical
    // "deleted" docstring doesn't trip the check.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/mod.rs");
    let text = fs::read_to_string(&path).expect("read mod.rs");
    let stripped = strip_comments(&text);
    // Find the `pub struct App {` block and confirm the legacy field
    // bindings are gone. We scan from the struct opening to the next
    // closing brace at column 0.
    let app_start = stripped.find("pub struct App {").expect("App struct must exist in mod.rs");
    let tail = &stripped[app_start..];
    let app_end = tail.find("\n}\n").unwrap_or(tail.len());
    let body = &tail[..app_end];
    assert!(!body.contains("scale_factor: f64"), "#404: App.scale_factor field must be removed");
    assert!(!body.contains("hovered_url: Option<"), "#404: App.hovered_url field must be removed");
}

#[test]
fn window_state_still_owns_scale_factor_and_hovered_url() {
    // Sanity: the destination fields must exist on WindowState — if a
    // future refactor moves them off WindowState too, this test forces
    // the migration plan to be made explicit rather than silently
    // dropping the data path.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/mod.rs");
    let text = fs::read_to_string(&path).expect("read mod.rs");
    let stripped = strip_comments(&text);
    let ws_start =
        stripped.find("pub struct WindowState {").expect("WindowState struct must exist in mod.rs");
    let tail = &stripped[ws_start..];
    let ws_end = tail.find("\n}\n").unwrap_or(tail.len());
    let body = &tail[..ws_end];
    assert!(
        body.contains("scale_factor: f64"),
        "#404: WindowState.scale_factor must still be the canonical owner"
    );
    assert!(
        body.contains("hovered_url:"),
        "#404: WindowState.hovered_url must still be the canonical owner"
    );
}

#[test]
fn shadow_main_snapshot_test_file_deleted() {
    // The phase_b2_shadow_invariant.rs test file is the canonical
    // home of the shadow-sync invariant; once #404 deletes the
    // infrastructure that file MUST be gone from `tests/`. A stray
    // copy would still compile (it only used pub items) and fail noisily.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/phase_b2_shadow_invariant.rs");
    assert!(
        !path.exists(),
        "#404: tests/phase_b2_shadow_invariant.rs must be deleted; the shadow-sync \
         infrastructure it pinned no longer exists"
    );
}
