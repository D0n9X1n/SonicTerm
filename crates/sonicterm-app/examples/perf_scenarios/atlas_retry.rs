//! S1/atlas-retry: injected atlas recovery episodes, observed through the main window's counters.
//!
//! Each episode is four frames. A changes the glyph atlas during assembly, so it retries and
//! presents nothing; B is the retry's first recovered presentation; C and D are forced frames of
//! the unchanged scene. The probe feeds this pure machine one counter delta per forwarded dispatch;
//! the machine says what to arm next and records each frame. Any delta that does not fit its frame
//! ends the run as invalid; nothing is folded or guessed.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Recovery episodes per run.
pub(crate) const EPISODES: usize = 8;
/// How long one frame of an episode may take to be attempted.
pub(crate) const STEP_BOUND: Duration = Duration::from_secs(2);
/// How long the scene may take to settle before the first episode.
pub(crate) const SETTLE_BOUND: Duration = Duration::from_secs(10);
/// Consecutive steady frames that settle the scene.
pub(crate) const STEADY_FRAMES: u32 = 3;
/// Text every fixture row starts with, followed by its two-digit number.
pub(crate) const ROW_PREFIX: &str = "atlas-retry row ";

/// The main renderer's counts, as an absolute reading or as the delta of one dispatch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Render attempts.
    pub(crate) attempts: u64,
    /// Attempts that presented.
    pub(crate) presented: u64,
    /// In-place glyph atlas resets.
    pub(crate) resets: u64,
    /// Row-cache hits.
    pub(crate) hits: u64,
    /// Row-cache misses.
    pub(crate) misses: u64,
    /// Shaping requests.
    pub(crate) shapes: u64,
    /// The glyph atlas dimension after the reading; never a delta.
    pub(crate) atlas_dim: u32,
}

impl Counts {
    /// The counts that moved from `before` to `self`; the atlas dimension is `self`'s.
    pub(crate) fn since(self, before: Counts) -> Counts {
        Counts {
            attempts: self.attempts.saturating_sub(before.attempts),
            presented: self.presented.saturating_sub(before.presented),
            resets: self.resets.saturating_sub(before.resets),
            hits: self.hits.saturating_sub(before.hits),
            misses: self.misses.saturating_sub(before.misses),
            shapes: self.shapes.saturating_sub(before.shapes),
            atlas_dim: self.atlas_dim,
        }
    }
}

/// One frame of an episode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// A: the atlas changes during assembly, so the frame retries and presents nothing.
    Retried,
    /// B: the retry's first recovered presentation.
    Recovered,
    /// C: the next forced frame of the unchanged scene, which reuses B's rows when they were kept.
    Reused,
    /// D: one more forced frame, which excludes a delayed clear.
    Repeated,
}

impl Frame {
    /// The frame's name in `result.json`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Retried => "A",
            Self::Recovered => "B",
            Self::Reused => "C",
            Self::Repeated => "D",
        }
    }

    /// The frame after this one in its episode, `None` after D.
    fn next(self) -> Option<Frame> {
        match self {
            Self::Retried => Some(Self::Recovered),
            Self::Recovered => Some(Self::Reused),
            Self::Reused => Some(Self::Repeated),
            Self::Repeated => None,
        }
    }

    /// Whether `counts` is what this frame must show: A resets and presents nothing, B–D present
    /// without a reset.
    fn fits(self, counts: Counts) -> bool {
        match self {
            Self::Retried => counts.resets == 1 && counts.presented == 0,
            Self::Recovered | Self::Reused | Self::Repeated => counts.resets == 0 && counts.presented == 1,
        }
    }
}

/// One recorded frame: its episode, its letter and the counts its single attempt moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    /// The episode, from 0.
    pub(crate) episode: usize,
    /// The frame.
    pub(crate) frame: Frame,
    /// What its attempt moved; `atlas_dim` is the reading after it.
    pub(crate) counts: Counts,
}

/// What the probe arms before the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arm {
    /// Forget the retained frame and request one redraw: a settling frame, C or D.
    Redraw,
    /// Change the atlas during the next assembly, forget the retained frame and request a redraw: A.
    ChangeAtlas,
    /// Forget the retained frame only; the retry's own request redraws it: B.
    InvalidateOnly,
}

