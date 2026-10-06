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

/// The attribute lines directly above the line `item` starts in `source`, in source order; comment lines
/// are skipped, and a blank line or any other item's line ends the item's attributes.
fn attributes_above<'source>(source: &'source str, item: &str) -> Vec<&'source str> {
    let lines: Vec<&str> = source.lines().collect();
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == item)
        .map(|(line_index, _)| line_index)
        .collect();
    assert_eq!(matches.len(), 1, "exactly one `{item}` line");
    let mut attributes = Vec::new();
    for line in lines[..matches[0]].iter().rev().map(|line| line.trim()) {
        if line.starts_with("#[") {
            attributes.push(line);
        } else if !line.starts_with("//") {
            // When: the line is blank or belongs to another item, the item's attributes have ended.
            break;
        }
    }
    attributes.reverse();
    attributes
}

/// Whether any attribute line, outer or inner, names a `cfg` or `cfg_attr`.
fn gates(attributes: &[&str]) -> bool {
    attributes.iter().any(|line| line.contains("cfg"))
}

/// The methods are ordinary doc-hidden methods, never behind a cfg or a Cargo feature, so a harness
/// overlaid on any base that has them compiles and both sides build the same feature set. Every
/// enclosing gate counts: the file's inner attributes, the impl's attributes, the methods inside it,
/// and the `mod perf_present` declaration in `app/mod.rs`.
#[test]
fn the_attribution_watch_methods_are_never_cfg_gated() {
    let source = include_str!("perf_present.rs");
    let inner: Vec<&str> =
        source.lines().map(str::trim).filter(|line| line.starts_with("#![")).collect();
    assert!(!gates(&inner), "the module is gated by an inner attribute: {inner:?}");
    let impl_attributes = attributes_above(source, "impl super::App {");
    assert!(!gates(&impl_attributes), "the impl is gated: {impl_attributes:?}");
    let methods = &source[source.find("impl super::App {").expect("the App impl")..];
    // The impl ends at its first closing brace in column 0; the pin and the test module follow it.
    let methods = &methods[..methods.find("\n}\n").expect("the impl ends")];
    for name in ["pub fn arm_s10_attribution(", "pub fn disarm_s10_attribution("] {
        assert!(methods.contains(name), "{name} is defined in the impl");
    }
    assert!(!methods.contains("#[cfg"), "a method in the impl is cfg-gated");
    let module_attributes = attributes_above(include_str!("mod.rs"), "mod perf_present;");
    assert!(!gates(&module_attributes), "the module declaration is gated: {module_attributes:?}");
}
