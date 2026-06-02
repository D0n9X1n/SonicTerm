//! Regression tests for `DEFAULT_FILTER` (issue #448).
//!
//! Post-#430 the workspace rename `sonic-*` → `sonicterm-*` left the
//! filter referencing a `sonic` target that no longer exists. These
//! tests pin the filter's behaviour to the renamed crates so future
//! renames cannot silently drop logging again.

use sonicterm_logging::DEFAULT_FILTER;
use tracing::Level;
use tracing_subscriber::{filter::LevelFilter, EnvFilter};

/// The filter must parse — a typo in `DEFAULT_FILTER` would otherwise
/// only surface at runtime when init falls back to it.
#[test]
fn default_filter_parses() {
    EnvFilter::try_new(DEFAULT_FILTER).expect("DEFAULT_FILTER must be a valid EnvFilter spec");
}

/// The filter must NOT reference the dead `sonic=` rule from before
/// PR #430. If this triggers, somebody reintroduced the rename-stale
/// rule that issue #448 fixed.
#[test]
fn default_filter_has_no_dead_sonic_crate_rule() {
    // Exact-match rule `sonic=` (followed by a level). The substring
    // `sonic_exit=` and `sonicterm` should NOT trip this assertion, so
    // we scan comma-separated directives.
    for directive in DEFAULT_FILTER.split(',') {
        let directive = directive.trim();
        let Some((target, _)) = directive.split_once('=') else {
            continue;
        };
        assert_ne!(
            target, "sonic",
            "DEFAULT_FILTER still contains rename-stale `sonic=` rule: {DEFAULT_FILTER:?}"
        );
    }
}

/// Required directives that must be present so post-rename diagnostic
/// logging from the renamed crates reaches the rolling file.
#[test]
fn default_filter_contains_required_directives() {
    for needle in [
        "sonicterm=info",      // umbrella INFO floor for renamed crates
        "sonicterm_vt=warn",   // noisy parser pinned
        "sonicterm_grid=warn", // noisy grid pinned
        "sonic_exit=warn",     // exit-marker target
        "wgpu=warn",
        "naga=warn",
        "cosmic_text=warn",
        "glyphon=warn",
    ] {
        assert!(
            DEFAULT_FILTER.contains(needle),
            "DEFAULT_FILTER missing required directive `{needle}`: {DEFAULT_FILTER:?}"
        );
    }
}

/// Behaviour test: load the filter into a real `EnvFilter` and confirm
/// per-crate level decisions match expectations. Uses `max_level_hint`
/// scoped to each crate's static metadata via the public
/// [`EnvFilter`] API.
#[test]
fn default_filter_levels_match_expectations() {
    use tracing_subscriber::layer::{Filter, SubscriberExt};
    use tracing_subscriber::Registry;

    // Build the filter and wrap in a registry so we can query it via
    // `Filter::callsite_enabled`-style helpers indirectly through
    // `max_level_hint`.
    let filter: EnvFilter = EnvFilter::try_new(DEFAULT_FILTER).expect("parses");
    let subscriber = Registry::default().with(filter);
    let _guard = tracing::subscriber::set_default(subscriber);

    // sonicterm_text => INFO should be enabled (via umbrella rule).
    assert!(
        tracing::event_enabled!(target: "sonicterm_text", Level::INFO),
        "INFO must be enabled for sonicterm_text via `sonicterm=info` umbrella"
    );
    // sonicterm_windows => INFO should be enabled.
    assert!(
        tracing::event_enabled!(target: "sonicterm_windows", Level::INFO),
        "INFO must be enabled for sonicterm_windows via `sonicterm=info` umbrella"
    );
    // sonicterm_app => INFO should be enabled.
    assert!(
        tracing::event_enabled!(target: "sonicterm_app", Level::INFO),
        "INFO must be enabled for sonicterm_app via `sonicterm=info` umbrella"
    );

    // sonicterm_vt => INFO must be DISABLED (pinned at WARN).
    assert!(
        !tracing::event_enabled!(target: "sonicterm_vt", Level::INFO),
        "INFO must be suppressed for sonicterm_vt (pinned WARN)"
    );
    // sonicterm_vt => WARN must be enabled.
    assert!(
        tracing::event_enabled!(target: "sonicterm_vt", Level::WARN),
        "WARN must be enabled for sonicterm_vt"
    );
    // sonicterm_grid => INFO must be DISABLED.
    assert!(
        !tracing::event_enabled!(target: "sonicterm_grid", Level::INFO),
        "INFO must be suppressed for sonicterm_grid (pinned WARN)"
    );

    // wgpu => INFO must be DISABLED, WARN enabled.
    assert!(
        !tracing::event_enabled!(target: "wgpu", Level::INFO),
        "INFO must be suppressed for wgpu"
    );
    assert!(tracing::event_enabled!(target: "wgpu", Level::WARN), "WARN must be enabled for wgpu");

    // sonic_exit => WARN enabled (preserved exit-marker target).
    assert!(
        tracing::event_enabled!(target: "sonic_exit", Level::WARN),
        "WARN must be enabled for sonic_exit"
    );

    // Silence unused-import lints when the helper traits aren't called.
    let _ = (LevelFilter::INFO, std::any::type_name::<dyn Filter<Registry>>());
}
