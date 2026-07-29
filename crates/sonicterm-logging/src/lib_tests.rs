//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{filter_for_level, LogLevel, LoggingConfig, CUSTOM_DEBUG_TARGETS, DEFAULT_FILTER};

#[test]
fn exports_default_filter_and_config() {
    assert!(DEFAULT_FILTER.contains("sonicterm=warn"));
    assert_eq!(LoggingConfig::default().max_rotated_files, 3);
}

// ---------------------------------------------------------------------------
// Custom target reachability
//
// `EnvFilter` admits a target only when a directive names it. A target named
// by no directive is never enabled, silently — no build warning, no runtime
// warning, and `tracing::enabled!` simply returns false.
// ---------------------------------------------------------------------------

/// Is a DEBUG event on `target` admitted by `filter`?
///
/// `tracing::enabled!` needs a literal target, so this drives the real
/// predicate through a macro that takes one, dispatched over the fixed set of
/// targets the app actually uses. Verbose, but it exercises the same code path
/// production does rather than a string search over the filter.
fn enabled_at(filter: &str, target: &str) -> bool {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let subscriber = Registry::default().with(EnvFilter::try_new(filter).expect("valid filter"));
    tracing::subscriber::with_default(subscriber, || match target {
        "memory" => tracing::enabled!(target: "memory", tracing::Level::DEBUG),
        "memory::reclaimed" => {
            tracing::enabled!(target: "memory::reclaimed", tracing::Level::DEBUG)
        }
        "state_machine" => tracing::enabled!(target: "state_machine", tracing::Level::DEBUG),
        "state_machine.log" => {
            tracing::enabled!(target: "state_machine.log", tracing::Level::DEBUG)
        }
        "render_timing" => tracing::enabled!(target: "render_timing", tracing::Level::DEBUG),
        "tear_out_timing" => tracing::enabled!(target: "tear_out_timing", tracing::Level::DEBUG),
        "sonic::glyph_atlas" => {
            tracing::enabled!(target: "sonic::glyph_atlas", tracing::Level::DEBUG)
        }
        "sonic::render::glyph" => {
            tracing::enabled!(target: "sonic::render::glyph", tracing::Level::DEBUG)
        }
        "sonic_exit" => tracing::enabled!(target: "sonic_exit", tracing::Level::DEBUG),
        "sonicterm_font::shaper::harfbuzz" => {
            tracing::enabled!(target: "sonicterm_font::shaper::harfbuzz", tracing::Level::DEBUG)
        }
        other => panic!(
            "no probe arm for target {other:?}; add one so the reachability tests \
             cover every target the source emits"
        ),
    })
}

/// Is a TRACE event on the shaper's module target admitted by `filter`?
///
/// Separate from [`enabled_at`], which probes DEBUG. The shaper's per-glyph
/// and per-shape-call dumps sit at TRACE precisely so no configured level
/// reaches them, and asserting that needs a probe at that level.
fn shaper_trace_enabled_at(filter: &str) -> bool {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let subscriber = Registry::default().with(EnvFilter::try_new(filter).expect("valid filter"));
    tracing::subscriber::with_default(subscriber, || {
        tracing::enabled!(
            target: "sonicterm_font::shaper::harfbuzz",
            tracing::Level::TRACE
        )
    })
}

/// No configured level turns on the shaper's hot-path diagnostics.
///
/// Those sites fire once per shaped glyph and once per shape call, and two of
/// them pretty-print whole collections. At DEBUG they produced 82 million
/// lines and 2.3 GB from a session that sat idle — while `wiki/Logging.md`
/// tells a user investigating *memory* to set `level = "debug"`. The
/// documented procedure for diagnosing a memory problem could therefore fill
/// the disk, and the cost landed on the shaping hot path where it also
/// distorted what was being measured.
///
/// They live at TRACE now, which no `LogLevel` admits, so they stay available
/// through `RUST_LOG` for shaping work without reaching a user who followed
/// the documentation.
///
/// The DEBUG assertion is the pair to this one: it pins that the crate is
/// still reachable at all, so a future change cannot satisfy this test by
/// silencing the whole target.
#[test]
fn no_configured_level_admits_the_shaper_hot_path_dumps() {
    for level in [LogLevel::Error, LogLevel::Warn, LogLevel::Info, LogLevel::Debug] {
        let filter = filter_for_level(level);
        assert!(
            !shaper_trace_enabled_at(filter),
            "{level:?} admits TRACE on the shaper target; its per-glyph dumps would \
             flood the log a user was told to enable while investigating memory"
        );
    }

    // And the crate is still reachable at debug, so this cannot be satisfied
    // by turning the target off entirely.
    let debug = filter_for_level(LogLevel::Debug);
    assert!(
        enabled_at(debug, "sonicterm_font::shaper::harfbuzz"),
        "the shaper's ordinary DEBUG diagnostics must still be reachable"
    );
}

