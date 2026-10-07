//! S1/atlas-retry: injected atlas recovery episodes, observed through the main window's counters.
//!
//! Each episode is four frames. A changes the glyph atlas during assembly, so it retries and
//! presents nothing; B is the retry's first recovered presentation; C and D are forced frames of
//! the unchanged scene. The probe feeds this pure machine one counter delta per forwarded dispatch;
//! the machine says what to arm next and records each frame. Any delta that does not fit its frame
//! ends the run as invalid; nothing is folded or guessed.
//!
//! The scene is qualified as well as the counts. The probe samples it (title, font fallback state,
//! grid, cursor and every visible row) at the boundaries of each forwarded dispatch: just before
//! and just after it. Settling records it once the last presented frame drew no character as
//! missing and the rows are exactly the fixture; any later sample that differs ends the run.
//!
//! Settling also waits for the App to show the role's final process. The tab title's icon comes from
//! the pane's applied foreground sample, which the App refreshes at most every 500 ms; a sample taken
//! while the shell was still running would otherwise settle a stale icon that a later sample replaces
//! mid-episode. So once the final process is observed (T: the sentinel on Windows, a completed lookup
//! naming it on macOS), settling counts only applied samples that name a process, taken strictly after
//! T and after the last one counted. Two of them, the latest naming the final process, and the title that
//! process gives, meet the barrier; a missing entry or a cleared sample restarts the count. The first
//! counted sample may still be stale, since its observation can begin before T and be stamped after it.
//!
//! What the samples cannot see: a change that appears and reverts inside one dispatch leaves both
//! samples equal, so it is not detected. Row text is each cell's character with trailing blanks
//! trimmed; it does not capture cell attributes (colours, bold, underline). The fallback state is
//! read through APIs the comparison base shares, since the base runs this same harness source.

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
/// Applied foreground samples taken after the final process that the settle barrier needs.
pub(crate) const QUALIFYING_OBSERVATIONS: u32 = 2;
/// The least spacing between two of the harness's own final-process lookups.
pub(crate) const LOOKUP_SPACING: Duration = Duration::from_millis(100);
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
    // Only the macOS/Windows probe takes these deltas; elsewhere this module builds for its tests alone.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
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

/// What a frame of the planned scene shows, read around every forwarded dispatch. Settling records
/// it; any later reading that differs means C and D no longer redraw one unchanged scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scene {
    /// The active tab's title.
    pub(crate) title: String,
    /// The font fallback state: the body stack's fallback notice id, the cumulative count of frames
    /// that applied a fallback (`font_fallback_applies`), and how many characters the last presented
    /// frame drew as missing, in the grid and in chrome.
    pub(crate) fallback: (u64, u64, usize),
    /// The grid's columns and rows.
    pub(crate) grid: (u16, u16),
    /// The cursor's row and column.
    pub(crate) cursor: (u16, u16),
    /// Every visible row's text, trailing blanks trimmed.
    pub(crate) rows: Vec<String>,
}

impl Scene {
    /// Whether the last presented frame drew no grid or chrome character as missing. A fallback
    /// that was published but not yet applied is not observed here; once a frame applies it, the
    /// cumulative `font_fallback_applies` count in `fallback` moves, which ends a settled run.
    pub(crate) fn fallback_settled(&self) -> bool {
        self.fallback.2 == 0
    }

    /// The first field in which `other` differs from this scene, for a reason: the field and its value
    /// in both scenes, or for row text the first differing row, so a failure names what changed.
    pub(crate) fn difference(&self, other: &Scene) -> Option<String> {
        [
            ("title", self.title != other.title),
            ("font fallback", self.fallback != other.fallback),
            ("grid size", self.grid != other.grid),
            ("cursor", self.cursor != other.cursor),
            ("row text", self.rows != other.rows),
        ]
        .into_iter()
        .find_map(|(field, differs)| differs.then(|| self.describe(field, other)))
    }

