//! Grid geometry value types (positions). `Rect` intentionally lives in
//! per-crate modules because each consumer uses different units (pixels vs
//! cells vs config-space) — there is no canonical workspace `Rect`.

/// (row, col) position. (0, 0) is top-left of the visible region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}
