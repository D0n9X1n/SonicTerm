//! T6/G2A glue — the substitution boundary between the
//! verbatim-vendored `customglyph.rs` (lands in T7) and sonicterm's
//! own types. Customglyph imports `window::{BitmapImage, Image, Point,
//! Rect, Size}` and `window::color::SrgbaPixel`; this module provides
//! the substitutions named in the spec's import table:
//!
//! - `Image`         → [`Bitmap`]      (BGRA-premul `Vec<u8>` buffer)
//! - `BitmapImage`   → [`BitmapImage`] trait (clear_rect + draw_line)
//! - `SrgbaPixel`    → [`BgraPixel`]   (`rgba()`/`a()` accessors)
//! - `Point/Rect/Size` → euclid aliases over this crate's [`PixelUnit`]
//!
//! These are Sonic-native unit aliases used by the converted customglyph
//! code. We keep the same numeric representation as WezTerm, but no longer
//! depend on `wezterm-input-types` or `sonicterm-font::units` for these value
//! types.

/// Phantom marker for raster-pixel geometry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PixelUnit {}
/// Floating-point pixel length used by customglyph metrics.
pub type PixelLength = euclid::Length<f64, PixelUnit>;
/// Integer pixel length used by customglyph metrics.
pub type IntPixelLength = isize;

/// 2-D point in raster pixels, alias-compatible with wezterm's
/// `window::Point`.
pub type Point = euclid::Point2D<isize, PixelUnit>;
/// Axis-aligned rectangle in raster pixels, alias-compatible with
/// wezterm's `window::Rect`.
pub type Rect = euclid::Rect<isize, PixelUnit>;
/// Width × height in raster pixels, alias-compatible with wezterm's
/// `window::Size`.
pub type Size = euclid::Size2D<isize, PixelUnit>;

/// A single BGRA-premultiplied 8-bit pixel — the substitution for
/// wezterm's `window::color::SrgbaPixel`. Field order matches the
/// in-memory byte order written into [`Bitmap::bgra`]: `(b, g, r, a)`.
///
/// Construct via [`Self::rgba`] which reorders R,G,B,A inputs into
/// B,G,R,A storage to match wezterm's `SrgbaPixel::rgba` packing
/// (see wezterm `color-types/src/lib.rs:230`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BgraPixel(pub u8, pub u8, pub u8, pub u8);

impl BgraPixel {
    /// Construct a pixel from sRGBA u8 inputs. Stored as (b, g, r, a).
    /// Matches wezterm's `SrgbaPixel::rgba(red, green, blue, alpha)`
    /// constructor shape so customglyph's `SrgbaPixel::rgba(...)` call
    /// sites compile after the import substitution.
    pub fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self(blue, green, red, alpha)
    }

    /// Alpha channel byte — matches the `.a()` accessor customglyph uses
    /// when promoting alphas through the Poly* path.
    pub fn a(&self) -> u8 {
        self.3
    }

    /// Pack as a host-endian `u32` whose in-memory bytes are
    /// `[b, g, r, a]`. Matches wezterm `SrgbaPixel::as_srgba32` so a
    /// `*pixel_word = color.as_bgra32()` write into a `&mut [u32]`
    /// view of the buffer produces the same byte layout.
    #[inline]
    pub fn as_bgra32(self) -> u32 {
        let Self(b, g, r, a) = self;
        let word = ((b as u32) << 24) | ((g as u32) << 16) | ((r as u32) << 8) | (a as u32);
        word.to_be()
    }
}

/// Read/write surface for a BGRA-premul pixel buffer. Substitution for
/// wezterm's `window::bitmaps::BitmapImage` trait — same default
/// implementations of [`Self::clear_rect`] and [`Self::draw_line`]
/// over the abstract `image_dimensions` + `pixel_data_slice_mut`
/// surface, so concrete buffers (here [`Bitmap`]) get the drawing
/// primitives "for free."
pub trait BitmapImage {
    /// Returns `(width, height)` of the image, measured in pixels.
    fn image_dimensions(&self) -> (usize, usize);

