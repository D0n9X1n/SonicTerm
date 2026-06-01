//! Clipboard backend trait. Copy / paste indirection so the app loop
//! doesn't depend on `arboard` directly.
//!
//! Must be **object-safe**.

/// Minimal clipboard abstraction.
pub trait ClipboardBackend: Send {
    /// Read the current clipboard contents as UTF-8 text. Returns
    /// `None` if the clipboard is empty or holds non-text data.
    fn get_text(&mut self) -> Option<String>;

    /// Replace the clipboard contents with `text`.
    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError>;
}

/// Reasons clipboard operations can fail.
#[derive(Debug)]
pub enum ClipboardError {
    /// Backend reported an error (e.g. another process holds the
    /// pasteboard).
    Backend(String),
}
