//! Status line model: segment inputs, the segment registry and formatters,
//! narrow-strip fitting, the shared sanitizer and the shared number, byte and
//! duration formatter.
//!
//! Pure and device-free. The App fills [`StatusLineInputs`], [`build_view`]
//! turns them into styled segments, and the renderer measures text and draws
//! what [`fit`] places. Every segment id has an exhaustive [`segment_spec`]
//! entry, so a new catalog id does not compile until it is registered.

use std::hash::{Hash, Hasher};

use sonicterm_cfg::status_line::{
    ClickBinding, MemSettings, ResolvedStatusLine, Sensitivity, ShowMode, StatusIcons,
    StatusSegmentId, StatusSide,
};
use sonicterm_types::Action;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::i18n::I18n;

/// Bytes in one MiB.
const MIB: u64 = 1024 * 1024;
/// Graphemes an ellipsized value keeps at least, so it stays recognisable.
const MIN_KEPT_GRAPHEMES: usize = 4;
/// The middle ellipsis inserted by every shortening.
const ELLIPSIS: &str = "…";
/// Byte cap of one link or path value handed to the strip.
const LINK_MAX_BYTES: usize = 4096;
/// Column cap of one link or path value before the renderer fits it.
const LINK_MAX_COLUMNS: usize = 512;

/// How a part of a segment is colored. Color is used only where the user
/// must act, and always with a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusTone {
    /// A dim name (`Mem:`) or a quiet state word.
    Name,
    /// A normal value.
    Value,
    /// Amber caution.
    Warn,
    /// Red failure.
    Error,
    /// A mode chip on a tinted background.
    Chip,
    /// A mode chip that needs attention (broadcast input).
    ChipWarn,
}

impl StatusTone {
    /// Whether the part is drawn as a chip with its own background.
    pub fn is_chip(self) -> bool {
        matches!(self, Self::Chip | Self::ChipWarn)
    }
}

/// One styled run of a segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StatusPart {
    /// Display text, already sanitized.
    pub text: String,
    /// Color role.
    pub tone: StatusTone,
    /// Removed when `hide_sensitive` is on.
    pub sensitive: bool,
    /// A text value that fitting may middle-ellipsize. Numbers never are.
    pub ellipsize: bool,
    /// A trailing detail that fitting may drop before the whole segment.
    pub droppable: bool,
}

impl StatusPart {
    /// A plain part with `tone`.
    pub fn new(text: impl Into<String>, tone: StatusTone) -> Self {
        Self { text: text.into(), tone, sensitive: false, ellipsize: false, droppable: false }
    }

    /// Mark the part as a text value fitting may middle-ellipsize.
    pub fn ellipsized(mut self) -> Self {
        self.ellipsize = true;
        self
    }

    /// Mark the part as a detail fitting may drop.
    pub fn droppable(mut self) -> Self {
        self.droppable = true;
        self
    }

    /// Mark the part as sensitive.
    pub fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }
}

/// Plain-words accessible name and value of a segment, with no glyphs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpokenSegment {
    /// The segment's spoken name (`Memory`).
    pub name: String,
    /// Its spoken value (`412 MB rising`).
    pub value: String,
}

/// What a formatter produced for one segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedSegment {
    /// The full form.
    pub parts: Vec<StatusPart>,
    /// A shorter form fitting tries before dropping anything.
    pub compact: Option<Vec<StatusPart>>,
    /// Dropped only after every unprotected segment.
    pub protected: bool,
    /// Whether the registered default click applies in this state.
    pub default_click: bool,
    /// Spoken value, with no glyphs.
    pub spoken: String,
}

impl FormattedSegment {
    /// A segment with `parts` and no compact form, protection or default click.
    fn plain(parts: Vec<StatusPart>, spoken: String) -> Self {
        Self { parts, compact: None, protected: false, default_click: false, spoken }
    }
}

/// Handlers the App runs for a segment's `default` click.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentHandler {
    /// Return the focused pane to the bottom of its history.
    ScrollToBottom,
    /// Acknowledge the focused pane's budget-trimmed history, hiding `History: trimmed`.
    AcknowledgeHistory,
    /// Go to the next marked tab after the active one, then to the next window with marks.
    NextAlert,
    /// Go to the tab whose program sent the latest OSC 52 request.
    ClipSource,
    /// Show a notification listing the missing codepoints.
    ListGlyphs,
}

/// A segment's registered `default` click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultClick {
    /// Inert.
    None,
    /// Run a keymap action for the clicked window.
    Action(Action),
    /// Run an App handler.
    Handler(SegmentHandler),
}

/// What clicking a presented segment does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentClick {
    /// Run a keymap action for the clicked window.
    Action(Action),
    /// Run an App handler.
    Handler(SegmentHandler),
}

/// How a segment's value changes, so idle cost is predictable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateKind {
    /// Changes only when something happens; no idle cost.
    Event,
    /// Read on the App-owned sampler's timer.
    Sampled,
}

/// Formatter signature shared by every segment.
pub type SegmentFormatter = fn(&StatusLineInputs, &FormatEnv<'_>) -> Option<FormattedSegment>;

/// Registry entry of one segment id.
#[derive(Debug, Clone)]
pub struct SegmentSpec {
    /// How the segment updates.
    pub update: UpdateKind,
    /// Produces the segment's runs, or `None` when it has nothing to show.
    pub format: SegmentFormatter,
    /// The `default` click.
    pub default_click: DefaultClick,
}

/// The registry entry of `id`. Exhaustive: a new id must be registered.
/// Ids whose issue has not landed format nothing; presets never list them.
pub fn segment_spec(id: StatusSegmentId) -> SegmentSpec {
    use StatusSegmentId as Seg;
    let (update, format, default_click): (UpdateKind, SegmentFormatter, DefaultClick) = match id {
        Seg::Mem => {
            (UpdateKind::Sampled, format_mem, DefaultClick::Action(Action::ShowMemoryUsage))
        }
        Seg::Health => {
            (UpdateKind::Event, format_health, DefaultClick::Action(Action::EditConfigFile))
        }
        Seg::Mode => (UpdateKind::Event, format_mode, DefaultClick::None),
        Seg::Scroll => (
            UpdateKind::Event,
            format_scroll,
            DefaultClick::Handler(SegmentHandler::ScrollToBottom),
        ),
        Seg::Link => (UpdateKind::Event, format_link, DefaultClick::None),
        Seg::Sel => (UpdateKind::Event, format_sel, DefaultClick::Action(Action::CopyToClipboard)),
        Seg::Paste => (UpdateKind::Event, format_paste, DefaultClick::None),
        Seg::Alerts => {
            (UpdateKind::Event, format_alerts, DefaultClick::Handler(SegmentHandler::NextAlert))
        }
        Seg::Clip => {
            (UpdateKind::Event, format_clip, DefaultClick::Handler(SegmentHandler::ClipSource))
        }
        Seg::Input => (UpdateKind::Event, format_input, DefaultClick::None),
        Seg::History => (
            UpdateKind::Event,
            format_history,
            DefaultClick::Handler(SegmentHandler::AcknowledgeHistory),
        ),
        Seg::Font => (UpdateKind::Event, format_font, DefaultClick::Action(Action::ResetFontSize)),
        Seg::Size => (UpdateKind::Event, format_size, DefaultClick::None),
        Seg::Zoom => (UpdateKind::Event, format_zoom, DefaultClick::Action(Action::TogglePaneZoom)),
        Seg::Ime => (UpdateKind::Event, format_ime, DefaultClick::None),
        Seg::Glyphs => {
            (UpdateKind::Event, format_glyphs, DefaultClick::Handler(SegmentHandler::ListGlyphs))
        }
        Seg::PathCheck | Seg::Cpu | Seg::Frame | Seg::Out | Seg::TabMem => {
            (UpdateKind::Event, format_unavailable, DefaultClick::None)
        }
    };
    SegmentSpec { update, format, default_click }
}

/// Everything a formatter needs besides the inputs.
#[derive(Clone, Copy)]
pub struct FormatEnv<'a> {
    /// Labels and words for the UI locale.
    pub i18n: &'a I18n,
    /// Symbol set.
    pub icons: StatusIcons,
    /// Decimal separator and digit grouping.
    pub numbers: NumberStyle,
    /// Memory segment settings.
    pub mem: MemSettings,
    /// The segment's configured `show`.
    pub show: ShowMode,
}

/// The facts the strip shows, gathered by the App.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusLineInputs {
    /// Focused-pane and window modes.
    pub mode: ModeInput,
    /// Lines the focused pane is scrolled back; 0 at the bottom.
    pub scroll_lines: u64,
    /// What detection found under the pointer.
    pub link: Option<LinkInput>,
    /// Problems SonicTerm itself has.
    pub health: HealthInput,
    /// SonicTerm's process memory.
    pub mem: MemInput,
    /// The window's selection size or its last requested copy's result.
    pub sel: Option<SelInput>,
    /// A paste into the focused pane would be unbracketed at a confirmed prompt.
    pub paste_raw: bool,
    /// An input-method problem in this window.
    pub ime: Option<ImeInput>,
    /// Rows the byte budget trimmed from the focused pane's history since it was acknowledged.
    pub history_trimmed: Option<u64>,
    /// The live font size while it differs from the configured size.
    pub font: Option<FontInput>,
    /// The focused pane's `(columns, rows)` while a recent resize is held.
    pub size: Option<(u16, u16)>,
    /// A split pane of the active tab is zoomed to fill it.
    pub zoomed: bool,
    /// This window's marked tabs and how many other windows have marks.
    pub alerts: AlertsInput,
    /// The latest OSC 52 request while it is held.
    pub clip: Option<ClipInput>,
    /// Input SonicTerm refused or blocked, or an accepted write still outstanding.
    pub input: Option<InputState>,
    /// Characters in this window's presented panes that no font SonicTerm can find covers.
    pub glyphs: Option<GlyphsInput>,
}

