//! An opt-in record of the text a pane printed, for a reader that must see output the screen no
//! longer shows: a progress line rewritten in place with `\r`, or rows scrolled out between two
//! checks. Unarmed it holds nothing and costs one branch per printed character.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Characters the tap keeps; older text is dropped first.
pub const OUTPUT_TAP_CHARS: usize = 64 * 1024;

/// The printed text of one parser while at least one reader has it armed.
#[derive(Debug, Default)]
pub(crate) struct OutputTap {
    /// The newest printed characters, a line feed for each LF, VT or FF.
    ring: VecDeque<char>,
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

impl OutputTap {
    /// Record one printed character, dropping the oldest when the ring is full.
    pub(crate) fn push(&mut self, character: char) {
        if self.ring.len() == OUTPUT_TAP_CHARS {
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

    /// Free the tap when its last hold was dropped; the parser calls this at each batch.
    // Ordering: readers Acquire pairs with TapHold's Release decrement.
    pub(super) fn release_idle_tap(&mut self) {
        let idle =
            self.performer.tap.as_ref().is_some_and(|tap| tap.readers.load(Ordering::Acquire) == 0);
        if idle {
            // The last hold was dropped: nothing records and the ring is freed.
            self.performer.tap = None;
        }
    }

    /// The text printed at or after offset `start`, as far as the tap still holds it; `None` when
    /// the tap is not armed.
    pub fn output_tap_since(&self, start: u64) -> Option<TapText> {
        let tap = self.performer.tap.as_ref()?;
        let retained_from = tap.end - tap.ring.len() as u64;
        let from = start.max(retained_from).min(tap.end);
        let skip = (from - retained_from) as usize;
        Some(TapText {
            text: tap.ring.iter().skip(skip).collect(),
            from,
            end: tap.end,
            truncated: retained_from > start,
        })
    }
}

#[cfg(test)]
#[path = "vt_tap_tests.rs"]
mod vt_tap_tests;
