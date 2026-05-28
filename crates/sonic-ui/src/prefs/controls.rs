//! Form-control widgets for the preferences window.
//!
//! Every widget is a plain struct holding its current pixel-space rect
//! and value. Hit-testing is `rect.contains(x, y)` — there is no
//! retained-mode tree. The owning [`PrefsState`](super::state::PrefsState)
//! reads back values after the host (`app.rs`) routes pointer events
//! through [`Control::on_pointer_down`] / [`Control::on_drag`] etc.
//!
//! Rendering reads each widget's pixel rect + value and emits quads +
//! text via the standard [`crate::render::GpuRenderer`].

use std::fmt;
use std::time::Instant;

/// Stable id used by [`super::state::PrefsState`] to dispatch events to
/// the right control without holding a `&mut` to the whole form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u32);

impl fmt::Display for WidgetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "w{}", self.0)
    }
}

/// Pixel-space axis-aligned rectangle. Top-left origin (matches winit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Visual interaction state shared across all control primitives. Drives
/// hover tints, press feedback and focus halos in the renderer.
///
/// Added in the issue #173 slice-2 redesign so Toggle / Dropdown / Button
/// can be rendered with consistent pressed / hovered / focused affordances
/// without per-widget bespoke fields.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InteractionState {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
}

impl InteractionState {
    pub const fn new() -> Self {
        Self { hovered: false, pressed: false, focused: false }
    }
}

/// On/off switch backed by a `bool`. The redesigned (issue #173) toggle is
/// a 44×24 pill with a 20px sliding thumb; the active fill comes from
/// `theme.accent`. The struct itself only carries data — the renderer
/// reads `value`, `interaction`, and the layout's `TOGGLE_*` constants.
///
/// `knob_anim_start` records the `Instant` at which the most recent
/// state flip happened (set by [`Toggle::toggle`] and [`Toggle::set`]
/// when the value actually changes). The renderer interpolates the
/// thumb between the off- and on-position over [`Toggle::ANIM_MS`]
/// using [`Toggle::knob_x_animated`]. Call
/// [`Toggle::clear_anim_if_done`] after reading the animated position so
/// completed animations are not needlessly re-interpolated every frame.
#[derive(Debug, Clone)]
pub struct Toggle {
    pub id: WidgetId,
    pub label: String,
    pub rect: Rect,
    pub value: bool,
    pub interaction: InteractionState,
    /// Timestamp of the last state flip, used by the renderer to lerp
    /// the sliding thumb position. `None` means "no animation in
    /// flight; render the snapped end position".
    pub knob_anim_start: Option<Instant>,
}

impl Toggle {
    /// Duration of the sliding-thumb animation in milliseconds.
    /// Matches the figma spec for issue #173 slice-2c — short enough
    /// to feel snappy, long enough to read as movement (vs. an
    /// instantaneous jump).
    pub const ANIM_MS: u64 = 120;

    pub fn new(id: WidgetId, label: impl Into<String>, rect: Rect, value: bool) -> Self {
        Self {
            id,
            label: label.into(),
            rect,
            value,
            interaction: InteractionState::new(),
            knob_anim_start: None,
        }
    }

    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    pub fn toggle(&mut self) -> bool {
        self.value = !self.value;
        self.knob_anim_start = Some(Instant::now());
        self.value
    }

    pub fn set(&mut self, v: bool) {
        if self.value != v {
            self.knob_anim_start = Some(Instant::now());
        }
        self.value = v;
    }

    pub fn get(&self) -> bool {
        self.value
    }

    /// X coordinate of the sliding thumb's left edge for a given track
    /// rect, honoring `TOGGLE_KNOB` + `TOGGLE_KNOB_MARGIN` from the layout.
    /// Pure math so the renderer and tests agree. Returns the *snapped*
    /// position — use [`Toggle::knob_x_animated`] for the in-flight
    /// interpolated value.
    pub fn knob_x(&self, knob_size: f32, margin: f32) -> f32 {
        if self.value {
            self.rect.x + self.rect.w - knob_size - margin
        } else {
            self.rect.x + margin
        }
    }