/// How many distinct uncovered characters a window's presented panes draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphsInput {
    /// Exactly this many.
    Exact(u64),
    /// A cap was hit; at least this many.
    AtLeast(u64),
    /// The unresolved set overflowed; no count can be proven.
    Capped,
}

/// Why the input queue refused a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSentReason {
    /// The bounded queue had no free slot.
    Busy,
    /// The writer had stopped.
    Closed,
    /// The message exceeded the per-message cap.
    TooLarge,
}

/// What the input segment shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputState {
    /// The input queue refused a message.
    NotSent(NotSentReason),
    /// The writer stopped on a native error with this many accepted writes not delivered.
    WriteFailed(u32),
    /// A paste or drop was refused for `refused` of `attempted` destinations.
    PasteRefused {
        /// Destinations that refused it.
        refused: usize,
        /// Destinations it was meant for.
        attempted: usize,
    },
    /// READONLY copy mode consumed the gesture; `panes` it was meant for.
    Blocked(usize),
    /// A native key could not be encoded and was not sent (Windows win32-input).
    KeyNotSent,
    /// The oldest accepted write has been outstanding this many seconds.
    Held {
        /// Whole seconds since it was admitted.
        secs: u64,
        /// The writer is busy with a terminal reply ahead of it.
        on_reply: bool,
        /// Bytes still queued.
        queued_bytes: u64,
    },
}

/// What SonicTerm did with the latest OSC 52 request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipOutcome {
    /// Written: this many decoded UTF-8 bytes reached the clipboard.
    Written(u64),
    /// Accepted, but the native clipboard write failed.
    Failed,
    /// A clipboard read; reads are never answered.
    ReadRefused,
    /// Another selection than the clipboard.
    SelectionRefused(char),
    /// The payload was over the size cap.
    TooLarge,
    /// Empty, malformed Base64 or not UTF-8.
    Invalid,
}

/// Where the latest OSC 52 request came from, as seen from one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipSource {
    /// This window's tab with this 1-based number.
    Tab(usize),
    /// A tab of another window.
    OtherWindow,
    /// A tab that has closed since.
    Closed,
}

/// The clipboard segment's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipInput {
    /// The latest request's outcome.
    pub outcome: ClipOutcome,
    /// Requests the hold covers.
    pub attempts: u64,
    /// Where they came from.
    pub source: ClipSource,
}

/// What the alerts segment shows: tabs of this window that need the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertsInput {
    /// 1-based tab numbers and their marks, in tab order.
    pub tabs: Vec<(usize, crate::tabs::TabMarks)>,
    /// Other windows with at least one marked tab.
    pub other_windows: usize,
}

/// Tab numbers the alerts segment lists before it counts the rest.
const ALERT_TABS_SHOWN: usize = 4;

/// The live and configured font sizes, in tenths of a point so the inputs stay comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontInput {
    /// The live size.
    pub live_tenths: u32,
    /// The configured size.
    pub configured_tenths: u32,
}

/// What the input-method segment shows; it appears only for a problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeInput {
    /// A commit was dropped because copy mode owned the keyboard.
    DiscardedInCopyMode,
    /// A composition is in progress and its commit will go to the search field.
    ToSearch,
    /// A composition is in progress and its commit will go to the command palette.
    ToPalette,
}

/// What the selection segment shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelInput {
    /// The copied text would have `lines` logical lines and `chars` characters.
    Extent {
        /// Logical lines, at least 1.
        lines: u64,
        /// Unicode scalar values, newlines included.
        chars: u64,
    },
    /// The selection spans `rows` rows, past the counting bound.
    Capped {
        /// Exact row span of the selection.
        rows: u64,
    },
    /// A copy the user requested reached the clipboard.
    Copied,
    /// A copy the user requested could not write the clipboard.
    CopyFailed,
}

/// Modes shown as chips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModeInput {
    /// Copy mode is active in this window.
    pub copy: bool,
    /// Broadcast input is on, with this many receiving panes.
    pub broadcast_receivers: Option<usize>,
    /// A program enabled mouse reporting in the focused pane.
    pub mouse_captured: bool,
}

/// How this window's frames are rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RenderHealth {
    /// A hardware GPU adapter at full pacing.
    #[default]
    Hardware,
    /// Frames go to the Windows GDI presenter.
    SoftwareGdi,
    /// The wgpu adapter is a CPU rasterizer.
    SoftwareAdapter,
    /// The software degrade policy is forced on a hardware adapter.
    SoftwareForced,
}

/// One component's last reload failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReloadFailure {
    /// The 1-based line of the parse error, when the error names one.
    pub line: Option<u32>,
}

/// Problems the health segment reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HealthInput {
    /// The config file's last reload failure.
    pub config: Option<ReloadFailure>,
    /// The theme's last reload failure.
    pub theme: Option<ReloadFailure>,
    /// The keymap's last reload failure.
    pub keymap: Option<ReloadFailure>,
    /// How this window renders.
    pub render: RenderHealth,
    /// This window's renderer waits on GPU recovery.
    pub gpu_recovering: bool,
}

/// SonicTerm's process memory samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInput {
    /// The latest figure, or `None` when the OS query failed or none ran yet.
    pub bytes: Option<u64>,
    /// The sample before it, for the trend arrow.
    pub previous_bytes: Option<u64>,
    /// The sparkline's completed 15 s buckets, oldest first; `None` is a bucket with no sample.
    pub history: [Option<u64>; SPARK_CELLS],
}

/// Cells of the memory sparkline: two minutes of 15 s buckets.
pub const SPARK_CELLS: usize = 8;
/// Width of one sparkline bucket, in wall-clock milliseconds.
pub const SPARK_BUCKET_MS: u64 = 15_000;

/// Memory samples kept as the maximum per wall-clock-aligned 15 s bucket.
///
/// Buckets are keyed by wall time, so a span with no sample (sampling paused while every window
/// was covered, or the machine asleep) leaves its buckets blank; nothing is interpolated, and a
/// monotonic clock that stood still never stretches a bucket. Only completed buckets are drawn, so
/// the sparkline changes at bucket rollover and never between.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemHistory {
    /// `(bucket index, maximum bytes)`, oldest first; at most one more than the drawn cells.
    buckets: std::collections::VecDeque<(u64, u64)>,
}

impl MemHistory {
    /// Fold a sample of `bytes` taken at `wall_ms` into its bucket.
    pub fn record(&mut self, wall_ms: u64, bytes: u64) {
        let index = wall_ms / SPARK_BUCKET_MS;
        match self.buckets.back_mut() {
            Some((last, peak)) if *last == index => *peak = (*peak).max(bytes),
            _ => {
                self.buckets.push_back((index, bytes));
                while self.buckets.len() > SPARK_CELLS + 1 {
                    self.buckets.pop_front();
                }
            }
        }
    }

    /// The completed buckets before the one holding `wall_ms`, oldest first.
    pub fn cells(&self, wall_ms: u64) -> [Option<u64>; SPARK_CELLS] {
        let current = wall_ms / SPARK_BUCKET_MS;
        let mut cells = [None; SPARK_CELLS];
        for (index, peak) in &self.buckets {
            let age =
                current.checked_sub(*index).filter(|age| (1..=SPARK_CELLS as u64).contains(age));
            if let Some(age) = age {
                cells[SPARK_CELLS - age as usize] = Some(*peak);
            }
        }
        cells
    }

    /// Milliseconds from `wall_ms` to the next bucket boundary, always at least 1.
    pub fn next_rollover_ms(wall_ms: u64) -> u64 {
        SPARK_BUCKET_MS - wall_ms % SPARK_BUCKET_MS
    }
}

/// The sparkline for `cells`, scaled between the minimum and maximum present cell; a blank cell is
/// a space. `None` when no cell has a sample.
pub fn sparkline(cells: &[Option<u64>; SPARK_CELLS], icons: StatusIcons) -> Option<String> {
    const UNICODE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    const ASCII: [char; 8] = [' ', '.', ':', '-', '=', '+', '*', '#'];
    let ramp = match icons {
        StatusIcons::Ascii => ASCII,
        StatusIcons::Unicode | StatusIcons::Nerd => UNICODE,
    };
    let low = cells.iter().flatten().min().copied()?;
    let high = cells.iter().flatten().max().copied()?;
    let span = high - low;
    Some(
        cells
            .iter()
            .map(|cell| match cell {
                None => ' ',
                Some(_) if span == 0 => ramp[0],
                Some(bytes) => {
                    let level =
                        (u128::from(bytes - low) * 7 + u128::from(span) / 2) / u128::from(span);
                    ramp[level as usize]
                }
            })
            .collect(),
    )
}

/// What a click on a local path does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAction {
    /// The file is selected in its folder.
    Reveal,
    /// The folder is opened.
    OpenFolder,
}

/// What link or path detection found under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkInput {
    /// A URL: its destination host and the rest, without the scheme.
    Url {
        /// Destination host (protected when narrowing).
        host: String,
        /// Path, query and fragment (shortened first).
        rest: String,
    },
    /// An OSC 8 URL whose visible text could not be read whole, so it was not compared.
    UrlUnchecked {
        /// Destination host (protected when narrowing).
        host: String,
        /// Path, query and fragment (shortened first).
        rest: String,
    },
    /// An OSC 8 link whose visible text names a different host.
    Mismatch {
        /// Where the link really goes.
        target_host: String,
        /// The host its visible text shows.
        label_host: String,
    },
    /// A local path and what a click does.
    Path {
        /// Path for display (`~` for home).
        path: String,
        /// What the click does.
        action: PathAction,
    },
    /// A local path still being checked.
    Checking {
        /// Path for display.
        path: String,
    },
    /// A local path that does not exist.
    NotFound {
        /// Path for display.
        path: String,
    },
    /// A local path that cannot be opened, and why.
    Blocked {
        /// Path for display.
        path: String,
        /// Localized reason.
        reason: String,
    },
}

