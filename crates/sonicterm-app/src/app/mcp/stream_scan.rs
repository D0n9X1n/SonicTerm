//! The incremental scan behind a stream wait. Under the parser lock it copies only the characters
//! printed since its last read; outside the lock it keeps the text since the wait began that the
//! tap still holds, and reruns the regex over all of it, so anchors, word boundaries, empty matches
//! and multi-line patterns behave exactly as a search of `output_tap_since(start)` would.

use sonicterm_vt::vt::Parser;

/// One stream wait's view of the tap: the retained text since it armed, and how far it has read.
#[derive(Debug)]
pub(crate) struct StreamScan {
    /// The offset the wait armed at.
    start: u64,
    /// The next offset to read.
    next: u64,
    /// The text since `start` that the tap still holds.
    buffer: String,
    /// The offset of `buffer`'s first character.
    buffer_start: u64,
    /// Text since `start` was dropped from the tap: a match spanning it may be lost.
    truncated: bool,
    /// New text arrived since the last search, or nothing was searched yet.
    search_due: bool,
    /// Characters copied by the last read, under the lock.
    scratch: String,
}

/// A match, located in the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamMatch {
    /// The offset in the stream where the match starts.
    pub(crate) offset: u64,
    /// The printed line holding the match.
    pub(crate) line: String,
}

impl StreamScan {
    /// A scan of the stream from offset `start`, where its hold armed.
    pub(crate) fn new(start: u64) -> Self {
        Self {
            start,
            next: start,
            buffer: String::new(),
            buffer_start: start,
            truncated: false,
            search_due: true,
            scratch: String::new(),
        }
    }

    /// Copy the characters printed since the last read, under `parser`'s lock. Returns where the
    /// read ended (`None` when the tap is not armed); [`Self::absorb`] takes the copy afterwards.
    pub(crate) fn read(&mut self, parser: &Parser) -> Option<sonicterm_vt::vt::TapAppend> {
        self.scratch.clear();
        let append = parser.output_tap_append(self.next, &mut self.scratch)?;
        #[cfg(test)]
        COPIED_UNDER_LOCK.with(|copied| copied.set(copied.get() + self.scratch.chars().count()));
        Some(append)
    }

    /// Fold a read into the buffer, outside the lock: drop what the tap dropped, then add the new
    /// characters. A gap replaces the buffer with the whole retained ring.
    pub(crate) fn absorb(&mut self, append: sonicterm_vt::vt::TapAppend) {
        self.truncated |= append.retained_start > self.start;
        if append.gap {
            // The tap dropped text at `next`, so the buffer is the retained ring alone.
            std::mem::swap(&mut self.buffer, &mut self.scratch);
            self.buffer_start = append.from;
        } else {
            // When: no `gap`, the read continues the buffer, less what the ring dropped meanwhile.
            let keep_from = append.retained_start.max(self.start);
            if keep_from > self.buffer_start {
                // The ring dropped text the buffer still holds; drop the same prefix.
                let drop_chars = (keep_from - self.buffer_start) as usize;
                let drop_bytes =
                    self.buffer.char_indices().nth(drop_chars).map_or(self.buffer.len(), |at| at.0);
                self.buffer.drain(..drop_bytes);
                self.buffer_start = keep_from;
                self.search_due = true;
            }
            self.buffer.push_str(&self.scratch);
        }
        if append.end != self.next || append.gap {
            self.search_due = true;
        }
        self.next = append.end;
    }

    /// Search the buffer when it changed since the last search; `None` otherwise or with no match.
    pub(crate) fn find(&mut self, regex: &regex::Regex) -> Option<StreamMatch> {
        if !std::mem::take(&mut self.search_due) {
            // When: `search_due` is clear, nothing changed since the last search.
            return None;
        }
        locate(&self.buffer, self.buffer_start, regex)
    }

    /// Whether text since the wait began was dropped before every read could keep it.
    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }

    /// The offset just past the last character read.
    pub(crate) fn end(&self) -> u64 {
        self.next
    }
}

/// The first match of `regex` in `text`, whose first character is at offset `from`: its stream
/// offset and the printed line holding it.
pub(crate) fn locate(text: &str, from: u64, regex: &regex::Regex) -> Option<StreamMatch> {
    let found = regex.find(text)?;
    let line_start = text[..found.start()].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[found.end()..].find('\n').map_or(text.len(), |at| found.end() + at);
    Some(StreamMatch {
        offset: from + text[..found.start()].chars().count() as u64,
        line: text[line_start..line_end].to_owned(),
    })
}

#[cfg(test)]
thread_local! {
    /// Characters this thread's scans copied under the parser lock.
    pub(crate) static COPIED_UNDER_LOCK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "stream_scan_tests.rs"]
mod stream_scan_tests;
