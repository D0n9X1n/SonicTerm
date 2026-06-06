//! `DimensionContext` + `Dimension` — fraction-of-cell sizing port from
//! wezterm `config/src/units.rs`. Only the surface customglyph reads:
//! `Dimension::evaluate_as_pixels(DimensionContext { dpi, pixel_max,
//! pixel_cell })`. Vendored 2026-06-04 from wezterm@577474d, MIT.

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Dimension {
    Points(f32),
    Pixels(f32),
    Percent(f32),
    Cells(f32),
}

#[derive(Clone, Copy, Debug)]
pub struct DimensionContext {
    pub dpi: f32,
    pub pixel_max: f32,
    pub pixel_cell: f32,
}

impl Dimension {
    pub fn evaluate_as_pixels(&self, context: DimensionContext) -> f32 {
        match self {
            Self::Pixels(n) => n.floor(),
            Self::Points(pt) => (pt * context.dpi / 72.0).floor(),
            Self::Percent(p) => (p * context.pixel_max).floor(),
            Self::Cells(c) => (c * context.pixel_cell).floor(),
        }
    }
}
