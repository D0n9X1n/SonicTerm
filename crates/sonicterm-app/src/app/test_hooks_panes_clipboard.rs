//! The fake clipboard F-ConPTY reply panes read from and write to: one text beside fixed HTML, PNG
//! and custom representations, never the user's clipboard, and the commit step that writes it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use sonicterm_vt::vt::KittyClipboardStatus;

/// A fake clipboard holding one text beside [`REPLY_PANE_TYPES`], for reply panes; never the
/// user's clipboard. A committed write replaces the text unless writes were made to fail.
#[cfg(any(test, feature = "test-support"))]
pub(super) struct FixedClipboard {
    /// The text a read returns; a committed write replaces it.
    text: Mutex<String>,
    /// Whether a write fails, as a native setter that refuses does, changing nothing.
    fail_writes: AtomicBool,
}

#[cfg(any(test, feature = "test-support"))]
impl FixedClipboard {
    /// A fake clipboard holding `text`, whose writes succeed.
    pub(super) fn new(text: &str) -> Self {
        Self { text: Mutex::new(text.to_owned()), fail_writes: AtomicBool::new(false) }
    }

    /// Replace the text with `text`; false, changing nothing, while writes fail.
    fn set_text(&self, text: String) -> bool {
        if self.fail_writes.load(Ordering::SeqCst) {
            // When: fail_writes is set, the setter refuses and the text stays as it was.
            return false;
        }
        *self.text.lock().unwrap_or_else(PoisonError::into_inner) = text;
        true
    }
}

/// The non-text representations every reply pane's fake clipboard holds, which the F-ConPTY
/// multi-type rows read.
#[cfg(any(test, feature = "test-support"))]
const REPLY_PANE_TYPES: &[(&str, &[u8])] = &[
    ("text/html", b"<b>fixture</b>"),
    ("image/png", b"PNG"),
    ("application/x-sonicterm-test", b"custom"),
];

#[cfg(any(test, feature = "test-support"))]
impl crate::app::osc52_read_source::ClipboardTextSource for FixedClipboard {
    fn read_text_bounded(
        &self,
        _location: crate::app::osc52_read_source::ReadLocation,
        max_bytes: usize,
    ) -> crate::app::osc52_read_source::ReadOutcome {
        let text = self.text.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if text.len() > max_bytes {
            // When: the text is longer than max_bytes, it is refused as a native read would be.
            return crate::app::osc52_read_source::ReadOutcome::TooLarge;
        }
        crate::app::osc52_read_source::ReadOutcome::Text(text)
    }

    fn read_type_bounded(
        &self,
        _location: crate::app::osc52_read_source::ReadLocation,
        mime: &str,
        max_bytes: usize,
    ) -> crate::app::osc52_read_source::TypeRead {
        use crate::app::osc52_read_source::TypeRead;
        match REPLY_PANE_TYPES.iter().find(|(name, _)| *name == mime) {
            Some((_, bytes)) if bytes.len() > max_bytes => TypeRead::TooLarge,
            Some((_, bytes)) => TypeRead::Bytes(bytes.to_vec()),
            None => TypeRead::Absent,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl super::TestReplyPane {
    /// Commit each posted write in `writes` as the clipboard worker does, behind its binding: a
    /// stale one changes nothing and answers nothing; otherwise the fake clipboard's setter takes
    /// its text, and an OSC 5522 write answers `DONE` when it did and `EIO` when it refused. An
    /// OSC 52 write has no status. A plain reply pane has no fake clipboard and counts the write.
    pub(super) fn commit_writes(
        &mut self,
        writes: Vec<crate::app::osc52_stream::ClipboardJobHandle>,
        replies: &mut Vec<u8>,
    ) {
        for job in writes.iter().filter_map(|write| write.take()) {
            if !job.binding.may_commit() {
                // When: may_commit refuses, the write went stale after posting and is dropped.
                continue;
            }
            // An offer carries its text beside other types; the fake keeps only the text.
            let text = match &job.offer {
                Some(offer) => String::from_utf8_lossy(offer.text().unwrap_or_default()).into(),
                None => job.text.clone(),
            };
            let written = self.clipboard.as_ref().is_none_or(|clipboard| clipboard.set_text(text));
            let status = if written {
                self.writes_committed += 1;
                KittyClipboardStatus::Done
            } else {
                // When: written is false, the setter refused: nothing changed, so `EIO`.
                KittyClipboardStatus::Eio
            };
            if let Some(kitty) = &job.kitty {
                replies.extend(kitty.status(status));
            }
        }
    }

    /// Test-only: make every later write to this pane's fake clipboard fail, as a native setter
    /// that refuses does.
    #[doc(hidden)]
    pub fn fail_clipboard_writes(&self) {
        let clipboard = self.clipboard.as_ref().expect("a clipboard reply pane");
        clipboard.fail_writes.store(true, Ordering::SeqCst);
    }
}