    /// X coordinate of the sliding thumb's left edge at time `now`,
    /// linearly interpolating between the previous end and the new
    /// end over [`Toggle::ANIM_MS`] milliseconds since
    /// [`Toggle::knob_anim_start`].
    ///
    /// * If `reduced_motion` is true, returns the snapped position.
    /// * If `knob_anim_start` is `None`, returns the snapped position.
    /// * If `now - start >= ANIM_MS`, returns the snapped position.
    /// * Otherwise returns `lerp(start_pos, end_pos, t)` where
    ///   `t = elapsed / ANIM_MS` and `start_pos` is the position
    ///   the toggle was in *before* the most recent flip
    ///   (i.e. the opposite of the current `value`).
    ///
    /// Pure math + a single `now.duration_since(start)` read, so tests
    /// can inject any `now` and assert mid-animation behavior.
    pub fn knob_x_animated(
        &self,
        now: Instant,
        knob_size: f32,
        margin: f32,
        reduced_motion: bool,
    ) -> f32 {
        let (x, _) = self.knob_x_animated_with_done(now, knob_size, margin, reduced_motion);
        x
    }

    /// Same as [`Self::knob_x_animated`], plus a completion flag the
    /// renderer can use to clear [`Self::knob_anim_start`] after the
    /// final snapped frame has been computed.
    pub fn knob_x_animated_with_done(
        &self,
        now: Instant,
        knob_size: f32,
        margin: f32,
        reduced_motion: bool,
    ) -> (f32, bool) {
        let end = self.knob_x(knob_size, margin);
        if reduced_motion {
            return (end, self.knob_anim_start.is_some());
        }
        let Some(start) = self.knob_anim_start else {
            return (end, false);
        };
        let elapsed_ms = now.saturating_duration_since(start).as_millis() as u64;
        if elapsed_ms >= Self::ANIM_MS {
            return (end, true);
        }
        // start_pos == the snapped position of the *previous* value,
        // which is `!self.value`.
        let start_pos = if !self.value {
            self.rect.x + self.rect.w - knob_size - margin
        } else {
            self.rect.x + margin
        };
        let t = elapsed_ms as f32 / Self::ANIM_MS as f32;
        (start_pos + (end - start_pos) * t, false)
    }

    pub fn clear_anim_if_done(&mut self, done: bool) {
        if done {
            self.knob_anim_start = None;
        }
    }
}

/// Numeric range control. Drag the thumb to update.
#[derive(Debug, Clone)]
pub struct Slider {
    pub id: WidgetId,
    pub label: String,
    pub rect: Rect,
    pub min: f32,
    pub max: f32,
    pub value: f32,
    /// Optional snap step (e.g. `1.0` for integer-only sliders). 0 = none.
    pub step: f32,
}

impl Slider {
    pub fn new(
        id: WidgetId,
        label: impl Into<String>,
        rect: Rect,
        min: f32,
        max: f32,
        value: f32,
    ) -> Self {
        assert!(min < max, "slider min must be < max");
        let mut s = Self { id, label: label.into(), rect, min, max, value, step: 0.0 };
        s.value = s.clamp(value);
        s
    }

    pub fn with_step(mut self, step: f32) -> Self {
        self.step = step;
        self.value = self.snap(self.value);
        self
    }

    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    pub fn clamp(&self, v: f32) -> f32 {
        v.clamp(self.min, self.max)
    }

    fn snap(&self, v: f32) -> f32 {
        if self.step <= 0.0 {
            return v;
        }
        let n = ((v - self.min) / self.step).round();
        (self.min + n * self.step).clamp(self.min, self.max)
    }

    /// Update value from a horizontal pixel coordinate inside the
    /// slider's track. Coordinates outside are clamped.
    pub fn drag_to(&mut self, x: f32) -> f32 {
        let t = ((x - self.rect.x) / self.rect.w).clamp(0.0, 1.0);
        let raw = self.min + t * (self.max - self.min);
        self.value = self.snap(raw);
        self.value
    }

    pub fn set(&mut self, v: f32) {
        self.value = self.snap(self.clamp(v));
    }

    pub fn get(&self) -> f32 {
        self.value
    }

    /// Position of the thumb in [0,1] for rendering.
    pub fn fraction(&self) -> f32 {
        if (self.max - self.min).abs() < f32::EPSILON {
            0.0
        } else {
            ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        }
    }
}

/// Pop-down list of string options. The redesigned (issue #173 slice-2)
/// combobox is 32 px tall with a 10 px radius, a visible right-edge
/// chevron, and a popover that opens on click and closes on outside click
/// or selection. The struct only carries data — the renderer reads
/// `selected`, `open`, `interaction`, and uses [`chevron_rect`] to position
/// the chevron quad.
#[derive(Debug, Clone)]
pub struct Dropdown {
    pub id: WidgetId,
    pub label: String,
    pub rect: Rect,
    pub options: Vec<String>,
    pub selected: usize,
    pub open: bool,
    pub interaction: InteractionState,
}

