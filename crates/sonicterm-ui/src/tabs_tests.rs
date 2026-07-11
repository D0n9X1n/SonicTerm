use super::*;

#[test]
fn empty_rename_reverts_to_auto_title() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 ~/work"));
    bar.set_active_custom_title("my work");
    assert_eq!(bar.tabs[0].custom_title.as_deref(), Some("my work"));
    assert_eq!(bar.tabs[0].title, "#1 my work");

    bar.set_active_custom_title("");
    assert_eq!(bar.tabs[0].custom_title, None);
    assert_eq!(bar.tabs[0].title, "#1 ~/work");

    bar.set_active_custom_title("renamed");
    bar.set_active_title("#1 ~/new-auto");
    assert_eq!(bar.tabs[0].title, "#1 renamed");
    bar.set_active_custom_title("   ");
    assert_eq!(bar.tabs[0].custom_title, None);
    assert_eq!(bar.tabs[0].title, "#1 ~/new-auto");
}

#[test]
fn active_custom_color_is_stored_on_active_tab() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 ~/work"));

    bar.set_active_custom_color("#fabd2f");

    assert_eq!(bar.active_custom_color(), Some("#fabd2f"));
    assert_eq!(bar.tabs[0].custom_color.as_deref(), Some("#fabd2f"));
    bar.clear_active_custom_color();
    assert_eq!(bar.active_custom_color(), None);
}

