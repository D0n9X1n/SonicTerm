//! OSC 7501 program status (kitty's Program Status Protocol): what a program says it is doing.
//!
//! The raw collector owns the whole `7501;` OSC (at most [`MAX_STATUS_BODY_BYTES`] wire bytes)
//! and hands it here only once complete. A report follows kitty's grammar and limits: a bad report
//! is dropped whole, a bad pair is skipped. Each pane keeps at most [`MAX_STATUS_RECORDS`]
//! records. Ids, apps, titles and messages are program-supplied display data: never logged.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;

use super::{Parser, Performer, VtEvent};

/// The largest report body after `7501;`, in wire bytes: kitty's 4096-byte sequence less the
/// 7-byte introducer and the 2-byte ST, for either terminator.
pub(super) const MAX_STATUS_BODY_BYTES: usize = 4087;
/// The most records one pane holds; the least recently reported is evicted first.
pub(super) const MAX_STATUS_RECORDS: usize = 32;
/// The longest key, in characters; a longer one discards the report.
const MAX_KEY_CHARS: usize = 16;
/// The longest `app`, in bytes; a longer one discards the report.
const MAX_APP_BYTES: usize = 32;
/// `title`'s encoded and decoded caps, in bytes.
const MAX_TITLE_ENCODED_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 192;
/// `msg`'s encoded and decoded caps, in bytes.
const MAX_MSG_ENCODED_BYTES: usize = 2732;
const MAX_MSG_BYTES: usize = 2048;
/// One `id` segment's cap in bytes, and the most segments.
const MAX_ID_SEGMENT_BYTES: usize = 32;
const MAX_ID_DEPTH: usize = 8;
/// The most heap one pane's store owns: a record vector that doubles up to twice
/// [`MAX_STATUS_RECORDS`] slots before its eviction, and each record's shared block and strings.
/// Decoded text is sized from its encoding, so the encoded caps bound `title` and `msg`.
pub(super) const MAX_PROGRAM_STATUS_BYTES: usize =
    2 * MAX_STATUS_RECORDS * std::mem::size_of::<StoredRecord>()
        + MAX_STATUS_RECORDS
            * (2 * std::mem::size_of::<usize>()
                + std::mem::size_of::<StatusRecord>()
                + MAX_ID_SEGMENT_BYTES * MAX_ID_DEPTH
                + MAX_ID_DEPTH
                + MAX_APP_BYTES
                + MAX_TITLE_ENCODED_BYTES
                + MAX_MSG_ENCODED_BYTES);
/// The fixed query reply body; it lists what SonicTerm accepts and discloses no stored data.
const QUERY_REPLY: &str =
    "7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

/// Standard base64 with optional padding, as kitty accepts.
const TEXT_ENGINE: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Orders record changes across every pane, so the tab marker breaks urgency ties by the most
/// recently changed record of any pane.
static CHANGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// The next change stamp.
// Ordering: CHANGE_SEQUENCE uses Relaxed; it only orders stamps and publishes no other memory.
fn next_sequence() -> u64 {
    CHANGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// What a program says it is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusState {
    /// Waiting for work.
    Idle,
    /// Working, optionally with a percentage.
    Working,
    /// Finished; shown until the user sees it.
    Done,
    /// Waiting for the user.
    Blocked,
    /// Failed; shown until the user sees it.
    Error,
}

impl StatusState {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    /// Higher is more urgent: blocked, error, done, working, idle, as kitty ranks them.
    pub fn urgency(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Working => 1,
            Self::Done => 2,
            Self::Error => 3,
            Self::Blocked => 4,
        }
    }

    /// The state a wire name names, or `None` for an unknown one.
    fn parse(name: &str) -> Option<Self> {
        [Self::Idle, Self::Working, Self::Done, Self::Blocked, Self::Error]
            .into_iter()
            .find(|state| state.as_str() == name)
    }

    /// Whether a new prompt or the program's exit ends the record.
    fn is_transient(self) -> bool {
        matches!(self, Self::Idle | Self::Working | Self::Blocked)
    }
}

/// Why a blocked program waits for the user. Advisory only: never a request to approve anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusKind {
    /// A permission decision.
    Permission,
    /// A question.
    Question,
    /// Authentication.
    Auth,
}

