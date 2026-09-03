use super::*;
use std::time::Duration;

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

#[test]
fn command_badges_respect_activity_delay_exit_status_and_expiry() {
    let now = Instant::now();

    assert_eq!(CommandStatus::Running(now - Duration::from_secs(6)).badge(now, false), Some("…"));
    assert_eq!(CommandStatus::Running(now - Duration::from_secs(6)).badge(now, true), None);
    assert_eq!(CommandStatus::Running(now - Duration::from_secs(5)).badge(now, false), None);

    let future = now + Duration::from_secs(1);
    assert_eq!(CommandStatus::Done { exit: Some(0), until: future }.badge(now, false), Some("✓"));
    assert_eq!(CommandStatus::Done { exit: Some(1), until: future }.badge(now, false), Some("✗"));
    assert_eq!(CommandStatus::Done { exit: None, until: future }.badge(now, false), Some("✗"));
    assert_eq!(CommandStatus::Done { exit: Some(0), until: now }.badge(now, false), None);
}

#[test]
fn clearing_badges_expires_only_completed_commands_at_or_before_now() {
    let now = Instant::now();
    let mut bar = TabBar::new();
    bar.push(Tab::new("running"));
    bar.push(Tab::new("expired"));
    bar.push(Tab::new("future"));
    bar.set_command_status(0, CommandStatus::Running(now - Duration::from_secs(10)));
    bar.set_command_status(1, CommandStatus::Done { exit: Some(0), until: now });
    bar.set_command_status(
        2,
        CommandStatus::Done { exit: Some(1), until: now + Duration::from_secs(1) },
    );

    bar.clear_expired_command_badges(now);

    assert!(matches!(bar.tabs[0].command, CommandStatus::Running(_)));
    assert_eq!(bar.tabs[1].command, CommandStatus::Idle);
    assert!(matches!(bar.tabs[2].command, CommandStatus::Done { exit: Some(1), .. }));
}

#[test]
fn closing_tabs_keeps_the_same_surviving_tab_active_and_renumbers_titles() {
    let mut bar = TabBar::new();
    let first = bar.push(Tab::new("#8 first"));
    let second = bar.push(Tab::new("#9 second"));
    bar.push(Tab::new("#10 third"));
    bar.activate(1);

    bar.close(first);

    assert_eq!(bar.active().map(|tab| tab.id), Some(second));
    assert_eq!(bar.active_index(), 0);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 second", "#2 third"]
    );

    bar.close(second);
    assert_eq!(bar.active_index(), 0);
    assert_eq!(bar.active().map(|tab| tab.title.as_str()), Some("#1 third"));
}

#[test]
fn reorder_tracks_tab_identity_when_other_tabs_cross_the_active_slot() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 A"));
    let active = bar.push(Tab::new("#2 B"));
    bar.push(Tab::new("#3 C"));
    bar.push(Tab::new("#4 D"));
    bar.activate(1);

    bar.reorder(3, 0);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 2);

    bar.reorder(0, 3);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 1);

    bar.reorder(1, 3);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 3);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 A", "#2 C", "#3 D", "#4 B"]
    );
}

#[test]
fn insertion_clamps_to_the_end_and_makes_the_inserted_tab_active() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("#8 A"));
    bar.push(Tab::new("#9 B"));
    let inserted = bar.insert(usize::MAX, Tab::new("#20 C"));

    assert_eq!(bar.active().map(|tab| tab.id), Some(inserted));
    assert_eq!(bar.active_index(), 2);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 A", "#2 B", "#3 C"]
    );
}

#[test]
fn detaching_an_inactive_tab_to_the_left_keeps_the_same_tab_active() {
    let mut bar = TabBar::new();
    let first = bar.push(Tab::new("#1 A"));
    let active = bar.push(Tab::new("#2 B"));
    bar.push(Tab::new("#3 C"));
    bar.activate(1);

    let detached = bar.detach(first).expect("first tab should detach");

    assert_eq!(detached.id, first);
    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.active_index(), 0);
    assert_eq!(
        bar.tabs.iter().map(|tab| tab.title.as_str()).collect::<Vec<_>>(),
        vec!["#1 B", "#2 C"]
    );
}