/// What one observation did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Progress {
    /// No attempt was made (a deferred dispatch), or the machine has ended.
    Waiting,
    /// A frame completed; arm this before the next.
    Arm(Arm),
    /// The eighth episode's D completed.
    Done,
    /// The run cannot be read; the reason names the frame and its counts.
    Invalid(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Settling { steady: u32, deadline: Instant },
    Step { episode: usize, frame: Frame, deadline: Instant },
    Done,
    Invalid,
}

/// The settle-then-episodes machine for one run.
#[derive(Debug)]
pub(crate) struct RecoveryEpisodes {
    stage: Stage,
    records: Vec<Record>,
    recovered_dim: Option<u32>,
}

impl RecoveryEpisodes {
    /// A machine settling the scene from `now`; the probe arms [`Arm::Redraw`] first.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            stage: Stage::Settling { steady: 0, deadline: now + SETTLE_BOUND },
            records: Vec::new(),
            recovered_dim: None,
        }
    }

    /// Feed the counts one forwarded dispatch moved, or `None` when a counter field is missing.
    pub(crate) fn observe(&mut self, delta: Option<Counts>, now: Instant) -> Progress {
        if matches!(self.stage, Stage::Done | Stage::Invalid) {
            // When: the machine has ended, later dispatches are not part of the run.
            return Progress::Waiting;
        }
        let Some(delta) = delta else {
            // When: a counter field is missing, nothing can be measured, never read as zero.
            return self.fail("counters unavailable".to_owned());
        };
        if delta.attempts == 0 {
            // When: the dispatch attempted no frame (a deferral), the current step waits.
            return Progress::Waiting;
        }
        let label = self.label();
        if delta.attempts > 1 {
            return self.fail(format!("{label}: {} attempts in one dispatch: {delta:?}", delta.attempts));
        }
        match self.stage {
            Stage::Settling { steady, deadline } => {
                let steady_frame = delta.presented == 1 && delta.resets == 0 && delta.misses == 0;
                let steady = if steady_frame { steady + 1 } else { 0 };
                if steady >= STEADY_FRAMES {
                    // When: the scene drew STEADY_FRAMES steady frames in a row, the episodes start.
                    self.stage =
                        Stage::Step { episode: 0, frame: Frame::Retried, deadline: now + STEP_BOUND };
                    return Progress::Arm(Arm::ChangeAtlas);
                }
                self.stage = Stage::Settling { steady, deadline };
                Progress::Arm(Arm::Redraw)
            }
            Stage::Step { episode, frame, .. } => self.complete(episode, frame, delta, now),
            Stage::Done | Stage::Invalid => Progress::Waiting,
        }
    }

    /// Record `frame` of `episode` from `delta` and move to the next frame.
    fn complete(&mut self, episode: usize, frame: Frame, delta: Counts, now: Instant) -> Progress {
        if !frame.fits(delta) {
            return self.fail(format!("{}: counts do not fit the frame: {delta:?}", self.label()));
        }
        if frame != Frame::Retried {
            // When: frame is B, C or D, every recovered frame must draw at one atlas dimension.
            let expected = *self.recovered_dim.get_or_insert(delta.atlas_dim);
            if delta.atlas_dim != expected {
                return self.fail(format!(
                    "{}: atlas dimension {} differs from {expected}",
                    self.label(),
                    delta.atlas_dim
                ));
            }
        }
        self.records.push(Record { episode, frame, counts: delta });
        let (next_episode, next_frame) = match frame.next() {
            Some(next) => (episode, next),
            None => (episode + 1, Frame::Retried),
        };
        if next_episode == EPISODES {
            // When: the last episode's D is recorded, the run's episodes are complete.
            self.stage = Stage::Done;
            return Progress::Done;
        }
        self.stage =
            Stage::Step { episode: next_episode, frame: next_frame, deadline: now + STEP_BOUND };
        Progress::Arm(match next_frame {
            Frame::Retried => Arm::ChangeAtlas,
            Frame::Recovered => Arm::InvalidateOnly,
            Frame::Reused | Frame::Repeated => Arm::Redraw,
        })
    }

    /// End the run as invalid when the current step's deadline has passed at `now`.
    pub(crate) fn expire(&mut self, now: Instant) -> Option<String> {
        let deadline = self.deadline()?;
        if now < deadline {
            // When: the deadline is still ahead, the step may yet be attempted.
            return None;
        }
        let reason = format!("{}: not attempted within its bound", self.label());
        self.stage = Stage::Invalid;
        Some(reason)
    }

    /// When the current step must have been attempted; `None` once the machine has ended.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        match self.stage {
            Stage::Settling { deadline, .. } | Stage::Step { deadline, .. } => Some(deadline),
            Stage::Done | Stage::Invalid => None,
        }
    }

    /// Whether the scene has settled and the episodes have started or ended.
    pub(crate) fn settled(&self) -> bool {
        !matches!(self.stage, Stage::Settling { .. })
    }

    /// Whether every episode is recorded.
    pub(crate) fn is_done(&self) -> bool {
        self.stage == Stage::Done
    }

    /// The frames recorded so far, in order.
    pub(crate) fn records(&self) -> &[Record] {
        &self.records
    }

    /// The step the machine is on, for a reason.
    fn label(&self) -> String {
        match self.stage {
            Stage::Settling { steady, .. } => format!("settle (steady {steady})"),
            Stage::Step { episode, frame, .. } => format!("episode {episode} {}", frame.as_str()),
            Stage::Done => "done".to_owned(),
            Stage::Invalid => "invalid".to_owned(),
        }
    }

    /// End the run as invalid for `reason`.
    fn fail(&mut self, reason: String) -> Progress {
        self.stage = Stage::Invalid;
        Progress::Invalid(reason)
    }
}

