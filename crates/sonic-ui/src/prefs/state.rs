//! Edit-buffer + persistence for the preferences window.
//!
//! [`PrefsState`] wraps a mutable copy of the user [`Config`] alongside
//! the form's [`Control`] widgets. Mutations go through helper methods
//! that mark the buffer dirty; [`apply`] serializes back to TOML on disk
//! and clears the flag, [`cancel`] discards changes by restoring the
//! original snapshot.

use std::path::PathBuf;

use anyhow::Result;
use sonic_cfg::config::{Config, CursorShape};
use sonic_cfg::theme::Theme;

use super::controls::{
    known_theme_preview, Button, ButtonAction, ButtonKind, ColorSwatch, Control, Dropdown, Rect,
    Slider, TextField, Toggle, WidgetId,
};
use super::layout::{Category, PrefsLayout};

/// Resolve the swatch RGBA (`[u8; 4]`) for the active theme's accent.
/// The swatch widget stores sRGB 8-bit channels, so we read the
/// theme's accent hex (`tab.active_fg` — the same source
/// `UiPalette::accent` uses) directly instead of round-tripping
/// through the linear-sRGB premultiplied palette.
fn accent_swatch_rgba(theme: &Theme) -> [u8; 4] {
    // Neutral sentinel on parse failure — never bake a theme-specific default
    // (a Tokyo-Night blue here used to leak through when a user-supplied theme
    // had a malformed accent hex). The renderer overlays the active palette
    // accent at draw time, so a transparent fallback is correct.
    ColorSwatch::from_hex(&theme.colors.tab.active_fg.0).unwrap_or([0, 0, 0, 0])
}

/// Classified result of a pointer click inside the preferences window.
/// Returned by [`PrefsState::classify_click`] so the host (app.rs) can
/// dispatch without re-implementing the priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefsHit {
    Apply,
    Cancel,
    ResetSection,
    Sidebar(Category),
    Toggle(WidgetId),
    SliderTrack(WidgetId),
    DropdownHeader(WidgetId),
    DropdownOption { id: WidgetId, index: usize },
    ColorCell { id: WidgetId, index: usize },
    TextField(WidgetId),
    Button(WidgetId),
}

/// Pre-canned theme list shown in the Appearance picker. Matches the
/// themes bundled under `assets/themes/`.
pub const KNOWN_THEMES: &[&str] =
    &["gruvbox-dark-hard", "wezterm", "tokyo-night", "dracula", "nord", "catppuccin-mocha"];

/// Pre-canned keymaps shown in the Keymap picker.
pub const KNOWN_KEYMAPS: &[&str] = &["wezterm"];

/// Cursor shape options shown in the Behavior picker.
pub const KNOWN_CURSOR_SHAPES: &[&str] = &["block", "bar", "underline"];

/// Pre-canned monospace families. Free-form text still wins for advanced
/// users; this is just a quick picker.
pub const KNOWN_FONTS: &[&str] = &[
    "JetBrainsMono Nerd Font",
    "Fira Code",
    "Menlo",
    "Cascadia Code",
    "Source Code Pro",
    "Rec Mono Casual",
    // "Rec Mono St.Helens" is the brand-default family (see
    // `sonic_cfg::config::DEFAULT_FONT_FAMILY`). It IS bundled in
    // `assets/fonts/` (4 variants, SIL OFL 1.1). Listed last so existing
    // test expectations against `KNOWN_FONTS[1]` ("Fira Code") keep holding.
    "Rec Mono St.Helens",
];

/// (tag, native display label) pairs for the Language dropdown. The empty
/// tag means "auto-detect from OS locale". The native script for the
/// non-default rows is chosen so the user can identify the option without
/// already speaking the current UI language.
pub const LANGUAGE_OPTIONS: &[(&str, &str)] =
    &[("", "Auto"), ("en", "English"), ("zh-CN", "中文"), ("ja", "日本語")];

/// In-memory edit state for the preferences window.
pub const RESET_TO_DEFAULT_KEY: &str = "prefs-reset-to-default";