impl StatusKind {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }

    /// The kind a wire name names, or `None` for an unknown one.
    fn parse(name: &str) -> Option<Self> {
        [Self::Permission, Self::Question, Self::Auth]
            .into_iter()
            .find(|kind| kind.as_str() == name)
    }
}

/// One record, keyed by `id` (empty for the root record).
#[derive(Clone, PartialEq, Eq)]
pub struct StatusRecord {
    id: String,
    state: StatusState,
    kind: Option<StatusKind>,
    progress: Option<u8>,
    app: String,
    title: String,
    msg: String,
}

impl StatusRecord {
    /// The record id; empty for the root record.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// The state.
    pub fn state(&self) -> StatusState {
        self.state
    }
    /// The kind, kept only for `blocked`.
    pub fn kind(&self) -> Option<StatusKind> {
        self.kind
    }
    /// The percentage 0-100, kept only for `working` and `blocked`.
    pub fn progress(&self) -> Option<u8> {
        self.progress
    }
    /// The reporting program's name; empty when absent or invalid. Never identity.
    pub fn app(&self) -> &str {
        &self.app
    }
    /// The decoded title; untrusted display text.
    pub fn title(&self) -> &str {
        &self.title
    }
    /// The decoded message; untrusted display text.
    pub fn msg(&self) -> &str {
        &self.msg
    }

    /// Heap bytes this record owns: its shared block (two counts and the record) and the
    /// capacity of each of its strings.
    fn owned_bytes(&self) -> usize {
        2 * std::mem::size_of::<usize>()
            + std::mem::size_of::<StatusRecord>()
            + self.id.capacity()
            + self.app.capacity()
            + self.title.capacity()
            + self.msg.capacity()
    }
}

impl std::fmt::Debug for StatusRecord {
    /// The typed fields only: ids, apps, titles and messages are program-supplied.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StatusRecord")
            .field("state", &self.state)
            .field("kind", &self.kind)
            .field("progress", &self.progress)
            .finish()
    }
}

/// A stored record with its two clocks: last changed (tie order) and last reported (eviction).
#[derive(Clone)]
struct StoredRecord {
    record: Arc<StatusRecord>,
    changed_at: u64,
    reported_at: u64,
}

/// One pane's records, oldest change first, shared with readers as a snapshot.
#[derive(Clone, Default)]
pub struct ProgramStatus {
    records: Vec<StoredRecord>,
    /// Moves on every change, acknowledgment and clear included, never on an identical report.
    generation: u64,
}

impl ProgramStatus {
    /// Each record, oldest change first.
    pub fn records(&self) -> impl Iterator<Item = &StatusRecord> {
        self.records.iter().map(|stored| stored.record.as_ref())
    }

    /// Whether no record is held.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// How many records are held.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// The change count a published summary carries.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The most urgent record, ties broken by the most recent change, and whether any `done` or
    /// `error` record waits for the user.
    pub fn summary(&self) -> StatusSummary {
        let top = self
            .records
            .iter()
            .max_by_key(|stored| (stored.record.state.urgency(), stored.changed_at))
            .map(|stored| StatusTop {
                state: stored.record.state,
                progress: stored.record.progress,
                changed_at: stored.changed_at,
            });
        let unseen = self
            .records
            .iter()
            .any(|stored| matches!(stored.record.state, StatusState::Done | StatusState::Error));
        StatusSummary { generation: self.generation, top, unseen }
    }

    /// Heap bytes the store owns: its record vector's capacity and every record's shared block,
    /// its share of [`super::MAX_PROGRAM_METADATA_BYTES`]. Persistent pane state, never capture.
    pub fn retained_bytes(&self) -> usize {
        self.records.capacity() * std::mem::size_of::<StoredRecord>()
            + self.records.iter().map(|stored| stored.record.owned_bytes()).sum::<usize>()
    }