    /// How `field` changed from this scene to `other`: both values, or the first differing row.
    fn describe(&self, field: &str, other: &Scene) -> String {
        let quoted = |title: &str| format!("\"{}\"", escape_text(title));
        let values = match field {
            "title" => format!("from {} to {}", quoted(&self.title), quoted(&other.title)),
            "font fallback" => format!("from {:?} to {:?}", self.fallback, other.fallback),
            "grid size" => format!("from {:?} to {:?}", self.grid, other.grid),
            "cursor" => format!("from {:?} to {:?}", self.cursor, other.cursor),
            _ => {
                let shorter = self.rows.len().min(other.rows.len());
                let row = self
                    .rows
                    .iter()
                    .zip(&other.rows)
                    .position(|(settled, read)| settled != read)
                    .unwrap_or(shorter);
                format!("at row {row}")
            }
        };
        format!("{field} changed {values}")
    }
}

/// The scene read before and after one forwarded dispatch, each with the foreground sample the App had
/// applied for the active pane at that same reading; a side is `None` when it could not be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SceneReading {
    /// Read just before the dispatch.
    pub(crate) before: Option<Scene>,
    /// Read just after it.
    pub(crate) after: Option<Scene>,
    /// The App's applied foreground sample, read with `before`; `None` when the pane has no cache entry.
    pub(crate) cached_before: Option<AppliedSample>,
    /// The App's applied foreground sample, read with `after`; `None` when the pane has no cache entry.
    pub(crate) cached_after: Option<AppliedSample>,
}

/// The foreground sample the App had applied for the active pane: when its worker took it, and the
/// process it named (`None` for a cleared sample).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppliedSample {
    /// When the App's worker took the sample.
    pub(crate) sampled_at: Instant,
    /// The process it named; `None` for a cleared sample (unavailable sampler or exited child).
    pub(crate) process: Option<String>,
}

/// What settling must see beyond a steady scene: the final process applied by the App's own sampler, and
/// the title that process gives. The probe sets it before each settling dispatch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Barrier {
    /// T: when the final process was observed (the sentinel on Windows, a completed lookup on macOS).
    pub(crate) final_at: Option<Instant>,
    /// The final process's name as the App's sampler reports it.
    pub(crate) final_name: String,
    /// The title the App shows once it applies the final process; `None` when it cannot be computed.
    pub(crate) expected_title: Option<String>,
    /// Why the fixture cannot give a final title, such as a custom tab title; it ends the attempt at once.
    pub(crate) fixture_problem: Option<String>,
}

/// The fixture problem a custom tab title makes: the App would show it instead of the derived title.
pub(crate) fn custom_title_problem(custom: &str) -> String {
    format!("the tab has a custom title \"{}\"", escape_text(custom))
}

/// The applied samples counted toward the barrier since the last reset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Qualifying {
    /// How many were counted.
    count: u32,
    /// The last counted sample's `sampled_at`; a later one must be strictly greater.
    last_at: Option<Instant>,
    /// The process the last counted sample named.
    latest: Option<String>,
}

/// The reading that ended a settled run, kept as it was when the judge refused it, so the failure is
/// logged from the observation that caused it, never from a later reread of the App.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rejection {
    /// Which reading of the dispatch differed: `before` or `after`.
    pub(crate) side: &'static str,
    /// The settled scene's title.
    pub(crate) settled_title: String,
    /// The refused reading's title.
    pub(crate) read_title: String,
    /// The App's applied foreground sample at that same reading.
    pub(crate) cached: Option<AppliedSample>,
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
            Self::Recovered | Self::Reused | Self::Repeated => {
                counts.resets == 0 && counts.presented == 1
            }
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
    /// While settling, the scene the current run of steady frames showed.
    candidate: Option<Scene>,
    /// The scene settling recorded; every later reading must equal it.
    scene: Option<Scene>,
    /// The reading that ended the run after settling, kept unchanged from the moment it was refused.
    rejection: Option<Rejection>,
    /// What settling must see beyond a steady scene; unset, the final process is never observed.
    barrier: Barrier,
    /// The applied samples counted toward the barrier.
    qualifying: Qualifying,
    /// The last settling reading's title, for an expiry reason.
    last_title: Option<String>,
    /// The last settling reading's applied process: `None` for no entry, `Some(None)` for a cleared sample.
    last_cached: Option<Option<String>>,
}