/// `url` split into destination host and rest, scheme removed, both sanitized.
fn split_url(url: &str) -> (String, String) {
    let bare = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (host, rest) = bare.find('/').map_or((bare, ""), |slash| bare.split_at(slash));
    (sanitize_link_text(host), sanitize_link_text(rest))
}

impl LinkInput {
    /// A URL split into destination host and rest, scheme removed, both sanitized.
    pub fn from_url(url: &str) -> Self {
        let (host, rest) = split_url(url);
        Self::Url { host, rest }
    }

    /// The link for a hovered target `url` and, for an OSC 8 link, its visible text and whether
    /// that text was read whole. A label that was not read whole is never reported as matching.
    pub fn from_hover(url: &str, label: Option<(&str, bool)>) -> Self {
        let checked = label.map(|(text, complete)| check_link_label(text, complete, url));
        match checked {
            Some(LinkLabelCheck::Mismatch { label_host }) => Self::Mismatch {
                target_host: url_authority(url).map_or_else(
                    || split_url(url).0,
                    |authority| sanitize_link_text(&authority.host),
                ),
                label_host: sanitize_link_text(&label_host),
            },
            Some(LinkLabelCheck::NotChecked) => {
                let (host, rest) = split_url(url);
                Self::UrlUnchecked { host, rest }
            }
            Some(LinkLabelCheck::Plain | LinkLabelCheck::Matches) | None => Self::from_url(url),
        }
    }
}

/// The host part of a URL or bare host name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlAuthority {
    /// Host, lowercased, without a trailing dot or a port.
    pub host: String,
    /// Userinfo before `@`, which makes the visible host misleading.
    pub userinfo: Option<String>,
}

/// The authority of `text` when it is a URL with a host or a bare host name such as
/// `example.com/a`; `None` for plain words, paths without a host, and `mailto:` style URIs.
pub fn url_authority(text: &str) -> Option<UrlAuthority> {
    let text = text.trim();
    let (has_scheme, rest) = text.split_once("://").map_or((false, text), |(_, rest)| (true, rest));
    let opaque_scheme = !has_scheme
        && text.split_once(':').is_some_and(|(scheme, after)| {
            !scheme.contains('/') && !after.starts_with(|next: char| next.is_ascii_digit())
        });
    if opaque_scheme {
        // When: `opaque_scheme` (`mailto:`, `tel:`) has no `//`, the text names no host.
        return None;
    }
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let (userinfo, host_port) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(userinfo, host_port)| (Some(userinfo.to_string()), host_port));
    let host = match host_port.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|digit| digit.is_ascii_digit()) => host,
        _ => host_port,
    };
    let host = host.trim_end_matches('.').to_lowercase();
    let top_label = host.rsplit('.').next().unwrap_or_default();
    // A bare host needs a dot and an alphabetic top-level label, so `docs` or `v1.2` are words.
    let bare_host =
        host.contains('.') && top_label.len() >= 2 && top_label.chars().all(char::is_alphabetic);
    let plausible =
        !host.is_empty() && !host.contains(char::is_whitespace) && (has_scheme || bare_host);
    plausible.then_some(UrlAuthority { host, userinfo })
}

/// How an OSC 8 link's visible text relates to where it goes. A heuristic, not spoof protection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkLabelCheck {
    /// The text names no host to compare, such as `Docs`.
    Plain,
    /// The text names the destination's host.
    Matches,
    /// The text names another host, or userinfo makes a host misleading; the misleading host.
    Mismatch {
        /// The host the text (or the userinfo) shows.
        label_host: String,
    },
    /// The text was not read whole (cut by the view edge, the span cap or the byte cap).
    NotChecked,
}

/// Compare an OSC 8 link's visible `label` with its `target`. A `label` that is not `complete`
/// is never classified as matching.
pub fn check_link_label(label: &str, complete: bool, target: &str) -> LinkLabelCheck {
    if !complete {
        // When: `complete` is false, a truncated label could hide a different host.
        return LinkLabelCheck::NotChecked;
    }
    let Some(destination) = url_authority(target) else {
        // When: `target` has no host, there is nothing a label could misrepresent.
        return LinkLabelCheck::Plain;
    };
    let shown = url_authority(label);
    let misleading = shown
        .as_ref()
        .and_then(|shown| shown.userinfo.clone())
        .or_else(|| destination.userinfo.clone());
    if let Some(label_host) = misleading {
        // When: `misleading` userinfo is present, the host a reader sees is not the destination.
        return LinkLabelCheck::Mismatch { label_host };
    }
    match shown {
        Some(shown) if shown.host != destination.host => {
            LinkLabelCheck::Mismatch { label_host: shown.host }
        }
        Some(_) => LinkLabelCheck::Matches,
        None => LinkLabelCheck::Plain,
    }
}

/// Sanitize link or path text with the link caps.
pub fn sanitize_link_text(text: &str) -> String {
    sanitize_display_text(
        text,
        SanitizePolicy::Ordinary,
        SanitizeLimits {
            max_bytes: LINK_MAX_BYTES,
            max_columns: LINK_MAX_COLUMNS,
            collapse_whitespace: true,
        },
    )
}

/// The symbol for `icons`. The `nerd` set has no glyphs of its own for these
/// segments, so it uses the Unicode symbol.
fn symbol(icons: StatusIcons, unicode: &'static str, ascii: &'static str) -> &'static str {
    match icons {
        StatusIcons::Ascii => ascii,
        StatusIcons::Unicode | StatusIcons::Nerd => unicode,
    }
}

/// A dim `Name: ` part.
fn name_part(env: &FormatEnv<'_>, key: &str) -> StatusPart {
    StatusPart::new(format!("{}: ", env.i18n.translate(key)), StatusTone::Name)
}

/// The memory segment: `Mem: 412 MB ↑`, amber or red at the thresholds.
fn format_mem(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let Some(bytes) = inputs.mem.bytes else {
        // When: `inputs.mem.bytes` is None, `always` shows `n/a` and `active` hides.
        let unavailable = env.i18n.translate("status-value-na");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    name_part(env, "status-name-mem"),
                    StatusPart::new(unavailable.clone(), StatusTone::Name),
                ],
                unavailable,
            )
        });
    };
    let figure = format_bytes(bytes, env.numbers);
    let mib = bytes / MIB;
    let (tone, level_icon) = if mib >= env.mem.critical_mib {
        (StatusTone::Error, Some(symbol(env.icons, "✗ ", "x ")))
    } else if mib >= env.mem.warn_mib {
        // When: `mib` reaches `warn_mib` but not `critical_mib`, amber with a warning symbol.
        (StatusTone::Warn, Some(symbol(env.icons, "⚠ ", "! ")))
    } else {
        // When: `mib` is under `warn_mib`, the figure needs no colour.
        (StatusTone::Value, None)
    };
    let mut parts = vec![name_part(env, "status-name-mem")];
    parts.extend(level_icon.map(|icon| StatusPart::new(icon, tone)));
    parts.push(StatusPart::new(figure.clone(), tone));
    let compact = parts.clone();
    let mut spoken = figure;
    let trend = env.mem.show_trend.then(|| mem_trend(inputs.mem.previous_bytes, bytes)).flatten();
    if let Some(trend) = trend {
        // The figure moved by at least 1%, show the arrow and say the direction.
        let (arrow, word) = match trend {
            Trend::Up => (symbol(env.icons, "↑", "^"), "status-spoken-rising"),
            Trend::Down => (symbol(env.icons, "↓", "v"), "status-spoken-falling"),
        };
        parts.push(StatusPart::new(format!(" {arrow}"), tone).droppable());
        spoken = format!("{spoken} {}", env.i18n.translate(word));
    }
    let line = env.mem.sparkline.then(|| sparkline(&inputs.mem.history, env.icons)).flatten();
    if let Some(line) = line {
        // The sparkline is configured and a completed bucket has a sample: draw it static and dim.
        parts.push(StatusPart::new(format!(" {line}"), StatusTone::Name).droppable());
    }
    Some(FormattedSegment {
        compact: (compact != parts).then_some(compact),
        default_click: true,
        ..FormattedSegment::plain(parts, spoken)
    })
}

