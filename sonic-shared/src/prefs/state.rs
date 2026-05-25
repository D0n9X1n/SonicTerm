//! Edit-buffer + persistence for the preferences window.
//!
//! [`PrefsState`] wraps a mutable copy of the user [`Config`] alongside
//! the form's [`Control`] widgets. Mutations go through helper methods
//! that mark the buffer dirty; [`apply`] serializes back to TOML on disk
//! and clears the flag, [`cancel`] discards changes by restoring the
//! original snapshot.

use std::path::PathBuf;

use anyhow::{Context, Result};
use sonic_core::config::Config;

use super::controls::{ColorSwatch, Control, Dropdown, Rect, Slider, TextField, Toggle, WidgetId};
use super::layout::{Category, PrefsLayout};

/// Pre-canned theme list shown in the Appearance picker. Matches the
/// themes bundled under `assets/themes/`.
pub const KNOWN_THEMES: &[&str] = &["tokyo-night", "dracula", "solarized-dark", "solarized-light"];

/// Pre-canned keymaps shown in the Keymap picker.
pub const KNOWN_KEYMAPS: &[&str] = &["wezterm"];

/// Pre-canned monospace families. Free-form text still wins for advanced
/// users; this is just a quick picker.
pub const KNOWN_FONTS: &[&str] =
    &["JetBrainsMono Nerd Font", "Fira Code", "Menlo", "Cascadia Code", "Source Code Pro"];

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
}

impl PrefsState {
    /// Build a fresh edit buffer over `config` saving to `config_path`.
    pub fn new(config: Config, config_path: PathBuf) -> Self {
        let layout = PrefsLayout::default_size();
        let mut s = Self {
            original: config.clone(),
            config,
            active_category: Category::General,
            dirty: false,
            config_path,
            controls: Vec::new(),
            layout,
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
                    "Theme",
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
            }
            Category::Font => {
                let sel =
                    KNOWN_FONTS.iter().position(|f| *f == self.config.font.family).unwrap_or(0);
                out.push(Control::Dropdown(Dropdown::new(
                    next_id(),
                    "Family",
                    l.control_slot(0),
                    KNOWN_FONTS.iter().map(|s| (*s).to_string()).collect(),
                    sel,
                )));
                out.push(Control::Slider({
                    let mut s = Slider::new(
                        next_id(),
                        "Size",
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
                    "Cursor blink",
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
                        self.config.window.padding,
                    );
                    s = s.with_step(1.0);
                    s
                }));
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
        match self.find_mut(id)? {
            Control::ColorSwatch(c) => Some(c.pick(cell)),
            _ => None,
        }
    }

    /// Push a typed character into a focused text field.
    pub fn type_into(&mut self, id: WidgetId, ch: char) -> Option<()> {
        match self.find_mut(id)? {
            Control::TextField(f) => {
                f.push_char(ch);
                Some(())
            }
            _ => None,
        }?;
        self.commit_widget_to_config(id);
        Some(())
    }

    fn find_mut(&mut self, id: WidgetId) -> Option<&mut Control> {
        self.controls.iter_mut().find(|c| c.id() == id)
    }

    /// Push the value of the widget identified by `id` back into the
    /// [`Config`] and mark dirty.
    fn commit_widget_to_config(&mut self, id: WidgetId) {
        let Some(ctrl) = self.controls.iter().find(|c| c.id() == id) else { return };
        // Map widget → field by (category, position). Index-based to
        // avoid carrying string keys around.
        let pos = self.controls.iter().position(|c| c.id() == id).unwrap_or(0);
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
                self.config.window.padding = s.get();
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Persist the edit buffer to disk. Resets dirty + snapshot.
    pub fn apply(&mut self) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {parent:?}"))?;
        }
        let toml = self.config.to_toml()?;
        std::fs::write(&self.config_path, toml)
            .with_context(|| format!("write {:?}", self.config_path))?;
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
        // Appearance has 4 controls (theme, opacity, blur, accent).
        assert_eq!(s.controls.len(), 4);
        assert_ne!(s.controls.len(), n0 + 99); // sanity
                                               // Setting the same category again is a no-op.
        s.set_category(Category::Appearance);
        assert_eq!(s.controls.len(), 4);
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
}