pub struct PrefsState {
    /// Live mutable copy of the config.
    pub config: Config,
    /// Snapshot taken at construction, used by [`cancel`] to revert.
    pub original: Config,
    /// Currently-displayed category.
    pub active_category: Category,
    /// Has anything changed since the last apply / open?
    pub dirty: bool,
    /// Path the config will be written to on apply.
    pub config_path: PathBuf,
    /// All controls in the current view, in display order.
    pub controls: Vec<Control>,
    /// Cached layout for hit testing.
    pub layout: PrefsLayout,
    /// Currently focused TextField (for keyboard typing), if any.
    pub focused_field: Option<WidgetId>,
    /// Translation bundle used to localize widget labels. Rebuilt
    /// whenever the user picks a new locale in the Appearance category.
    pub i18n: crate::i18n::I18n,
    /// Active terminal theme. Drives chrome-derived colors in the
    /// preferences UI (e.g. the Accent swatch in Appearance).
    pub theme: Theme,
    /// Footer **Apply** button primitive (issue #173 slice-2). Owns its
    /// own [`InteractionState`] so the mouse handler can flip
    /// hover/press flags and the renderer can read them back for the
    /// bg color and the rounded-rect radius.
    pub apply_button: Button,
    /// Footer **Cancel** button primitive (issue #173 slice-2).
    pub cancel_button: Button,
    /// Footer link that restores the active section to Config::default().
    pub reset_button: Button,
}

impl PrefsState {
    /// Build a fresh edit buffer over `config` saving to `config_path`.
    /// `theme` is the currently-active terminal theme; the preferences
    /// UI uses it to derive chrome colors (e.g. the Accent swatch).
    pub fn new(config: Config, config_path: PathBuf, theme: Theme) -> Self {
        let layout = PrefsLayout::default_size();
        let i18n = crate::i18n::I18n::new(if config.locale.is_empty() {
            None
        } else {
            Some(config.locale.as_str())
        });
        // Footer button primitives (issue #173 slice-2). Stable widget
        // IDs let the mouse handler dispatch hover/press events.
        let apply_button = Button::new(
            WidgetId(u32::MAX - 1),
            i18n.t("prefs-apply"),
            layout.apply_button,
            ButtonKind::Primary,
        );
        let cancel_button = Button::new(
            WidgetId(u32::MAX - 2),
            i18n.t("prefs-cancel"),
            layout.cancel_button,
            ButtonKind::Secondary,
        );
        let reset_button = Button::new(
            WidgetId(u32::MAX - 3),
            i18n.t(RESET_TO_DEFAULT_KEY),
            layout.reset_link,
            ButtonKind::Link,
        );
        let mut s = Self {
            original: config.clone(),
            config,
            active_category: Category::Font,
            dirty: false,
            config_path,
            controls: Vec::new(),
            layout,
            focused_field: None,
            i18n,
            theme,
            apply_button,
            cancel_button,
            reset_button,
        };
        s.rebuild_controls();
        s
    }

