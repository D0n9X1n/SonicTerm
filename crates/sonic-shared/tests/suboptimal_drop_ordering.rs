//! Regression guard for CLAUDE.md §4 land-mine:
//! "`wgpu::CurrentSurfaceTexture::Suboptimal(frame)` must drop the
//! SurfaceTexture before calling `surface.configure(...)`. Otherwise wgpu 29
//! panics ('texture still alive')."
//!
//! This is a documentary / source-level test: we cannot mock a real
//! `wgpu::SurfaceTexture` (it's opaque and only obtainable from a live
//! adapter+surface) and a headless wgpu test cannot reliably force the
//! `Suboptimal` branch on every CI host. Instead we parse the source of
//! `crates/sonic-shared/src/render/core.rs`, locate the Suboptimal handler,
//! and assert that within that arm `drop(frame)` appears textually BEFORE
//! `self.surface.configure(`.
//!
//! If someone reorders those two calls, the wgpu 29 runtime panic will return
//! — and this test will fail at build/test time so the regression never ships.
//!
//! Verified to fail when the order is reversed (see PR body).

use std::fs;
use std::path::PathBuf;

fn render_core_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/render/core.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Slice from `Suboptimal(` to the next `=> {` arm boundary that closes it.
/// We use a depth-tracking brace scan starting at the `{` after `=>`.
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

#[test]
fn suboptimal_drops_frame_before_reconfiguring_surface() {
    let src = render_core_source();
    let arm = extract_suboptimal_arm(&src);

    let drop_idx = arm.find("drop(frame)").unwrap_or_else(|| {
        panic!(
            "CLAUDE.md §4 regression: Suboptimal arm no longer contains \
             `drop(frame)` before `surface.configure(...)`. wgpu 29 will \
             panic ('texture still alive') at runtime.\n\nArm body:\n{arm}"
        )
    });

    let configure_idx = arm.find("surface.configure(").unwrap_or_else(|| {
        panic!(
            "Suboptimal arm no longer calls `surface.configure(...)`; \
             surface won't recover after a Suboptimal frame.\n\nArm body:\n{arm}"
        )
    });

    assert!(
        drop_idx < configure_idx,
        "CLAUDE.md §4 land-mine regression: in the Suboptimal arm, \
         `drop(frame)` (at byte {drop_idx}) must appear BEFORE \
         `surface.configure(...)` (at byte {configure_idx}). \
         Reversing the order makes wgpu 29 panic with 'texture still alive'.\n\n\
         Arm body:\n{arm}"
    );
}