impl RecoveryEpisodes {
    /// A machine settling the scene from `now`; the probe arms [`Arm::Redraw`] first.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            stage: Stage::Settling { steady: 0, deadline: now + SETTLE_BOUND },
            records: Vec::new(),
            recovered_dim: None,
            candidate: None,
            scene: None,
            rejection: None,
            barrier: Barrier::default(),
            qualifying: Qualifying::default(),
            last_title: None,
            last_cached: None,
        }
    }

    /// Replace the settle barrier's inputs; the probe calls this before every dispatch while settling.
    pub(crate) fn set_barrier(&mut self, barrier: Barrier) {
        self.barrier = barrier;
    }

    /// Feed one forwarded dispatch: the counts it moved (`None` when a counter field is missing),
    /// the scene read around it, and `now`, the instant the dispatch completed.
    ///
    /// Deadlines are judged at completion: a dispatch completing at or after the current deadline
    /// (the settle's while settling) ends the run before its counts are read, as [`Self::expire`]
    /// would at that instant, so a late frame never installs the next step's deadline. Once
    /// settled, a reading on either side of any dispatch that differs from the settled scene ends
    /// the run, whether or not the dispatch attempted a frame.
    pub(crate) fn observe(
        &mut self,
        delta: Option<Counts>,
        reading: &SceneReading,
        now: Instant,
    ) -> Progress {
        if matches!(self.stage, Stage::Done | Stage::Invalid) {
            // When: the machine has ended, later dispatches are not part of the run.
            return Progress::Waiting;
        }
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            // When: the dispatch completed at or past the current deadline, its frame is late.
            let reason = self.timeout_reason("completed past its bound");
            return self.fail(reason);
        }
        let (Some(before), Some(after)) = (&reading.before, &reading.after) else {
            // When: the scene could not be read around the dispatch, nothing can be qualified.
            return self.fail(format!("{}: the scene cannot be read", self.label()));
        };
        // The before reading is judged first, so a change that reverts inside the dispatch is refused,
        // and recorded, as the before reading that showed it.
        let refused = self.scene.as_ref().and_then(|settled| {
            [("before", before, &reading.cached_before), ("after", after, &reading.cached_after)]
                .into_iter()
                .find_map(|(side, read, cached)| {
                    let change = settled.difference(read)?;
                    let rejection = Rejection {
                        side,
                        settled_title: settled.title.clone(),
                        read_title: read.title.clone(),
                        cached: cached.clone(),
                    };
                    Some((change, rejection))
                })
        });
        if let Some((change, rejection)) = refused {
            // When: the settled scene differs on either side, C and D no longer redraw it.
            self.rejection = Some(rejection);
            return self.fail(format!("{}: the scene's {change}", self.label()));
        }
        if self.scene.is_none() {
            // When: no scene is recorded yet (settling), the barrier's inputs are read before any action.
            if let Some(problem) = self.barrier.fixture_problem.clone() {
                // When: the fixture cannot give a final title, waiting for it is pointless: invalid at once.
                return self.fail(format!("{}: fixture: {problem}", self.label()));
            }
            // Every settling dispatch, zero-attempt ones included, counts its readings in order.
            self.account(reading.cached_before.as_ref());
            self.account(reading.cached_after.as_ref());
            self.last_title = Some(after.title.clone());
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
            return self
                .fail(format!("{label}: {} attempts in one dispatch: {delta:?}", delta.attempts));
        }
        match self.stage {
            Stage::Settling { steady, deadline } => {
                // A steady frame drew without a miss or reset, left the scene as it found it, showed
                // the scene of the frames before it, with nothing drawn missing and exactly the fixture.
                let unchanged = before == after
                    && self.candidate.as_ref().is_none_or(|candidate| candidate == after);
                let steady_frame = delta.presented == 1
                    && delta.resets == 0
                    && delta.misses == 0
                    && unchanged
                    && after.fallback_settled()
                    && scene_problem(&after.rows).is_none()
                    && self.barrier_unmet(Some(&after.title)).is_none();
                let steady = if steady_frame { steady + 1 } else { 0 };
                self.candidate = steady_frame.then(|| after.clone());
                if steady >= STEADY_FRAMES {
                    // When: the scene drew STEADY_FRAMES steady frames in a row, the episodes start.
                    self.scene = Some(after.clone());
                    self.stage = Stage::Step {
                        episode: 0,
                        frame: Frame::Retried,
                        deadline: now + STEP_BOUND,
                    };
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
        let reason = self.timeout_reason("not attempted within its bound");
        self.stage = Stage::Invalid;
        Some(reason)
    }

    /// The reason a step ran out of time, `what` saying how: a late dispatch (`observe`) or none at all
    /// (`expire`). While settling it adds the unmet barrier condition from the last in-time reading, so
    /// either path names why the scene never settled.
    fn timeout_reason(&self, what: &str) -> String {
        let bound = format!("{}: {what}", self.label());
        let unmet = matches!(self.stage, Stage::Settling { .. })
            .then(|| self.barrier_unmet(self.last_title.as_deref()))
            .flatten();
        match unmet {
            Some(unmet) => format!("{bound}; barrier unmet: {unmet}"),
            None => bound,
        }
    }

    /// Count one applied foreground reading toward the barrier. A missing entry or a cleared sample resets
    /// the count and the steady run; a sample naming a process counts once, when it was taken after T and
    /// after the last one counted.
    fn account(&mut self, cached: Option<&AppliedSample>) {
        self.last_cached = cached.map(|sample| sample.process.clone());
        let named = cached.and_then(|sample| Some((sample.sampled_at, sample.process.as_ref()?)));
        let Some((sampled_at, process)) = named else {
            // When: no entry or a cleared sample, nothing shows the final process applied: start over.
            self.qualifying = Qualifying::default();
            self.candidate = None;
            if let Stage::Settling { deadline, .. } = self.stage {
                // When: still settling, the steady run restarts with the count.
                self.stage = Stage::Settling { steady: 0, deadline };
            }
            return;
        };
        let after_final = self.barrier.final_at.is_some_and(|final_at| sampled_at > final_at);
        let newer = self.qualifying.last_at.is_none_or(|last_at| sampled_at > last_at);
        if after_final && newer {
            // When: taken after T and after the last counted sample, this is a new worker observation.
            self.qualifying.count += 1;
            self.qualifying.last_at = Some(sampled_at);
            self.qualifying.latest = Some(process.clone());
        }
    }

    /// Why the settle barrier is not met for a reading titled `title`; `None` once it is.
    fn barrier_unmet(&self, title: Option<&str>) -> Option<String> {
        let barrier = &self.barrier;
        if barrier.final_at.is_none() {
            // When: `final_at` is unset, the final process was never observed, so no sample can count.
            let name = escape_text(&barrier.final_name);
            return Some(format!("the final process \"{name}\" was never observed"));
        }
        let count = self.qualifying.count;
        if count < QUALIFYING_OBSERVATIONS {
            // When: fewer than two samples were taken after T, the one counted may still be stale.
            let plural = if count == 1 { "" } else { "s" };
            return Some(format!(
                "{count} qualifying foreground observation{plural} after the final process \
                 (need {QUALIFYING_OBSERVATIONS}), latest cached {}",
                self.cached_text()
            ));
        }
        let latest = self.qualifying.latest.as_deref().unwrap_or_default();
        if latest != barrier.final_name {
            // When: the latest counted sample names another process, the final one is not applied.
            return Some(format!(
                "the latest qualifying foreground observation names \"{}\", not \"{}\"",
                escape_text(latest),
                escape_text(&barrier.final_name)
            ));
        }
        match (barrier.expected_title.as_deref(), title) {
            (Some(expected), Some(read)) if expected == read => None,
            (Some(expected), Some(read)) => Some(format!(
                "the title \"{}\" is not the expected \"{}\"",
                escape_text(read),
                escape_text(expected)
            )),
            (Some(_), None) => Some("no title was read".to_owned()),
            (None, _) => Some("the expected final title cannot be computed".to_owned()),
        }
    }

    /// The last settling reading's applied process, for a reason.
    fn cached_text(&self) -> String {
        match &self.last_cached {
            None => "nothing (no cache entry)".to_owned(),
            Some(None) => "none".to_owned(),
            Some(Some(name)) => format!("\"{}\"", escape_text(name)),
        }
    }

    /// When the current step must have been attempted; `None` once the machine has ended.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        match self.stage {
            Stage::Settling { deadline, .. } | Stage::Step { deadline, .. } => Some(deadline),
            Stage::Done | Stage::Invalid => None,
        }
    }

    /// Whether the scene has settled and the episodes have started or ended; tests read it.
    #[cfg(test)]
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

    /// The scene settling recorded, `None` before it settles.
    pub(crate) fn scene(&self) -> Option<&Scene> {
        self.scene.as_ref()
    }

    /// The reading that ended a settled run, as it was when refused; `None` when no scene change ended it
    /// (an expired step, counts that do not fit, or no failure).
    pub(crate) fn rejection(&self) -> Option<&Rejection> {
        self.rejection.as_ref()
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
            return Some(format!(
                "record {index} is episode {} {}",
                record.episode,
                record.frame.as_str()
            ));
        }
        if record.counts.attempts != 1 || !frame.fits(record.counts) {
            return Some(format!(
                "record {index} does not fit {}: {:?}",
                frame.as_str(),
                record.counts
            ));
        }
        if frame != Frame::Retried && record.counts.atlas_dim != recovered_dim {
            return Some(format!("record {index} draws at another atlas dimension"));
        }
    }
    None
}

