//! Edit-buffer + persistence for the preferences window.
//!
//! [`PrefsState`] wraps a mutable copy of the user [`Config`] alongside
//! the form's [`Control`] widgets. Mutations go through helper methods
//! that mark the buffer dirty; [`apply`] serializes back to TOML on disk
//! and clears the flag, [`cancel`] discards changes by restoring the
//! original snapshot.

use std::path::PathBuf;

use anyhow::Result;
use sonic_core::config::{Config, CursorShape};

use super::controls::{ColorSwatch, Control, Dropdown, Rect, Slider, TextField, Toggle, WidgetId};
use super::layout::{Category, PrefsLayout};

/// Classified result of a pointer click inside the preferences window.
/// Returned by [`PrefsState::classify_click`] so the host (app.rs) can
/// dispatch without re-implementing the priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefsHit {
    Apply,
    Cancel,
    Sidebar(Category),
    Toggle(WidgetId),
    SliderTrack(WidgetId),
    DropdownHeader(WidgetId),
    DropdownOption { id: WidgetId, index: usize },
    ColorCell { id: WidgetId, index: usize },
    TextField(WidgetId),
}

/// Pre-canned theme list shown in the Appearance picker. Matches the
/// themes bundled under `assets/themes/`.
pub const KNOWN_THEMES: &[&str] =
    &["wezterm", "gruvbox-dark-hard", "tokyo-night", "dracula", "nord", "catppuccin-mocha"];

/// Pre-canned keymaps shown in the Keymap picker.
pub const KNOWN_KEYMAPS: &[&str] = &["wezterm"];

/// Cursor shape options shown in the Behavior picker.
pub const KNOWN_CURSOR_SHAPES: &[&str] = &["block", "bar", "underline"];

/// Pre-canned monospace families. Free-form text still wins for advanced
/// users; this is just a quick picker.
pub const KNOWN_FONTS: &[&str] =
    &["JetBrainsMono Nerd Font", "Fira Code", "Menlo", "Cascadia Code", "Source Code Pro"];

/// (tag, native display label) pairs for the Language dropdown. The empty
/// tag means "auto-detect from OS locale". The native script for the
/// non-default rows is chosen so the user can identify the option without
/// already speaking the current UI language.
pub const LANGUAGE_OPTIONS: &[(&str, &str)] =
    &[("", "Auto"), ("en", "English"), ("zh-CN", "中文"), ("ja", "日本語")];

/// In-memory edit state for the preferences window.
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
}

impl PrefsState {
    /// Build a fresh edit buffer over `config` saving to `config_path`.
    pub fn new(config: Config, config_path: PathBuf) -> Self {
        let layout = PrefsLayout::default_size();
        let i18n = crate::i18n::I18n::new(if config.locale.is_empty() {
            None
        } else {
            Some(config.locale.as_str())
        });
        let mut s = Self {
            original: config.clone(),
            config,
            active_category: Category::General,
            dirty: false,
            config_path,
            controls: Vec::new(),
            layout,
            focused_field: None,
            i18n,
        };
        s.rebuild_controls();
        s
    }