/// Why `records` is not a complete run: exactly `EPISODES` × A–D in order, one attempt each, A
/// resetting without presenting, B–D presenting without a reset at one atlas dimension.
pub(crate) fn records_problem(records: &[Record]) -> Option<String> {
    if records.len() != EPISODES * 4 {
        return Some(format!("{} records, not {}", records.len(), EPISODES * 4));
    }
    let frames = [Frame::Retried, Frame::Recovered, Frame::Reused, Frame::Repeated];
    let recovered_dim = records[1].counts.atlas_dim;
    for (index, record) in records.iter().enumerate() {
        let (episode, frame) = (index / 4, frames[index % 4]);
        if record.episode != episode || record.frame != frame {
            return Some(format!("record {index} is episode {} {}", record.episode, record.frame.as_str()));
        }
        if record.counts.attempts != 1 || !frame.fits(record.counts) {
            return Some(format!("record {index} does not fit {}: {:?}", frame.as_str(), record.counts));
        }
        if frame != Frame::Retried && record.counts.atlas_dim != recovered_dim {
            return Some(format!("record {index} draws at another atlas dimension"));
        }
    }
    None
}

/// `result.json`'s `atlas_recovery`: the episode count, the scene's distinct row keys and every record.
pub(crate) fn recovery_json(records: &[Record], distinct_keys: usize) -> Value {
    let records: Vec<Value> = records
        .iter()
        .map(|record| {
            json!({
                "episode": record.episode,
                "frame": record.frame.as_str(),
                "attempts": record.counts.attempts,
                "presented": record.counts.presented,
                "resets": record.counts.resets,
                "hits": record.counts.hits,
                "misses": record.counts.misses,
                "shapes": record.counts.shapes,
                "atlas_dim": record.counts.atlas_dim,
            })
        })
        .collect();
    json!({"episodes": EPISODES, "distinct_keys": distinct_keys, "records": records})
}

/// The fixture: one numbered line per row of the 70-row grid, every line distinct.
pub(crate) fn fixture_text() -> String {
    (1..=70).map(|number| format!("{ROW_PREFIX}{number:02} abcdefghijklmnopqrstuvwxyz {number:02}\n")).collect()
}

/// Why the visible rows are not the fixture: every row but at most the last two (the sentinel and
/// a prompt) must be a fixture line, numbered consecutively upward.
pub(crate) fn scene_problem(rows: &[String]) -> Option<String> {
    let numbers: Vec<u32> = rows
        .iter()
        .filter_map(|row| row.strip_prefix(ROW_PREFIX)?.get(..2)?.parse().ok())
        .collect();
    if numbers.len() + 2 < rows.len() {
        return Some(format!("{} of {} visible rows are fixture lines", numbers.len(), rows.len()));
    }
    if numbers.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        return Some(format!("the fixture lines are not consecutive: {numbers:?}"));
    }
    None
}

/// Distinct row texts among `rows`: the row-cache keys a full frame of this scene looks up.
pub(crate) fn distinct_keys(rows: &[String]) -> usize {
    rows.iter().collect::<std::collections::BTreeSet<_>>().len()
}

#[cfg(test)]
#[path = "atlas_retry_tests.rs"]
mod atlas_retry_tests;