    /// Mutable byte slice over the BGRA buffer. Length is
    /// `width * height * 4`.
    fn pixel_data_slice_mut(&mut self) -> &mut [u8];

    /// Fill `rect` with `color`. Clips to the image bounds; out-of-
    /// bounds rects degrade gracefully (matches wezterm semantics —
    /// see `window/src/bitmaps/mod.rs:184`).
    fn clear_rect(&mut self, rect: Rect, color: BgraPixel) {
        let (dim_w, dim_h) = self.image_dimensions();
        let max_x = rect.max_x().min(dim_w as isize).max(0) as usize;
        let max_y = rect.max_y().min(dim_h as isize).max(0) as usize;
        let dest_x = rect.origin.x.max(0) as usize;
        let dest_y = rect.origin.y.max(0) as usize;
        if dest_x >= dim_w || dest_y >= dim_h {
            return;
        }
        let word = color.as_bgra32();
        let bytes = word.to_ne_bytes();
        let row_stride = dim_w * 4;
        let buf = self.pixel_data_slice_mut();
        for y in dest_y..max_y {
            for x in dest_x..max_x {
                let off = y * row_stride + x * 4;
                buf[off] = bytes[0];
                buf[off + 1] = bytes[1];
                buf[off + 2] = bytes[2];
                buf[off + 3] = bytes[3];
            }
        }
    }

    /// Draw a 1-pixel-wide line from `(x0, y0)` to `(x1, y1)` in
    /// `color`. Bresenham, no anti-aliasing — customglyph does not
    /// call this for the verbatim paste (it routes through tiny-skia
    /// `draw_polys` instead), but the spec lists it as a required
    /// surface for the substitution boundary.
    fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: BgraPixel) {
        let (dim_w, dim_h) = self.image_dimensions();
        let word = color.as_bgra32();
        let bytes = word.to_ne_bytes();
        let row_stride = dim_w * 4;

        let dx = (x1 - x0).abs();
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy: i32 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut x = x0;
        let mut y = y0;
        let buf = self.pixel_data_slice_mut();
        loop {
            if x >= 0 && y >= 0 && (x as usize) < dim_w && (y as usize) < dim_h {
                let off = (y as usize) * row_stride + (x as usize) * 4;
                buf[off] = bytes[0];
                buf[off + 1] = bytes[1];
                buf[off + 2] = bytes[2];
                buf[off + 3] = bytes[3];
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                if x == x1 {
                    break;
                }
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                if y == y1 {
                    break;
                }
                err += dx;
                y += sy;
            }
        }
    }
}

/// BGRA-premultiplied pixel buffer. Substitution for wezterm's
/// `window::bitmaps::Image`. `bgra` byte order per pixel is
/// `[b, g, r, a]` — same as wezterm.
///
/// `width` and `height` are stored as `u32` per the spec's acceptance
/// criterion; the constructor accepts `usize` to keep parity with
/// wezterm's `Image::new(width: usize, height: usize)` so customglyph's
/// `Image::new(metrics.cell_size.width as usize, ...)` call sites
/// compile unmodified after the import substitution.
pub struct Bitmap {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
}