/// Reload-error components, in display order.
fn reload_components(health: &HealthInput) -> [(&'static str, Option<ReloadFailure>); 3] {
    [
        ("status-name-config", health.config),
        ("status-name-theme", health.theme),
        ("status-name-keymap", health.keymap),
    ]
}

/// The health segment: reload errors, software rendering, GPU recovery.
fn format_health(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let health = &inputs.health;
    let mut pairs: Vec<(Vec<StatusPart>, String, bool)> = Vec::new();
    for (key, failure) in reload_components(health) {
        let Some(failure) = failure else {
            // When: `failure` is None, this component's last reload succeeded and adds nothing.
            continue;
        };
        let value = match failure.line {
            Some(line) => {
                env.i18n.t_args("status-value-line", Some(&[("line", &line.to_string())]))
            }
            None => env.i18n.translate("status-value-error"),
        };
        let name = env.i18n.translate(key);
        pairs.push((
            vec![
                name_part(env, key),
                StatusPart::new(symbol(env.icons, "✗ ", "x "), StatusTone::Error),
                StatusPart::new(value.clone(), StatusTone::Error),
            ],
            format!("{name} {value}"),
            true,
        ));
    }
    let render_key = match health.render {
        RenderHealth::Hardware => None,
        RenderHealth::SoftwareGdi => Some("status-value-software-gdi"),
        RenderHealth::SoftwareAdapter => Some("status-value-software-adapter"),
        RenderHealth::SoftwareForced => Some("status-value-software-forced"),
    };
    if let Some(value_key) = render_key {
        // The window renders in software, name which kind.
        let value = env.i18n.translate(value_key);
        pairs.push((
            vec![
                name_part(env, "status-name-render"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(value.clone(), StatusTone::Warn),
            ],
            format!("{} {value}", env.i18n.translate("status-name-render")),
            false,
        ));
    }
    if health.gpu_recovering {
        // This window waits on GPU recovery, say so.
        let value = env.i18n.translate("status-value-recovering");
        pairs.push((
            vec![
                name_part(env, "status-name-gpu"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(value.clone(), StatusTone::Warn),
            ],
            format!("{} {value}", env.i18n.translate("status-name-gpu")),
            false,
        ));
    }
    if pairs.is_empty() {
        // When: `pairs` is empty, nothing is wrong; `always` shows `normal` and `active` hides.
        let normal = env.i18n.translate("status-value-normal");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    StatusPart::new(
                        format!("{}: ", env.i18n.translate("status-spoken-health")),
                        StatusTone::Name,
                    ),
                    StatusPart::new(normal.clone(), StatusTone::Name),
                ],
                normal,
            )
        });
    }
    let has_error = pairs.iter().any(|(_, _, error)| *error);
    let spoken = pairs.iter().map(|(_, words, _)| words.as_str()).collect::<Vec<_>>().join(", ");
    let joined = |selected: Vec<&Vec<StatusPart>>| {
        let mut parts = Vec::new();
        for (index, pair) in selected.into_iter().enumerate() {
            if index > 0 {
                parts.push(StatusPart::new("  ", StatusTone::Name));
            }
            parts.extend(pair.iter().cloned());
        }
        parts
    };
    let parts = joined(pairs.iter().map(|(parts, _, _)| parts).collect());
    // The compact form keeps the errors, or the first warning when none.
    let compact_pairs: Vec<&Vec<StatusPart>> = if has_error {
        pairs.iter().filter(|(_, _, error)| *error).map(|(parts, _, _)| parts).collect()
    } else {
        // When: `has_error` is false, the compact form keeps the first warning.
        pairs.iter().take(1).map(|(parts, _, _)| parts).collect()
    };
    let compact = joined(compact_pairs);
    Some(FormattedSegment {
        compact: (compact != parts).then_some(compact),
        protected: has_error,
        default_click: has_error,
        ..FormattedSegment::plain(parts, spoken)
    })
}

/// The mode segment: `COPY`, broadcast `⇶ 3`, `MOUSE` chips; no names.
fn format_mode(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let mode = inputs.mode;
    let mut parts = Vec::new();
    let mut spoken = Vec::new();
    if let Some(count) = mode.broadcast_receivers {
        // Broadcast input is on, show it first with its receiver count.
        let count_text = format_count(count as u64, env.numbers);
        parts.push(StatusPart::new(
            format!("{} {count_text}", symbol(env.icons, "⇶", ">>")),
            StatusTone::ChipWarn,
        ));
        spoken.push(env.i18n.t_args("status-spoken-broadcast", Some(&[("count", &count_text)])));
    }
    if mode.copy {
        parts.push(StatusPart::new(env.i18n.translate("status-chip-copy"), StatusTone::Chip));
        spoken.push(env.i18n.translate("status-spoken-copy-mode"));
    }
    let compact = parts.clone();
    if mode.mouse_captured {
        // A program captured the mouse, explain why clicks go to it.
        parts.push(
            StatusPart::new(env.i18n.translate("status-chip-mouse"), StatusTone::Chip).droppable(),
        );
        spoken.push(env.i18n.translate("status-spoken-mouse-captured"));
    }
    if parts.is_empty() {
        // When: `parts` is empty, no mode is active; `always` shows `normal` and `active` hides.
        let normal = env.i18n.translate("status-value-normal");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(vec![StatusPart::new(normal.clone(), StatusTone::Name)], normal)
        });
    }
    Some(FormattedSegment {
        compact: (compact != parts && !compact.is_empty()).then_some(compact),
        protected: mode.broadcast_receivers.is_some(),
        ..FormattedSegment::plain(parts, spoken.join(", "))
    })
}

/// The scroll segment: `Scroll: ↑ 340` while scrolled back.
fn format_scroll(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    if inputs.scroll_lines == 0 {
        // When: `scroll_lines` is 0, the pane is at the bottom; `always` shows `bottom`.
        let bottom = env.i18n.translate("status-value-bottom");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    name_part(env, "status-name-scroll"),
                    StatusPart::new(bottom.clone(), StatusTone::Name),
                ],
                bottom,
            )
        });
    }
    let count = format_count(inputs.scroll_lines, env.numbers);
    let parts = vec![
        name_part(env, "status-name-scroll"),
        StatusPart::new(format!("{} {count}", symbol(env.icons, "↑", "^")), StatusTone::Value),
    ];
    let spoken = env.i18n.t_args("status-spoken-lines-up", Some(&[("count", &count)]));
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The link segment: the URL, a host mismatch, or a local path's state.
fn format_link(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let link = inputs.link.as_ref()?;
    let separator = symbol(env.icons, " · ", " - ");
    let detail = |key: &str, tone: StatusTone| {
        [
            StatusPart::new(separator, tone).droppable(),
            StatusPart::new(env.i18n.translate(key), tone).droppable(),
        ]
    };
    let path_name = env.i18n.translate("status-name-path");
    let (parts, protected, spoken) = match link {
        LinkInput::Url { host, rest } => {
            let mut parts = vec![
                name_part(env, "status-name-link"),
                StatusPart::new(host.clone(), StatusTone::Value),
            ];
            if !rest.is_empty() {
                parts.push(StatusPart::new(rest.clone(), StatusTone::Value).ellipsized());
            }
            (parts, true, format!("{host}{rest}"))
        }
        LinkInput::UrlUnchecked { host, rest } => {
            let mut parts = vec![
                name_part(env, "status-name-link"),
                StatusPart::new(host.clone(), StatusTone::Value),
            ];
            if !rest.is_empty() {
                parts.push(StatusPart::new(rest.clone(), StatusTone::Value).ellipsized());
            }
            parts.extend(detail("status-value-label-not-checked", StatusTone::Name));
            let note = env.i18n.translate("status-value-label-not-checked");
            (parts, true, format!("{host}{rest}, {note}"))
        }
        LinkInput::Mismatch { target_host, label_host } => (
            vec![
                name_part(env, "status-name-link"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(target_host.clone(), StatusTone::Warn),
                StatusPart::new(symbol(env.icons, " ≠ ", " != "), StatusTone::Warn).droppable(),
                StatusPart::new(label_host.clone(), StatusTone::Warn).ellipsized().droppable(),
            ],
            true,
            format!("{target_host}, {label_host}"),
        ),
        LinkInput::Path { path, action } => {
            let action_key = match action {
                PathAction::Reveal => "status-value-reveal",
                PathAction::OpenFolder => "status-value-open-folder",
            };
            let mut parts = vec![
                name_part(env, "status-name-path"),
                StatusPart::new(path.clone(), StatusTone::Value).ellipsized(),
            ];
            parts.extend(detail(action_key, StatusTone::Name));
            (parts, false, format!("{path_name} {path}, {}", env.i18n.translate(action_key)))
        }
        LinkInput::Checking { path } => {
            let mut parts = vec![
                name_part(env, "status-name-path"),
                StatusPart::new(path.clone(), StatusTone::Name).ellipsized(),
            ];
            parts.extend(detail("status-value-checking", StatusTone::Name));
            (
                parts,
                false,
                format!("{path_name} {path}, {}", env.i18n.translate("status-value-checking")),
            )
        }
        LinkInput::NotFound { path } => (
            vec![
                name_part(env, "status-name-path"),
                StatusPart::new(symbol(env.icons, "✗ ", "x "), StatusTone::Error),
                StatusPart::new(path.clone(), StatusTone::Value).ellipsized(),
                StatusPart::new(separator, StatusTone::Error),
                StatusPart::new(env.i18n.translate("status-value-not-found"), StatusTone::Error),
            ],
            false,
            format!("{path_name} {path}, {}", env.i18n.translate("status-value-not-found")),
        ),
        LinkInput::Blocked { path, reason } => (
            vec![
                name_part(env, "status-name-path"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(path.clone(), StatusTone::Value).ellipsized(),
                StatusPart::new(separator, StatusTone::Warn),
                StatusPart::new(env.i18n.translate("status-value-blocked"), StatusTone::Warn),
                StatusPart::new(format!(": {reason}"), StatusTone::Warn).ellipsized().droppable(),
            ],
            false,
            format!("{path_name} {path}, {}, {reason}", env.i18n.translate("status-value-blocked")),
        ),
    };
    Some(FormattedSegment { protected, ..FormattedSegment::plain(parts, spoken) })
}

/// The selection segment: `Sel: 3 lines`, `Sel: 142 chars`, a capped row
/// span, or the result of the last requested copy.
fn format_sel(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let Some(sel) = inputs.sel else {
        // When: `inputs.sel` is None there is no selection; `always` shows `none`.
        let none = env.i18n.translate("status-value-none");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    name_part(env, "status-name-sel"),
                    StatusPart::new(none.clone(), StatusTone::Name),
                ],
                none,
            )
        });
    };
    let count_args = |key: &str, count: u64| {
        env.i18n.t_args(key, Some(&[("count", &format_count(count, env.numbers))]))
    };
    let mut parts = vec![name_part(env, "status-name-sel")];
    let spoken = match sel {
        SelInput::Extent { lines, chars } => {
            let (value, spoken) = if lines > 1 {
                (
                    count_args("status-value-sel-lines", lines),
                    count_args("status-spoken-sel-lines", lines),
                )
            } else if chars == 1 {
                // When: chars is 1; the catalogs carry no plural selectors, so it has its own words.
                (
                    env.i18n.translate("status-value-sel-char"),
                    env.i18n.translate("status-spoken-sel-char"),
                )
            } else {
                // When: `lines` is 1 and `chars` is not 1, a single line shows its character count.
                (
                    count_args("status-value-sel-chars", chars),
                    count_args("status-spoken-sel-chars", chars),
                )
            };
            parts.push(StatusPart::new(value, StatusTone::Value));
            spoken
        }
        SelInput::Capped { rows } => {
            parts.push(StatusPart::new(
                count_args("status-value-sel-rows", rows),
                StatusTone::Value,
            ));
            parts.push(
                StatusPart::new(symbol(env.icons, " · ", " - "), StatusTone::Name).droppable(),
            );
            parts.push(
                StatusPart::new(env.i18n.translate("status-value-count-capped"), StatusTone::Name)
                    .droppable(),
            );
            count_args("status-spoken-sel-capped", rows)
        }
        SelInput::Copied => {
            let copied = env.i18n.translate("status-value-copied");
            parts.push(StatusPart::new(
                format!("{} {copied}", symbol(env.icons, "✓", "ok")),
                StatusTone::Value,
            ));
            copied
        }
        SelInput::CopyFailed => {
            let failed = env.i18n.translate("status-value-copy-failed");
            parts.push(StatusPart::new(
                format!("{} {failed}", symbol(env.icons, "✗", "x")),
                StatusTone::Error,
            ));
            failed
        }
    };
    // Only a live selection has something for the `default` click to copy.
    let default_click = matches!(sel, SelInput::Extent { .. } | SelInput::Capped { .. });
    Some(FormattedSegment { default_click, ..FormattedSegment::plain(parts, spoken) })
}

