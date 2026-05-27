//! Shared render-model: the seam between UI state and GPU drawing.
//! UI builds these structs; GPU consumes them via the Painter trait.

pub mod geometry;
pub mod inputs;
pub mod painter;

pub use geometry::*;
pub use inputs::*;
pub use painter::*;