    /// Rebuild the [`Control`] list for the active category. Called any
    /// time the category changes or controls are mutated.
    pub fn rebuild_controls(&mut self) {
        let mut id: u32 = 1;
        let mut next_id = || {
            let i = id;
            id += 1;
            WidgetId(i)
        };
        let mut out: Vec<Control> = Vec::new();
        let l = self.layout;
        match self.active_category {
            Category::General => {
                out.push(Control::TextField(TextField::new(
                    next_id(),
                    "Shell",
                    l.control_slot(0),
                    self.config.terminal.shell.clone().unwrap_or_default(),
                )));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        "Scrollback",
                        l.control_slot(1),
                        1_000.0,
                        100_000.0,
                        self.config.terminal.scrollback as f32,
                    );
                    s = s.with_step(1_000.0);
                    s
                }));
                out.push(Control::Toggle(Toggle::new(
                    next_id(),
                    "Window decorations",
                    l.control_slot(2),
                    self.config.window.decorations,
                )));
            }
            Category::Appearance => {
                let sel = KNOWN_THEMES.iter().position(|t| *t == self.config.theme).unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    self.i18n.t("prefs-theme"),
                    l.control_slot(0),
                    KNOWN_THEMES.iter().map(|s| (*s).to_string()).collect(),
                    sel,
                )));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        "Opacity",
                        l.control_slot(1),
                        0.3,
                        1.0,
                        self.config.window.opacity,
                    );
                    s = s.with_step(0.05);
                    s
                }));
                out.push(Control::Toggle(Toggle::new(
                    next_id(),
                    "Background blur",
                    l.control_slot(2),
                    self.config.window.blur,
                )));
                out.push(Control::ColorSwatch(ColorSwatch::new(
                    next_id(),
                    "Accent",
                    l.control_slot(3),
                    [0x7a, 0xa2, 0xf7, 0xff],
                )));
                // Language picker: "" = auto, then the three shipped
                // locales in their native script. Order matches
                // `LANGUAGE_OPTIONS` so a dropdown selection maps back
                // by index in `commit_widget_to_config`.
                let lang_sel = LANGUAGE_OPTIONS
                    .iter()
                    .position(|(tag, _)| *tag == self.config.locale.as_str())
                    .unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    self.i18n.t("prefs-language"),
                    l.control_slot(4),
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
                        "Line height",
                        l.control_slot(2),
                        1.0,
                        2.0,
                        self.config.font.line_height,
                    );
                    s = s.with_step(0.05);
                    s
                }));
            }
            Category::Keymap => {
                let sel = KNOWN_KEYMAPS.iter().position(|k| *k == self.config.keymap).unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    "Keymap",
                    l.control_slot(0),
                    KNOWN_KEYMAPS.iter().map(|s| (*s).to_string()).collect(),
                    sel,
                )));
            }
            Category::Behavior => {
                out.push(Control::Toggle(Toggle::new(
                    next_id(),
                    self.i18n.t("prefs-cursor-blink"),
                    l.control_slot(0),
                    self.config.terminal.cursor_blink,
                )));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        "Padding",
                        l.control_slot(1),
                        0.0,
                        32.0,
                        self.config.window.padding_left,
                    );
                    s = s.with_step(1.0);
                    s
                }));
                let cur_shape = self.config.terminal.cursor_shape.as_str();
                let sel = KNOWN_CURSOR_SHAPES.iter().position(|s| *s == cur_shape).unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    "Cursor shape",
                    l.control_slot(2),
                    KNOWN_CURSOR_SHAPES.iter().map(|s| (*s).to_string()).collect(),
                    sel,
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
            (Category::General, 0, Control::TextField(t)) => {
                let v = t.get().to_string();
                self.config.terminal.shell = if v.is_empty() { None } else { Some(v) };
            }
            (Category::General, 1, Control::Slider(s)) => {
                self.config.terminal.scrollback = s.get() as usize;
            }
            (Category::General, 2, Control::Toggle(t)) => {
                self.config.window.decorations = t.get();
            }
            (Category::Appearance, 0, Control::Dropdown(d)) => {
                if let Some(v) = d.value() {
                    self.config.theme = v.to_string();
                }
            }
            (Category::Appearance, 1, Control::Slider(s)) => {
                self.config.window.opacity = s.get();
            }
            (Category::Appearance, 2, Control::Toggle(t)) => {
                self.config.window.blur = t.get();
            }
            (Category::Appearance, 4, Control::Dropdown(d)) => {
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
            (Category::Keymap, 0, Control::Dropdown(d)) => {
                if let Some(v) = d.value() {
                    self.config.keymap = v.to_string();
                }
            }
            (Category::Behavior, 0, Control::Toggle(t)) => {
                self.config.terminal.cursor_blink = t.get();
            }
            (Category::Behavior, 1, Control::Slider(s)) => {
                // Single "Padding" slider in the prefs UI drives all four
                // per-side values (matching how WezTerm's prefs surface
                // exposes one knob with sensible symmetric defaults).
                let v = s.get();
                self.config.window.padding_left = v;
                self.config.window.padding_right = v;
                self.config.window.padding_top = v;
                self.config.window.padding_bottom = v;
            }
            (Category::Behavior, 2, Control::Dropdown(d)) => {
                if let Some(v) = d.value() {
                    if let Some(shape) = CursorShape::from_str_ci(v) {
                        self.config.terminal.cursor_shape = shape;
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh() -> (PrefsState, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sonic.toml");
        let state = PrefsState::new(Config::default(), path);
        (state, dir)
    }

    #[test]
    fn new_state_is_clean_and_has_general_controls() {
        let (s, _d) = fresh();
        assert!(!s.is_dirty());
        assert_eq!(s.active_category, Category::General);
        assert!(!s.controls.is_empty());
    }

    #[test]
    fn switching_category_rebuilds_controls() {
        let (mut s, _d) = fresh();
        let n0 = s.controls.len();
        s.set_category(Category::Appearance);
        // Appearance has 5 controls (theme, opacity, blur, accent, language).
        assert_eq!(s.controls.len(), 5);
        assert_ne!(s.controls.len(), n0 + 99); // sanity
                                               // Setting the same category again is a no-op.
        s.set_category(Category::Appearance);
        assert_eq!(s.controls.len(), 5);
    }

    #[test]
    fn flip_toggle_dirties_and_writes_through() {
        let (mut s, _d) = fresh();
        let toggle_id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Toggle(t) => Some(t.id),
                _ => None,
            })
            .unwrap();
        let before = s.config.window.decorations;
        s.flip_toggle(toggle_id);
        assert_ne!(s.config.window.decorations, before);
        assert!(s.is_dirty());
    }

    #[test]
    fn drag_slider_updates_config_value() {
        let (mut s, _d) = fresh();
        // General's slider is "Scrollback".
        let (id, rect_x, rect_w) = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Slider(sl) => Some((sl.id, sl.rect.x, sl.rect.w)),
                _ => None,
            })
            .unwrap();
        s.drag_slider(id, rect_x + rect_w); // max
        assert_eq!(s.config.terminal.scrollback, 100_000);
        assert!(s.is_dirty());
    }

    #[test]
    fn select_dropdown_updates_string_field() {
        let (mut s, _d) = fresh();
        s.set_category(Category::Appearance);
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) => Some(d.id),
                _ => None,
            })
            .unwrap();
        s.select_dropdown(id, 1);
        assert_eq!(s.config.theme, KNOWN_THEMES[1]);
        assert!(s.is_dirty());
    }

    #[test]
    fn cancel_restores_snapshot_and_clears_dirty() {
        let (mut s, _d) = fresh();
        s.set_category(Category::Appearance);
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) => Some(d.id),
                _ => None,
            })
            .unwrap();
        let before = s.config.theme.clone();
        s.select_dropdown(id, 1);
        assert_ne!(s.config.theme, before);
        s.cancel();
        assert_eq!(s.config.theme, before);
        assert!(!s.is_dirty());
    }

    #[test]
    fn apply_writes_toml_and_clears_dirty() {
        let (mut s, _d) = fresh();
        let tid = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Toggle(t) => Some(t.id),
                _ => None,
            })
            .unwrap();
        s.flip_toggle(tid);
        assert!(s.is_dirty());
        s.apply().unwrap();
        assert!(!s.is_dirty());
        let text = std::fs::read_to_string(&s.config_path).unwrap();
        assert!(text.contains("[window]"));
        // Snapshot now matches; cancel is a no-op.
        let v = s.config.window.decorations;
        s.cancel();
        assert_eq!(s.config.window.decorations, v);
    }

    #[test]
    fn hit_apply_and_cancel_use_layout_rects() {
        let (s, _d) = fresh();
        let apply = s.layout.apply_button;
        let cancel = s.layout.cancel_button;
        assert!(s.hit_apply(apply.x + 1.0, apply.y + 1.0));
        assert!(!s.hit_apply(0.0, 0.0));
        assert!(s.hit_cancel(cancel.x + 1.0, cancel.y + 1.0));
        assert!(!s.hit_cancel(apply.x + 1.0, apply.y + 1.0));
    }

    #[test]
    fn hit_sidebar_returns_category() {
        let (s, _d) = fresh();
        let row = s.layout.category_row(1);
        assert_eq!(s.hit_sidebar(row.x + 1.0, row.y + 1.0), Some(Category::Appearance));
        assert_eq!(s.hit_sidebar(9999.0, 9999.0), None);
    }

    #[test]
    fn preview_lines_nonempty() {
        let (s, _d) = fresh();
        assert!(!s.preview_lines().is_empty());
    }

    #[test]
    fn hit_test_finds_widget_by_position() {
        let (s, _d) = fresh();
        let first = &s.controls[0];
        let r = match first {
            Control::Toggle(t) => t.rect,
            Control::Slider(sl) => sl.rect,
            Control::Dropdown(d) => d.rect,
            Control::ColorSwatch(c) => c.rect,
            Control::TextField(tf) => tf.rect,
        };
        assert_eq!(s.hit_test(r.x + 1.0, r.y + 1.0), Some(first.id()));
    }

    #[test]
    fn type_into_text_field_writes_through_to_shell() {
        let (mut s, _d) = fresh();
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::TextField(tf) => Some(tf.id),
                _ => None,
            })
            .unwrap();
        s.type_into(id, '/');
        s.type_into(id, 'b');
        s.type_into(id, 'i');
        s.type_into(id, 'n');
        s.type_into(id, '/');
        s.type_into(id, 'z');
        s.type_into(id, 's');
        s.type_into(id, 'h');
        assert_eq!(s.config.terminal.shell.as_deref(), Some("/bin/zsh"));
        assert!(s.is_dirty());
    }

    // ---- Bug 1: commit_widget_to_config should NOT dirty on no-op writes ----

    #[test]
    fn reselecting_current_dropdown_option_is_not_dirty() {
        let (mut s, _d) = fresh();
        s.set_category(Category::Appearance);
        let (id, current) = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) => Some((d.id, d.selected)),
                _ => None,
            })
            .unwrap();
        // Selecting the already-selected option must be a no-op.
        s.select_dropdown(id, current);
        assert!(!s.is_dirty(), "no-op dropdown reselect must not dirty");
    }

    #[test]
    fn dragging_slider_to_current_value_is_not_dirty() {
        let (mut s, _d) = fresh();
        // Find a slider and drag the thumb exactly to its current value
        // in pixel space.
        let (id, x_at_current) = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Slider(sl) => {
                    let frac = sl.fraction();
                    let x = sl.rect.x + frac * sl.rect.w;
                    Some((sl.id, x))
                }
                _ => None,
            })
            .unwrap();
        s.drag_slider(id, x_at_current);
        assert!(!s.is_dirty(), "no-op slider drag must not dirty");
    }

    #[test]
    fn typing_into_textfield_at_max_len_is_not_dirty() {
        let (mut s, _d) = fresh();
        // Force the shell text field to its maximum capacity.
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::TextField(tf) => Some(tf.id),
                _ => None,
            })
            .unwrap();
        let max_len = match s.controls.iter_mut().find(|c| c.id() == id).unwrap() {
            Control::TextField(tf) => {
                let m = tf.max_len;
                tf.value = "x".repeat(m);
                m
            }
            _ => unreachable!(),
        };
        // First commit to sync config and clear dirty.
        s.dirty = false;
        // Typing another char at max_len must be a true no-op.
        s.type_into(id, 'y');
        assert!(!s.is_dirty(), "type at max_len must not dirty");
        // Sanity: the field's length did not grow past max_len.
        if let Control::TextField(tf) = s.controls.iter().find(|c| c.id() == id).unwrap() {
            assert_eq!(tf.value.chars().count(), max_len);
        }
    }

    // ---- Bug 2: apply() must write atomically + create parent dirs ----

    #[test]
    fn apply_writes_atomically_to_nested_missing_dir() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a/b/c/sonic.toml");
        let mut s = PrefsState::new(Config::default(), nested.clone());
        // Make sure dirty so apply has work to do.
        let tid = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Toggle(t) => Some(t.id),
                _ => None,
            })
            .unwrap();
        s.flip_toggle(tid);
        assert!(s.is_dirty());
        s.apply().unwrap();
        assert!(nested.exists(), "config file must exist after apply");
        let text = std::fs::read_to_string(&nested).unwrap();
        assert!(text.contains("[window]"));
        // No leftover .tmp sibling.
        let mut tmp = nested.clone();
        tmp.set_file_name("sonic.toml.tmp");
        assert!(!tmp.exists(), ".tmp sibling must be renamed away");
    }

    // ---- Bug 3: classify_click + focused-field typing ----

    #[test]
    fn classify_click_resolves_dropdown_option_when_open() {
        let (mut s, _d) = fresh();
        s.set_category(Category::Appearance);
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) => Some(d.id),
                _ => None,
            })
            .unwrap();
        s.toggle_dropdown(id);
        // Pick a point that lands on the 2nd option row.
        let (rx, ry, rw, rh) = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) if d.id == id => {
                    Some((d.rect.x, d.rect.y, d.rect.w, d.rect.h))
                }
                _ => None,
            })
            .unwrap();
        let x = rx + rw / 2.0;
        let y = ry + rh + rh + rh / 2.0; // 2nd row
        let hit = s.classify_click(x, y);
        assert!(matches!(hit, Some(PrefsHit::DropdownOption { .. })));
        if let Some(PrefsHit::DropdownOption { id: hid, index }) = hit {
            assert_eq!(hid, id);
            s.select_dropdown(hid, index);
            assert_eq!(s.config.theme, KNOWN_THEMES[index]);
        }
    }

    #[test]
    fn classify_click_resolves_color_swatch_cell() {
        let (mut s, _d) = fresh();
        s.set_category(Category::Appearance);
        let (id, top, left) = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::ColorSwatch(cs) => Some((cs.id, cs.rect.y + cs.rect.h + 4.0, cs.rect.x)),
                _ => None,
            })
            .unwrap();
        // Click cell index 1 (col=1, row=0).
        let x = left + ColorSwatch::CELL * 1.5;
        let y = top + ColorSwatch::CELL * 0.5;
        let hit = s.classify_click(x, y);
        match hit {
            Some(PrefsHit::ColorCell { id: hid, index }) => {
                assert_eq!(hid, id);
                assert_eq!(index, 1);
                assert!(s.pick_color(hid, index).unwrap());
                assert!(s.is_dirty());
            }
            other => panic!("expected ColorCell, got {other:?}"),
        }
    }

    #[test]
    fn focus_text_field_then_type_writes_through() {
        let (mut s, _d) = fresh();
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::TextField(tf) => Some(tf.id),
                _ => None,
            })
            .unwrap();
        assert!(s.focus_text_field(id));
        assert_eq!(s.focused_field, Some(id));
        // Verify the TextField reports focused.
        let focused = matches!(
            s.controls.iter().find(|c| c.id() == id).unwrap(),
            Control::TextField(tf) if tf.focused
        );
        assert!(focused);
        assert!(s.type_into_focused('z'));
        assert!(s.type_into_focused('s'));
        assert!(s.type_into_focused('h'));
        assert!(s.config.terminal.shell.as_deref().unwrap().ends_with("zsh"));
        assert!(s.is_dirty());
        // type_into_focused with no focus is a no-op.
        s.blur_text_fields();
        assert!(!s.type_into_focused('x'));
    }

    // ---- Persistence wiring: every visible control reaches disk ----

    #[test]
    fn toggle_blink_then_apply_writes_blink_false_to_disk() {
        let (mut s, _d) = fresh();
        s.set_category(Category::Behavior);
        // First control in Behavior is the cursor-blink toggle.
        let id = match &s.controls[0] {
            Control::Toggle(t) => t.id,
            other => panic!("expected Toggle as first Behavior control, got {other:?}"),
        };
        assert!(s.config.terminal.cursor_blink);
        s.flip_toggle(id);
        assert!(!s.config.terminal.cursor_blink);
        s.apply().unwrap();
        let text = std::fs::read_to_string(&s.config_path).unwrap();
        assert!(text.contains("cursor_blink = false"), "missing blink=false in {text}");
    }

    #[test]
    fn select_cursor_shape_then_apply_writes_to_disk() {
        let (mut s, _d) = fresh();
        s.set_category(Category::Behavior);
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) => Some(d.id),
                _ => None,
            })
            .expect("Behavior must expose a cursor-shape dropdown");
        // Select "bar".
        let bar_idx = KNOWN_CURSOR_SHAPES.iter().position(|s| *s == "bar").unwrap();
        s.select_dropdown(id, bar_idx);
        assert_eq!(s.config.terminal.cursor_shape, CursorShape::Bar);
        s.apply().unwrap();
        let text = std::fs::read_to_string(&s.config_path).unwrap();
        assert!(text.contains("cursor_shape = \"bar\""), "missing bar in {text}");
    }

    #[test]
    fn theme_keymap_font_opacity_scrollback_all_reach_disk() {
        let (mut s, _d) = fresh();
        // Theme
        s.set_category(Category::Appearance);
        let id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) => Some(d.id),
                _ => None,
            })
            .unwrap();
        let tn = KNOWN_THEMES.iter().position(|t| *t == "tokyo-night").unwrap();
        s.select_dropdown(id, tn);
        // Opacity slider — slot 1.
        let op_id = match &s.controls[1] {
            Control::Slider(sl) => sl.id,
            other => panic!("expected slider, got {other:?}"),
        };
        // Drag to min so we get a deterministic value below default 1.0.
        let (rect_x, _rect_w) = match &s.controls[1] {
            Control::Slider(sl) => (sl.rect.x, sl.rect.w),
            _ => unreachable!(),
        };
        s.drag_slider(op_id, rect_x); // min => 0.3
        assert!((s.config.window.opacity - 0.3).abs() < 1e-3);
        // Font family + size.
        s.set_category(Category::Font);
        let font_id = s
            .controls
            .iter()
            .find_map(|c| match c {
                Control::Dropdown(d) => Some(d.id),
                _ => None,
            })
            .unwrap();
        s.select_dropdown(font_id, 1); // "Fira Code"
        let size_id = match &s.controls[1] {
            Control::Slider(sl) => sl.id,
            _ => unreachable!(),
        };
        let (rx, rw) = match &s.controls[1] {
            Control::Slider(sl) => (sl.rect.x, sl.rect.w),
            _ => unreachable!(),
        };
        s.drag_slider(size_id, rx + rw); // max => 32.0
                                         // Scrollback via General slot 1.
        s.set_category(Category::General);
        let (sb_id, sx, sw) = match &s.controls[1] {
            Control::Slider(sl) => (sl.id, sl.rect.x, sl.rect.w),
            _ => unreachable!(),
        };
        s.drag_slider(sb_id, sx + sw); // max => 100_000
                                       // Keymap (only one option today, so just verify it round-trips).
        s.set_category(Category::Keymap);
        s.apply().unwrap();
        let cfg = Config::load_or_default(&s.config_path).unwrap();
        assert_eq!(cfg.theme, "tokyo-night");
        assert_eq!(cfg.keymap, "wezterm");
        assert_eq!(cfg.font.family, KNOWN_FONTS[1]);
        assert!((cfg.font.size - 32.0).abs() < 1e-3);
        assert!((cfg.window.opacity - 0.3).abs() < 1e-3);
        assert_eq!(cfg.terminal.scrollback, 100_000);
    }

    #[test]
    fn apply_uses_config_save_atomic_no_tmp_left_behind() {
        let (mut s, _d) = fresh();
        s.dirty = true;
        s.apply().unwrap();
        let mut tmp = s.config_path.clone();
        tmp.set_file_name("sonic.toml.tmp");
        assert!(!tmp.exists());
        assert!(s.config_path.exists());
    }
}