/// The paste segment: `Paste: ⚠ raw` while a paste would be unbracketed at a confirmed prompt.
fn format_paste(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    if !inputs.paste_raw {
        // When: `paste_raw` is false, bracketed paste is on or the pane is not at a prompt.
        return None;
    }
    let raw = env.i18n.translate("status-value-raw");
    let parts = vec![
        name_part(env, "status-name-paste"),
        StatusPart::new(format!("{} {raw}", symbol(env.icons, "⚠", "!")), StatusTone::Warn),
    ];
    Some(FormattedSegment::plain(parts, env.i18n.translate("status-spoken-paste-raw")))
}

/// The input-method segment: a dropped commit, or a composition bound away from the pane.
fn format_ime(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let ime = inputs.ime?;
    let (text, tone, spoken_key) = match ime {
        ImeInput::DiscardedInCopyMode => (
            format!(
                "{} {}",
                symbol(env.icons, "⚠", "!"),
                env.i18n.translate("status-value-discarded-copy-mode")
            ),
            StatusTone::Warn,
            "status-spoken-ime-discarded",
        ),
        ImeInput::ToSearch => (
            format!(
                "{} {}",
                symbol(env.icons, "→", "->"),
                env.i18n.translate("status-value-search")
            ),
            StatusTone::Value,
            "status-spoken-ime-search",
        ),
        ImeInput::ToPalette => (
            format!(
                "{} {}",
                symbol(env.icons, "→", "->"),
                env.i18n.translate("status-value-palette")
            ),
            StatusTone::Value,
            "status-spoken-ime-palette",
        ),
    };
    let parts = vec![name_part(env, "status-name-ime"), StatusPart::new(text, tone)];
    Some(FormattedSegment::plain(parts, env.i18n.translate(spoken_key)))
}

/// `tenths` of a point as `16` or `13.5`, with the locale's decimal separator.
fn format_tenths(tenths: u32, style: NumberStyle) -> String {
    let whole = format_count(u64::from(tenths / 10), style);
    match tenths % 10 {
        0 => whole,
        fraction => format!("{whole}{}{fraction}", style.decimal),
    }
}

