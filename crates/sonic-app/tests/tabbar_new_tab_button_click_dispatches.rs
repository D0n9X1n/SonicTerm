use sonic_core::keymap::Action;
use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};

fn action_for_tabbar_hit(hit: Option<TabHit>) -> Option<Action> {
    match hit {
        Some(TabHit::NewTab) => Some(Action::NewTab),
        _ => None,
    }
}

#[test]
fn tabbar_new_tab_button_click_dispatches() {
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("shell"));
    let layout = TabBarLayout::compute(&tabs, 1000.0);
    let click =
        (layout.new_tab.x + layout.new_tab.w / 2.0, layout.new_tab.y + layout.new_tab.h / 2.0);

    let dispatched = action_for_tabbar_hit(layout.hit(click.0, click.1));

    assert_eq!(dispatched, Some(Action::NewTab));
}
