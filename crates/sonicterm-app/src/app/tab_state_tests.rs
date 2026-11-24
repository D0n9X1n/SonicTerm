use std::{sync::Arc, time::Instant};

use parking_lot::Mutex;
use sonicterm_grid::grid::Grid;
use sonicterm_io::proc_info::ForegroundProcess;
use sonicterm_ui::{tab_title::format_tab_title, tabs::TabBar};

use super::*;

/// A one-tab bar with no custom title, and a pane whose parser has consumed `output`.
fn single_tab(output: &[u8]) -> (TabBar, PaneState, Arc<Mutex<Parser>>) {
    let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    parser.lock().advance(output);
    let pane = PaneState::new(Arc::clone(&parser), None);
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell"));
    (tabs, pane, parser)
}

/// Cache `name` as the pane's applied foreground process.
fn cache(pane: &mut PaneState, name: &str) {
    pane.fg_proc_cache =
        Some((Instant::now(), Some(ForegroundProcess { name: name.into(), privileged: false })));
}

/// The perf harness computes S1/atlas-retry's expected final title as
/// `format_tab_title(0, cwd, Some(name), raw_title)`. It must equal what the App shows for a single tab
/// with no custom title, for both platforms' final names (`sleep` on macOS, the harness binary's
/// basename on Windows), with and without a cwd and raw title.
#[test]
fn the_harnesss_expected_title_equals_the_apps_for_both_final_names() {
    let outputs: [&[u8]; 2] =
        [b"", b"\x1b]7;file://localhost/work/project\x07\x1b]2;fixture title\x07"];
    for output in outputs {
        for name in ["sleep", "perf_scenarios"] {
            let (mut tabs, mut pane, parser) = single_tab(output);
            cache(&mut pane, name);
            refresh_active_tab_title(&mut tabs, &pane, &parser.lock(), 0);
            let parser = parser.lock();
            let expected = format_tab_title(0, parser.cwd(), Some(name), parser.title());
            assert_eq!(
                tabs.active().map(|tab| tab.title.as_str()),
                Some(expected.as_str()),
                "{name}"
            );
        }
    }
}

/// The failure Stage 1 recorded: a cached `bash` sample draws the E760 icon, and the later `sleep` sample
/// replaces it with F489. The real `sh` mapping stays E691.
#[test]
fn a_shell_then_sleep_cached_sample_changes_the_tab_icon() {
    let (mut tabs, mut pane, parser) = single_tab(b"");
    for (name, title) in
        [("bash", "#1 \u{E760} shell"), ("sleep", "#1 \u{F489} shell"), ("sh", "#1 \u{E691} shell")]
    {
        cache(&mut pane, name);
        refresh_active_tab_title(&mut tabs, &pane, &parser.lock(), 0);
        assert_eq!(tabs.active().map(|tab| tab.title.as_str()), Some(title), "{name}");
    }
}

/// The output fixtures the tab title reads: none, a CWD, an OSC title, and both.
const TITLE_FIXTURES: [&[u8]; 4] = [
    b"",
    b"\x1b]7;file://localhost/work/project\x07",
    b"\x1b]2;remote session\x07",
    b"\x1b]7;file://localhost/work/project\x07\x1b]2;remote session\x07",
];

/// With no OSC 1 seen, the tab label and the native caption body are exactly what the title
/// formatter makes, for each fixture and a multiplexer as well as a plain shell.
#[test]
fn without_osc1_the_label_and_caption_are_unchanged() {
    for output in TITLE_FIXTURES {
        for name in ["zsh", "tmux"] {
            let (mut tabs, mut pane, parser) = single_tab(output);
            cache(&mut pane, name);
            refresh_active_tab_title(&mut tabs, &pane, &parser.lock(), 0);
            let parser = parser.lock();
            let expected = format_tab_title(0, parser.cwd(), Some(name), parser.title());
            let tab = tabs.active().expect("one tab");
            assert_eq!(tab.title, expected, "{name} {output:?}: label");
            let mut plain = Tab::new(expected.clone());
            plain.auto_title = expected;
            assert_eq!(
                super::super::native_title::tab_display_body(tab),
                super::super::native_title::tab_display_body(&plain),
                "{name} {output:?}: caption"
            );
        }
    }
}

/// An OSC 1 name replaces the tab label's body even where a CWD would name it, keeping the tab's
/// `#N` and icon, while the native caption body keeps the CWD-based text.
#[test]
fn osc1_names_the_tab_label_but_not_the_caption() {
    let (mut tabs, mut pane, parser) =
        single_tab(b"\x1b]7;file://localhost/work/project\x07\x1b]1;~/project\x07");
    cache(&mut pane, "zsh");
    refresh_active_tab_title(&mut tabs, &pane, &parser.lock(), 0);
    let automatic = {
        let parser = parser.lock();
        format_tab_title(0, parser.cwd(), Some("zsh"), parser.title())
    };
    let tab = tabs.active().expect("one tab");
    assert_eq!(tab.title, sonicterm_ui::tabs::title_with_replaced_body(&automatic, "~/project"));
    assert_eq!(super::super::native_title::tab_display_body(tab), "work/project", "the caption");
}

/// A user rename wins over OSC 1, and clearing it brings the OSC 1 name back.
#[test]
fn a_rename_wins_over_osc1_until_it_is_cleared() {
    let (mut tabs, mut pane, parser) = single_tab(b"\x1b]1;tab name\x07");
    cache(&mut pane, "zsh");
    tabs.set_active_custom_title("mine");
    refresh_active_tab_title(&mut tabs, &pane, &parser.lock(), 0);
    assert!(tabs.active().unwrap().title.ends_with(" mine"), "{}", tabs.active().unwrap().title);
    tabs.set_active_custom_title("");
    refresh_active_tab_title(&mut tabs, &pane, &parser.lock(), 0);
    assert!(
        tabs.active().unwrap().title.ends_with(" tab name"),
        "{}",
        tabs.active().unwrap().title
    );
}

/// In a split the label follows the active pane: each refresh reads the pane that is active, so
/// switching panes switches between one pane's OSC 1 name and the other's automatic body.
#[test]
fn the_label_follows_the_active_panes_osc1() {
    let (mut tabs, mut named, named_parser) = single_tab(b"\x1b]1;alpha\x07");
    let other_parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
    other_parser.lock().advance(b"\x1b]7;file://localhost/work/beta\x07");
    let mut other = PaneState::new(Arc::clone(&other_parser), None);
    cache(&mut named, "zsh");
    cache(&mut other, "zsh");
    for (pane, parser, body) in [
        (&named, &named_parser, " alpha"),
        (&other, &other_parser, " work/beta"),
        (&named, &named_parser, " alpha"),
    ] {
        refresh_active_tab_title(&mut tabs, pane, &parser.lock(), 0);
        let title = &tabs.active().unwrap().title;
        assert!(title.ends_with(body), "{title:?} ends with {body:?}");
    }
}