/// The history segment: `History: trimmed` after the byte budget dropped rows from the focused
/// pane's scrollback, until the trim is acknowledged.
fn format_history(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let rows = inputs.history_trimmed?;
    let parts = vec![
        name_part(env, "status-name-history"),
        StatusPart::new(env.i18n.translate("status-value-trimmed"), StatusTone::Warn),
    ];
    let count = format_count(rows, env.numbers);
    let spoken = env.i18n.t_args("status-spoken-history-trimmed", Some(&[("count", &count)]));
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The font segment: `Font: 16pt (+2)` while the live size differs from the configured size.
fn format_font(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let font = inputs.font.filter(|font| font.live_tenths != font.configured_tenths)?;
    let live = format_tenths(font.live_tenths, env.numbers);
    let (sign, spoken_key, delta) = if font.live_tenths > font.configured_tenths {
        ("+", "status-spoken-font-larger", font.live_tenths - font.configured_tenths)
    } else {
        // When: live_tenths is below configured_tenths, the delta is a reduction.
        ("\u{2212}", "status-spoken-font-smaller", font.configured_tenths - font.live_tenths)
    };
    let delta = format_tenths(delta, env.numbers);
    let parts = vec![
        name_part(env, "status-name-font"),
        StatusPart::new(format!("{live}pt ({sign}{delta})"), StatusTone::Value),
    ];
    let spoken = env.i18n.t_args(spoken_key, Some(&[("size", &live), ("delta", &delta)]));
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The size segment: `Size: 120×40` for a while after the focused pane's grid was resized.
fn format_size(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let (cols, rows) = inputs.size?;
    let parts = vec![
        name_part(env, "status-name-size"),
        StatusPart::new(format!("{cols}{}{rows}", symbol(env.icons, "×", "x")), StatusTone::Value),
    ];
    let spoken = env.i18n.t_args(
        "status-spoken-size-grid",
        Some(&[("cols", &cols.to_string()), ("rows", &rows.to_string())]),
    );
    Some(FormattedSegment::plain(parts, spoken))
}

/// The alerts segment: `Alerts: #2 #3`, `#2 #3 #5 #6 +2`, and `+1 window` for marks elsewhere.
/// The spoken value names each source; it never claims a program is waiting for input.
fn format_alerts(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let alerts = &inputs.alerts;
    if alerts.tabs.is_empty() && alerts.other_windows == 0 {
        // When: alerts.tabs is empty and other_windows is 0, no tab anywhere is marked.
        return None;
    }
    let mut words: Vec<String> =
        alerts.tabs.iter().take(ALERT_TABS_SHOWN).map(|(number, _)| format!("#{number}")).collect();
    if alerts.tabs.len() > ALERT_TABS_SHOWN {
        words.push(format!("+{}", alerts.tabs.len() - ALERT_TABS_SHOWN));
    }
    let mut parts = vec![name_part(env, "status-name-alerts")];
    if !words.is_empty() {
        parts.push(StatusPart::new(words.join(" "), StatusTone::Value));
    }
    let count = format_count(alerts.other_windows as u64, env.numbers);
    if alerts.other_windows > 0 {
        let key = if alerts.other_windows == 1 {
            "status-value-other-window"
        } else {
            // More than one other window takes the plural form with its count.
            "status-value-other-windows"
        };
        let separator = if words.is_empty() { "" } else { " " };
        let text = format!("{separator}{}", env.i18n.t_args(key, Some(&[("count", &count)])));
        parts.push(StatusPart::new(text, StatusTone::Value).droppable());
    }
    let mut spoken: Vec<String> = alerts
        .tabs
        .iter()
        .map(|(number, marks)| {
            let mut sources = Vec::new();
            if marks.bell {
                sources.push(env.i18n.translate("status-spoken-alert-bell"));
            }
            if marks.notify {
                sources.push(env.i18n.translate("status-spoken-alert-notify"));
            }
            if let Some(secs) = marks.finished_secs {
                let duration = format_duration(secs);
                sources.push(
                    env.i18n
                        .t_args("status-spoken-alert-finished", Some(&[("duration", &duration)])),
                );
            }
            env.i18n.t_args(
                "status-spoken-alert-tab",
                Some(&[("tab", &number.to_string()), ("sources", &sources.join(", "))]),
            )
        })
        .collect();
    if alerts.other_windows > 0 {
        spoken.push(env.i18n.t_args("status-spoken-alert-windows", Some(&[("count", &count)])));
    }
    Some(FormattedSegment {
        protected: true,
        default_click: true,
        ..FormattedSegment::plain(parts, spoken.join(". "))
    })
}

/// The glyphs segment: distinct characters drawn as boxes because no font SonicTerm can find
/// covers them. Dim, with `⚠`; it never shows an unproven figure.
fn format_glyphs(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let glyphs = inputs.glyphs?;
    let count = |key: &str, count: u64| {
        env.i18n.t_args(key, Some(&[("count", &format_count(count, env.numbers))]))
    };
    let (value, spoken) = match glyphs {
        GlyphsInput::Exact(0) => {
            // When: glyphs is Exact(0), nothing is missing, so the segment hides.
            return None;
        }
        GlyphsInput::Exact(missing) => (
            count("status-value-glyphs-missing", missing),
            count("status-spoken-glyphs-missing", missing),
        ),
        GlyphsInput::AtLeast(missing) => (
            count("status-value-glyphs-at-least", missing),
            count("status-spoken-glyphs-at-least", missing),
        ),
        GlyphsInput::Capped => (
            env.i18n.translate("status-value-glyphs-capped"),
            env.i18n.translate("status-value-glyphs-capped"),
        ),
    };
    let parts = vec![
        name_part(env, "status-name-glyphs"),
        StatusPart::new(format!("{} {value}", symbol(env.icons, "⚠", "!")), StatusTone::Name),
    ];
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The input segment: input SonicTerm refused or blocked, or an accepted write that has not
/// completed. It never claims the program is hung or waiting, and shows no input bytes.
fn format_input(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let state = inputs.input?;
    let cross = symbol(env.icons, "✗", "x");
    let warn = symbol(env.icons, "⚠", "!");
    let translate = |key: &str| env.i18n.translate(key);
    let count = |key: &str, count: u64| {
        env.i18n.t_args(key, Some(&[("count", &format_count(count, env.numbers))]))
    };
    // The outcome, its tone, an optional parenthetical dropped first, and the spoken value.
    let (outcome, tone, detail, spoken) = match state {
        InputState::NotSent(reason) => {
            let key = match reason {
                NotSentReason::Busy => "status-value-input-busy",
                NotSentReason::Closed => "status-value-input-closed",
                NotSentReason::TooLarge => "status-value-input-too-large",
            };
            let detail = translate(key);
            let spoken = format!("{}, {detail}", translate("status-value-input-not-sent"));
            (
                format!("{cross} {}", translate("status-value-input-not-sent")),
                StatusTone::Error,
                Some(detail),
                spoken,
            )
        }
        InputState::WriteFailed(lost) => {
            let detail = count("status-value-input-lost", u64::from(lost));
            let spoken = format!("{}, {detail}", translate("status-value-input-write-failed"));
            (
                format!("{cross} {}", translate("status-value-input-write-failed")),
                StatusTone::Error,
                Some(detail),
                spoken,
            )
        }
        InputState::PasteRefused { refused, attempted } => {
            let detail = (attempted > 1).then(|| format!("{refused}/{attempted}"));
            let spoken = translate("status-value-input-paste-refused");
            (format!("{warn} {spoken}"), StatusTone::Warn, detail, spoken)
        }
        InputState::Blocked(panes) => {
            let blocked = if panes > 1 {
                count("status-value-input-blocked-panes", panes as u64)
            } else {
                // When: panes is at most 1, the gesture's own pane is the one blocked.
                translate("status-value-input-blocked")
            };
            let detail = translate("status-value-input-read-only");
            let spoken = format!("{blocked}, {detail}");
            (
                format!("{} {blocked}", symbol(env.icons, "⊘", "-")),
                StatusTone::Name,
                Some(detail),
                spoken,
            )
        }
        InputState::KeyNotSent => {
            let spoken = translate("status-value-input-key-not-sent");
            (format!("{cross} {spoken}"), StatusTone::Error, None, spoken)
        }
        InputState::Held { secs, on_reply, queued_bytes } => {
            let age = format_duration(secs);
            let held = env.i18n.t_args("status-value-input-held", Some(&[("age", &age)]));
            let detail = (queued_bytes >= 1 << 20).then(|| {
                let size = format_bytes(queued_bytes, env.numbers);
                env.i18n.t_args("status-value-input-queued", Some(&[("size", &size)]))
            });
            let mut spoken = held.clone();
            if on_reply {
                spoken = format!("{spoken}, {}", translate("status-spoken-input-behind-reply"));
            }
            (format!("{warn} {held}"), StatusTone::Warn, detail, spoken)
        }
    };
    let mut parts = vec![name_part(env, "status-name-input"), StatusPart::new(outcome, tone)];
    if let Some(detail) = detail {
        let held = matches!(state, InputState::Held { .. });
        let text = if held {
            format!(" {} {detail}", symbol(env.icons, "·", "-"))
        } else {
            // When: held is false, the detail is a parenthetical.
            format!(" ({detail})")
        };
        parts.push(StatusPart::new(text, tone).droppable());
    }
    Some(FormattedSegment::plain(parts, spoken))
}

/// The clipboard segment: what SonicTerm did with the latest OSC 52 request and which tab sent it,
/// `Clip: 2.1 KB by #2` or `Clip: ⚠ read refused ×1000 by #2`. It shows no clipboard text and does
/// not claim the clipboard still holds what was written.
fn format_clip(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    let clip = inputs.clip?;
    let (text, tone, spoken) = match clip.outcome {
        ClipOutcome::Written(bytes) => {
            let size = format_bytes(bytes, env.numbers);
            let spoken = env.i18n.t_args("status-spoken-clip-written", Some(&[("size", &size)]));
            (size, StatusTone::Value, spoken)
        }
        ClipOutcome::Failed => (
            format!(
                "{} {}",
                symbol(env.icons, "✗", "x"),
                env.i18n.translate("status-value-clip-failed")
            ),
            StatusTone::Error,
            env.i18n.translate("status-spoken-clip-failed"),
        ),
        ClipOutcome::ReadRefused => (
            format!(
                "{} {}",
                symbol(env.icons, "⚠", "!"),
                env.i18n.translate("status-value-clip-read-refused")
            ),
            StatusTone::Warn,
            env.i18n.translate("status-spoken-clip-read-refused"),
        ),
        ClipOutcome::SelectionRefused(selection) => {
            let selection = selection.to_string();
            let args = [("selection", selection.as_str())];
            (
                env.i18n.t_args("status-value-clip-selection-refused", Some(&args)),
                StatusTone::Name,
                env.i18n.t_args("status-spoken-clip-selection-refused", Some(&args)),
            )
        }
        ClipOutcome::TooLarge => (
            env.i18n.translate("status-value-clip-too-large"),
            StatusTone::Name,
            env.i18n.translate("status-spoken-clip-too-large"),
        ),
        ClipOutcome::Invalid => (
            env.i18n.translate("status-value-clip-invalid"),
            StatusTone::Name,
            env.i18n.translate("status-spoken-clip-invalid"),
        ),
    };
    let mut parts = vec![name_part(env, "status-name-clip"), StatusPart::new(text, tone)];
    let mut spoken = vec![spoken];
    if clip.attempts > 1 {
        let count = format_count(clip.attempts, env.numbers);
        parts.push(StatusPart::new(format!(" \u{00D7}{count}"), tone).droppable());
        spoken.push(env.i18n.t_args("status-spoken-clip-attempts", Some(&[("count", &count)])));
    }
    let (source_text, source_spoken) = match clip.source {
        ClipSource::Tab(number) => {
            let tab = number.to_string();
            (
                env.i18n.t_args("status-value-clip-by-tab", Some(&[("tab", &tab)])),
                env.i18n.t_args("status-spoken-clip-by-tab", Some(&[("tab", &tab)])),
            )
        }
        ClipSource::OtherWindow => (
            env.i18n.translate("status-value-clip-by-window"),
            env.i18n.translate("status-spoken-clip-by-window"),
        ),
        ClipSource::Closed => (
            env.i18n.translate("status-value-clip-by-closed"),
            env.i18n.translate("status-spoken-clip-by-closed"),
        ),
    };
    // The source goes first when narrowed, then the count; the outcome always stays.
    parts.push(StatusPart::new(format!(" {source_text}"), StatusTone::Name).droppable());
    spoken.push(source_spoken);
    // Only a tab that still exists can be switched to.
    let default_click = !matches!(clip.source, ClipSource::Closed);
    Some(FormattedSegment { default_click, ..FormattedSegment::plain(parts, spoken.join(", ")) })
}

/// The zoom segment: a `ZOOM` chip with no name while a split pane is zoomed.
fn format_zoom(inputs: &StatusLineInputs, env: &FormatEnv<'_>) -> Option<FormattedSegment> {
    if !inputs.zoomed {
        // When: `zoomed` is false, every pane of the active tab is drawn.
        return None;
    }
    let parts = vec![StatusPart::new(env.i18n.translate("status-chip-zoom"), StatusTone::Chip)];
    let spoken = env.i18n.translate("status-spoken-pane-zoomed");
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// Formatter of a catalog id whose segment has not landed: shows nothing.
fn format_unavailable(
    _inputs: &StatusLineInputs,
    _env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    None
}

/// One segment ready for fitting and drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSegmentView {
    /// Which segment.
    pub id: StatusSegmentId,
    /// The list it is drawn in.
    pub side: StatusSide,
    /// Narrow-strip priority.
    pub priority: u16,
    /// Dropped only after every unprotected segment.
    pub protected: bool,
    /// The full form.
    pub parts: Vec<StatusPart>,
    /// The compact form, if shorter.
    pub compact: Option<Vec<StatusPart>>,
    /// What a click does; `None` is inert.
    pub click: Option<SegmentClick>,
    /// Accessible name and value.
    pub spoken: SpokenSegment,
}

/// Every segment the strip may present, with a hash of its pixels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusLineView {
    /// Segments in list order: left, center, right.
    pub segments: Vec<StatusSegmentView>,
    /// Hash of visible text, colors and geometry inputs. Spoken text and
    /// click bindings do not change pixels and are not part of it.
    pub pixel_hash: u64,
}

impl StatusLineView {
    /// A view of `segments` with its pixel hash.
    pub fn new(segments: Vec<StatusSegmentView>) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for segment in &segments {
            segment.id.hash(&mut hasher);
            segment.side.hash(&mut hasher);
            segment.priority.hash(&mut hasher);
            segment.protected.hash(&mut hasher);
            segment.parts.hash(&mut hasher);
            segment.compact.hash(&mut hasher);
        }
        Self { segments, pixel_hash: hasher.finish() }
    }
}

/// Format every configured segment for `inputs`.
pub fn build_view(
    resolved: &ResolvedStatusLine,
    hide_sensitive: bool,
    inputs: &StatusLineInputs,
    i18n: &I18n,
    numbers: NumberStyle,
) -> StatusLineView {
    let mut segments = Vec::new();
    for configured in &resolved.segments {
        let meta = configured.id.meta();
        if hide_sensitive && meta.sensitivity == Sensitivity::Whole {
            // When: `hide_sensitive` and the segment is whole-sensitive, it has no text, bounds or click.
            continue;
        }
        let spec = segment_spec(configured.id);
        let env = FormatEnv {
            i18n,
            icons: resolved.icons,
            numbers,
            mem: resolved.mem,
            show: configured.show,
        };
        let Some(mut formatted) = (spec.format)(inputs, &env) else {
            // When: `format` returns None, the segment has nothing to say and takes no slot.
            continue;
        };
        if hide_sensitive {
            formatted.parts.retain(|part| !part.sensitive);
            if let Some(compact) = formatted.compact.as_mut() {
                compact.retain(|part| !part.sensitive);
            }
        }
        if formatted.parts.is_empty() {
            // When: only sensitive parts existed, the segment is removed entirely.
            continue;
        }
        let click = match (&configured.click, spec.default_click) {
            (ClickBinding::None, _) => None,
            (ClickBinding::Action(action), _) => Some(SegmentClick::Action(action.clone())),
            (ClickBinding::Default, _) if !formatted.default_click => None,
            (ClickBinding::Default, DefaultClick::None) => None,
            (ClickBinding::Default, DefaultClick::Action(action)) => {
                Some(SegmentClick::Action(action))
            }
            (ClickBinding::Default, DefaultClick::Handler(handler)) => {
                Some(SegmentClick::Handler(handler))
            }
        };
        segments.push(StatusSegmentView {
            id: configured.id,
            side: configured.side,
            priority: configured.priority,
            protected: formatted.protected,
            parts: formatted.parts,
            compact: formatted.compact,
            click,
            spoken: SpokenSegment {
                name: i18n.translate(meta.spoken_name_key),
                value: formatted.spoken,
            },
        });
    }
    StatusLineView::new(segments)
}

/// Strip geometry for [`fit`], in the renderer's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitMetrics {
    /// Strip width.
    pub width: f32,
    /// Space at each strip edge.
    pub edge_pad: f32,
    /// Space between segments and between lists.
    pub segment_gap: f32,
    /// Horizontal padding inside a chip's background.
    pub chip_pad: f32,
    /// Space between two adjacent chips.
    pub chip_gap: f32,
}

