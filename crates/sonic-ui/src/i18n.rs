//! Lightweight Fluent-backed translation layer.
//!
//! Bundles for the three shipped locales (`en`, `zh-CN`, `ja`) are embedded
//! at compile time via [`include_str!`] so the running binary does not need
//! filesystem access to translate strings. Locale negotiation:
//!
//! 1. Explicit `SONIC_LOCALE` env var (highest priority — used by tests and
//!    by users who want a one-shot override without editing config).
//! 2. Explicit `locale` value from `sonic.toml` (for example,
//!    `locale = "zh-CN"`).
//! 3. OS locale via [`sys_locale::get_locale`].
//! 4. `"en"` as the ultimate fallback.
//!
//! Missing keys fall back to the English bundle, and a missing key in *that*
//! bundle returns the key itself so visible UI never shows an empty string.
//!
//! The module is intentionally tiny — Fluent's full API surface is large,
//! but Sonic's UI strings are simple labels and a couple of `{ $name }`
//! placeholder formats. We expose just the two helpers (`t` and `t_args`)
//! that cover those cases.
use std::borrow::Cow;

use fluent_bundle::{bundle::FluentBundle, FluentArgs, FluentResource, FluentValue};
use fluent_langneg::{negotiate_languages, NegotiationStrategy};
use intl_memoizer::concurrent::IntlLangMemoizer;
use unic_langid::{langid, LanguageIdentifier};

type Bundle = FluentBundle<FluentResource, IntlLangMemoizer>;

const EN_FTL: &str = include_str!("../../../assets/i18n/en/messages.ftl");
const ZH_CN_FTL: &str = include_str!("../../../assets/i18n/zh-CN/messages.ftl");
const JA_FTL: &str = include_str!("../../../assets/i18n/ja/messages.ftl");

/// The locales we ship FTL bundles for. Anything else negotiates to one of
/// these (or falls back to English).
pub const SHIPPED_LOCALES: &[&str] = &["en", "zh-CN", "ja"];

/// Holds one parsed bundle per shipped locale plus the active locale used
/// for new translations. Cheap to construct (parses three small `.ftl`
/// files); reconstructed on locale change so changes apply live.
pub struct I18n {
    active: LanguageIdentifier,
    active_bundle: Bundle,
    /// English bundle, kept separately so missing-key lookups can fall back
    /// without re-parsing.
    fallback: Bundle,
}

impl I18n {
    /// Build an [`I18n`] for the given user-requested locale. Pass `None`
    /// (or any unrecognized tag) to trigger OS-locale detection.
    pub fn new(requested: Option<&str>) -> Self {
        let active_tag = pick_locale(requested);
        let active_id: LanguageIdentifier = active_tag.parse().unwrap_or(langid!("en"));
        let active_bundle = build_bundle(&active_tag);
        let fallback = build_bundle("en");
        Self { active: active_id, active_bundle, fallback }
    }

    /// Currently active locale tag (`"en"`, `"zh-CN"`, `"ja"`).
    pub fn locale(&self) -> String {
        self.active.to_string()
    }

    /// Replace the active locale bundle in-place. This is the runtime
    /// live-reload hook for config language changes; existing `I18n`
    /// owners can keep their cached struct while all future lookups use the
    /// newly-negotiated bundle.
    pub fn reload_locale(&mut self, requested: Option<&str>) {
        let active_tag = pick_locale(requested);
        self.active = active_tag.parse().unwrap_or(langid!("en"));
        self.active_bundle = build_bundle(&active_tag);
    }

    /// Translate a message id. Missing keys fall back to English; missing in
    /// English too returns the key itself so UIs never show an empty string.
    pub fn t(&self, key: &str) -> String {
        self.t_args(key, None)
    }

    /// Translate with positional `{ $name }` arguments. The arg slice is
    /// `(name, value)` tuples; values are forwarded as Fluent strings.
    pub fn t_args(&self, key: &str, args: Option<&[(&str, &str)]>) -> String {
        let fluent_args = args.map(|pairs| {
            let mut a = FluentArgs::new();
            for (k, v) in pairs {
                a.set(*k, FluentValue::from(Cow::Borrowed(*v)));
            }
            a
        });
        if let Some(s) = format_in(&self.active_bundle, key, fluent_args.as_ref()) {
            return s;
        }
        if let Some(s) = format_in(&self.fallback, key, fluent_args.as_ref()) {
            return s;
        }
        key.to_string()
    }
}

impl Default for I18n {
    fn default() -> Self {
        Self::new(None)
    }
}

fn format_in(bundle: &Bundle, key: &str, args: Option<&FluentArgs<'_>>) -> Option<String> {
    let msg = bundle.get_message(key)?;
    let pattern = msg.value()?;
    let mut errs = vec![];
    let out = bundle.format_pattern(pattern, args, &mut errs);
    // Strip Fluent's bidi isolates so plain ASCII UI labels stay byte-equal
    // to their FTL source — much easier to assert in tests and renders the
    // same in cosmic-text either way.
    Some(out.replace(['\u{2068}', '\u{2069}'], ""))
}

fn build_bundle(tag: &str) -> Bundle {
    let id: LanguageIdentifier = tag.parse().unwrap_or(langid!("en"));
    let mut b = FluentBundle::new_concurrent(vec![id]);
    // Disable Unicode isolate markers; we strip them anyway, and turning
    // them off avoids paying for the runtime insertion.
    b.set_use_isolating(false);
    let src = match tag {
        "zh-CN" => ZH_CN_FTL,
        "ja" => JA_FTL,
        _ => EN_FTL,
    };
    let res = FluentResource::try_new(src.to_string())
        // PANIC: safe — `src` is one of the `const &str` FTL blobs included
        // via `include_str!` at module top. Parse failures would surface in
        // CI (build fails) or the unit tests in tests/i18n.rs which load
        // every shipped locale.
        .expect("embedded FTL must parse — this is a build-time guarantee");
    // PANIC: safe — single resource per bundle in this fn; "duplicate keys"
    // can only fire if the FTL file itself defines the same id twice, caught
    // by the per-locale unit tests.
    b.add_resource(res).expect("embedded FTL must not have duplicate keys");
    b
}

/// Decide which shipped locale we should serve. Priority: explicit
/// `SONIC_LOCALE` env var > caller-supplied `requested` > OS locale > `"en"`.
fn pick_locale(requested: Option<&str>) -> String {
    if let Ok(env) = std::env::var("SONIC_LOCALE") {
        if !env.is_empty() {
            return negotiate(&env);
        }
    }
    if let Some(r) = requested.filter(|s| !s.is_empty()) {
        return negotiate(r);
    }
    if let Some(sys) = sys_locale::get_locale() {
        return negotiate(&sys);
    }
    "en".to_string()
}

fn negotiate(requested: &str) -> String {
    let req: LanguageIdentifier = match requested.parse() {
        Ok(id) => id,
        Err(_) => return "en".to_string(),
    };
    let available: Vec<LanguageIdentifier> =
        // PANIC: safe — `SHIPPED_LOCALES` is a const &[&str] of canonical BCP-47
        // tags ("en", "zh-CN", "ja"). Each parses as a LanguageIdentifier;
        // any addition that doesn't is caught by tests/i18n.rs.
        SHIPPED_LOCALES.iter().map(|s| s.parse().unwrap()).collect();
    let default: LanguageIdentifier = langid!("en");
    let supported =
        negotiate_languages(&[req], &available, Some(&default), NegotiationStrategy::Filtering);
    supported.first().map(|id| id.to_string()).unwrap_or_else(|| "en".to_string())
}

// Unit tests live in `tests/src_i18n.rs`.