impl Bitmap {
    /// Allocate a `width × height` BGRA buffer initialized to all zeros
    /// (transparent black). Matches wezterm `Image::new` shape.
    pub fn new(width: usize, height: usize) -> Self {
        let w = width as u32;
        let h = height as u32;
        let len = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(4))
            .expect("Bitmap::new: width*height*4 overflows usize");
        Self { bgra: vec![0u8; len], width: w, height: h }
    }

    /// Read-only view of the BGRA byte buffer. Bytes are
    /// `[b, g, r, a]` per pixel in row-major order.
    pub fn bgra(&self) -> &[u8] {
        &self.bgra
    }

    /// Consume the bitmap and return its BGRA byte buffer. Used at the
    /// tail of `customglyph::block_sprite` to hand the rasterized glyph
    /// to the atlas as a `RasterTile { coverage: bytes, .. }` without a
    /// copy (the buffer is already in the BGRA-premul layout the
    /// shader expects when `is_color == true`).
    pub fn into_bgra_vec(self) -> Vec<u8> {
        self.bgra
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Debug-print the pixel buffer at info level. Inherent on the
    /// concrete type to match wezterm's `impl Image { pub fn log_bits }`
    /// — customglyph's `buffer.log_bits()` call site at upstream
    /// `customglyph.rs:6000` resolves here.
    pub fn log_bits(&self) {
        log::info!("Bitmap pixels:");
        let row_stride = (self.width as usize) * 4;
        for y in 0..self.height as usize {
            let mut line = String::new();
            for x in 0..self.width as usize {
                let off = y * row_stride + x * 4;
                line.push_str(&format!(
                    "{:02x}{:02x}{:02x}{:02x} ",
                    self.bgra[off],
                    self.bgra[off + 1],
                    self.bgra[off + 2],
                    self.bgra[off + 3]
                ));
            }
            log::info!("{}", line);
        }
    }
}

impl BitmapImage for Bitmap {
    fn image_dimensions(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }

    fn pixel_data_slice_mut(&mut self) -> &mut [u8] {
        &mut self.bgra
    }
}

/// `block_sprite`'s return payload. Structurally identical to
/// `sonicterm_text::glyph_atlas::RasterTile` (same 7 fields, same
/// `is_empty()` helper) — kept LOCAL to this crate because, at the
/// time T7 lands, `sonicterm-text` does not compile (cosmic-text /
/// `load_font_data_with_sonic_overrides` / `terminal_font_attrs`
/// references the T6 commit message flags as "still mid-flight,
/// scheduled for T10/G2D"). T9's `flush_shape_run` rewire is the
/// natural place to land the trivial field-for-field copy when the
/// consumer wants a `sonicterm_text::glyph_atlas::RasterTile`.
///
/// Once T10 lands the cosmic-text deletes and `sonicterm-text` builds
/// again, this type can be deleted and `block_sprite` can return
/// `sonicterm_text::glyph_atlas::RasterTile` directly — the field
/// shape is identical so the swap is one line in `Cargo.toml` plus
/// one line in `customglyph.rs`.
#[derive(Debug, Clone)]
pub struct BlockRasterTile {
    /// Glyph tile width in pixels.
    pub width: u32,
    /// Glyph tile height in pixels.
    pub height: u32,
    /// Top-left offset of the visible pixels relative to the cell box.
    pub offset_x: i32,
    /// Top-left vertical offset of the visible pixels relative to the cell box.
    pub offset_y: i32,
    /// Horizontal advance after drawing this glyph, in pixels.
    pub advance: f32,
    /// `width * height * 4` bytes of premultiplied BGRA pixels,
    /// row-major (block glyphs always set `is_color = true`).
    pub coverage: Vec<u8>,
    /// Mirrors the `RasterTile` field — always `true` for
    /// `block_sprite` output.
    pub is_color: bool,
}

impl BlockRasterTile {
    /// True when the tile carries any pixels worth uploading.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.coverage.is_empty()
    }
}

/// `RenderMetrics`-shaped record customglyph reads. Structurally
/// identical to the subset of WezTerm render metrics customglyph reads.
///
/// Customglyph reads only `cell_size`, `underline_height`, and
/// (under the `PolyWithCustomMetrics` arm of `block_sprite`)
/// constructs a `RenderMetrics` struct literal naming all six fields,
/// so all six are present here as well.
#[derive(Copy, Clone, Debug)]
pub struct BlockCellMetrics {
    pub descender: PixelLength,
    pub descender_row: IntPixelLength,
    pub descender_plus_two: IntPixelLength,
    pub underline_height: IntPixelLength,
    pub strike_row: IntPixelLength,
    pub cell_size: Size,
}