/// `text` on one ASCII line for a log or a reason: quotes and backslashes escaped, and every other
/// character outside printable ASCII written as `\u{…}`, so an icon glyph reads as its code point.
pub(crate) fn escape_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' | '\\' => {
                escaped.push('\\');
                escaped.push(character);
            }
            ' '..='~' => escaped.push(character),
            _ => escaped.extend(character.escape_unicode()),
        }
    }
    escaped
}

/// Milliseconds from `origin` to `at`; negative when `at` is earlier, saturating at the `i64` range.
pub(crate) fn relative_ms(at: Instant, origin: Instant) -> i64 {
    match at.checked_duration_since(origin) {
        Some(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        None => {
            i64::try_from(origin.duration_since(at).as_millis()).map_or(i64::MIN, |before| -before)
        }
    }
}

/// The role session's process identity, from `sessions/0.json`: the macOS script's leader and its
/// cleanup anchor, or the Windows role program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionIdentity {
    /// The pane's leader: the macOS script's `leader_pid`, or the Windows `program_pid`.
    pub(crate) leader_pid: u32,
    /// The macOS cleanup anchor; Windows has none.
    pub(crate) anchor_pid: Option<u32>,
}

/// The session identity in `text`, a `sessions/<role>.json` record; `None` when it is not JSON or its
/// leader pid is missing, not a positive integer, or out of range.
pub(crate) fn parse_session(text: &str) -> Option<SessionIdentity> {
    let record: Value = serde_json::from_str(text).ok()?;
    let pid = |key: &str| {
        let number = record.get(key)?.as_u64().filter(|number| *number > 0)?;
        u32::try_from(number).ok()
    };
    let leader_pid = pid("leader_pid").or_else(|| pid("program_pid"))?;
    Some(SessionIdentity { leader_pid, anchor_pid: pid("anchor_pid") })
}

