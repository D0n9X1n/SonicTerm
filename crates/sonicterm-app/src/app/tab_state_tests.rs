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
