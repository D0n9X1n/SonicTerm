//! UI message translation and live locale switching.

use super::*;

impl App {
    /// Translate a UI message id. See [`sonicterm_ui::i18n::I18n::translate`].
    /// Returns the key itself if no bundle (active or English fallback) has
    /// it, so the UI never renders an empty label.
    pub fn translate(&self, key: &str) -> String {
        self.i18n.translate(key)
    }

    /// Translate with `{ $name }` arguments. See
    /// [`sonicterm_ui::i18n::I18n::t_args`].
    pub fn t_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.i18n.t_args(key, Some(args))
    }

    /// Currently active locale tag (e.g. `"en"`, `"zh-CN"`).
    pub fn locale(&self) -> String {
        self.i18n.locale()
    }

    /// Live-apply a new locale. Persists the choice to `self.config.locale`.
    /// Pass `""` to mean "auto-detect from OS locale".
    pub fn set_locale(&mut self, requested: &str) {
        self.palette_pointer_capture = None;
        self.config.locale = requested.to_string();
        self.i18n = sonicterm_ui::i18n::I18n::new(if requested.is_empty() {
            None
        } else {
            // When: `requested` names a locale tag, so it selects the bundle
            // directly instead of leaving the OS default to decide.
            Some(requested)
        });
        self.command_palette.set_locale(&self.i18n);
        if self.command_palette.is_open() {
            // Locale changes do not advance grid revisions, so wake the window hosting the palette.
            self.request_redraw_for_overlay(self.palette_attached_window);
        }
    }
}