/// The longest role session record read: a real one is under 200 bytes, so anything longer is refused
/// rather than read whole.
pub(crate) const SESSION_RECORD_LIMIT: u64 = 4096;

/// The role session identity in the file at `path`, reading at most `SESSION_RECORD_LIMIT` bytes; `None`
/// when it cannot be read, is longer than that, is not UTF-8, or does not parse.
pub(crate) fn read_session(path: &std::path::Path) -> Option<SessionIdentity> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(SESSION_RECORD_LIMIT + 1)
        .read_to_string(&mut text)
        .ok()?;
    let within_limit = text.len() as u64 <= SESSION_RECORD_LIMIT;
    within_limit.then(|| parse_session(&text)).flatten()
}

/// The reading an evidence line describes, taken when the judge saw it and never reread from the App.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observed {
    /// `after` (the settling or refused after reading), `before` (a refused before reading) or `none`.
    pub(crate) observation: &'static str,
    /// The settled scene's title; `None` before settling.
    pub(crate) settled_title: Option<String>,
    /// That reading's title; `None` when there is no reading.
    pub(crate) read_title: Option<String>,
    /// The App's applied foreground sample at that reading.
    pub(crate) cached: Option<AppliedSample>,
}

/// What the settle line describes: the after reading of the dispatch that settled `settled`, with the
/// sample captured alongside it.
pub(crate) fn settle_observation(settled: &Scene, reading: &SceneReading) -> Observed {
    Observed {
        observation: "after",
        settled_title: Some(settled.title.clone()),
        read_title: reading.after.as_ref().map(|scene| scene.title.clone()),
        cached: reading.cached_after.clone(),
    }
}