    /// Apply one parsed report; returns whether anything changed.
    fn apply(&mut self, report: Report) -> bool {
        let record = match report {
            Report::Clear(None) => {
                // When: `report` is a `Clear` without an id, every record goes.
                return self.remove_where(|_| true);
            }
            Report::Clear(Some(ancestor)) => {
                // When: `report` is a `Clear` of `ancestor`, it and its descendants go.
                return self.remove_where(|record| is_descendant_or_self(&record.id, &ancestor));
            }
            Report::Set(record) => record,
        };
        let stamp = next_sequence();
        if let Some(stored) = self.records.iter_mut().find(|stored| stored.record.id == record.id) {
            // When: a `stored` record has this `record.id`, its report stamp moves either way.
            stored.reported_at = stamp;
            if *stored.record == record {
                // When: `stored.record` equals `record`, only its eviction recency moves.
                return false;
            }
        }
        self.records.retain(|stored| stored.record.id != record.id);
        self.records.push(StoredRecord {
            record: Arc::new(record),
            changed_at: stamp,
            reported_at: stamp,
        });
        if self.records.len() > MAX_STATUS_RECORDS {
            // The store is past its cap, so the least recently reported record goes.
            if let Some(oldest) = self
                .records
                .iter()
                .enumerate()
                .min_by_key(|(_, stored)| stored.reported_at)
                .map(|(index, _)| index)
            {
                self.records.remove(oldest);
            }
        }
        self.generation += 1;
        true
    }

    /// Remove every record `ends` selects; returns whether any was removed.
    fn remove_where(&mut self, ends: impl Fn(&StatusRecord) -> bool) -> bool {
        let before = self.records.len();
        self.records.retain(|stored| !ends(&stored.record));
        let changed = self.records.len() != before;
        if changed {
            self.generation += 1;
        }
        changed
    }
}

impl std::fmt::Debug for ProgramStatus {
    /// Counts only: record text printed in a log line would leak program-supplied data.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgramStatus")
            .field("records", &self.records.len())
            .field("generation", &self.generation)
            .finish()
    }
}

/// The most urgent record of a pane, for the tab marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusTop {
    /// Its state.
    pub state: StatusState,
    /// Its percentage, for `working` and `blocked`.
    pub progress: Option<u8>,
    /// When it last changed, comparable across panes.
    pub changed_at: u64,
}

/// What a pane's records show, small enough to publish per batch and copy without the parser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusSummary {
    /// The store's generation when this was taken; an older summary never replaces a newer one.
    pub generation: u64,
    /// The most urgent record, or `None` when none is held.
    pub top: Option<StatusTop>,
    /// Whether a `done` or `error` record waits for the user to focus the pane or type into it.
    pub unseen: bool,
}

/// One valid report.
enum Report {
    /// Store this record.
    Set(StatusRecord),
    /// Remove this id and its descendants, or every record for `None`.
    Clear(Option<String>),
}

/// Whether `id` is `ancestor` or below it.
fn is_descendant_or_self(id: &str, ancestor: &str) -> bool {
    id == ancestor || id.strip_prefix(ancestor).is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `byte` may appear in a value (`[A-Za-z0-9_.,+/=-]`).
fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte)
}

/// Whether `text` is a non-empty name (`[A-Za-z0-9_.+-]+`).
fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte))
}

/// Whether `id` is `/`-separated names, each at most 32 bytes, at most 8 deep.
fn is_valid_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('/').collect();
    segments.len() <= MAX_ID_DEPTH
        && segments.iter().all(|segment| segment.len() <= MAX_ID_SEGMENT_BYTES && is_name(segment))
}

/// Decode a base64 `title` or `msg` as strict UTF-8 without controls, within its caps.
fn decode_text(
    encoded: &str,
    max_encoded: usize,
    max_decoded: usize,
) -> Result<String, &'static str> {
    if encoded.len() > max_encoded {
        // When: `encoded` is past its encoded cap, the report is discarded undecoded.
        return Err("text_length");
    }
    // Padding is checked as supplied: canonical or none decodes, excess `=` or padding alone fails.
    let decoded = TEXT_ENGINE.decode(encoded).map_err(|_| "text_base64")?;
    if decoded.len() > max_decoded {
        // When: `decoded` is past its decoded cap, the report is discarded, never cut.
        return Err("text_length");
    }
    let text = String::from_utf8(decoded).map_err(|_| "text_utf8")?;
    if text.chars().any(char::is_control) {
        // When: `text` holds a C0, DEL or C1 control, the report is discarded.
        return Err("text_control");
    }
    Ok(text)
}

