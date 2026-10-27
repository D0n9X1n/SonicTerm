//! Status line model: segment inputs, the segment registry and formatters,
//! narrow-strip fitting, the shared sanitizer and the shared number, byte and
//! duration formatter.
//!
//! Pure and device-free. The App fills [`StatusLineInputs`], [`build_view`]
//! turns them into styled segments, and the renderer measures text and draws
//! what [`fit`] places. Every segment id has an exhaustive [`segment_spec`]
//! entry, so a new catalog id does not compile until it is registered.

use std::hash::{Hash, Hasher};
use std::time::Duration;

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
    /// A mode chip: bold text in the value color, with no background.
    Chip,
    /// A mode chip that needs attention (broadcast input): bold text in the caution color.
    ChipWarn,
}

impl StatusTone {
    /// Whether the part is a mode chip, drawn bold in its color with no background.
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
        Seg::Cpu => {
            (UpdateKind::Sampled, format_cpu, DefaultClick::Action(Action::ShowMemoryUsage))
        }
        Seg::Out => (UpdateKind::Sampled, format_out, DefaultClick::None),
        Seg::Frame => (UpdateKind::Sampled, format_frame, DefaultClick::None),
        Seg::PathCheck => (UpdateKind::Event, format_path_check, DefaultClick::None),
        Seg::TabMem => (UpdateKind::Sampled, format_tab_mem, DefaultClick::None),
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
    /// SonicTerm's own CPU use in whole percent, while shown by its hysteresis.
    pub cpu_percent: Option<u64>,
    /// The window's content frames over the last sample interval, and its presenter.
    pub frame: Option<FrameInput>,
    /// PTY output bytes per second over the last interval, while shown by its hysteresis.
    pub out_rate: Option<u64>,
    /// A path check that has run past `show_after_ms`, or one held after ending.
    pub path_check: Option<PathCheckInput>,
    /// The terminal state SonicTerm keeps for the active tab's panes, from cached charges.
    pub tab_mem: Option<TabMemInput>,
}

/// Which presenter draws a window's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenterKind {
    /// wgpu on a hardware adapter.
    Hardware,
    /// wgpu on a CPU adapter.
    SoftwareAdapter,
    /// The Windows GDI software presenter.
    Gdi,
}

/// A window's content frames over one sample interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInput {
    /// Content frames presented in the interval.
    pub frames: u64,
    /// Their mean CPU-side frame time, in microseconds.
    pub mean_frame_us: u64,
    /// Frames per second over the interval.
    pub fps: u64,
    /// The presenter.
    pub presenter: PresenterKind,
}

/// The path-check segment's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathCheckInput {
    /// Whole seconds the oldest running check has run; the last age while held.
    pub secs: u64,
    /// It ran past `warn_after_secs`.
    pub warn: bool,
    /// Checks queued behind it, waiting for a worker.
    pub queued: u64,
}

/// The tab-memory segment's input: a sum of cached per-pane charges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabMemInput {
    /// The sum as last sampled.
    pub bytes: u64,
    /// The oldest charge's age in seconds when some pane's charge is stale.
    pub stale_secs: Option<u64>,
}

/// Show-and-hide hysteresis for a sampled figure: it appears at `min`, and hides only after
/// `hold_samples` consecutive samples below `min × hide_permille / 1000`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hysteresis {
    /// Whether the figure is shown.
    pub shown: bool,
    /// Consecutive samples below the hide level so far.
    below: u64,
}

impl Hysteresis {
    /// Apply one sample of `value` and return whether the figure is shown after it.
    pub fn step(&mut self, value: u64, min: u64, hide_permille: u64, hold_samples: u64) -> bool {
        if value >= min {
            // When: value reaches min, the figure shows and any run of low samples ends.
            *self = Self { shown: true, below: 0 };
            return true;
        }
        if !self.shown {
            // When: shown is false and value is below min, it stays hidden.
            return false;
        }
        let hide_level = min.saturating_mul(hide_permille) / 1000;
        if value >= hide_level {
            // When: value is between the hide level and min, the run of low samples resets.
            self.below = 0;
            return true;
        }
        self.below += 1;
        if self.below > hold_samples {
            *self = Self::default();
        }
        self.shown
    }
}

/// CPU use in whole percent for `cpu` CPU time over `wall` wall time: of one core, or of all
/// `cores` when `total`.
pub fn cpu_percent(cpu: Duration, wall: Duration, total: bool, cores: u64) -> u64 {
    if wall.is_zero() {
        // When: wall is zero, no interval elapsed, so there is no rate.
        return 0;
    }
    let divisor = if total {
        wall.as_secs_f64() * cores.max(1) as f64
    } else {
        // When: total is false, 100% is one busy core.
        wall.as_secs_f64()
    };
    (cpu.as_secs_f64() / divisor * 100.0).round() as u64
}

/// Bytes per second for `bytes` over `wall`.
pub fn byte_rate(bytes: u64, wall: Duration) -> u64 {
    if wall.is_zero() {
        // When: wall is zero, no interval elapsed, so there is no rate.
        return 0;
    }
    (bytes as f64 / wall.as_secs_f64()).round() as u64
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

/// The live font size as a scale of the configured size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontInput {
    /// The live size as a whole percent of the configured size, rounded half up from the real
    /// sizes; `None` for a zero configured size, which has no scale.
    pub scale_percent: Option<u32>,
}

impl FontInput {
    /// The scale of `live` points over `configured` points, from the sizes themselves, not from
    /// sizes rounded first, so 14.34 over 13.34 is 107%.
    pub fn from_sizes(live: f32, configured: f32) -> Self {
        let scale_percent = (configured > 0.0)
            .then(|| (f64::from(live) / f64::from(configured) * 100.0).round())
            .filter(|percent| percent.is_finite() && *percent >= 0.0)
            .map(|percent| percent.min(f64::from(u32::MAX)) as u32);
        Self { scale_percent }
    }
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

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod status_line_tests;

#[path = "status_line_format.rs"]
mod status_line_format;
use status_line_format::*;

#[path = "status_line_links.rs"]
mod status_line_links;
use status_line_links::*;
pub use status_line_links::{
    check_link_label, sanitize_link_text, url_authority, LinkInput, LinkLabelCheck, PathAction,
    UrlAuthority,
};

#[path = "status_line_fit.rs"]
mod status_line_fit;
pub use status_line_fit::{
    ellipsize_middle_to_width, fit, separators, FitMetrics, PlacedPart, PlacedSegment,
    SEGMENT_SEPARATOR,
};

#[path = "status_line_text.rs"]
mod status_line_text;
pub use status_line_text::{
    ellipsize_middle_bytes, ellipsize_middle_columns, format_bytes, format_count, format_duration,
    mem_trend, sanitize_display_text, NumberStyle, SanitizeLimits, SanitizePolicy, Trend,
};

#[path = "status_line_accessibility.rs"]
mod status_line_accessibility;
pub use status_line_accessibility::{
    accessible_strip, AccessibleRole, AccessibleSegment, AccessibleStrip, CommittedSegment,
};
