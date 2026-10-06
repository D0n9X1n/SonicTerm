use super::*;
use crate::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// The prerequisite has no compile-time control, so every phase call is a no-op that answers
/// `NoControl`: no pause is requested, queried or resumed, and no state carries from one call to the
/// next, whatever the order or the pane.
#[test]
fn every_phase_reads_no_control_and_keeps_no_state() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let calls = [
        PerfControlPhaseV1::Typing { flood_pane: 7 },
        PerfControlPhaseV1::Typing { flood_pane: 7 },
        PerfControlPhaseV1::Typing { flood_pane: 9 },
        PerfControlPhaseV1::Other,
        PerfControlPhaseV1::Other,
    ];
    for phase in calls {
        assert_eq!(app.perf_control_phase(phase), PerfControlOutcomeV1::NoControl, "{phase:?}");
    }
}

/// The frozen phase schema: both variants and the `Typing` field, built from public fields and matched
/// with no wildcard arm, so adding, removing or reshaping a phase fails this test's build.
#[test]
fn the_control_phase_schema_is_the_contracts() {
    let phases = [PerfControlPhaseV1::Typing { flood_pane: 7 }, PerfControlPhaseV1::Other];
    let names: Vec<&str> = phases
        .iter()
        .map(|phase| match phase {
            PerfControlPhaseV1::Typing { flood_pane: _ } => "typing",
            PerfControlPhaseV1::Other => "other",
        })
        .collect();
    assert_eq!(names, ["typing", "other"]);
}

/// The frozen outcome schema: every variant and field the contract names, built and matched
/// exhaustively, so removing or reshaping one fails this test's build.
#[test]
fn the_control_outcome_schema_is_the_contracts() {
    let at = std::time::Instant::now();
    let pause = PerfControlPauseV1 {
        flood_pane: 7,
        requested_at: at,
        acknowledged_at: Some(at),
        resume_requested_at: at,
        reads_after_ack: 3,
    };
    let stop = PerfControlStopV1 {
        flood_pane: 7,
        requested_at: at,
        acknowledged_at: None,
        stopped_at: at,
    };
    let outcomes = [
        PerfControlOutcomeV1::NoControl,
        PerfControlOutcomeV1::NoPane,
        PerfControlOutcomeV1::Conflict { outstanding: 7 },
        PerfControlOutcomeV1::PauseRequested { requested_at: at },
        PerfControlOutcomeV1::Paused { requested_at: at, acknowledged_at: at },
        PerfControlOutcomeV1::ResumeRequested(pause),
        PerfControlOutcomeV1::WorkerStopped(stop),
    ];
    let names: Vec<&str> = outcomes
        .iter()
        .map(|outcome| match outcome {
            PerfControlOutcomeV1::NoControl => "no-control",
            PerfControlOutcomeV1::NoPane => "no-pane",
            PerfControlOutcomeV1::Conflict { .. } => "conflict",
            PerfControlOutcomeV1::PauseRequested { .. } => "pause-requested",
            PerfControlOutcomeV1::Paused { .. } => "paused",
            PerfControlOutcomeV1::ResumeRequested(_) => "resume-requested",
            PerfControlOutcomeV1::WorkerStopped(_) => "worker-stopped",
        })
        .collect();
    assert_eq!(names.len(), 7);
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

/// Check that `method` in `source` is never gated: not by the file's inner attributes, the
/// `impl super::App {` block's attributes, any attribute inside that impl, or the module declaration
/// `module_line` in `mod_source` (`app/mod.rs`).
fn assert_never_gated(source: &str, method: &str, module_line: &str, mod_source: &str) {
    // A CRLF checkout (Windows) is read as LF, so the `\n` searches below find the same lines.
    let source = &source.replace("\r\n", "\n");
    let mod_source = &mod_source.replace("\r\n", "\n");
    let inner: Vec<&str> =
        source.lines().map(str::trim).filter(|line| line.starts_with("#![")).collect();
    assert!(!gates(&inner), "{method}: the module is gated by an inner attribute: {inner:?}");
    let impl_attributes = attributes_above(source, "impl super::App {");
    assert!(!gates(&impl_attributes), "{method}: the impl is gated: {impl_attributes:?}");
    let methods = &source[source.find("impl super::App {").expect("the App impl")..];
    // The impl ends at its first closing brace in column 0; the test module follows it.
    let methods = &methods[..methods.find("\n}\n").expect("the impl ends")];
    assert!(methods.contains(method), "{method} is defined in the impl");
    assert!(!methods.contains("cfg"), "{method}: an item in the impl is cfg-gated");
    let module_attributes = attributes_above(mod_source, module_line);
    assert!(!gates(&module_attributes), "{module_line} is gated: {module_attributes:?}");
}

/// Both prerequisite methods are ordinary doc-hidden methods, never behind a cfg or a Cargo
/// feature, so a harness overlaid on any base that has them compiles and both sides build the same
/// feature set. Every enclosing gate counts.
#[test]
fn the_prerequisite_methods_are_never_cfg_gated() {
    let mod_source = include_str!("mod.rs");
    assert_never_gated(
        include_str!("perf_control.rs"),
        "pub fn perf_control_phase(",
        "mod perf_control;",
        mod_source,
    );
    assert_never_gated(
        include_str!("echo_timeline.rs"),
        "pub fn take_echo_timeline_v1(",
        "mod echo_timeline;",
        mod_source,
    );
}

/// A Windows checkout with CRLF line endings reads the same as an LF one: the scan finds the impl's
/// end and the module declarations whatever the line endings, so CI on Windows runs the same check.
#[test]
fn the_gate_check_reads_a_crlf_checkout_as_an_lf_one() {
    let crlf = |source: &str| source.replace("\r\n", "\n").replace('\n', "\r\n");
    let mod_source = crlf(include_str!("mod.rs"));
    assert_never_gated(
        &crlf(include_str!("perf_control.rs")),
        "pub fn perf_control_phase(",
        "mod perf_control;",
        &mod_source,
    );
    assert_never_gated(
        &crlf(include_str!("echo_timeline.rs")),
        "pub fn take_echo_timeline_v1(",
        "mod echo_timeline;",
        &mod_source,
    );
}

/// The attribute reader sees a gate however Rust lets it be written: across a blank line, behind a
/// comment, or spread over several lines; an ungated item reads as ungated.
#[test]
fn the_attribute_reader_follows_rust_attribute_placement() {
    let item = "mod perf_control;";
    let gated = [
        "#[cfg(test)]\n\nmod perf_control;\n",
        "#[cfg(\n    test\n)]\nmod perf_control;\n",
        "#[cfg(test)]\n// the control module\nmod perf_control;\n",
        "#[cfg(test)]\n#[allow(\n    dead_code\n)]\nmod perf_control;\n",
    ];
    for source in gated {
        assert!(gates(&attributes_above(source, item)), "{source:?} is gated");
    }
    let ungated = [
        "mod path_target;\nmod perf_control;\n",
        "#[doc(hidden)]\n#[allow(dead_code)]\nmod perf_control;\n",
        "const ROWS: [u8; 2] = [\n    1, 2,\n]\nmod perf_control;\n",
    ];
    for source in ungated {
        assert!(!gates(&attributes_above(source, item)), "{source:?} is not gated");
    }
}