#[test]
fn renumbering_preserves_the_automatic_title_behind_a_custom_title() {
    let folder = '\u{f07b}';
    let mut bar = TabBar::new();
    bar.push(Tab::new(format!("#8 {folder} work/project")));
    bar.push(Tab::new(format!("#9 {folder} other/path")));
    bar.activate(0);
    bar.set_active_custom_title("renamed");

    bar.reorder(0, 1);

    assert_eq!(
        bar.active().map(|tab| tab.title.as_str()),
        Some(format!("#2 {folder} renamed").as_str())
    );
    bar.set_active_custom_title(" ");
    assert_eq!(
        bar.active().map(|tab| tab.title.as_str()),
        Some(format!("#2 {folder} work/project").as_str())
    );
}

#[test]
fn title_body_helpers_preserve_icons_and_handle_unformatted_titles() {
    let folder = '\u{f07b}';
    assert_eq!(
        title_with_replaced_body(&format!("#12 {folder} old/path"), "renamed"),
        format!("#12 {folder} renamed")
    );
    assert_eq!(title_with_replaced_body("#2 ~/work", "renamed"), "#2 renamed");
    assert_eq!(title_with_replaced_body("Welcome", "renamed"), "renamed");

    let mut bar = TabBar::new();
    bar.push(Tab::new(format!("#1 {folder} old/path")));
    assert_eq!(bar.active_title_body().as_deref(), Some("old/path"));
    bar.set_active_custom_title("custom");
    assert_eq!(bar.active_title_body().as_deref(), Some("custom"));
}

#[test]
fn title_truncation_preserves_the_index_and_process_icon_before_shortening_the_body() {
    // Protect privilege-badge width reservation from clipping structural tab identity first.
    let folder = '\u{f07b}';
    let title = format!("#12 {folder} workspace/project");

    assert_eq!(truncate_title_body(&title, 12), format!("#12 {folder} works…"));
    assert_eq!(truncate_title_body(&title, title.chars().count()), title);
    assert_eq!(truncate_title_body("任务完成", 3), "任务…");
}

#[test]
fn title_truncation_does_not_mutate_automatic_or_custom_title_state() {
    // Protect rename state from absorbing renderer-only privilege chrome.
    let folder = '\u{f07b}';
    let mut bar = TabBar::new();
    bar.push(Tab::new(format!("#1 {folder} workspace/project")));
    bar.set_active_custom_title("renamed project");
    let before = bar.active().expect("active tab").clone();

    let _ = truncate_title_body(&before.title, 10);

    let after = bar.active().expect("active tab");
    assert_eq!(after.title, before.title);
    assert_eq!(after.auto_title, before.auto_title);
    assert_eq!(after.custom_title, before.custom_title);
}

#[test]
fn foreground_privilege_is_independent_tab_state_and_survives_rename_and_detach() {
    // Protect a gsudo warning from being stored in or erased by editable title text.
    let mut bar = TabBar::new();
    let id = bar.push(Tab::new("#1 shell"));

    assert!(bar.set_active_foreground_privileged(true));
    assert!(bar.active().expect("active tab").foreground_privileged);
    assert_eq!(bar.active().expect("active tab").title, "#1 shell");

    bar.set_active_custom_title("renamed");
    assert!(bar.active().expect("active tab").foreground_privileged);
    assert_eq!(bar.active().expect("active tab").title, "#1 renamed");
    assert!(!bar.set_active_foreground_privileged(true));

    let detached = bar.detach(id).expect("tab detaches");
    assert!(detached.foreground_privileged);
    assert_eq!(detached.custom_title.as_deref(), Some("renamed"));
}

#[test]
fn each_tab_keeps_its_own_foreground_privilege_state() {
    // Protect one gsudo tab from warning every tab in a regular SonicTerm process.
    let mut bar = TabBar::new();
    bar.push(Tab::new("#1 regular"));
    bar.push(Tab::new("#2 gsudo"));
    assert!(bar.set_active_foreground_privileged(true));
    bar.activate(0);

    assert!(!bar.tabs()[0].foreground_privileged);
    assert!(bar.tabs()[1].foreground_privileged);
}

#[test]
fn indexed_foreground_privilege_updates_an_inactive_tab_without_moving_focus() {
    // Protect background gsudo state from being written through the active-tab selector.
    let mut bar = TabBar::new();
    let active = bar.push(Tab::new("#1 regular"));
    bar.push(Tab::new("#2 background"));
    bar.activate(0);

    assert!(bar.set_foreground_privileged(1, true));

    assert_eq!(bar.active().map(|tab| tab.id), Some(active));
    assert_eq!(bar.tabs()[0].title, "#1 regular");
    assert_eq!(bar.tabs()[1].title, "#2 background");
    assert!(!bar.tabs()[0].foreground_privileged);
    assert!(bar.tabs()[1].foreground_privileged);
    assert!(!bar.set_foreground_privileged(1, true));
}
