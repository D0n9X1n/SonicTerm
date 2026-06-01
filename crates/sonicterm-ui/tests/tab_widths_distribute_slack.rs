use sonicterm_ui::tabbar_view::{TabBarLayout, TAB_MAX_WIDTH, TAB_MIN_WIDTH};
use sonicterm_ui::tabs::{Tab, TabBar};

#[test]
fn single_tab_in_wide_strip_grows_beyond_legacy_small_cap() {
    let mut bar = TabBar::new();
    bar.push(Tab::new(
        "Administrator: C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    ));

    let layout = TabBarLayout::compute(&bar, 2000.0);
    let width = layout.tabs[0].bg.w;

    assert!(
        width > TAB_MIN_WIDTH,
        "wide strip should distribute slack to the tab instead of leaving it at {TAB_MIN_WIDTH}px"
    );
    assert!(
        (width - TAB_MAX_WIDTH).abs() < 0.5,
        "single tab should grow to the configured max width {TAB_MAX_WIDTH}, got {width}"
    );
    assert!(layout.tabs[0].bg.x + width <= layout.bar.w);
}
