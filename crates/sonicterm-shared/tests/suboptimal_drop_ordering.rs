//! Regression guard for CLAUDE.md §4 land-mine:
//! "`wgpu::CurrentSurfaceTexture::Suboptimal(frame)` must drop the
//! SurfaceTexture before calling `surface.configure(...)`. Otherwise wgpu 29
//! panics ('texture still alive')."
//!
//! This is a documentary / source-level test: we cannot mock a real
//! `wgpu::SurfaceTexture` (it's opaque and only obtainable from a live
//! adapter+surface) and a headless wgpu test cannot reliably force the
//! `Suboptimal` branch on every CI host. Instead we parse the source of
//! `crates/sonicterm-shared/src/render/core.rs`, locate the Suboptimal handler,
//! and assert that within that arm `drop(frame)` appears textually BEFORE
//! `self.surface.configure(`.
//!
//! If someone reorders those two calls, the wgpu 29 runtime panic will return
//! — and this test will fail at build/test time so the regression never ships.
//!
//! IMPORTANT: a naïve textual scan can be fooled by a *commented* `drop(frame)`
//! line sitting above an active `surface.configure(...)`. We therefore strip
//! Rust line and block comments (and skip over string / char literal contents)
//! from the arm body BEFORE running the ordering check. See
//! `comment_only_drop_does_not_satisfy_ordering` for the regression that
//! motivated the sanitizer.

use std::fs;
use std::path::PathBuf;

fn render_core_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/render/core.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Slice from `Suboptimal(` to the next `=> {` arm boundary that closes it.
/// We use a depth-tracking brace scan starting at the `{` after `=>`.
///
/// NOTE: this slice can contain comments / string literals. Callers that want
/// to reason about *code* (not comments) must run [`strip_comments_and_strings`]
/// on the result first.
fn extract_suboptimal_arm(src: &str) -> &str {
    let start = src
        .find("CurrentSurfaceTexture::Suboptimal")
        .expect("expected a CurrentSurfaceTexture::Suboptimal match arm in render/core.rs");
    let arrow = src[start..].find("=>").expect("expected `=>` after Suboptimal pattern");
    let body_start_rel =
        src[start + arrow..].find('{').expect("expected `{` opening the Suboptimal arm body");
    let body_open = start + arrow + body_start_rel;

    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut i = body_open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[start..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("could not find end of Suboptimal arm body");
}

/// Strip Rust line comments (`// ...\n`), block comments (`/* ... */`,
/// nesting allowed), and the *contents* of string (`"..."`, including raw
/// strings `r#"..."#`) and char (`'.'`) literals from `src`. Replaces those
/// byte ranges with spaces / newlines so that line numbers stay stable.
///
/// This is intentionally conservative — it does NOT attempt to fully parse
/// Rust — but it is enough to defeat the "commented-out `drop(frame)` fools
/// the scanner" failure mode flagged on PR #206.
fn strip_comments_and_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Line comment: blank to end of line.
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        // Block comment with nesting.
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
        // Raw string: r"..." or r#"..."# (any number of hashes).
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
        // Regular string literal.
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
        // Char literal vs lifetime: only blank if we see a closing `'` within
        // 5 bytes (covers `'\u{XXXX}'` etc.). Otherwise leave alone — it's a
        // lifetime, not a char literal.
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

/// Run the ordering check on already-extracted arm source. Returns `Ok(())`
/// when the source contains an *active* (non-commented, non-stringified)
/// `drop(frame)` strictly before an active `surface.configure(`. Returns
/// `Err(message)` otherwise.
fn check_ordering(arm: &str) -> Result<(), String> {
    let sanitized = strip_comments_and_strings(arm);

    let drop_idx = sanitized
        .find("drop(frame)")
        .ok_or_else(|| format!("no active `drop(frame)` in arm:\n{arm}"))?;
    let configure_idx = sanitized
        .find("surface.configure(")
        .ok_or_else(|| format!("no active `surface.configure(` in arm:\n{arm}"))?;

    if drop_idx < configure_idx {
        Ok(())
    } else {
        Err(format!(
            "drop(frame) at {drop_idx} is not before surface.configure( at {configure_idx}"
        ))
    }
}

#[test]
fn suboptimal_drops_frame_before_reconfiguring_surface() {
    let src = render_core_source();
    let arm = extract_suboptimal_arm(&src);

    if let Err(msg) = check_ordering(arm) {
        panic!(
            "CLAUDE.md §4 land-mine regression: in the Suboptimal arm, \
             `drop(frame)` must appear BEFORE `surface.configure(...)`. \
             Reversing the order makes wgpu 29 panic with 'texture still alive'.\n\n\
             Detail: {msg}\n\nArm body:\n{arm}"
        );
    }
}

// ---------------------------------------------------------------------------
// Sanity tests for the scanner itself — these guarantee the scanner cannot be
// fooled by commented-out code (the failure mode Haiku flagged on PR #206).
// ---------------------------------------------------------------------------

#[test]
fn scanner_accepts_real_drop_before_configure() {
    let arm = "CurrentSurfaceTexture::Suboptimal(frame) => {
            drop(frame);
            self.surface.configure(&self.device, &self.surface_config);
            return;
        }";
    check_ordering(arm).expect("ordering should be accepted when drop(frame) is real and first");
}

#[test]
fn scanner_ignores_commented_drop_above_real_drop() {
    // Even though there's a `// drop(frame);` line above, the real one is
    // still before `surface.configure(`, so this must still pass.
    let arm = "CurrentSurfaceTexture::Suboptimal(frame) => {
            // drop(frame); // historical note: must precede configure
            drop(frame);
            self.surface.configure(&self.device, &self.surface_config);
            return;
        }";
    check_ordering(arm).expect("real drop(frame) should still be detected past a comment");
}

#[test]
fn comment_only_drop_does_not_satisfy_ordering() {
    // The bug Haiku flagged: only a commented `drop(frame)` exists, while an
    // active `surface.configure(` runs first. Scanner MUST reject.
    let arm = "CurrentSurfaceTexture::Suboptimal(frame) => {
            // drop(frame);
            self.surface.configure(&self.device, &self.surface_config);
            return;
        }";
    let err = check_ordering(arm)
        .expect_err("scanner must reject when only a commented drop(frame) is present");
    assert!(
        err.contains("no active `drop(frame)`") || err.contains("not before"),
        "unexpected error message: {err}"
    );
}

#[test]
fn block_comment_drop_does_not_satisfy_ordering() {
    let arm = "CurrentSurfaceTexture::Suboptimal(frame) => {
            /* drop(frame); */
            self.surface.configure(&self.device, &self.surface_config);
            return;
        }";
    let err = check_ordering(arm)
        .expect_err("scanner must reject when only a block-commented drop(frame) is present");
    assert!(
        err.contains("no active `drop(frame)`") || err.contains("not before"),
        "unexpected error message: {err}"
    );
}

#[test]
fn string_literal_drop_does_not_satisfy_ordering() {
    let arm = "CurrentSurfaceTexture::Suboptimal(frame) => {
            log::warn!(\"drop(frame) was skipped\");
            self.surface.configure(&self.device, &self.surface_config);
            return;
        }";
    let err = check_ordering(arm)
        .expect_err("scanner must reject when drop(frame) only appears inside a string literal");
    assert!(
        err.contains("no active `drop(frame)`") || err.contains("not before"),
        "unexpected error message: {err}"
    );
}
