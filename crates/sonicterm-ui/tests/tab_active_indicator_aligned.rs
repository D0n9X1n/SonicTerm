use sonicterm_ui::tabbar_view::TabBarLayout;
use sonicterm_ui::tabs::{Tab, TabBar};

#[test]
fn active_indicator_rect_matches_active_widget_bg_rect() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("one"));
    bar.push(Tab::new("two"));
    bar.push(Tab::new("three"));
    bar.activate(1);

    let layout = TabBarLayout::compute(&bar, 1000.0);
    let active = &layout.tabwidgets()[1];
    let indicator = layout.active_indicator_rect().expect("active indicator");

    assert_eq!(indicator.x, active.bg_rect.x);
    assert_eq!(indicator.y, active.bg_rect.y);
    assert_eq!(indicator.w, active.bg_rect.w);
}
