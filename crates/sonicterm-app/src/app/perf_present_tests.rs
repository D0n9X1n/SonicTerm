use std::sync::{Arc, Mutex};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use crate::app::App;

/// Records the target of every event, whatever its level.
#[derive(Clone, Default)]
struct TargetLayer {
    targets: Arc<Mutex<Vec<String>>>,
}

impl<S: tracing::Subscriber> Layer<S> for TargetLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        self.targets.lock().expect("not poisoned").push(event.metadata().target().to_string());
    }
}

/// Run `body` with a subscriber that admits every event and return their targets.
fn event_targets(body: impl FnOnce()) -> Vec<String> {
    let layer = TargetLayer::default();
    let subscriber = Registry::default().with(layer.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let targets = layer.targets.lock().expect("not poisoned").clone();
    targets
}

/// In this build the watch records nothing: arming any pane, seeded or unknown, returns no arming id,
/// so a harness reads attribution as unavailable, and neither arming nor disarming emits a line.
#[test]
fn the_attribution_watch_arms_nothing_and_emits_nothing() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("s10");
    let mut armed = Vec::new();
    let targets = event_targets(|| {
        armed.push(app.arm_s10_attribution(pane_id, "nonce-0123456789abcdef", "$ ", 300));
        armed.push(app.arm_s10_attribution(pane_id + 1_000, "nonce", "$ ", 1_200));
        app.disarm_s10_attribution(pane_id);
        app.disarm_s10_attribution(pane_id + 1_000);
    });
    assert_eq!(armed, vec![None, None], "no arming id is issued");
    assert!(targets.is_empty(), "no line is emitted: {targets:?}");
}

/// The methods are ordinary doc-hidden methods, never behind a cfg or a Cargo feature, so a harness
/// overlaid on any base that has them compiles and both sides build the same feature set.
#[test]
fn the_attribution_watch_methods_are_never_cfg_gated() {
    let source = include_str!("perf_present.rs");
    let methods = &source[source.find("impl super::App {").expect("the App impl")..];
    // The impl ends at its first closing brace in column 0; the test module after it is cfg(test).
    let methods = &methods[..methods.find("\n}\n").expect("the impl ends")];
    for name in ["pub fn arm_s10_attribution(", "pub fn disarm_s10_attribution("] {
        let at = methods.find(name).unwrap_or_else(|| panic!("{name} is defined"));
        let attributes = &methods[..at];
        let item_start = attributes.rfind("\n\n").map_or(0, |index| index + 2);
        assert!(!attributes[item_start..].contains("#[cfg"), "{name} is cfg-gated");
    }
    assert!(!methods.contains("#[cfg"), "nothing in the impl is cfg-gated");
}