impl Dropdown {
    pub fn new(
        id: WidgetId,
        label: impl Into<String>,
        rect: Rect,
        options: Vec<String>,
        selected: usize,
    ) -> Self {
        let selected = if options.is_empty() { 0 } else { selected.min(options.len() - 1) };
        Self {
            id,
            label: label.into(),
            rect,
            options,
            selected,
            open: false,
            interaction: InteractionState::new(),
        }
    }

    /// Width of the chevron column on the right edge of the closed combobox.
    pub const CHEVRON_W: f32 = 22.0;

    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y) || self.hit_option(x, y).is_some()
    }

    /// Rect occupied by the chevron glyph on the closed combobox.
    pub fn chevron_rect(&self) -> Rect {
        Rect::new(
            self.rect.x + self.rect.w - Self::CHEVRON_W,
            self.rect.y,
            Self::CHEVRON_W,
            self.rect.h,
        )
    }

    /// Returns the index of the option list row hit when the dropdown is
    /// open, or `None`. Each row uses the same height as the closed
    /// header, stacked directly below.
    pub fn hit_option(&self, x: f32, y: f32) -> Option<usize> {
        if !self.open || self.options.is_empty() {
            return None;
        }
        if x < self.rect.x || x >= self.rect.x + self.rect.w {
            return None;
        }
        let top = self.rect.y + self.rect.h;
        let row_h = self.rect.h;
        let bottom = top + row_h * self.options.len() as f32;
        if y < top || y >= bottom {
            return None;
        }
        let idx = ((y - top) / row_h) as usize;
        if idx < self.options.len() {
            Some(idx)
        } else {
            None
        }
    }

    pub fn toggle_open(&mut self) {
        self.open = !self.open;
    }

    /// Close the popover (used when the user clicks outside).
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx < self.options.len() {
            self.selected = idx;
            self.open = false;
            true
        } else {
            false
        }
    }

    /// Convenience: select by string match. Returns true on success.
    pub fn select_by_name(&mut self, name: &str) -> bool {
        if let Some(i) = self.options.iter().position(|o| o == name) {
            self.selected = i;
            true
        } else {
            false
        }
    }

    pub fn value(&self) -> Option<&str> {
        self.options.get(self.selected).map(String::as_str)
    }

    pub fn set(&mut self, idx: usize) {
        let _ = self.select(idx);
    }

    pub fn get(&self) -> usize {
        self.selected
    }
}

/// Pill-shaped button with center-aligned text. Added in the issue #173
/// slice-2 redesign so the Apply / Cancel buttons (and any future bindable
/// in-prefs actions) can share a single control primitive that
/// participates in the standard hover / press / focus pipeline.
///
/// The struct only carries data; the renderer reads `label`, `rect`,
/// `kind`, and `interaction`. Layout constants (radius, height) live in
/// [`super::layout`] so the GPU side stays in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Filled with `theme.accent`, contrasting text.
    Primary,
    /// Outlined / subtle fill, theme.fg text.
    Secondary,
}

#[derive(Debug, Clone)]
pub struct Button {
    pub id: WidgetId,
    pub label: String,
    pub rect: Rect,
    pub kind: ButtonKind,
    pub interaction: InteractionState,
}

impl Button {
    pub fn new(id: WidgetId, label: impl Into<String>, rect: Rect, kind: ButtonKind) -> Self {
        Self { id, label: label.into(), rect, kind, interaction: InteractionState::new() }
    }

    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    /// Center coordinates of the button's text. The renderer uses this to
    /// emit text at the correct anchor (fixes issue #169 where the Apply
    /// button text was left-aligned).
    pub fn text_center(&self) -> (f32, f32) {
        (self.rect.x + self.rect.w / 2.0, self.rect.y + self.rect.h / 2.0)
    }
}

/// 16-cell ANSI palette swatch + a hex text entry; clicking a cell
/// copies that color into the bound field.
#[derive(Debug, Clone)]
pub struct ColorSwatch {
    pub id: WidgetId,
    pub label: String,
    pub rect: Rect,
    /// RGBA in 0..=255.
    pub value: [u8; 4],
    /// Palette presented as quick picks.
    pub palette: Vec<[u8; 4]>,
}

impl ColorSwatch {
    pub fn new(id: WidgetId, label: impl Into<String>, rect: Rect, value: [u8; 4]) -> Self {
        Self { id, label: label.into(), rect, value, palette: default_ansi_palette() }
    }