    /// Replace the active theme (e.g. when the user picks a new theme
    /// from the Appearance dropdown live) and rebuild controls so the
    /// accent swatch + any other theme-derived widgets pick up the
    /// new palette.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.rebuild_controls();
    }

    /// Rebuild the [`Control`] list for the active category. Called any
    /// time the category changes or controls are mutated.
    pub fn rebuild_controls(&mut self) {
        self.apply_button.label = self.i18n.t("prefs-apply");
        self.cancel_button.label = self.i18n.t("prefs-cancel");
        self.reset_button.label = self.i18n.t(RESET_TO_DEFAULT_KEY);

        let mut id: u32 = 1;
        let mut next_id = || {
            let i = id;
            id += 1;
            WidgetId(i)
        };
        let mut out: Vec<Control> = Vec::new();
        let l = self.layout;
        match self.active_category {
            Category::Font => {
                let sel =
                    KNOWN_FONTS.iter().position(|f| *f == self.config.font.family).unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    self.i18n.t("prefs-font-family"),
                    l.control_slot(0),
                    KNOWN_FONTS.iter().map(|s| (*s).to_string()).collect(),
                    sel,
                )));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        self.i18n.t("prefs-font-size"),
                        l.control_slot(1),
                        8.0,
                        32.0,
                        self.config.font.size,
                    );
                    s = s.with_step(1.0);
                    s
                }));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        self.i18n.t("prefs-line-height"),
                        l.control_slot(2),
                        1.0,
                        2.0,
                        self.config.font.line_height,
                    );
                    s = s.with_step(0.05);
                    s
                }));
            }
            Category::Theme => {
                let sel = KNOWN_THEMES.iter().position(|t| *t == self.config.theme).unwrap_or(0);
                let options: Vec<String> = KNOWN_THEMES.iter().map(|s| (*s).to_string()).collect();
                let previews =
                    KNOWN_THEMES.iter().filter_map(|name| known_theme_preview(name)).collect();
                out.push(Control::Dropdown(
                    Dropdown::new(
                        next_id(),
                        self.i18n.t("prefs-theme"),
                        l.control_slot(0),
                        options,
                        sel,
                    )
                    .with_option_previews(previews),
                ));
                out.push(Control::ColorSwatch(ColorSwatch::new(
                    next_id(),
                    self.i18n.t("prefs-accent"),
                    l.control_slot(1),
                    accent_swatch_rgba(&self.theme),
                )));
            }
            Category::Keymap => {
                out.push(Control::Button(
                    Button::new(
                        next_id(),
                        self.i18n.t("prefs-open-keymap-file"),
                        l.control_slot(0),
                        ButtonKind::Secondary,
                    )
                    .with_action(ButtonAction::OpenKeymapFile),
                ));
            }
            Category::Window => {
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        self.i18n.t("prefs-opacity"),
                        l.control_slot(0),
                        0.3,
                        1.0,
                        self.config.window.opacity,
                    );
                    s = s.with_step(0.05);
                    s
                }));
                out.push(Control::Toggle(Toggle::new(
                    next_id(),
                    self.i18n.t("prefs-background-blur"),
                    l.control_slot(1),
                    self.config.window.blur,
                )));
                out.push(Control::Toggle(Toggle::new(
                    next_id(),
                    self.i18n.t("prefs-window-decorations"),
                    l.control_slot(2),
                    self.config.window.decorations,
                )));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        self.i18n.t("prefs-padding"),
                        l.control_slot(3),
                        0.0,
                        32.0,
                        self.config.window.padding_left,
                    );
                    s = s.with_step(1.0);
                    s
                }));
            }
            Category::Cursor => {
                let cur_shape = self.config.terminal.cursor_shape.as_str();
                let sel = KNOWN_CURSOR_SHAPES.iter().position(|s| *s == cur_shape).unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    self.i18n.t("prefs-cursor-shape"),
                    l.control_slot(0),
                    KNOWN_CURSOR_SHAPES.iter().map(|s| (*s).to_string()).collect(),
                    sel,
                )));
                out.push(Control::Toggle(Toggle::new(
                    next_id(),
                    self.i18n.t("prefs-cursor-blink"),
                    l.control_slot(1),
                    self.config.terminal.cursor_blink,
                )));
            }
            Category::Advanced => {
                out.push(Control::TextField(TextField::new(
                    next_id(),
                    self.i18n.t("prefs-shell"),
                    l.control_slot(0),
                    self.config.terminal.shell.clone().unwrap_or_default(),
                )));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        self.i18n.t("prefs-scrollback"),
                        l.control_slot(1),
                        1_000.0,
                        100_000.0,
                        self.config.terminal.scrollback as f32,
                    );
                    s = s.with_step(1_000.0);
                    s
                }));
                let lang_sel = LANGUAGE_OPTIONS
                    .iter()
                    .position(|(tag, _)| *tag == self.config.locale.as_str())
                    .unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    self.i18n.t("prefs-language"),
                    l.control_slot(2),
                    LANGUAGE_OPTIONS
                        .iter()
                        .map(|(tag, label)| {
                            if tag.is_empty() {
                                self.i18n.t("prefs-language-auto")
                            } else {
                                (*label).to_string()
                            }
                        })
                        .collect(),
                    lang_sel,
                )));
            }
        }
        self.controls = out;
    }

    /// Switch category and refresh widgets. No effect on dirty state.
    pub fn set_category(&mut self, c: Category) {
        if c != self.active_category {
            self.active_category = c;
            self.rebuild_controls();
        }
    }

    /// True if the user has edited anything since the last apply.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Find the control under (x, y).
    pub fn hit_test(&self, x: f32, y: f32) -> Option<WidgetId> {
        self.controls.iter().find(|c| c.hit_test(x, y)).map(|c| c.id())
    }

    /// Toggle the boolean of a [`Toggle`] widget. Returns the new value.
    pub fn flip_toggle(&mut self, id: WidgetId) -> Option<bool> {
        let v = match self.find_mut(id)? {
            Control::Toggle(t) => Some(t.toggle()),
            _ => None,
        }?;
        self.commit_widget_to_config(id);
        Some(v)
    }

    /// Drag a slider thumb to the given x coordinate (in window pixels).
    pub fn drag_slider(&mut self, id: WidgetId, x: f32) -> Option<f32> {
        let v = match self.find_mut(id)? {
            Control::Slider(s) => Some(s.drag_to(x)),
            _ => None,
        }?;
        self.commit_widget_to_config(id);
        Some(v)
    }

    /// Open/close a dropdown's option list.
    pub fn toggle_dropdown(&mut self, id: WidgetId) -> Option<bool> {
        match self.find_mut(id)? {
            Control::Dropdown(d) => {
                d.toggle_open();
                Some(d.open)
            }
            _ => None,
        }
    }

    /// Close any open dropdown whose header AND popover do not contain
    /// the click `(x, y)`. Returns `true` when at least one popover was
    /// closed — the caller (host) can then `request_redraw`. Added in
    /// issue #173 slice-2b so the prefs window's mouse handler can
    /// dismiss combobox popovers on any click outside them, including
    /// clicks that miss every widget entirely (the previous code only
    /// closed the dropdown when the user clicked the header again,
    /// trapping the popover open if they clicked anywhere else).
    pub fn close_dropdowns_outside_click(&mut self, x: f32, y: f32) -> bool {
        let mut any_closed = false;
        for ctrl in self.controls.iter_mut() {
            if let Control::Dropdown(d) = ctrl {
                if !d.open {
                    continue;
                }
                let inside_header = d.rect.contains(x, y);
                let inside_option = d.hit_option(x, y).is_some();
                if !inside_header && !inside_option {
                    d.close();
                    any_closed = true;
                }
            }
        }
        any_closed
    }

    /// Select a dropdown option by index.
    pub fn select_dropdown(&mut self, id: WidgetId, idx: usize) -> Option<bool> {
        let ok = match self.find_mut(id)? {
            Control::Dropdown(d) => Some(d.select(idx)),
            _ => None,
        }?;
        if ok {
            self.commit_widget_to_config(id);
        }
        Some(ok)
    }

    /// Pick a palette cell on a color swatch.
    pub fn pick_color(&mut self, id: WidgetId, cell: usize) -> Option<bool> {
        let ok = match self.find_mut(id)? {
            Control::ColorSwatch(c) => Some(c.pick(cell)),
            _ => None,
        }?;
        if ok {
            // Color swatches do not currently map to a Config field, but
            // we still mark dirty to signal the user picked something.
            // (Mapping lives in commit_widget_to_config's future expansion.)
            self.dirty = true;
        }
        Some(ok)
    }

    /// Set `id` as the keyboard-focused TextField; blurs all others.
    /// Returns true if `id` resolved to a TextField.
    pub fn focus_text_field(&mut self, id: WidgetId) -> bool {
        let mut found = false;
        for c in self.controls.iter_mut() {
            if let Control::TextField(tf) = c {
                if tf.id == id {
                    tf.focus();
                    found = true;
                } else {
                    tf.blur();
                }
            }
        }
        if found {
            self.focused_field = Some(id);
        }
        found
    }

    /// Drop keyboard focus from any TextField.
    pub fn blur_text_fields(&mut self) {
        for c in self.controls.iter_mut() {
            if let Control::TextField(tf) = c {
                tf.blur();
            }
        }
        self.focused_field = None;
    }

    /// Type a character into the currently-focused TextField (if any).
    /// No-op when nothing is focused or the field is at `max_len`.
    /// Returns true if the character was appended.
    pub fn type_into_focused(&mut self, ch: char) -> bool {
        let Some(id) = self.focused_field else { return false };
        let appended = match self.find_mut(id) {
            Some(Control::TextField(tf)) => {
                let before = tf.value.chars().count();
                tf.push_char(ch);
                tf.value.chars().count() != before
            }
            _ => false,
        };
        if appended {
            self.commit_widget_to_config(id);
        }
        appended
    }

    /// Clear completed toggle thumb animations after the renderer has emitted
    /// the final snapped frame for them.
    pub fn clear_completed_toggle_anims(&mut self, now: std::time::Instant) {
        for c in &mut self.controls {
            if let Control::Toggle(t) = c {
                let (_, done) = t.knob_x_animated_with_done(
                    now,
                    super::layout::TOGGLE_KNOB,
                    super::layout::TOGGLE_KNOB_MARGIN,
                    self.config.accessibility.reduced_motion,
                );
                t.clear_anim_if_done(done);
            }
        }
    }

    /// Classify a pointer click into a [`PrefsHit`] using the same
    /// priority order the host should honor: chrome → sidebar → open
    /// dropdown options → widgets.
    pub fn classify_click(&self, x: f32, y: f32) -> Option<PrefsHit> {
        if self.hit_apply(x, y) {
            return Some(PrefsHit::Apply);
        }
        if self.hit_cancel(x, y) {
            return Some(PrefsHit::Cancel);
        }
        if self.hit_reset(x, y) {
            return Some(PrefsHit::ResetSection);
        }
        if let Some(cat) = self.hit_sidebar(x, y) {
            return Some(PrefsHit::Sidebar(cat));
        }
        // Open-dropdown option rows take precedence — they may overlap
        // controls drawn beneath them.
        for c in &self.controls {
            if let Control::Dropdown(d) = c {
                if let Some(idx) = d.hit_option(x, y) {
                    return Some(PrefsHit::DropdownOption { id: d.id, index: idx });
                }
            }
        }
        for c in &self.controls {
            match c {
                Control::Toggle(t) if t.hit_test(x, y) => return Some(PrefsHit::Toggle(t.id)),
                Control::Slider(s) if s.hit_test(x, y) => return Some(PrefsHit::SliderTrack(s.id)),
                Control::Dropdown(d) if d.rect.contains(x, y) => {
                    return Some(PrefsHit::DropdownHeader(d.id))
                }
                Control::ColorSwatch(cs) => {
                    if let Some(idx) = cs.hit_cell(x, y) {
                        return Some(PrefsHit::ColorCell { id: cs.id, index: idx });
                    }
                    if cs.rect.contains(x, y) {
                        return Some(PrefsHit::ColorCell { id: cs.id, index: 0 });
                    }
                }
                Control::TextField(tf) if tf.hit_test(x, y) => {
                    return Some(PrefsHit::TextField(tf.id))
                }
                Control::Button(b) if b.hit_test(x, y) => return Some(PrefsHit::Button(b.id)),
                _ => {}
            }
        }
        None
    }

    fn find_mut(&mut self, id: WidgetId) -> Option<&mut Control> {
        self.controls.iter_mut().find(|c| c.id() == id)
    }

    /// Push a typed character into the TextField identified by `id`.
    /// No-op when the field is at `max_len`; only marks dirty when the
    /// config actually changed.
    pub fn type_into(&mut self, id: WidgetId, ch: char) -> Option<()> {
        let appended = match self.find_mut(id)? {
            Control::TextField(tf) => {
                let before = tf.value.chars().count();
                tf.push_char(ch);
                tf.value.chars().count() != before
            }
            _ => return None,
        };
        if appended {
            self.commit_widget_to_config(id);
        }
        Some(())
    }

    /// Push the value of the widget identified by `id` back into the
    /// [`Config`] and mark dirty *only if* the config actually changed.
    fn commit_widget_to_config(&mut self, id: WidgetId) {
        // Capture a comparable snapshot of config BEFORE the write.
        let before = self.config.to_toml().unwrap_or_default();
        let Some(ctrl) = self.controls.iter().find(|c| c.id() == id) else { return };
        // Map widget → field by (category, position). Index-based to
        // avoid carrying string keys around.
        let pos = self.controls.iter().position(|c| c.id() == id).unwrap_or(0);
        let mut relocalize = false;
        match (self.active_category, pos, ctrl) {
            (Category::Advanced, 0, Control::TextField(t)) => {
                let v = t.get().to_string();
                self.config.terminal.shell = if v.is_empty() { None } else { Some(v) };
            }
            (Category::Advanced, 1, Control::Slider(s)) => {
                self.config.terminal.scrollback = s.get() as usize;
            }
            (Category::Theme, 0, Control::Dropdown(d)) => {
                if let Some(v) = d.value() {
                    self.config.theme = v.to_string();
                }
            }
            (Category::Window, 0, Control::Slider(s)) => {
                self.config.window.opacity = s.get();
            }
            (Category::Window, 1, Control::Toggle(t)) => {
                self.config.window.blur = t.get();
            }
            (Category::Advanced, 2, Control::Dropdown(d)) => {
                let idx = d.get();
                if let Some((tag, _)) = LANGUAGE_OPTIONS.get(idx) {
                    let new_tag = (*tag).to_string();
                    if new_tag != self.config.locale {
                        self.config.locale = new_tag.clone();
                        // Live-apply: rebuild the bundle so the very
                        // next rebuild_controls() renders labels in
                        // the new language.
                        self.i18n = crate::i18n::I18n::new(if new_tag.is_empty() {
                            None
                        } else {
                            Some(new_tag.as_str())
                        });
                        // Defer the rebuild until after the match arm
                        // releases its borrow of `self.controls`.
                        relocalize = true;
                    }
                }
            }
            (Category::Font, 0, Control::Dropdown(d)) => {
                if let Some(v) = d.value() {
                    self.config.font.family = v.to_string();
                }
            }
            (Category::Font, 1, Control::Slider(s)) => {
                self.config.font.size = s.get();
            }
            (Category::Font, 2, Control::Slider(s)) => {
                self.config.font.line_height = s.get();
            }
            (Category::Window, 2, Control::Toggle(t)) => {
                self.config.window.decorations = t.get();
            }
            (Category::Window, 3, Control::Slider(s)) => {
                // Single "Padding" slider in the prefs UI drives all four
                // per-side values (matching how WezTerm's prefs surface
                // exposes one knob with sensible symmetric defaults).
                let v = s.get();
                self.config.window.padding_left = v;
                self.config.window.padding_right = v;
                self.config.window.padding_top = v;
                self.config.window.padding_bottom = v;
            }
            (Category::Cursor, 0, Control::Dropdown(d)) => {
                if let Some(v) = d.value() {
                    if let Some(shape) = CursorShape::from_str_ci(v) {
                        self.config.terminal.cursor_shape = shape;
                    }
                }
            }
            (Category::Cursor, 1, Control::Toggle(t)) => {
                self.config.terminal.cursor_blink = t.get();
            }
            _ => {}
        }
        if relocalize {
            // Rebuild the control list now so the prefs UI re-reads
            // every label through the new i18n bundle on the very next
            // frame, instead of waiting until the user closes and
            // re-opens prefs.
            self.rebuild_controls();
        }
        let after = self.config.to_toml().unwrap_or_default();
        if before != after {
            self.dirty = true;
        }
    }

    /// Persist the edit buffer to disk. Atomic via [`Config::save`].
    /// Resets dirty + snapshot on success.
    pub fn apply(&mut self) -> Result<()> {
        self.config.save(&self.config_path)?;
        self.original = self.config.clone();
        self.dirty = false;
        Ok(())
    }

    /// Discard edits since open by restoring the snapshot.
    pub fn cancel(&mut self) {
        self.config = self.original.clone();
        self.dirty = false;
        self.rebuild_controls();
    }

    /// Hit-test buttons in the footer.
    pub fn hit_apply(&self, x: f32, y: f32) -> bool {
        let r = self.layout.apply_button;
        Rect::new(r.x, r.y, r.w, r.h).contains(x, y)
    }

    pub fn hit_cancel(&self, x: f32, y: f32) -> bool {
        let r = self.layout.cancel_button;
        Rect::new(r.x, r.y, r.w, r.h).contains(x, y)
    }

    pub fn hit_reset(&self, x: f32, y: f32) -> bool {
        self.reset_button.hit_test(x, y)
    }

    pub fn reset_active_section_to_default(&mut self) {
        if self.active_category == Category::Keymap {
            return;
        }
        let before = self.config.to_toml().unwrap_or_default();
        let defaults = Config::default();
        match self.active_category {
            Category::Font => self.config.font = defaults.font,
            Category::Theme => self.config.theme = defaults.theme,
            Category::Keymap => self.config.keymap = defaults.keymap,
            Category::Window => self.config.window = defaults.window,
            Category::Cursor => {
                self.config.terminal.cursor_shape = defaults.terminal.cursor_shape;
                self.config.terminal.cursor_blink = defaults.terminal.cursor_blink;
            }
            Category::Advanced => {
                self.config.terminal.shell = defaults.terminal.shell;
                self.config.terminal.scrollback = defaults.terminal.scrollback;
                self.config.locale = defaults.locale;
                self.i18n = crate::i18n::I18n::new(None);
                self.config.logging = defaults.logging;
                self.config.accessibility = defaults.accessibility;
                self.config.tab_close_button_color = defaults.tab_close_button_color;
            }
        }
        self.rebuild_controls();
        let after = self.config.to_toml().unwrap_or_default();
        if before != after {
            self.dirty = true;
        }
    }

    /// Hit-test the sidebar; returns the clicked category, if any.
    pub fn hit_sidebar(&self, x: f32, y: f32) -> Option<Category> {
        self.layout.hit_category(x, y)
    }

    /// Helper used by the renderer's live-preview pane: fixed example
    /// content rendered with the currently-selected theme.
    pub fn preview_lines(&self) -> Vec<String> {
        vec![
            format!("user@sonic ~ $ ls -1"),
            "Cargo.toml".to_string(),
            "README.md".to_string(),
            "src/".to_string(),
            "user@sonic ~ $ ".to_string(),
        ]
    }
}
