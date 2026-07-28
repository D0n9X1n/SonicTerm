//! Does one long-lived glyph atlas stay bounded across many frames?
//!
//! #888 named **"atlas/cache churn in a long-lived window"** as its strongest
//! lead, on a process that reached 51.1 GB before a fatal `wgpu` OOM. Two
//! existing checks bracket that without covering it: the renderer churn
//! baseline (#1034) opens and closes windows, and the long-session pane soak
//! (#1042) drives `App` state. Neither drives one atlas for a long time.
//!
//! This does. The atlas is a CPU-side structure — `tick_frame`, `evictions`,
//! `retained_amount` — so a long life is a matter of frames driven, not hours
//! waited, and no window or GPU is needed.
//!
//! **It asserts on the index, not the pixel buffer.** A first version bounded
//! `retained_amount().bytes`, which is `pixels.capacity()` — a fixed
//! 512x512x4 texture that cannot change. Baseline, peak, and final sample were
//! byte-identical, so the assertion compared a constant to itself and would
//! have passed against any defect whatsoever. The index (`items`) is the part
//! that can grow, and it is what an atlas leak would grow.
//!
//! **Two assertions, because one is not enough — measured, not assumed.**
//! With `set_eviction_enabled(false)` the index still peaks at 1,024: the
//! atlas stops *admitting* glyphs once full rather than growing, so the bound
//! holds either way and only the eviction assertion fails. That is the same
//! shape the hyperlink registry showed in the sibling soak — memory stays
//! flat while the feature silently dies, every later glyph missing from a
//! full atlas. A memory-only assertion cannot see it.

use sonicterm_text::glyph_atlas::{GlyphAtlas, RasterTile, Rasterizer};
use sonicterm_types::glyph_key::GlyphKey;

/// Frames driven. Each inserts a run of distinct glyphs, so the working set
/// turns over many times against an atlas that is never recreated.
const FRAMES: u64 = 2_000;

/// Distinct glyphs per frame. Wide enough that the atlas cannot hold every
/// frame's set at once, which is what forces eviction rather than a one-time
/// fill.
const GLYPHS_PER_FRAME: u32 = 64;

/// A rasterizer that always succeeds, so the atlas is exercised rather than
/// the rasterizer's failure path.
struct FixedTile(RasterTile);

impl Rasterizer for FixedTile {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(self.0.clone())
    }
}

fn tile(width: u32, height: u32) -> RasterTile {
    RasterTile {
        width,
        height,
        offset_x: 0,
        offset_y: 0,
        advance: width as f32,
        coverage: vec![0xFF; (width * height) as usize],
        is_color: false,
        is_subpixel: false,
    }
}

/// A long-lived atlas stays bounded, and its eviction really runs.
#[test]
fn a_long_lived_atlas_stays_bounded_and_its_eviction_runs() {
    let mut atlas = GlyphAtlas::new(512, 512);
    let mut rasterizer = FixedTile(tile(16, 16));

    let baseline = atlas.retained_amount().items;
    let mut samples = Vec::with_capacity(FRAMES as usize);

    for frame in 0..FRAMES {
        atlas.tick_frame();
        // A fresh glyph_id run per frame: the working set turns over, which is
        // what a long session does as content scrolls through.
        for index in 0..GLYPHS_PER_FRAME {
            let glyph_id = (frame * u64::from(GLYPHS_PER_FRAME)) + u64::from(index);
            let key = GlyphKey {
                ch: char::from_u32(0x41 + (index % 26)).unwrap_or('A'),
                font_slot: (index % 4) as u8,
                weight_bold: index % 2 == 0,
                italic: index % 3 == 0,
                glyph_id: u32::try_from(glyph_id % u64::from(u32::MAX)).unwrap_or(1),
            };
            let _ = atlas.get_or_insert(key, &mut rasterizer);
        }
        // The index, not the pixel buffer: `bytes` is the fixed texture size
        // and is constant by construction.
        samples.push(atlas.retained_amount().items);
    }

    let peak = *samples.iter().max().expect("non-empty");
    let evictions = atlas.evictions();

    println!(
        "frames={FRAMES} glyphs_per_frame={GLYPHS_PER_FRAME} inserted={} baseline_items={baseline} \
         peak_items={peak} last_items={} evictions={evictions} resident={} texture_bytes={}",
        FRAMES * u64::from(GLYPHS_PER_FRAME),
        samples[samples.len() - 1],
        atlas.len(),
        atlas.retained_amount().bytes
    );

    // Without this the run could fill the atlas once and coast, proving
    // nothing about a long session. Eviction is the mechanism a long-lived
    // atlas depends on, so it has to be shown running.
    assert!(
        evictions > 0,
        "no glyph was ever evicted across {FRAMES} frames, so this proves nothing about a \
         long-lived atlas. Raise FRAMES or GLYPHS_PER_FRAME until the working set turns over."
    );

    // The bound, on the index. 128,000 glyphs are inserted over this run; an
    // index that retained even a small fraction of them would be orders of
    // magnitude past what the texture can hold entries for. The ceiling is
    // generous against that scale and still catches any retention.
    const INDEX_CEILING: usize = 8 * 1024;
    let inserted = FRAMES * u64::from(GLYPHS_PER_FRAME);
    assert!(
        peak <= INDEX_CEILING,
        "the atlas index peaked at {peak} entries against a {INDEX_CEILING}-entry ceiling, \
         with {inserted} glyphs inserted across {FRAMES} frames. #888 named atlas churn in a \
         long-lived window as its strongest lead, so an index that keeps everything it has \
         seen is the shape that incident had."
    );

    // A leak that grows and then plateaus at the cap would satisfy both
    // assertions above, so compare the run's halves: after the first
    // turnover the figure should be flat, not still climbing.
    let half = samples.len() / 2;
    let first_half_peak = *samples[..half].iter().max().expect("non-empty");
    let second_half_peak = *samples[half..].iter().max().expect("non-empty");
    assert!(
        second_half_peak <= first_half_peak + first_half_peak / 8,
        "the atlas index was still climbing in the second half of the run: first-half peak \
         {first_half_peak}, second-half peak {second_half_peak}. A bound reached late looks \
         like a bound; a plateau reached early is one."
    );
}
