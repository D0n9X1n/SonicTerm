//! Atlas retry evidence formatting, session identity and recorded verdicts.

use super::*;

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

impl FinalLookup {
    /// A search for `expected` that has not found it yet.
    pub(crate) fn new(expected: String) -> Self {
        Self { expected, last_started: None, found_at: None, lookups: 0, spent: Duration::ZERO }
    }
}

impl FinalLookup {
    /// A search already settled at `found_at`, with no lookup: Windows, where the sentinel fixes T.
    pub(crate) fn found(expected: String, found_at: Option<Instant>) -> Self {
        Self { found_at, ..Self::new(expected) }
    }
}

impl FinalLookup {
    /// The final process's name.
    pub(crate) fn expected(&self) -> &str {
        &self.expected
    }
}

impl FinalLookup {
    /// T, once found.
    pub(crate) fn found_at(&self) -> Option<Instant> {
        self.found_at
    }
}

impl FinalLookup {
    /// Lookups made so far.
    pub(crate) fn lookups(&self) -> u64 {
        self.lookups
    }
}

impl FinalLookup {
    /// The lookups' total time.
    pub(crate) fn spent(&self) -> Duration {
        self.spent
    }
}

impl FinalLookup {
    /// Whether a lookup naming `process` found the final process.
    pub(crate) fn accepts(&self, process: Option<&str>) -> bool {
        process == Some(self.expected.as_str())
    }
}

impl FinalLookup {
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
}

impl FinalLookup {
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
#[path = "atlas_retry_evidence_tests.rs"]
mod atlas_retry_evidence_tests;
