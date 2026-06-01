//! render_throughput — criterion microbench for the CPU side of the render path.
//!
//! Real frame throughput needs a wgpu surface (see CLAUDE.md §13's GUI smoke);
//! this bench instead covers the hot pure-CPU functions the render pipeline
//! calls per-frame and per-cell so the perf-bench gate can spot algorithmic
//! regressions without booting a window.  Add a new bench function here when
//! you add a new hot pure-CPU helper under `sonicterm_shared::render::*`.
//!
//! Run with: `cargo bench -p sonicterm-shared --bench render_throughput`.
#![allow(missing_docs)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use sonicterm_shared::render::color::{hex_to_rgba, hex_to_wgpu, srgb_u8_to_linear_lut};

fn bench_hex_to_rgba(c: &mut Criterion) {
    // Theme files contain ~30 hex colors; renderer parses them on every
    // theme reload and on each tab-bar palette refresh.
    let palette = [
        "#1a1b26", "#a9b1d6", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff",
        "#414868", "#f7768e", "#9ece6a", "#e0af68",
    ];
    c.bench_function("hex_to_rgba/palette12", |b| {
        b.iter(|| {
            let mut acc = [0.0f32; 4];
            for h in &palette {
                let rgba = hex_to_rgba(black_box(h), 1.0);
                for i in 0..4 {
                    acc[i] += rgba[i];
                }
            }
            black_box(acc)
        });
    });
}

fn bench_hex_to_wgpu(c: &mut Criterion) {
    c.bench_function("hex_to_wgpu/single", |b| {
        b.iter(|| black_box(hex_to_wgpu(black_box("#1a1b26"))));
    });
}

fn bench_srgb_lut(c: &mut Criterion) {
    // The sRGB→linear LUT is the hottest helper in the per-cell bg path
    // (one lookup per channel per cell).  At a typical 200×60 grid that's
    // 36 000 lookups per redraw; the bench drives 10× that to amortize
    // criterion's measurement floor.
    c.bench_function("srgb_lut/360k_lookups", |b| {
        let lut = srgb_u8_to_linear_lut();
        b.iter(|| {
            let mut acc = 0.0f32;
            for i in 0..360_000u32 {
                acc += lut[(i & 0xff) as usize];
            }
            black_box(acc)
        });
    });
}

criterion_group!(benches, bench_hex_to_rgba, bench_hex_to_wgpu, bench_srgb_lut);
criterion_main!(benches);
