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

fn linear_f32_to_srgb8(f: f32) -> u8 {
    let f = f.clamp(0.0, 1.0);
    let srgb = if f <= 0.003_130_8 { f * 12.92 } else { f.powf(1.0 / 2.4) * 1.055 - 0.055 };
    (srgb * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

pub fn linear_u8_to_srgb8(f: u8) -> u8 {
    RGB_TO_SRGB_TABLE[f as usize]
}

/// A pixel holding SRGBA32 data in big-endian format.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SrgbaPixel(u32);

impl SrgbaPixel {
    pub fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        let word = (blue as u32) << 24 | (green as u32) << 16 | (red as u32) << 8 | alpha as u32;
        Self(word.to_be())
    }

    pub fn as_rgba(self) -> (u8, u8, u8, u8) {
        let host = u32::from_be(self.0);
        ((host >> 8) as u8, (host >> 16) as u8, (host >> 24) as u8, (host & 0xff) as u8)
    }

    pub fn as_srgba32(self) -> u32 {
        self.0
    }

    pub fn as_srgba_tuple(self) -> (f32, f32, f32, f32) {
        let SrgbaTuple(r, g, b, a) = self.into();
        (r, g, b, a)
    }
}

/// A pixel value encoded as SRGBA RGBA values in f32 format (0.0..=1.0).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SrgbaTuple(pub f32, pub f32, pub f32, pub f32);

impl SrgbaTuple {
    pub fn premultiply(self) -> Self {
        let Self(r, g, b, a) = self;
        Self(r * a, g * a, b * a, a)
    }

    pub fn demultiply(self) -> Self {
        let Self(r, g, b, a) = self;
        if a != 0.0 {
            Self(r / a, g / a, b / a, a)
        } else {
            self
        }
    }

    pub fn interpolate(self, other: Self, k: f64) -> Self {
        let k = k as f32;
        let Self(r0, g0, b0, a0) = self.premultiply();
        let Self(r1, g1, b1, a1) = other.premultiply();
        Self(r0 + k * (r1 - r0), g0 + k * (g1 - g0), b0 + k * (b1 - b0), a0 + k * (a1 - a0))
            .demultiply()
    }
}

impl From<SrgbaPixel> for SrgbaTuple {
    fn from(pixel: SrgbaPixel) -> Self {
        pixel.as_rgba().into()
    }
}

impl From<(u8, u8, u8, u8)> for SrgbaTuple {
    fn from((r, g, b, a): (u8, u8, u8, u8)) -> Self {
        Self(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0)
    }
}
