//! An opt-in record of the text a pane printed, for a reader that must see output the screen no
//! longer shows: a progress line rewritten in place with `\r`, or rows scrolled out between two
//! checks. Unarmed it holds nothing and costs one branch per printed character.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Characters the tap keeps; older text is dropped first.
pub const OUTPUT_TAP_CHARS: usize = 64 * 1024;

/// The printed text of one parser while at least one reader has it armed.
#[derive(Debug)]
pub(crate) struct OutputTap {
    /// The newest printed characters, a line feed for each LF, VT or FF.
    ring: VecDeque<char>,
    /// The most characters `ring` keeps: [`OUTPUT_TAP_CHARS`], or a test's smaller ring.
    capacity: usize,
    /// Characters pushed since the tap was created; the ring's last character is at `end - 1`.
    end: u64,
    /// Readers holding the tap armed. A reader releases its hold without the parser lock; the
    /// parser frees the ring at its next batch once the count is zero.
    readers: Arc<AtomicUsize>,
}

/// One reader's hold on a parser's output tap. Dropping it releases the hold without taking the
/// parser lock, so a reader that ends never waits on a busy parser.
#[derive(Debug)]
pub struct TapHold {
    /// The tap's shared reader count.
    readers: Arc<AtomicUsize>,
    /// The offset this reader's stream starts at.
    pub start: u64,
}

// Lifecycle: TapHold releases its reader's count on drop; the parser frees the ring at its next batch.
impl Drop for TapHold {
    // Ordering: readers Release publishes this reader's exit to the parser's Acquire check.
    fn drop(&mut self) {
        self.readers.fetch_sub(1, Ordering::Release);
    }
}

impl Default for OutputTap {
    fn default() -> Self {
        Self::with_capacity(OUTPUT_TAP_CHARS)
    }
}

impl OutputTap {
    /// An empty tap keeping at most `capacity` characters.
    fn with_capacity(capacity: usize) -> Self {
        Self { ring: VecDeque::new(), capacity, end: 0, readers: Arc::default() }
    }

    /// The offset of the oldest character the ring still holds.
    fn retained_start(&self) -> u64 {
        self.end - self.ring.len() as u64
    }

    /// Record one printed character, dropping the oldest when the ring is full.
    pub(crate) fn push(&mut self, character: char) {
        if self.ring.len() == self.capacity {
            self.ring.pop_front();
        }
        self.ring.push_back(character);
        self.end += 1;
    }
}

/// What a stream reader sees of the tap since its start offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapText {
    /// The retained text from the reader's start, or from the oldest retained character.
    pub text: String,
    /// The offset of the first character of `text`.
    pub from: u64,
    /// The offset just past the last character of `text`.
    pub end: u64,
    /// Characters after the reader's start were dropped before it read them.
    pub truncated: bool,
}

impl super::Parser {
    /// Arm the output tap for one more reader; the hold carries the offset its stream starts at.
    /// Text printed from now on is recorded until every hold is dropped.
    // Ordering: readers AcqRel; the increment is made under the parser lock, before any batch frees.
    pub fn arm_output_tap(&mut self) -> TapHold {
        let tap = self.performer.tap.get_or_insert_with(Box::default);
        tap.readers.fetch_add(1, Ordering::AcqRel);
        TapHold { readers: Arc::clone(&tap.readers), start: tap.end }
    }

    /// Free the tap when its last hold was dropped. The parser calls this at each batch; a stream
    /// reader and the app's retention pass call it too, so a pane that prints nothing more after
    /// its last wait does not keep the ring. It frees nothing while any hold remains.
    // Ordering: readers Acquire pairs with TapHold's Release decrement.
    pub fn release_idle_tap(&mut self) {
        let idle =
            self.performer.tap.as_ref().is_some_and(|tap| tap.readers.load(Ordering::Acquire) == 0);
        if idle {
            // The last hold was dropped: nothing records and the ring is freed.
            self.performer.tap = None;
        }
    }

    /// How many holds keep the tap armed; 0 when it is not armed.
    // Ordering: readers Acquire pairs with TapHold's Release decrement.
    pub fn output_tap_readers(&self) -> usize {
        self.performer.tap.as_ref().map_or(0, |tap| tap.readers.load(Ordering::Acquire))
    }

    /// The text printed at or after offset `start`, as far as the tap still holds it; `None` when
    /// the tap is not armed.
    pub fn output_tap_since(&self, start: u64) -> Option<TapText> {
        let tap = self.performer.tap.as_ref()?;
        let retained_from = tap.retained_start();
        let from = start.max(retained_from).min(tap.end);
        let skip = (from - retained_from) as usize;
        Some(TapText {
            text: tap.ring.iter().skip(skip).collect(),
            from,
            end: tap.end,
            truncated: retained_from > start,
        })
    }

    /// Append to `out` only the characters printed at or after `next` that the tap still holds:
    /// the work under the parser lock is the new text, never the whole ring. When the ring already
    /// dropped text at `next` (a gap), `out` is cleared first and receives the whole retained
    /// ring, so stale text can never survive in front of it. `None` when the tap is not armed.
    pub fn output_tap_append(&self, next: u64, out: &mut String) -> Option<TapAppend> {
        let tap = self.performer.tap.as_ref()?;
        let retained_start = tap.retained_start();
        let gap = retained_start > next;
        if gap {
            // The ring dropped text at `next`, so the caller's earlier text is obsolete.
            out.clear();
        }
        let from = next.max(retained_start).min(tap.end);
        out.extend(tap.ring.iter().skip((from - retained_start) as usize));
        Some(TapAppend { from, end: tap.end, retained_start, gap })
    }

    /// [`Self::arm_output_tap`] on a fresh tap keeping only `capacity` characters, so a test can
    /// drive the ring past its size with little text. An armed tap keeps its capacity.
    #[doc(hidden)]
    pub fn arm_output_tap_with_capacity(&mut self, capacity: usize) -> TapHold {
        if self.performer.tap.is_none() {
            self.performer.tap = Some(Box::new(OutputTap::with_capacity(capacity)));
        }
        self.arm_output_tap()
    }
}

/// What one incremental read of the tap appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapAppend {
    /// The offset of the first character appended.
    pub from: u64,
    /// The offset just past the last character appended: where the next read starts.
    pub end: u64,
    /// The offset of the oldest character the ring holds.
    pub retained_start: u64,
    /// The ring had already dropped text at the read's start, so the caller's buffer was cleared
    /// and refilled from `retained_start`.
    pub gap: bool,
}

#[cfg(test)]
#[path = "vt_tap_tests.rs"]
mod vt_tap_tests;