/// What the failure line describes: the reading `machine` kept when it refused it, or, for a failure no
/// scene change caused, the settled title alone. Nothing is read from the App here.
pub(crate) fn failure_observation(machine: &RecoveryEpisodes) -> Observed {
    match machine.rejection() {
        Some(rejection) => Observed {
            observation: rejection.side,
            settled_title: Some(rejection.settled_title.clone()),
            read_title: Some(rejection.read_title.clone()),
            cached: rejection.cached.clone(),
        },
        None => Observed {
            observation: "none",
            settled_title: machine.scene().map(|scene| scene.title.clone()),
            read_title: None,
            cached: None,
        },
    }
}

/// The harness's own search for the final process. On macOS it looks up the leader's deepest descendant,
/// at most once per `LOOKUP_SPACING`, until a lookup completing before the settle deadline names the
/// expected process; Windows fixes it at the sentinel. The instant it was found is T for the barrier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinalLookup {
    /// The final process's name, as the App's sampler reports it.
    expected: String,
    /// When the last lookup started.
    last_started: Option<Instant>,
    /// T: when the final process was found.
    found_at: Option<Instant>,
    /// Lookups made.
    lookups: u64,
    /// Their total time.
    spent: Duration,
}

impl FinalLookup {
    /// A search for `expected` that has not found it yet.
    pub(crate) fn new(expected: String) -> Self {
        Self { expected, last_started: None, found_at: None, lookups: 0, spent: Duration::ZERO }
    }

    /// A search already settled at `found_at`, with no lookup: Windows, where the sentinel fixes T.
    pub(crate) fn found(expected: String, found_at: Option<Instant>) -> Self {
        Self { found_at, ..Self::new(expected) }
    }

    /// The final process's name.
    pub(crate) fn expected(&self) -> &str {
        &self.expected
    }

    /// T, once found.
    pub(crate) fn found_at(&self) -> Option<Instant> {
        self.found_at
    }

