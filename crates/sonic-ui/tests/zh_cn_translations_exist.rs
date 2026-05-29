use sonic_ui::{
    i18n::I18n,
    prefs::{layout::CATEGORIES, state::RESET_TO_DEFAULT_KEY},
};

const PREFS_KEYS: &[&str] = &[
    "prefs-title",
    "prefs-theme",
    "prefs-font-family",
    "prefs-font-size",
    "prefs-line-height",
    "prefs-accent",
    "prefs-open-keymap-file",
    "prefs-keymap-auto-reload",
    "prefs-opacity",
    "prefs-background-blur",
    "prefs-window-decorations",
    "prefs-padding",
    "prefs-cursor-shape",
    "prefs-cursor-blink",
    "prefs-shell",
    "prefs-scrollback",
    "prefs-language",
    "prefs-language-auto",
    "prefs-apply",
    "prefs-cancel",
    RESET_TO_DEFAULT_KEY,
    "prefs-unsaved-changes",
];

#[test]
fn prefs_keys_have_zh_cn_translations() {
    let zh = I18n::new(Some("zh-CN"));
    let en = I18n::new(Some("en"));

    for key in PREFS_KEYS
        .iter()
        .copied()
        .chain(CATEGORIES.iter().map(|category| category.label_key()))
        .chain(CATEGORIES.iter().map(|category| category.description_key()))
    {
        let translated = zh.t(key);
        assert_ne!(translated, key, "missing zh-CN translation for {key}");
        assert_ne!(
            translated,
            en.t(key),
            "zh-CN translation still falls back to English for {key}"
        );
    }
}
