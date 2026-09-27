//! Glue — the substitution boundary between the verbatim-vendored
//! `customglyph.rs` and sonicterm's own types. Customglyph imports
//! `window::{BitmapImage, Image, Point, Rect, Size}` and
//! `window::color::SrgbaPixel`; this module provides the substitutions:
//!
//! - `Image`         → [`Bitmap`]      (BGRA-premul `Vec<u8>` buffer)
//! - `BitmapImage`   → [`BitmapImage`] trait (clear_rect + draw_line)
//! - `SrgbaPixel`    → [`BgraPixel`]   (`rgba()`/`alpha()` accessors)
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

    /// Alpha channel byte: the fourth stored component.
    pub fn alpha(&self) -> u8 {
        self.3
    }

    /// Pack as a host-endian `u32` whose in-memory bytes are
    /// `[b, g, r, a]`. Matches wezterm `SrgbaPixel::as_srgba32` so a
    /// `*pixel_word = color.as_bgra32()` write into a `&mut [u32]`
    /// view of the buffer produces the same byte layout.
    #[inline]
    pub fn as_bgra32(self) -> u32 {
        let Self(blue, green, red, alpha) = self;
        let word =
            ((blue as u32) << 24) | ((green as u32) << 16) | ((red as u32) << 8) | (alpha as u32);
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
            // When: `dest_x` or `dest_y` starts outside the image, clipping leaves no pixels to clear.
            return;
        }
        let word = color.as_bgra32();
        let bytes = word.to_ne_bytes();
        let row_stride = dim_w * 4;
        let buf = self.pixel_data_slice_mut();
        for pixel_y in dest_y..max_y {
            for pixel_x in dest_x..max_x {
                let off = pixel_y * row_stride + pixel_x * 4;
                buf[off] = bytes[0];
                buf[off + 1] = bytes[1];
                buf[off + 2] = bytes[2];
                buf[off + 3] = bytes[3];
            }
        }
    }

    /// Draw a 1-pixel-wide line from `(start_x, start_y)` to `(end_x, end_y)` in
    /// `color`. Bresenham, no anti-aliasing — customglyph does not
    /// call this for the verbatim paste (it routes through tiny-skia
    /// `draw_polys` instead), but the spec lists it as a required
    /// surface for the substitution boundary.
    fn draw_line(&mut self, start_x: i32, start_y: i32, end_x: i32, end_y: i32, color: BgraPixel) {
        let (dim_w, dim_h) = self.image_dimensions();
        let word = color.as_bgra32();
        let bytes = word.to_ne_bytes();
        let row_stride = dim_w * 4;

        let delta_x = (end_x - start_x).abs();
        let step_x: i32 = if start_x < end_x { 1 } else { -1 };
        let delta_y = -(end_y - start_y).abs();
        let step_y: i32 = if start_y < end_y { 1 } else { -1 };
        let mut err = delta_x + delta_y;
        let mut pixel_x = start_x;
        let mut pixel_y = start_y;
        let buf = self.pixel_data_slice_mut();
        loop {
            if pixel_x >= 0
                && pixel_y >= 0
                && (pixel_x as usize) < dim_w
                && (pixel_y as usize) < dim_h
            {
                let off = (pixel_y as usize) * row_stride + (pixel_x as usize) * 4;
                buf[off] = bytes[0];
                buf[off + 1] = bytes[1];
                buf[off + 2] = bytes[2];
                buf[off + 3] = bytes[3];
            }
            if pixel_x == end_x && pixel_y == end_y {
                // When: `pixel_x == end_x` and `pixel_y == end_y`, both coordinates reached the endpoint and the line is complete.
                break;
            }
            let doubled_error = 2 * err;
            if doubled_error >= delta_y {
                // When: `doubled_error >= delta_y`, the Bresenham error permits one horizontal step.
                if pixel_x == end_x {
                    // When: `pixel_x` already equals `end_x`, another horizontal step would overshoot the endpoint.
                    break;
                }
                err += delta_y;
                pixel_x += step_x;
            }
            if doubled_error <= delta_x {
                // When: `doubled_error <= delta_x`, the Bresenham error permits one vertical step.
                if pixel_y == end_y {
                    // When: `pixel_y` already equals `end_y`, another vertical step would overshoot the endpoint.
                    break;
                }
                err += delta_x;
                pixel_y += step_y;
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
        let stored_width = width as u32;
        let stored_height = height as u32;
        let len = width
            .checked_mul(height)
            .and_then(|pixel_count| pixel_count.checked_mul(4))
            .expect("Bitmap::new: width*height*4 overflows usize");
        Self { bgra: vec![0u8; len], width: stored_width, height: stored_height }
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
        for pixel_y in 0..self.height as usize {
            let mut line = String::new();
            for pixel_x in 0..self.width as usize {
                let off = pixel_y * row_stride + pixel_x * 4;
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

/// `block_sprite`'s return payload: a tile of premultiplied BGRA pixels. The
/// type is local because this crate depends on no first-party crate (see the
/// `Cargo.toml` leaf rule). The GPU renderer's `flush_shape_run` converts it to
/// a `sonicterm_text::glyph_atlas::RasterTile`: it keeps the width, height,
/// offsets, and advance, extracts each pixel's alpha byte as the coverage
/// mask, and sets `is_color` and `is_subpixel` to `false`.
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
    /// True when the tile has no pixels to upload: zero width or height, or no coverage.
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
