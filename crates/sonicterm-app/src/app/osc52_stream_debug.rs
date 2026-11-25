//! `Debug` for clipboard jobs that prints lengths, never the text a program wrote.

use super::ClipboardJob;

impl std::fmt::Debug for ClipboardJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The text is what a program put on the clipboard, so only its length is printed.
        formatter
            .debug_struct("ClipboardJob")
            .field("text_bytes", &self.text.len())
            .field("targets", &self.targets)
            .field("kitty", &self.kitty)
            .field("offer", &self.offer)
            .finish_non_exhaustive()
    }
}