/// The instruction the wiki gives a user must actually produce output.
///
/// `wiki/Logging.md` tells a user investigating memory growth to set
/// `level = "debug"` and expect `pane retention` lines every 30 seconds.
/// Before this, that produced **nothing**: no directive named the `memory`
/// target, so `enabled!` was false and the sampling path returned immediately.
///
/// The consequence reached past diagnostics. Retention sampling — and with it
/// every governor charge — is gated on that same `enabled!`, so a shipped
/// session charged the governor nothing at all.
#[test]
fn setting_the_documented_debug_level_enables_memory_diagnostics() {
    let debug = filter_for_level(LogLevel::Debug);
    assert!(
        enabled_at(debug, "memory"),
        "the wiki tells users to set level = \"debug\" for memory diagnostics; \
         that level must enable the memory target"
    );
}

/// Every custom target the source emits must be admitted at debug level.
///
/// Scans the workspace rather than trusting a hand-kept list, because the
/// defect this guards is exactly a hand-kept list falling behind: `memory`
/// (25 sites) and `state_machine` (16 sites) were both missing while
/// `render_timing` (4) and `tear_out_timing` (2) were present. The two most
/// used custom targets were the two nobody had added.
#[test]
fn every_custom_target_in_the_source_is_reachable_at_debug_level() {
    // Sources scanned at compile time, so this cannot drift from the tree.
    // Every non-test source in the workspace that emits `target: "..."`,
    // enumerated by grepping the tree rather than by memory. Three files were
    // missing from the first draft of this list — which is the same
    // hand-kept-list failure the test exists to catch, reproduced inside the
    // test itself.
    const SOURCES: &[&str] = &[
        include_str!("../../sonicterm-app/src/app/mod.rs"),
        include_str!("../../sonicterm-app/src/app/media.rs"),
        include_str!("../../sonicterm-app/src/app/retention.rs"),
        include_str!("../../sonicterm-app/src/app/tear_out.rs"),
        include_str!("../../sonicterm-app/src/app/child_window.rs"),
        include_str!("../../sonicterm-app/src/app/render_timing.rs"),
        include_str!("../../sonicterm-app-core/src/state_machine.rs"),
        include_str!("../../sonicterm-gpu/src/core.rs"),
        include_str!("../../sonicterm-gpu/src/software_windows.rs"),
        include_str!("../../sonicterm-text/src/glyph_atlas.rs"),
        include_str!("../../sonicterm-vt/src/vt.rs"),
        include_str!("../../sonicterm-cfg/src/config.rs"),
        include_str!("../../sonicterm-cfg/src/keymap.rs"),
        include_str!("../../sonicterm-cfg/src/theme.rs"),
        include_str!("../../sonicterm-mac/src/main.rs"),
    ];

    let mut found: Vec<String> = Vec::new();
    for source in SOURCES {
        for (_, rest) in
            source.match_indices("target: \"").map(|(i, m)| (i, &source[i + m.len()..]))
        {
            if let Some(end) = rest.find('"') {
                let target = &rest[..end];
                // Crate-name targets are covered by the `sonicterm=` directive.
                // Crate-name targets ride the `sonicterm=` directive.
                // `sonic_exit` is deliberately WARN in every filter so exit
                // markers survive at any level, so it is not a debug target.
                let is_custom = !target.starts_with("sonicterm") && target != "sonic_exit";
                if is_custom && !found.iter().any(|f| f == target) {
                    found.push(target.to_string());
                }
            }
        }
    }

    assert!(!found.is_empty(), "the scan must find targets, or it is asserting nothing");

    let debug = filter_for_level(LogLevel::Debug);
    for target in &found {
        let listed = CUSTOM_DEBUG_TARGETS.contains(&target.as_str());
        assert!(
            listed,
            "target {target:?} is emitted by the source but missing from \
             CUSTOM_DEBUG_TARGETS; it will never be enabled"
        );
        // Reachability, not substring presence: `EnvFilter` matches by module
        // prefix, so `sonic=debug` admits `sonic::glyph_atlas` without the
        // filter string ever containing that name. Asserting on the string
        // would demand a redundant directive and reject a correct one.
        assert!(
            enabled_at(debug, target),
            "target {target:?} is listed but not admitted at debug level"
        );
    }
}

/// Every listed target is admitted, checked through the real predicate.
///
/// Separate from the scan above because `filter.contains(name)` is a string
/// check and `enabled!` is the thing that actually decides. A directive can be
/// present and still not admit the target — for instance if it were written at
/// a level above DEBUG.
#[test]
fn every_listed_custom_target_is_admitted_by_the_debug_filter() {
    let debug = filter_for_level(LogLevel::Debug);
    for target in CUSTOM_DEBUG_TARGETS {
        assert!(
            enabled_at(debug, target),
            "{target:?} is in the debug filter but enabled!(DEBUG) is false for it"
        );
    }
}

