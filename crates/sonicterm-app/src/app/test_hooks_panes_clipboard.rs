//! The fake clipboard F-ConPTY reply panes read from: one text beside fixed HTML, PNG and custom
//! representations, never the user's clipboard.

/// A fake clipboard that always holds one text and [`REPLY_PANE_TYPES`], for reply panes; never
/// the user's clipboard.
#[cfg(any(test, feature = "test-support"))]
pub(super) struct FixedClipboard(pub(super) String);

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
        if self.0.len() > max_bytes {
            // When: the text is longer than max_bytes, it is refused as a native read would be.
            return crate::app::osc52_read_source::ReadOutcome::TooLarge;
        }
        crate::app::osc52_read_source::ReadOutcome::Text(self.0.clone())
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
