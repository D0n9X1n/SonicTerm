//! SonicTerm font color primitives absorbed from WezTerm's color-types.

use std::sync::LazyLock;

static RGB_TO_SRGB_TABLE: LazyLock<[u8; 256]> = LazyLock::new(generate_rgb_to_srgb8_table);

fn generate_rgb_to_srgb8_table() -> [u8; 256] {
    let mut table = [0; 256];
    for (val, entry) in table.iter_mut().enumerate() {
        let linear = (val as f32) / 255.0;
        *entry = linear_f32_to_srgb8(linear);
    }
    table
}

fn linear_f32_to_srgb8(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let srgb = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        // When: `linear <= 0.003_130_8` is false, apply the nonlinear sRGB transfer curve.
        linear.powf(1.0 / 2.4) * 1.055 - 0.055
    };
    (srgb * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Converts an eight-bit linear-light channel to its eight-bit sRGB encoding.
pub fn linear_u8_to_srgb8(linear: u8) -> u8 {
    RGB_TO_SRGB_TABLE[linear as usize]
}

/// A pixel holding SRGBA32 data in big-endian format.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SrgbaPixel(u32);

impl SrgbaPixel {
    /// Packs red, green, blue, and alpha bytes into the stored big-endian pixel layout.
    pub fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        let word = (blue as u32) << 24 | (green as u32) << 16 | (red as u32) << 8 | alpha as u32;
        Self(word.to_be())
    }

    /// Unpacks this pixel into red, green, blue, and alpha bytes.
    pub fn as_rgba(self) -> (u8, u8, u8, u8) {
        let host = u32::from_be(self.0);
        ((host >> 8) as u8, (host >> 16) as u8, (host >> 24) as u8, (host & 0xff) as u8)
    }

    /// Returns the stored big-endian SRGBA32 word.
    pub fn as_srgba32(self) -> u32 {
        self.0
    }

    /// Converts this pixel to normalized red, green, blue, and alpha components.
    pub fn as_srgba_tuple(self) -> (f32, f32, f32, f32) {
        let SrgbaTuple(red, green, blue, alpha) = self.into();
        (red, green, blue, alpha)
    }
}

/// A pixel value encoded as SRGBA RGBA values in f32 format (0.0..=1.0).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SrgbaTuple(pub f32, pub f32, pub f32, pub f32);

impl SrgbaTuple {
    /// Multiplies each color channel by alpha while preserving alpha.
    pub fn premultiply(self) -> Self {
        let Self(red, green, blue, alpha) = self;
        Self(red * alpha, green * alpha, blue * alpha, alpha)
    }

    /// Divides premultiplied color channels by nonzero alpha.
    pub fn demultiply(self) -> Self {
        let Self(red, green, blue, alpha) = self;
        if alpha != 0.0 {
            Self(red / alpha, green / alpha, blue / alpha, alpha)
        } else {
            // When: `alpha != 0.0` is false, preserve transparent channels without division.
            self
        }
    }

    /// Interpolates two colors in premultiplied-alpha space by `factor`.
    pub fn interpolate(self, other: Self, factor: f64) -> Self {
        let factor = factor as f32;
        let Self(start_red, start_green, start_blue, start_alpha) = self.premultiply();
        let Self(end_red, end_green, end_blue, end_alpha) = other.premultiply();
        Self(
            start_red + factor * (end_red - start_red),
            start_green + factor * (end_green - start_green),
            start_blue + factor * (end_blue - start_blue),
            start_alpha + factor * (end_alpha - start_alpha),
        )
        .demultiply()
    }
}

impl From<SrgbaPixel> for SrgbaTuple {
    fn from(pixel: SrgbaPixel) -> Self {
        pixel.as_rgba().into()
    }
}

impl From<(u8, u8, u8, u8)> for SrgbaTuple {
    fn from((red, green, blue, alpha): (u8, u8, u8, u8)) -> Self {
        Self(red as f32 / 255.0, green as f32 / 255.0, blue as f32 / 255.0, alpha as f32 / 255.0)
    }
}
