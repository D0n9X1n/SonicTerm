use super::*;

fn translator(locale: &str) -> I18n {
    I18n {
        active: locale.parse().unwrap(),
        active_bundle: build_bundle(locale),
        fallback: build_bundle("en"),
    }
}

/// Every shipped locale parses, negotiates to itself, and serves a known message.
#[test]
fn shipped_locales_are_parseable_and_translatable() {
    for locale in SHIPPED_LOCALES {
        assert_eq!(negotiate(locale), *locale);
        let value = translator(locale).t("menu-file-new-tab");
        assert!(!value.is_empty());
        assert_ne!(value, "menu-file-new-tab");
    }
}

/// Invalid locale tags fall back to English rather than leaking an unsupported tag.
#[test]
fn invalid_locale_negotiates_to_english() {
    assert_eq!(negotiate("not a locale"), "en");
}

/// Missing message ids remain visible as their key after active and English lookup fail.
#[test]
fn missing_message_returns_its_key() {
    assert_eq!(translator("ja").t("missing-contract-key"), "missing-contract-key");
}

/// Reload replaces future translations without retaining the previous bundle.
#[test]
fn reload_switches_the_active_locale() {
    let expected = pick_locale(Some("ja"));
    let expected_label = translator(&expected).t("menu-file-new-tab");
    let mut i18n = translator("en");

    i18n.reload_locale(Some("ja"));

    assert_eq!(i18n.locale(), expected);
    assert_eq!(i18n.t("menu-file-new-tab"), expected_label);
}
