use super::*;

/// The app menu's About item opens SonicTerm's About window through the About action, which the
/// macOS and Windows menubars both dispatch as an action item, not the system About panel.
#[test]
fn the_about_menu_item_opens_the_about_window() {
    let about = blueprint()
        .into_iter()
        .flat_map(|submenu| submenu.items)
        .find(|item| item.title == "About SonicTerm")
        .expect("the app menu has an About item");
    assert!(
        matches!(about.binding, Binding::Action(sonicterm_cfg::keymap::Action::ShowAbout)),
        "{:?}",
        about.binding
    );
}
