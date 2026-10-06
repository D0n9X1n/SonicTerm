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

/// The attributes on the item whose line is `item` in `source`, in source order, each joined onto one
/// line. As in Rust, blank and comment lines between an attribute and its item are skipped, and an
/// attribute may span several lines; any other line ends the item's attributes.
fn attributes_above(source: &str, item: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == item)
        .map(|(line_index, _)| line_index)
        .collect();
    assert_eq!(matches.len(), 1, "exactly one `{item}` line");
    let mut attributes = Vec::new();
    let mut cursor = matches[0];
    'lines: while cursor > 0 {
        cursor -= 1;
        let line = lines[cursor];
        if line.is_empty() || line.starts_with("//") {
            // When: the line is blank or a comment, Rust still applies the attributes above it.
            continue;
        }
        if !line.ends_with(']') {
            // When: the line is no attribute's end, it belongs to another item.
            break;
        }
        // An attribute may span lines: walk back to the line that opens it.
        let mut opening = cursor;
        while !lines[opening].starts_with("#[") && !lines[opening].starts_with("#![") {
            let previous = lines[opening.saturating_sub(1)];
            if opening == 0 || previous.is_empty() || previous.ends_with([';', '{', '}']) {
                // When: no opening `#[` precedes the closing `]`, the line is not an attribute.
                break 'lines;
            }
            opening -= 1;
        }
        attributes.push(lines[opening..=cursor].join(" "));
        cursor = opening;
    }
    attributes.reverse();
    attributes
}

/// Whether any attribute, outer or inner, names a `cfg` or `cfg_attr`.
fn gates(attributes: &[impl AsRef<str>]) -> bool {
    attributes.iter().any(|attribute| attribute.as_ref().contains("cfg"))
}

/// The methods are ordinary doc-hidden methods, never behind a cfg or a Cargo feature, so a harness
/// overlaid on any base that has them compiles and both sides build the same feature set. Every
/// enclosing gate counts: the file's inner attributes, the impl's attributes, the methods inside it,
/// and the `mod perf_present` declaration in `app/mod.rs`.
#[test]
fn the_attribution_watch_methods_are_never_cfg_gated() {
    // A Windows checkout may use CRLF line endings; the brace search below needs plain LF.
    let source = include_str!("perf_present.rs").replace("\r\n", "\n");
    let source = source.as_str();
    let inner: Vec<&str> =
        source.lines().map(str::trim).filter(|line| line.starts_with("#![")).collect();
    assert!(!gates(&inner), "the module is gated by an inner attribute: {inner:?}");
    let impl_attributes = attributes_above(source, "impl super::App {");
    assert!(!gates(&impl_attributes), "the impl is gated: {impl_attributes:?}");
    let methods = &source[source.find("impl super::App {").expect("the App impl")..];
    // The impl ends at its first closing brace in column 0; the test module follows it.
    let methods = &methods[..methods.find("\n}\n").expect("the impl ends")];
    for name in ["pub fn arm_s10_attribution(", "pub fn disarm_s10_attribution("] {
        assert!(methods.contains(name), "{name} is defined in the impl");
    }
    assert!(!methods.contains("#[cfg"), "a method in the impl is cfg-gated");
    let module_attributes = attributes_above(include_str!("mod.rs"), "mod perf_present;");
    assert!(!gates(&module_attributes), "the module declaration is gated: {module_attributes:?}");
}

/// The attribute reader sees a gate however Rust lets it be written: across a blank line, behind a
/// comment, or spread over several lines. An ungated item, and attributes that are not gates, read as
/// ungated, and a line ending in `]` that opens no attribute ends the scan.
#[test]
fn the_attribute_reader_follows_rust_attribute_placement() {
    let item = "mod perf_present;";
    let gated = [
        ("across a blank line", "#[cfg(test)]\n\nmod perf_present;\n"),
        ("over several lines", "#[cfg(\n    test\n)]\nmod perf_present;\n"),
        ("behind a comment", "#[cfg(test)]\n// the watch module\nmod perf_present;\n"),
        (
            "under another attribute",
            "#[cfg(test)]\n#[allow(\n    dead_code\n)]\nmod perf_present;\n",
        ),
    ];
    for (spelling, source) in gated {
        assert!(gates(&attributes_above(source, item)), "a gate {spelling} is seen");
    }
    let ungated = [
        "mod path_target;\nmod perf_present;\n",
        "#[doc(hidden)]\n\n#[allow(\n    dead_code\n)]\nmod perf_present;\n",
        "const ROWS: [u8; 2] = [\n    1, 2,\n]\nmod perf_present;\n",
    ];
    for source in ungated {
        assert!(!gates(&attributes_above(source, item)), "{source:?} is not gated");
    }
    assert_eq!(
        attributes_above("#[cfg(\n    test\n)]\n\nmod perf_present;\n", item),
        vec!["#[cfg( test )]".to_owned()],
        "a multiline attribute is read whole"
    );
}
