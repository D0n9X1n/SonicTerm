//! Cached palette phrases and row presentation, resolved only on UI refresh.

use super::*;

#[derive(Debug, Clone, Hash)]
pub(super) struct CommandPresentation {
    pub(super) label: String,
    pub(super) search: String,
    pub(super) hint: Option<String>,
}

// Resolve Fluent once; insert literal values into cached phrase slots during layout.
pub(super) const TEXT_SLOT: &str = "\u{fdd0}";

#[derive(Debug, Clone, Hash)]
pub(super) struct TextSlot {
    before: String,
    after: String,
}

impl TextSlot {
    pub(super) fn new(message: String) -> Self {
        let (before, after) = message
            .split_once(TEXT_SLOT)
            .expect("embedded palette phrase retains its argument slot");
        Self { before: before.to_string(), after: after.to_string() }
    }

    pub(super) fn render(&self, value: &str) -> String {
        format!("{}{value}{}", self.before, self.after)
    }
}

#[derive(Debug, Clone, Hash)]
pub(crate) struct PaletteText {
    pub(crate) search_placeholder: String,
    pub(crate) tabs_placeholder: String,
    pub(crate) tabs_empty: String,
    pub(crate) tabs_hint: String,
    pub(crate) tabs_footer: String,
    pub(crate) rename_placeholder: String,
    pub(crate) window_name_placeholder: String,
    pub(crate) window_name_footer: String,
    pub(crate) window_name_controls: String,
    pub(crate) window_name_too_long: String,
    pub(crate) no_matches: String,
    pub(crate) empty_hint: String,
    pub(crate) rename_footer: String,
    pub(crate) color_footer: String,
    pub(crate) confirm_footer: String,
    command_footer_one: TextSlot,
    command_footer_other: TextSlot,
    color_title: TextSlot,
    pub(super) categories: [String; 7],
    pub(super) disabled_reasons: [String; 7],
}

impl PaletteText {
    pub(super) fn new(i18n: Option<&I18n>) -> Self {
        let text = |key, fallback: &str| {
            i18n.and_then(|i18n| i18n.try_t_args(key, None)).unwrap_or_else(|| fallback.to_string())
        };
        let footer = |kind, noun| {
            let args = [("count", TEXT_SLOT), ("count-kind", kind)];
            TextSlot::new(
                i18n.and_then(|i18n| i18n.try_t_args("palette-command-footer", Some(&args)))
                    .unwrap_or_else(|| {
                        format!("{TEXT_SLOT} {noun} · ↑↓ navigate · ↵ run · esc close")
                    }),
            )
        };
        let color_title = TextSlot::new(
            i18n.and_then(|i18n| {
                i18n.try_t_args("palette-color-title", Some(&[("title", TEXT_SLOT)]))
            })
            .unwrap_or_else(|| format!("Color for {TEXT_SLOT}")),
        );
        Self {
            search_placeholder: text(
                "palette-search-placeholder",
                "Search commands, settings, shortcuts…",
            ),
            tabs_placeholder: text("palette-tabs-placeholder", "Search tabs…"),
            tabs_empty: text("palette-tabs-empty", "No tabs found"),
            tabs_hint: text("palette-tabs-hint", "Search a tab title or position"),
            tabs_footer: text(
                "palette-tabs-footer",
                "All tabs · ↑↓ navigate · ↵ switch · esc close",
            ),
            rename_placeholder: text("palette-rename-placeholder", "New tab title…"),
            window_name_placeholder: text(
                "palette-window-name-placeholder",
                "Window name (blank resets)…",
            ),
            window_name_footer: text(
                "palette-window-name-footer",
                "↵ save · blank resets · esc cancel",
            ),
            window_name_controls: text(
                "palette-window-name-controls",
                "Names cannot contain controls or line breaks",
            ),
            window_name_too_long: text(
                "palette-window-name-too-long",
                "Names must be 128 characters or fewer",
            ),
            no_matches: text("palette-no-matches", "No commands found"),
            empty_hint: text("palette-empty-hint", "Try settings, split, font, shortcut"),
            rename_footer: text("palette-rename-footer", "↵ rename · esc cancel"),
            color_footer: text("palette-color-footer", "↑↓ choose color · ↵ apply · esc cancel"),
            confirm_footer: text("palette-confirm-footer", "↑↓ choose · ↵ confirm · esc cancel"),
            command_footer_one: footer("one", "command"),
            command_footer_other: footer("other", "commands"),
            color_title,
            categories: CommandCategory::ALL.map(|category| {
                let (key, fallback) = category.message();
                text(key, fallback)
            }),
            disabled_reasons: DisabledReason::ALL.map(|reason| {
                let (key, fallback) = reason.message();
                text(key, fallback)
            }),
        }
    }

    /// Format the current result count through the locale's cached whole phrase.
    pub(crate) fn command_footer(&self, count: usize) -> String {
        let phrase = if count == 1 { &self.command_footer_one } else { &self.command_footer_other };
        phrase.render(&count.to_string())
    }

    /// Preserve literal tab titles inside the locale's phrase; the renderer owns any caret.
    pub(crate) fn color_title(&self, title: &str) -> String {
        self.color_title.render(title)
    }
}

#[cfg(test)]
#[path = "command_palette_presentation_tests.rs"]
mod command_palette_presentation_tests;