    /// Cell size of a single palette entry; the grid is `4` wide.
    pub const CELL: f32 = 18.0;
    pub const COLS: usize = 8;

    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y) || self.hit_cell(x, y).is_some()
    }

    pub fn hit_cell(&self, x: f32, y: f32) -> Option<usize> {
        // Palette grid is rendered just below the rect.
        let top = self.rect.y + self.rect.h + 4.0;
        let rows = self.palette.len().div_ceil(Self::COLS);
        let bottom = top + rows as f32 * Self::CELL;
        let left = self.rect.x;
        let right = left + Self::COLS as f32 * Self::CELL;
        if x < left || x >= right || y < top || y >= bottom {
            return None;
        }
        let col = ((x - left) / Self::CELL) as usize;
        let row = ((y - top) / Self::CELL) as usize;
        let idx = row * Self::COLS + col;
        if idx < self.palette.len() {
            Some(idx)
        } else {
            None
        }
    }

    pub fn pick(&mut self, idx: usize) -> bool {
        if let Some(c) = self.palette.get(idx).copied() {
            self.value = c;
            true
        } else {
            false
        }
    }

    pub fn set(&mut self, rgba: [u8; 4]) {
        self.value = rgba;
    }

    pub fn get(&self) -> [u8; 4] {
        self.value
    }

    pub fn to_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.value[0], self.value[1], self.value[2])
    }

    /// Parse `#rrggbb` or `rrggbb`, alpha defaulting to 255.
    pub fn from_hex(s: &str) -> Option<[u8; 4]> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some([r, g, b, 255])
    }
}

fn default_ansi_palette() -> Vec<[u8; 4]> {
    // Classic 16-color VGA palette (8 normal + 8 bright).
    [
        [0x00, 0x00, 0x00, 0xff],
        [0x80, 0x00, 0x00, 0xff],
        [0x00, 0x80, 0x00, 0xff],
        [0x80, 0x80, 0x00, 0xff],
        [0x00, 0x00, 0x80, 0xff],
        [0x80, 0x00, 0x80, 0xff],
        [0x00, 0x80, 0x80, 0xff],
        [0xc0, 0xc0, 0xc0, 0xff],
        [0x80, 0x80, 0x80, 0xff],
        [0xff, 0x00, 0x00, 0xff],
        [0x00, 0xff, 0x00, 0xff],
        [0xff, 0xff, 0x00, 0xff],
        [0x00, 0x00, 0xff, 0xff],
        [0xff, 0x00, 0xff, 0xff],
        [0x00, 0xff, 0xff, 0xff],
        [0xff, 0xff, 0xff, 0xff],
    ]
    .to_vec()
}

/// Free-form string entry. IME / kanji input is Tier-2; this struct just
/// stores the buffer and exposes `set` / `push` / `pop`.
#[derive(Debug, Clone)]
pub struct TextField {
    pub id: WidgetId,
    pub label: String,
    pub rect: Rect,
    pub value: String,
    pub focused: bool,
    /// Hard cap to keep render path simple.
    pub max_len: usize,
}

impl TextField {
    pub fn new(
        id: WidgetId,
        label: impl Into<String>,
        rect: Rect,
        value: impl Into<String>,
    ) -> Self {
        Self { id, label: label.into(), rect, value: value.into(), focused: false, max_len: 256 }
    }

    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    pub fn focus(&mut self) {
        self.focused = true;
    }

    pub fn blur(&mut self) {
        self.focused = false;
    }

    pub fn push_char(&mut self, c: char) {
        if self.value.chars().count() < self.max_len {
            self.value.push(c);
        }
    }

    pub fn pop_char(&mut self) {
        let _ = self.value.pop();
    }

    pub fn set(&mut self, v: impl Into<String>) {
        let v = v.into();
        self.value = v.chars().take(self.max_len).collect();
    }

    pub fn get(&self) -> &str {
        &self.value
    }
}

/// Enum wrapper so a form can hold heterogeneous controls in one Vec.
#[derive(Debug, Clone)]
pub enum Control {
    Toggle(Toggle),
    Slider(Slider),
    Dropdown(Dropdown),
    ColorSwatch(ColorSwatch),
    TextField(TextField),
}

impl Control {
    pub fn id(&self) -> WidgetId {
        match self {
            Control::Toggle(t) => t.id,
            Control::Slider(s) => s.id,
            Control::Dropdown(d) => d.id,
            Control::ColorSwatch(c) => c.id,
            Control::TextField(f) => f.id,
        }
    }

    pub fn hit_test(&self, x: f32, y: f32) -> bool {
        match self {
            Control::Toggle(t) => t.hit_test(x, y),
            Control::Slider(s) => s.hit_test(x, y),
            Control::Dropdown(d) => d.hit_test(x, y),
            Control::ColorSwatch(c) => c.hit_test(x, y),
            Control::TextField(f) => f.hit_test(x, y),
        }
    }
}