/// Diagnostics stay off at the default level.
///
/// The fix must not turn 25 memory call sites and a 30-second sampling walk
/// into something an ordinary session pays for. `warn` is what a user runs by
/// default, and it must remain silent.
///
/// [`crate::MEMORY_RECLAIMED_TARGET`] is the deliberate exception and is
/// asserted the other way in
/// `reclamation_warnings_reach_a_default_session`: it reports data the user
/// has already lost, which is not a diagnostic. Exempting it here without
/// asserting the opposite elsewhere would let it fall silent unnoticed, so the
/// two tests are written as a pair.
#[test]
fn custom_targets_stay_off_at_the_default_level() {
    for target in CUSTOM_DEBUG_TARGETS {
        if *target == crate::MEMORY_RECLAIMED_TARGET {
            continue;
        }
        assert!(
            !enabled_at(DEFAULT_FILTER, target),
            "{target:?} must stay off at the default warn level"
        );
    }
    assert!(!enabled_at(filter_for_level(LogLevel::Info), "memory"), "and off at info");
}

/// Reclamation that destroys user-visible data reaches a default session.
///
/// A user whose image vanished is owed a reason in the log they actually have,
/// not one they would have had if they had enabled debug logging before the
/// thing happened. Checked at WARN because that is the level these sites emit
/// at and the level a default session admits — a target admitted only at DEBUG
/// would pass a DEBUG-level probe and still be silent in every shipped
/// session.
#[test]
fn reclamation_warnings_reach_a_default_session() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};

    for filter in [
        DEFAULT_FILTER,
        filter_for_level(LogLevel::Warn),
        filter_for_level(LogLevel::Info),
        filter_for_level(LogLevel::Debug),
    ] {
        let subscriber =
            Registry::default().with(EnvFilter::try_new(filter).expect("valid filter"));
        let admitted = tracing::subscriber::with_default(
            subscriber,
            || tracing::enabled!(target: "memory::reclaimed", tracing::Level::WARN),
        );
        assert!(
            admitted,
            "a WARN on {:?} must reach a session running {filter:?}; a user cannot be told \
             their image was discarded by a log line that never renders",
            crate::MEMORY_RECLAIMED_TARGET
        );
    }
}

/// Atlas-exhaustion warnings must reach a default session.
///
/// `sonic::glyph_atlas` carries "inline image atlas full; skipped older
/// images" — a warning about a symptom the user can see, images silently not
/// rendering. It was admitted by no directive at any level except a bare
/// `error`, so the one diagnostic explaining the symptom was invisible
/// precisely when someone would look for it.
#[test]
fn atlas_exhaustion_warnings_reach_a_default_session() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    for (name, filter) in [
        ("warn (default)", filter_for_level(LogLevel::Warn)),
        ("info", filter_for_level(LogLevel::Info)),
        ("debug", filter_for_level(LogLevel::Debug)),
    ] {
        let subscriber =
            Registry::default().with(EnvFilter::try_new(filter).expect("valid filter"));
        let reached = tracing::subscriber::with_default(
            subscriber,
            || tracing::enabled!(target: "sonic::glyph_atlas", tracing::Level::WARN),
        );
        assert!(
            reached,
            "atlas exhaustion is user-visible; its warning must reach a {name} session"
        );
    }
}

/// Verbose atlas detail stays off below debug.
///
/// The same family carries `debug!` lines about LRU reclamation, which are
/// per-frame-ish and would be noise in an ordinary session. Admitting the
/// family at warn must not drag those in.
#[test]
fn atlas_debug_detail_stays_off_below_debug_level() {
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    for (name, filter) in
        [("warn", filter_for_level(LogLevel::Warn)), ("info", filter_for_level(LogLevel::Info))]
    {
        let subscriber =
            Registry::default().with(EnvFilter::try_new(filter).expect("valid filter"));
        let reached = tracing::subscriber::with_default(
            subscriber,
            || tracing::enabled!(target: "sonic::glyph_atlas", tracing::Level::DEBUG),
        );
        assert!(!reached, "atlas DEBUG detail must stay off at {name}");
    }
}

/// The grep recipe the wiki gives for reclamation must match a real line.
///
/// `wiki/Logging.md` tells a user whose image vanished to run
/// `grep 'memory::reclaimed'`. That instruction is only true if the formatter
/// actually writes the target into the line — a default that is on today and
/// could be turned off by a `.with_target(false)` added for unrelated reasons,
/// silently breaking a documented recovery step.
///
/// Captures a real formatted event rather than asserting on the default.
#[test]
fn the_documented_grep_recipe_matches_a_real_reclamation_line() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Layer, Registry};

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);
    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("not poisoned").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let sink = Captured::default();
    // The shipped default filter and a fmt layer configured as `init` builds
    // one: no `.with_target(false)`, so whatever the default is, is what the
    // user's log gets.
    let subscriber = Registry::default().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(sink.clone())
            .with_filter(EnvFilter::try_new(DEFAULT_FILTER).expect("valid filter")),
    );

    tracing::subscriber::with_default(subscriber, || {
        tracing::warn!(
            target: "memory::reclaimed",
            released_bytes = 1234,
            "cancelled a media capture that stopped receiving"
        );
    });

    let written = String::from_utf8(sink.0.lock().expect("not poisoned").clone())
        .expect("formatted output is utf-8");

    assert!(
        !written.is_empty(),
        "a WARN on the reclamation target produced no output under the default filter"
    );
    assert!(
        written.contains("memory::reclaimed"),
        "the wiki tells users to `grep 'memory::reclaimed'`, but the formatted line does not \
         contain that string. The line was: {written:?}"
    );
}