/// Parse one report body (after `7501;`), or the reason code it is discarded for.
fn parse_report(body: &[u8]) -> Result<Report, &'static str> {
    // Invalid UTF-8 is outside the value alphabet, so its pair is skipped, as kitty reads it.
    let body = String::from_utf8_lossy(body);
    let mut values: Vec<(&str, &str)> = Vec::new();
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            // When: the pair has no `=`, it is skipped.
            continue;
        };
        let (key, value) =
            (key.trim_matches(is_kitty_whitespace), value.trim_matches(is_kitty_whitespace));
        if key.chars().count() > MAX_KEY_CHARS {
            // When: the key is past 16 characters, the report is discarded.
            return Err("key_length");
        }
        if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_lowercase()) {
            // When: the key is outside `[a-z]+`, the pair is skipped.
            continue;
        }
        if !value.bytes().all(is_value_byte) {
            // When: the value is outside the alphabet, the pair is skipped, but a bad `id` never
            // falls back to the root record.
            if key == "id" {
                // When: `key` is `id`, a malformed id discards the report instead of meaning the root.
                return Err("id");
            }
            continue;
        }
        // Every admitted value meets its cap, even when a later pair replaces it.
        let within = match key {
            "id" => is_valid_id(value),
            "app" => value.len() <= MAX_APP_BYTES,
            "msg" => value.len() <= MAX_MSG_ENCODED_BYTES,
            "title" => value.len() <= MAX_TITLE_ENCODED_BYTES,
            _ => true,
        };
        if !within {
            // When: `within` is false, an `id`, `app`, `msg` or `title` broke its rule and the
            // report is discarded.
            return Err("field_length");
        }
        values.retain(|(held, _)| *held != key);
        values.push((key, value));
    }
    let value_of =
        |key: &str| values.iter().find(|(held, _)| *held == key).map(|(_, value)| *value);
    let title = match value_of("title").filter(|text| !text.is_empty()) {
        Some(encoded) => decode_text(encoded, MAX_TITLE_ENCODED_BYTES, MAX_TITLE_BYTES)?,
        None => String::new(),
    };
    let msg = match value_of("msg").filter(|text| !text.is_empty()) {
        Some(encoded) => decode_text(encoded, MAX_MSG_ENCODED_BYTES, MAX_MSG_BYTES)?,
        None => String::new(),
    };
    let id = value_of("id");
    let state_name = value_of("state").unwrap_or("");
    if state_name == "clear" {
        // When: `state_name` is `clear`, an omitted `id` clears every record.
        return Ok(Report::Clear(id.map(str::to_owned)));
    }
    // An unknown state discards the report, so a state added later never becomes another one.
    let state = StatusState::parse(state_name).ok_or("state")?;
    let kind = (state == StatusState::Blocked)
        .then(|| value_of("kind").and_then(StatusKind::parse))
        .flatten();
    let progress = matches!(state, StatusState::Working | StatusState::Blocked)
        .then(|| value_of("progress").and_then(parse_percent))
        .flatten();
    let app = value_of("app").filter(|app| is_name(app)).unwrap_or("").to_owned();
    let id = id.unwrap_or("").to_owned();
    Ok(Report::Set(StatusRecord { id, state, kind, progress, app, title, msg }))
}

/// kitty's whitespace set: space, tab, LF, CR, VT and FF.
fn is_kitty_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// A `[0-9]+` percentage of at most 100.
fn parse_percent(text: &str) -> Option<u8> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        // When: `text` is not a digit run, there is no percentage.
        return None;
    }
    let value = text.bytes().fold(0u32, |value, digit| {
        value.saturating_mul(10).saturating_add(u32::from(digit - b'0'))
    });
    u8::try_from(value).ok().filter(|percent| *percent <= 100)
}

