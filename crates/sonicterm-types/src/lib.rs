//! Platform-agnostic value types shared across SonicTerm crates.
//!
//! This crate carries only **data types** (cells, colors, positions, actions,
//! glyph keys, hyperlink ids). It deliberately has **no** runtime dependencies
//! on rendering, ptys, or windowing — any crate that just needs the value
//! shapes can depend on `sonicterm-types` without pulling in the engine.
//!
//! All previous import paths in `sonicterm-core` etc. continue to work via
//! `pub use` re-exports, so this is a zero-behavior-change move.

#![deny(missing_docs)]

pub mod action;
pub mod cell;
pub mod geom;
pub mod glyph_key;
pub mod hyperlink_id;
pub mod lifecycle;
pub mod mod_key;
pub mod resource;
pub mod shell_quote;
pub mod traits;
pub mod window_key;

pub use action::{Action, BroadcastScope, Direction, ScrollAction};
pub use cell::{Cell, CellFlags, Color, FatAttributes, UnderlineStyle};
pub use geom::{GeometryQuad, Pos};
pub use glyph_key::GlyphKey;
pub use hyperlink_id::HyperlinkId;
pub use lifecycle::{
    CancelOutcome, CancelReason, IllegalTransition, LifecycleState, ReapAdmission, ReapResult,
};
pub use mod_key::ModKey;
pub use resource::{
    BudgetDimension, BudgetError, BudgetScope, DeliveryReceipt, DisconnectReason, DropReason,
    GovernorLimits, OwnerKind, OwnerLimits, OwnerState, OwnerView, PressureOutcome, ProcessKind,
    ProcessView, ResizeOperation, ResourceAmount, ResourceClass, ResourceOwnerId, ResourceSnapshot,
    RetryToken, RetryWakeup,
};
pub use shell_quote::shell_quote_posix;
pub use traits::{ClipboardBackend, FrameLike, PaintError, Painter, PtyTransport, WindowBackend};
pub use window_key::WindowKey;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
