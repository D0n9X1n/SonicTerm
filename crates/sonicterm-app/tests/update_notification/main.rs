use std::time::{Duration, Instant};

use sonicterm_app::app::{App, FrontmostKind};
use sonicterm_cfg::{config::Config, keymap::Action, keymap::Keymap, theme::Theme};
use sonicterm_ui::overlays::NotificationLevel;

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

#[test]
fn check_for_updates_command_surfaces_notification_bubble_on_main() {
    let mut app = app();
    app.__test_seed_tab("main");

    assert!(app.run_action(&Action::CheckForUpdates));

    assert_eq!(app.__test_main_notification_message(), Some("Unable to check updates"));
}

#[test]
fn notification_can_target_frontmost_child_window() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(Some(child));

    assert!(app.run_action(&Action::CheckForUpdates));

    assert_eq!(app.__test_child_notification_message(child), Some("Unable to check updates"));
    assert_eq!(app.__test_main_notification_message(), None);
}

#[test]
fn notification_deadline_is_capped_at_five_seconds() {
    let mut app = app();
    app.__test_seed_tab("main");
    let now = Instant::now();

    app.__test_show_notification_until(
        FrontmostKind::Main,
        NotificationLevel::Info,
        "Done",
        Some(now + Duration::from_secs(7)),
    );

    assert_eq!(app.__test_main_notification_message(), Some("Done"));
    assert!(app.__test_expire_notifications(now + Duration::from_secs(4)).is_some());
    assert_eq!(app.__test_main_notification_message(), Some("Done"));
    assert_eq!(app.__test_expire_notifications(now + Duration::from_secs(6)), None);
    assert_eq!(app.__test_main_notification_message(), None);
}

#[test]
fn notification_without_deadline_auto_closes_after_five_seconds() {
    let mut app = app();
    app.__test_seed_tab("main");
    let now = Instant::now();

    app.__test_show_notification_until(
        FrontmostKind::Main,
        NotificationLevel::Warning,
        "Checking for updates…",
        None,
    );

    assert_eq!(app.__test_main_notification_ongoing(), Some(false));
    assert!(app.__test_expire_notifications(now + Duration::from_secs(4)).is_some());
    assert_eq!(app.__test_main_notification_message(), Some("Checking for updates…"));
    assert_eq!(app.__test_expire_notifications(now + Duration::from_secs(6)), None);
    assert_eq!(app.__test_main_notification_message(), None);
}