impl Performer {
    /// Apply one complete OSC 7501 `body`: answer a query with the request's terminator, or apply
    /// a report. A discarded report logs a fixed reason code, rate-limited, never its bytes.
    pub(super) fn apply_program_status_osc(&mut self, body: &[u8], bell_terminated: bool) {
        if body == b"?" {
            // When: `body` is the `?` query, the fixed reply answers it and nothing is stored.
            let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
            self.reply(format!("\x1b]{QUERY_REPLY}{terminator}").as_bytes());
            return;
        }
        match parse_report(body) {
            Ok(report) => {
                // The store is copied only when a reader still holds the previous snapshot, and
                // records are shared, so the copy is one vector of pointers.
                if Arc::make_mut(&mut self.program_status).apply(report) {
                    self.note_program_status_change();
                }
            }
            Err(reason) => {
                self.program_status_refusals = self.program_status_refusals.saturating_add(1);
                // Powers of two only, so a flood of bad reports logs a handful of lines.
                if self.program_status_refusals.is_power_of_two() {
                    tracing::debug!(
                        target: "vt",
                        reason,
                        refusals = self.program_status_refusals,
                        "OSC 7501 report discarded"
                    );
                }
            }
        }
    }

    /// Remove every record `ends` selects; marks the batch for a summary when any went.
    pub(super) fn end_program_status(&mut self, ends: impl Fn(&StatusRecord) -> bool) -> bool {
        let selected = self.program_status.records().any(&ends);
        // When: nothing is selected, the shared snapshot is not copied.
        if selected && Arc::make_mut(&mut self.program_status).remove_where(ends) {
            self.note_program_status_change();
            return true;
        }
        false
    }

    /// Mark the batch for a summary and publish whether a `done` or `error` record is held.
    fn note_program_status_change(&mut self) {
        self.program_status_dirty = true;
        self.program_status_unseen.store(self.program_status.summary().unseen, Ordering::SeqCst);
    }

    /// An admitted OSC 133 prompt start ends working, blocked and idle records, in stream order.
    pub(super) fn end_transient_program_status(&mut self) -> bool {
        self.end_program_status(|record| record.state.is_transient())
    }

    /// Push one summary event when the batch changed the records.
    pub(super) fn publish_program_status(&mut self) {
        if std::mem::take(&mut self.program_status_dirty) {
            self.events.push(VtEvent::ProgramStatus(self.program_status.summary()));
        }
    }
}

impl Parser {
    /// The pane's OSC 7501 records as a shared snapshot, cheap to take under the parser lock and
    /// read after it. Program-supplied display metadata: never logged or evaluated.
    pub fn program_status(&self) -> Arc<ProgramStatus> {
        Arc::clone(&self.performer.program_status)
    }

    /// The flag the parser sets while it holds a `done` or `error` record, updated at every change
    /// under its lock, so the host reads its state without the lock and without the summary event.
    pub fn program_status_unseen_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.performer.program_status_unseen)
    }

    /// Heap the pane's OSC 7501 records and OSC 1337 user variables own, and how many of each it
    /// holds. Persistent pane state, separate from [`Parser::retained_amount`]'s capture bytes, so
    /// cancelling a capture never reports it as released; at most [`super::MAX_PROGRAM_METADATA_BYTES`].
    pub fn program_metadata_amount(&self) -> sonicterm_types::ResourceAmount {
        let status = &self.performer.program_status;
        let user_vars = &self.performer.user_vars;
        sonicterm_types::ResourceAmount {
            bytes: status.retained_bytes() + user_vars.retained_bytes(),
            items: status.len() + user_vars.len(),
        }
    }

    /// The summary of the pane's records, for the tab marker.
    pub fn program_status_summary(&self) -> StatusSummary {
        self.performer.program_status.summary()
    }

    /// The pane's child exited: end working, blocked and idle records. Returns whether any ended.
    pub fn end_transient_program_status(&mut self) -> bool {
        let changed = self.performer.end_transient_program_status();
        self.performer.program_status_dirty = false;
        changed
    }

    /// The user saw the pane (focus or a key): end `done` and `error` records. Returns whether
    /// any ended.
    pub fn acknowledge_program_status(&mut self) -> bool {
        let changed = self.performer.end_program_status(|record| {
            matches!(record.state, StatusState::Done | StatusState::Error)
        });
        self.performer.program_status_dirty = false;
        changed
    }
}

#[cfg(test)]
#[path = "vt_program_status_tests.rs"]
mod vt_program_status_tests;
