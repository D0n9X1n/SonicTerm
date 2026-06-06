//! Trait seams declared in the contract crate. Concrete implementations
//! live in their respective feature crates (`sonicterm-io::pty`,
//! `sonicterm-gpu`, `sonicterm-app`, etc.). Adding or modifying a trait
//! here is a workspace-wide contract change.

pub mod clipboard;
pub mod painter;
pub mod pty;
pub mod window;

pub use clipboard::ClipboardBackend;
pub use painter::{FrameLike, PaintError, Painter};
pub use pty::PtyTransport;
pub use window::WindowBackend;
