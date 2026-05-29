use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};

#[test]
fn hidden_tab_bar_does_not_hit_tabs() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("shell"));

    let layout = TabBarLayout::compute(&bar, 300.0).with_visible(false);

    assert_eq!(layout.hit(100.0, 15.0), None);
}

#[test]
fn visible_tab_bar_hits_tab() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("shell"));

    let layout = TabBarLayout::compute(&bar, 300.0);

    assert_eq!(layout.hit(60.0, 15.0), Some(TabHit::Activate(0)));
}