/// One placed part. For a chip, `x` and `width` cover its background and
/// the text starts `chip_pad` further right.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedPart {
    /// Text to draw.
    pub text: String,
    /// Color role.
    pub tone: StatusTone,
    /// Left edge.
    pub left: f32,
    /// Width including chip padding.
    pub width: f32,
}

/// One presented segment and its bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedSegment {
    /// Index of the segment in [`StatusLineView::segments`].
    pub index: usize,
    /// Left edge.
    pub left: f32,
    /// Width.
    pub width: f32,
    /// Its parts, as fitted.
    pub parts: Vec<PlacedPart>,
}

/// A segment's fitting state.
struct FitSlot {
    index: usize,
    side: StatusSide,
    /// Drop order: lower goes first.
    rank: (bool, u16, usize),
    parts: Vec<StatusPart>,
    part_widths: Vec<f32>,
    compact: Option<Vec<StatusPart>>,
    active: bool,
}

impl FitSlot {
    /// Width of the slot's parts with chip padding and gaps.
    fn width(&self, metrics: &FitMetrics) -> f32 {
        let mut total = 0.0;
        for (position, (part, width)) in self.parts.iter().zip(&self.part_widths).enumerate() {
            total += width;
            if part.tone.is_chip() {
                total += 2.0 * metrics.chip_pad;
                if position > 0 && self.parts[position - 1].tone.is_chip() {
                    total += metrics.chip_gap;
                }
            }
        }
        total
    }
}

/// Fit `view` into `metrics.width`, never overflowing.
///
/// Compact forms are tried first, lowest rank first. Then, lowest rank first,
/// a segment's text value is middle-ellipsized, its droppable details go, and
/// finally the whole segment goes. Rank orders unprotected before protected,
/// then ascending priority, then the segment nearest the center first.
pub fn fit(
    view: &StatusLineView,
    metrics: &FitMetrics,
    mut measure: impl FnMut(&str) -> f32,
) -> Vec<PlacedSegment> {
    let side_len =
        |side: StatusSide| view.segments.iter().filter(|segment| segment.side == side).count();
    let left_len = side_len(StatusSide::Left);
    let mut seen = [0usize; 3];
    let mut slots: Vec<FitSlot> = view
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let side_index = match segment.side {
                StatusSide::Left => 0,
                StatusSide::Center => 1,
                StatusSide::Right => 2,
            };
            let position = seen[side_index];
            seen[side_index] += 1;
            // Distance from the center: 0 for the center list, then outwards.
            let distance = match segment.side {
                StatusSide::Left => left_len - position,
                StatusSide::Center => 0,
                StatusSide::Right => position + 1,
            };
            FitSlot {
                index,
                side: segment.side,
                rank: (segment.protected, segment.priority, distance),
                part_widths: segment.parts.iter().map(|part| measure(&part.text)).collect(),
                parts: segment.parts.clone(),
                compact: segment.compact.clone(),
                active: true,
            }
        })
        .collect();
    let group_width = |slots: &[FitSlot], side: StatusSide| -> Option<f32> {
        let widths: Vec<f32> = slots
            .iter()
            .filter(|slot| slot.active && slot.side == side)
            .map(|slot| slot.width(metrics))
            .collect();
        (!widths.is_empty())
            .then(|| widths.iter().sum::<f32>() + metrics.segment_gap * (widths.len() - 1) as f32)
    };
    loop {
        let groups: Vec<f32> = [StatusSide::Left, StatusSide::Center, StatusSide::Right]
            .into_iter()
            .filter_map(|side| group_width(&slots, side))
            .collect();
        let needed = 2.0 * metrics.edge_pad
            + groups.iter().sum::<f32>()
            + metrics.segment_gap * groups.len().saturating_sub(1) as f32;
        let overflow = needed - metrics.width;
        if overflow <= 0.0 {
            // When: `overflow` is not positive, everything active fits.
            break;
        }
        let compactable = slots
            .iter_mut()
            .filter(|slot| slot.active && slot.compact.is_some())
            .min_by_key(|slot| slot.rank);
        if let Some(slot) = compactable {
            // When: `compactable` found a compact form, use the lowest-ranked one before dropping.
            let compact = slot.compact.take().unwrap_or_default();
            slot.part_widths = compact.iter().map(|part| measure(&part.text)).collect();
            slot.parts = compact;
            continue;
        }
        let Some(slot) = slots.iter_mut().filter(|slot| slot.active).min_by_key(|slot| slot.rank)
        else {
            // When: no active entry is left in `slots`, the strip is empty and cannot overflow.
            break;
        };
        if shrink_slot(slot, overflow, &mut measure) {
            // When: `shrink_slot` narrowed a text value, recheck the fit before dropping.
            continue;
        }
        if let Some(position) = slot.parts.iter().rposition(|part| part.droppable) {
            // When: a droppable detail is left, drop it before the whole segment.
            slot.parts.remove(position);
            slot.part_widths.remove(position);
            continue;
        }
        slot.active = false;
    }
    place(&slots, metrics)
}

/// Middle-ellipsize the widest text value of `slot` by `overflow`. Returns
/// whether it got narrower; a value at its minimum is no longer ellipsizable.
fn shrink_slot(slot: &mut FitSlot, overflow: f32, measure: &mut impl FnMut(&str) -> f32) -> bool {
    let widest = (0..slot.parts.len())
        .filter(|&position| slot.parts[position].ellipsize)
        .max_by(|&first, &second| slot.part_widths[first].total_cmp(&slot.part_widths[second]));
    let Some(position) = widest else {
        // When: `widest` is None, no ellipsizable value is left and the caller drops instead.
        return false;
    };
    let old_width = slot.part_widths[position];
    let target = old_width - overflow;
    let shortened = ellipsize_middle_to_width(&slot.parts[position].text, target, measure);
    let new_width = measure(&shortened);
    let part = &mut slot.parts[position];
    if new_width > target || new_width >= old_width {
        // The minimum form was reached, stop ellipsizing this value.
        part.ellipsize = false;
    }
    if new_width >= old_width {
        // When: `new_width` is not below `old_width`, shortening gained nothing.
        return false;
    }
    part.text = shortened;
    slot.part_widths[position] = new_width;
    true
}

/// `text` middle-ellipsized to at most `max_width`, keeping at least
/// four graphemes; the minimum form may exceed `max_width`.
pub fn ellipsize_middle_to_width(
    text: &str,
    max_width: f32,
    measure: &mut impl FnMut(&str) -> f32,
) -> String {
    if measure(text) <= max_width {
        // When: `measure(text)` is within `max_width`, keep it whole.
        return text.to_string();
    }
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    if graphemes.len() <= MIN_KEPT_GRAPHEMES + 1 {
        // When: `graphemes` is too short to shorten past `MIN_KEPT_GRAPHEMES`, keep it whole.
        return text.to_string();
    }
    let compose = |kept: usize| {
        let head = kept.div_ceil(2);
        let tail = kept / 2;
        let mut out: String = graphemes[..head].concat();
        out.push_str(ELLIPSIS);
        out.push_str(&graphemes[graphemes.len() - tail..].concat());
        out
    };
    let (mut low, mut high) = (MIN_KEPT_GRAPHEMES, graphemes.len() - 1);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if measure(&compose(middle)) <= max_width {
            low = middle;
        } else {
            // When: `measure` at `middle` exceeds `max_width`, search shorter forms.
            high = middle - 1;
        }
    }
    compose(low)
}

