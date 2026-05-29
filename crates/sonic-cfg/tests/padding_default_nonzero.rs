use sonic_cfg::config::Config;

#[test]
fn padding_default_nonzero() {
    let padding = Config::default().window;
    assert!(padding.padding_left > 0.0);
    assert_eq!(padding.padding_left, 12.0);
    assert_eq!(padding.padding_right, 12.0);
    assert_eq!(padding.padding_top, 4.0);
    assert_eq!(padding.padding_bottom, 4.0);
}
