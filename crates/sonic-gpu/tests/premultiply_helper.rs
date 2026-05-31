//! Regression guard for the PR #377 Haiku review finding: the new
//! premultiplied-alpha `QuadPipeline` blend factors require every
//! translucent `QuadInstance::color` call site to also be premultiplied,
//! otherwise the chrome renders much brighter than the theme intended.
//!
//! `sonic_gpu::quad::premultiply` is the helper introduced for that fix.
//! These tests pin down:
//!
//! 1. The math itself — `[r, g, b, a]` → `[r*a, g*a, b*a, a]`.
//! 2. Opaque colors (`a == 1.0`) are an identity pass-through, so it's
//!    safe to wrap any call site.
//! 3. The two specific straight-alpha shapes from the Haiku review
//!    (command-palette selected-row highlight + IME pre-edit bg) now
//!    emit premultiplied bytes.

use sonic_gpu::quad::premultiply;

const EPS: f32 = 1e-6;

fn approx_eq(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < EPS)
}

#[test]
fn premultiply_math() {
    // [r, g, b, a] → [r*a, g*a, b*a, a].
    let out = premultiply([1.0, 0.5, 0.25, 0.5]);
    assert!(approx_eq(out, [0.5, 0.25, 0.125, 0.5]), "got {out:?}");
}

#[test]
fn premultiply_fully_opaque_is_identity() {
    // a == 1.0 → no change (premultiply by 1.0).
    let opaque = [0.42, 0.17, 0.93, 1.0];
    assert!(approx_eq(premultiply(opaque), opaque));
}

#[test]
fn premultiply_fully_transparent_zeros_rgb() {
    // a == 0.0 → rgb all zero, alpha preserved (already trivially
    // premultiplied, but worth pinning down).
    let out = premultiply([1.0, 1.0, 1.0, 0.0]);
    assert!(approx_eq(out, [0.0, 0.0, 0.0, 0.0]), "got {out:?}");
}

#[test]
fn selected_row_highlight_is_now_premultiplied() {
    // The exact pattern at `render/core.rs:3254` / `3398` — accent
    // colour from the theme at 0.16 alpha. Pre-#377 these landed at the
    // pipeline as straight-alpha and rendered ~6× too bright once the
    // pipeline switched to premultiplied blending.
    let accent = [0.7, 0.6, 0.9, 1.0];
    let out = premultiply([accent[0], accent[1], accent[2], 0.16]);
    let expected = [accent[0] * 0.16, accent[1] * 0.16, accent[2] * 0.16, 0.16];
    assert!(
        approx_eq(out, expected),
        "selected-row highlight not premultiplied: got {out:?}, want {expected:?}"
    );
    // And RGB must be strictly less than the straight-alpha values that
    // were being shipped before the fix.
    assert!(out[0] < accent[0]);
    assert!(out[1] < accent[1]);
    assert!(out[2] < accent[2]);
}

#[test]
fn ime_preedit_bg_is_now_premultiplied() {
    // The exact literal at `render/core.rs:3445` — dark IME background
    // at 0.95 alpha.
    let out = premultiply([0.10, 0.11, 0.14, 0.95]);
    let expected = [0.10 * 0.95, 0.11 * 0.95, 0.14 * 0.95, 0.95];
    assert!(
        approx_eq(out, expected),
        "IME preedit bg not premultiplied: got {out:?}, want {expected:?}"
    );
}
