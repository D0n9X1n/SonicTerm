use sonicterm_cfg::config::Config;

#[test]
fn default_notifications_config_is_opt_in_with_ten_second_threshold() {
    let cfg = Config::default();
    assert!(!cfg.notifications.long_command);
    assert_eq!(cfg.notifications.threshold_secs, 10);
}