/// Place active slots: left from the left edge, right from the right edge,
/// center centered and kept between them.
fn place(slots: &[FitSlot], metrics: &FitMetrics) -> Vec<PlacedSegment> {
    let mut placed = Vec::new();
    let active =
        |side: StatusSide| slots.iter().filter(move |slot| slot.active && slot.side == side);
    let mut pen = metrics.edge_pad;
    let mut left_end = None;
    for slot in active(StatusSide::Left) {
        placed.push(place_slot(slot, pen, metrics));
        pen += slot.width(metrics);
        left_end = Some(pen);
        pen += metrics.segment_gap;
    }
    let mut pen = metrics.width - metrics.edge_pad;
    let mut right_start = None;
    for slot in active(StatusSide::Right).collect::<Vec<_>>().into_iter().rev() {
        pen -= slot.width(metrics);
        placed.push(place_slot(slot, pen, metrics));
        right_start = Some(pen);
        pen -= metrics.segment_gap;
    }
    let center: Vec<&FitSlot> = active(StatusSide::Center).collect();
    if !center.is_empty() {
        let total = center.iter().map(|slot| slot.width(metrics)).sum::<f32>()
            + metrics.segment_gap * (center.len() - 1) as f32;
        let low = left_end.map_or(metrics.edge_pad, |end| end + metrics.segment_gap);
        let high = right_start
            .map_or(metrics.width - metrics.edge_pad, |start| start - metrics.segment_gap)
            - total;
        let mut pen = ((metrics.width - total) / 2.0).clamp(low, high.max(low));
        for slot in center {
            placed.push(place_slot(slot, pen, metrics));
            pen += slot.width(metrics) + metrics.segment_gap;
        }
    }
    placed.sort_by_key(|segment| segment.index);
    placed
}

/// Place one slot's parts from `origin`.
fn place_slot(slot: &FitSlot, origin: f32, metrics: &FitMetrics) -> PlacedSegment {
    let mut pen = origin;
    let mut parts = Vec::with_capacity(slot.parts.len());
    for (position, (part, text_width)) in slot.parts.iter().zip(&slot.part_widths).enumerate() {
        let mut width = *text_width;
        if part.tone.is_chip() {
            if position > 0 && slot.parts[position - 1].tone.is_chip() {
                pen += metrics.chip_gap;
            }
            width += 2.0 * metrics.chip_pad;
        }
        parts.push(PlacedPart { text: part.text.clone(), tone: part.tone, left: pen, width });
        pen += width;
    }
    PlacedSegment { index: slot.index, left: origin, width: pen - origin, parts }
}

/// Which characters the sanitizer replaces besides the removed controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanitizePolicy {
    /// Paths, titles, process names and preedit text.
    Ordinary,
    /// Program-reported text: also replaces separator-like and origin-marker characters.
    Program,
}

/// Caps applied after character filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SanitizeLimits {
    /// Byte cap, applied on a character boundary.
    pub max_bytes: usize,
    /// Display-column cap, applied by middle ellipsis on grapheme boundaries.
    pub max_columns: usize,
    /// Collapse whitespace runs to one space and trim the ends.
    pub collapse_whitespace: bool,
}

/// Whether `character` is a general-category `Cf` format character.
fn is_format_character(character: char) -> bool {
    matches!(
        u32::from(character),
        0xAD | 0x600..=0x605
            | 0x61C
            | 0x6DD
            | 0x70F
            | 0x890..=0x891
            | 0x8E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// Whether the sanitizer removes `character` under every policy: C0 and C1
/// controls, line and paragraph separators, and every `Cf` except ZWJ, which
/// emoji sequences need.
fn is_removed(character: char) -> bool {
    matches!(u32::from(character), 0x00..=0x1F | 0x7F..=0x9F | 0x2028 | 0x2029)
        || (is_format_character(character) && character != '\u{200D}')
}

/// The one shared sanitizer for text shown on the strip.
pub fn sanitize_display_text(text: &str, policy: SanitizePolicy, limits: SanitizeLimits) -> String {
    let mut out = String::with_capacity(text.len().min(limits.max_bytes));
    let mut after_space = true;
    for character in text.chars() {
        if is_removed(character) {
            // When: `is_removed(character)`, it is a control or format character and is dropped.
            continue;
        }
        let character = match character {
            '│' | '|' | '⟨' | '⟩' | '<' | '>' if policy == SanitizePolicy::Program => ' ',
            other => other,
        };
        if limits.collapse_whitespace && character.is_whitespace() {
            // When: `collapse_whitespace` and `character` is whitespace, a run collapses to one space.
            if after_space {
                // When: `after_space`, a whitespace run is already open, so collapse into it.
                continue;
            }
            after_space = true;
        } else {
            // When: `character` is not collapsing whitespace, any open run ends.
            after_space = false;
        }
        let character =
            if limits.collapse_whitespace && character.is_whitespace() { ' ' } else { character };
        if out.len() + character.len_utf8() > limits.max_bytes {
            // When: the next character would pass `max_bytes`, stop on this boundary.
            break;
        }
        out.push(character);
    }
    if limits.collapse_whitespace {
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
    }
    ellipsize_middle_columns(&out, limits.max_columns)
}

/// `text` middle-ellipsized to at most `max_columns` display columns, cut on
/// grapheme boundaries.
pub fn ellipsize_middle_columns(text: &str, max_columns: usize) -> String {
    if text.width() <= max_columns {
        // When: the `width` is within `max_columns`, keep it whole.
        return text.to_string();
    }
    if max_columns == 0 {
        // When: `max_columns` is 0, nothing can be shown.
        return String::new();
    }
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let budget = max_columns - 1;
    let head_budget = budget.div_ceil(2);
    let mut head = String::new();
    let mut used = 0;
    let mut head_end = 0;
    for grapheme in &graphemes {
        if used + grapheme.width() > head_budget {
            // When: the next `grapheme` would pass `head_budget`, the head ends.
            break;
        }
        used += grapheme.width();
        head.push_str(grapheme);
        head_end += 1;
    }
    let tail_budget = budget - used;
    let mut tail_start = graphemes.len();
    let mut tail_used = 0;
    while tail_start > head_end && tail_used + graphemes[tail_start - 1].width() <= tail_budget {
        tail_start -= 1;
        tail_used += graphemes[tail_start].width();
    }
    format!("{head}{ELLIPSIS}{}", graphemes[tail_start..].concat())
}

/// Decimal separator and digit grouping of the UI locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NumberStyle {
    /// Thousands separator.
    pub group: char,
    /// Decimal separator.
    pub decimal: char,
}

impl NumberStyle {
    /// `12,345.6`: English, Chinese and Japanese.
    pub const POINT: Self = Self { group: ',', decimal: '.' };

    /// The style of a locale tag such as `de-DE` or `zh-CN`.
    pub fn for_locale(tag: &str) -> Self {
        let language = tag.split(['-', '_']).next().unwrap_or_default();
        match language {
            "de" | "es" | "it" | "nl" | "pt" | "tr" | "id" | "da" => {
                Self { group: '.', decimal: ',' }
            }
            "fr" | "ru" | "pl" | "cs" | "sv" | "fi" | "nb" | "uk" => {
                Self { group: '\u{202F}', decimal: ',' }
            }
            _ => Self::POINT,
        }
    }
}

/// `value` with the locale's digit grouping (`12,345`).
pub fn format_count(value: u64, style: NumberStyle) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 * 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            out.push(style.group);
        }
        out.push(digit);
    }
    out
}

/// Byte size in binary units with three significant figures and fixed unit
/// symbols (`412 MB`, `1.02 GB`); the unit rolls over at 1024.
pub fn format_bytes(bytes: u64, style: NumberStyle) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        // When: under 1 KB, whole bytes are exact.
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    // Three significant figures: 0, 1 or 2 decimals by magnitude, from these lower bounds.
    let decimals_for = |value: f64| {
        [(99.95, 0), (9.995, 1)]
            .into_iter()
            .find(|(lower, _)| value >= *lower)
            .map_or(2, |(_, decimals)| decimals)
    };
    let round_to = |value: f64, decimals: i32| {
        let scale = 10f64.powi(decimals);
        (value * scale).round() / scale
    };
    let mut decimals = decimals_for(value);
    let mut rounded = round_to(value, decimals);
    if rounded >= 1024.0 && unit < UNITS.len() - 1 {
        // Rounding reached 1024, roll over to the next unit.
        unit += 1;
        decimals = 2;
        rounded = round_to(rounded / 1024.0, decimals);
    }
    let number =
        format!("{rounded:.*}", decimals as usize).replace('.', &style.decimal.to_string());
    format!("{number} {}", UNITS[unit])
}

/// Duration with fixed unit suffixes, never localized: `42s`, `2m14s`, `1h05m`.
pub fn format_duration(total_secs: u64) -> String {
    let (hours, minutes, seconds) = (total_secs / 3600, total_secs / 60 % 60, total_secs % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m{seconds:02}s"),
        _ => format!("{hours}h{minutes:02}m"),
    }
}

/// Direction of a memory change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    /// Grew by at least 1%.
    Up,
    /// Shrank by at least 1%.
    Down,
}

/// The trend from `previous` to `current`, or `None` inside the 1% deadband.
pub fn mem_trend(previous: Option<u64>, current: u64) -> Option<Trend> {
    let previous = previous.filter(|previous| *previous > 0)?;
    let delta = current as f64 - previous as f64;
    if delta.abs() < previous as f64 * 0.01 {
        // When: `delta` is under 1% of `previous`, allocation noise shows no arrow.
        return None;
    }
    Some(if delta > 0.0 { Trend::Up } else { Trend::Down })
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod status_line_tests;
