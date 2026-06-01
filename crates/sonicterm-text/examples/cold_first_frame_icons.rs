//! Cold-first-frame icon-shape bench for issue #415.
//!
//! Measures the wall-clock cost of shaping + atlas-inserting a
//! representative status-line payload of Nerd Font PUA icons starting
//! from a freshly-constructed FontSystem / SwashRasterizer /
//! GlyphAtlas. By default the renderer's startup pre-warm
//! (`prewarm::prewarm_ascii_and_nerd`) runs first — set the env var
//! `SONIC_BENCH_NO_PREWARM=1` to skip it and observe the
//! before-the-fix cost.
//!
//! Output line (consumed by `scripts/bench.sh`):
//!
//!     BENCH cold_first_frame_icons_ms <float>

use cosmic_text::FontSystem;
use sonicterm_text::glyph_atlas::GlyphAtlas;
use sonicterm_text::prewarm::prewarm_ascii_and_nerd;
use sonicterm_text::shape::{shape_run, RunStyle};
use sonicterm_text::swash_rasterizer::{load_bundled_fonts, SwashRasterizer};
use sonicterm_types::{Cell, CellFlags, Color};

// Representative payload — what nvim + lualine + nvim-tree paint into
// 30 rows on launch: a mix of devicons + FontAwesome status icons
// known to live in the pre-warm range. Total 30 lines × ~6 icons.
const ICONS: &[char] = &[
    '\u{e702}', // devicons git
    '\u{e7a8}', // devicons rust
    '\u{e718}', // devicons npm
    '\u{e7c5}', // devicons logo
    '\u{f015}', // fa home
    '\u{f07b}', // fa folder
    '\u{f0c7}', // fa floppy
    '\u{f013}', // fa cog
    '\u{f0e7}', // fa bolt
    '\u{f0eb}', // fa lightbulb
    '\u{f120}', // fa terminal
    '\u{f126}', // fa branch
];

fn main() {
    // Use a Nerd-Font-patched family as primary so the pre-warm
    // gate (`primary_has_glyph`) actually claims the icon range.
    // This mirrors the real-world setup that issue #415 reproduces
    // on (the user's config names a Nerd Font as `font.family`).
    let font_family = "Rec Mono St.Helens";
    let font_size = 14.0_f32;
    let scale = 2.0_f32;

    let mut font_system = FontSystem::new();
    load_bundled_fonts(&mut font_system);
    let mut atlas = GlyphAtlas::new(2048, 2048);

    let skip_prewarm = std::env::var("SONIC_BENCH_NO_PREWARM").ok().as_deref() == Some("1");

    {
        let mut raster = SwashRasterizer::new(&mut font_system, font_family, font_size * scale);
        if !skip_prewarm {
            let _ = prewarm_ascii_and_nerd(&mut raster, &mut atlas);
        }
    }

    // Build 30 rows of icon cells.
    let mut rows: Vec<Vec<(u16, Cell)>> = Vec::with_capacity(30);
    for _ in 0..30 {
        let mut row = Vec::with_capacity(ICONS.len());
        for (i, ch) in ICONS.iter().enumerate() {
            row.push((
                i as u16,
                Cell::plain(*ch, Color::Default, Color::Default, CellFlags::empty()),
            ));
        }
        rows.push(row);
    }

    // Measure shape + atlas insertion on a NEW rasterizer instance —
    // this simulates the first paint after launch.
    let mut raster = SwashRasterizer::new(&mut font_system, font_family, font_size * scale);
    let style = RunStyle { bold: false, italic: false };

    let t0 = std::time::Instant::now();
    for row in &rows {
        let shaped = shape_run(&mut raster, font_family, font_size * scale, style, row);
        for g in shaped {
            // Ensure the glyph is in the atlas (mirrors the
            // text-pipeline's per-glyph `get_or_insert`).
            let key = sonicterm_types::GlyphKey::with_slot(g.ch, g.font_slot, false, false);
            let _ = atlas.get_or_insert(key, &mut raster);
        }
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    println!(
        "BENCH cold_first_frame_icons_ms {:.3}  (prewarm={})",
        elapsed_ms,
        if skip_prewarm { "off" } else { "on" }
    );
}