    /// Lookups made so far.
    pub(crate) fn lookups(&self) -> u64 {
        self.lookups
    }

    /// The lookups' total time.
    pub(crate) fn spent(&self) -> Duration {
        self.spent
    }

    /// Whether a lookup naming `process` found the final process.
    pub(crate) fn accepts(&self, process: Option<&str>) -> bool {
        process == Some(self.expected.as_str())
    }

    /// Whether a lookup may start at `now`, recording it when it may: never once found, never at or past
    /// `deadline` (`None` once the machine has ended), and never within `LOOKUP_SPACING` of the last.
    pub(crate) fn begin(&mut self, now: Instant, deadline: Option<Instant>) -> bool {
        if self.found_at.is_some() {
            // When: `found_at` is set, T is fixed and no further lookup is needed.
            return false;
        }
        if deadline.is_none_or(|deadline| now >= deadline) {
            // When: the deadline has passed, or the machine ended, a lookup could not count.
            return false;
        }
        if self.last_started.is_some_and(|last| now < last + LOOKUP_SPACING) {
            // When: the last lookup started within `LOOKUP_SPACING`, this one waits.
            return false;
        }
        self.last_started = Some(now);
        true
    }

    /// Record a lookup started at `started` and completed at `completed` naming `process`. It sets T to
    /// `completed` only when it names the expected process and completed before `deadline`; it says whether.
    pub(crate) fn finish(
        &mut self,
        process: Option<&str>,
        started: Instant,
        completed: Instant,
        deadline: Option<Instant>,
    ) -> bool {
        self.lookups += 1;
        self.spent += completed.saturating_duration_since(started);
        let in_time = deadline.is_some_and(|deadline| completed < deadline);
        if in_time && self.accepts(process) {
            // When: the expected process completed before the deadline, T is its completion instant.
            self.found_at = Some(completed);
            return true;
        }
        false
    }
}

/// The foreground sample the App had applied for the armed pane when the evidence was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedForeground {
    /// The cached process name; `None` for a cleared sample (unavailable sampler or exited child).
    pub(crate) process: Option<String>,
    /// When the App's worker took the sample, in ms relative to the driver's start.
    pub(crate) sampled_rel_ms: i64,
}

/// One foreground lookup the harness made itself. It is never the App's sample.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HarnessLookup {
    /// The deepest descendant's name the lookup returned.
    pub(crate) process: Option<String>,
    /// When the lookup completed, in ms relative to the driver's start.
    pub(crate) observed_rel_ms: i64,
}

/// The identity S1/atlas-retry logs once when the scene settles and once when the run fails: the titles
/// compared, the App's applied foreground sample, the role's process identity and the counters that
/// move with a title change, kept apart from any lookup the harness made itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Evidence {
    /// `settle` or `failure`.
    pub(crate) event: &'static str,
    /// The reading the titles and the App's sample come from: `after` (the settling or refused after
    /// reading), `before` (a refused before reading) or `none` (a failure no scene change caused).
    pub(crate) observation: &'static str,
    /// The settled scene's title; `None` before settling.
    pub(crate) settled_title: Option<String>,
    /// The title of that reading; `None` when there is none.
    pub(crate) read_title: Option<String>,
    /// The App's applied sample at that reading; `None` when the pane had no cache entry or no reading.
    pub(crate) app_cached: Option<CachedForeground>,
    /// The role session's identity; `None` when `sessions/0.json` is missing or malformed.
    pub(crate) session: Option<SessionIdentity>,
    /// The main window's tab titles shaped on a title-cache miss, cumulative, read when the line is logged.
    pub(crate) tab_title_prepares: Option<u64>,
    /// The App's completed foreground-worker batches.
    pub(crate) fg_worker_probes: Option<u64>,
    /// Foreground results dropped as stale.
    pub(crate) fg_results_stale: Option<u64>,
    /// Milliseconds from the driver's start to settling; `None` for a failure.
    pub(crate) settle_ms: Option<u64>,
    /// The harness's own lookup, labelled apart from the App's sample.
    pub(crate) harness_lookup: Option<HarnessLookup>,
    /// Final-process lookups made for the settle barrier; `None` where none can be made (Windows).
    pub(crate) final_lookups: Option<u64>,
    /// Their total time, in µs.
    pub(crate) final_lookup_us: Option<u64>,
    /// T, in ms relative to the driver's start; `None` when the final process was never observed.
    pub(crate) final_at_rel_ms: Option<i64>,
}

