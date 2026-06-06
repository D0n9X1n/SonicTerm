/// Phantom marker for raster-pixel font measurements.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PixelUnit {}

pub type PixelLength = euclid::Length<f64, PixelUnit>;
pub type IntPixelLength = isize;