impl Evidence {
    /// The evidence as one `key=value` log line; an absent value reads `n/a`.
    pub(crate) fn line(&self) -> String {
        let absent = || "n/a".to_owned();
        let quoted = |text: Option<&str>| {
            text.map_or_else(absent, |text| format!("\"{}\"", escape_text(text)))
        };
        let number = |value: Option<String>| value.unwrap_or_else(absent);
        // A sample that is present but names no process (cleared) reads `none`, apart from an absent one.
        let process = |found: Option<Option<&str>>| {
            found.map_or_else(absent, |name| {
                name.map_or_else(|| "none".to_owned(), |name| quoted(Some(name)))
            })
        };
        let cached = self.app_cached.as_ref();
        let lookup = self.harness_lookup.as_ref();
        [
            format!("event={}", self.event),
            format!("observation={}", self.observation),
            format!("settled_title={}", quoted(self.settled_title.as_deref())),
            format!("read_title={}", quoted(self.read_title.as_deref())),
            format!(
                "app_cached_process={}",
                process(cached.map(|sample| sample.process.as_deref()))
            ),
            format!(
                "app_cached_sampled_rel_ms={}",
                number(cached.map(|sample| sample.sampled_rel_ms.to_string()))
            ),
            format!(
                "leader_pid={}",
                number(self.session.map(|identity| identity.leader_pid.to_string()))
            ),
            format!(
                "anchor_pid={}",
                number(
                    self.session
                        .and_then(|identity| identity.anchor_pid)
                        .map(|pid| pid.to_string())
                )
            ),
            format!(
                "tab_title_prepares={}",
                number(self.tab_title_prepares.map(|count| count.to_string()))
            ),
            format!(
                "fg_worker_probes={}",
                number(self.fg_worker_probes.map(|count| count.to_string()))
            ),
            format!(
                "fg_results_stale={}",
                number(self.fg_results_stale.map(|count| count.to_string()))
            ),
            format!("settle_ms={}", number(self.settle_ms.map(|elapsed| elapsed.to_string()))),
            format!(
                "harness_lookup_process={}",
                process(lookup.map(|found| found.process.as_deref()))
            ),
            format!(
                "harness_lookup_rel_ms={}",
                number(lookup.map(|found| found.observed_rel_ms.to_string()))
            ),
            format!("final_lookups={}", number(self.final_lookups.map(|count| count.to_string()))),
            format!(
                "final_lookup_us={}",
                number(self.final_lookup_us.map(|elapsed| elapsed.to_string()))
            ),
            format!(
                "final_at_rel_ms={}",
                number(self.final_at_rel_ms.map(|elapsed| elapsed.to_string()))
            ),
        ]
        .join(" ")
    }
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
    (1..=70)
        .map(|number| format!("{ROW_PREFIX}{number:02} abcdefghijklmnopqrstuvwxyz {number:02}\n"))
        .collect()
}

/// Why the visible rows are not the fixture: every row but at most the last two (the sentinel and
/// a prompt) must be exactly a fixture line, and those lines must be consecutive.
pub(crate) fn scene_problem(rows: &[String]) -> Option<String> {
    let fixture: Vec<String> = fixture_text().lines().map(str::to_owned).collect();
    let numbers: Vec<usize> =
        rows.iter().filter_map(|row| fixture.iter().position(|line| line == row)).collect();
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
