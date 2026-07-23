use super::{warm_window_pool_should_spawn, warm_window_pool_target, WARM_WINDOW_POOL_MAX};

#[test]
fn zero_disables_warm_pool_on_every_adapter() {
    assert_eq!(warm_window_pool_target(0, false), 0);
    assert_eq!(warm_window_pool_target(0, true), 0);
    assert!(!warm_window_pool_should_spawn(0, 0, false));
    assert!(!warm_window_pool_should_spawn(0, 0, true));
}

#[test]
fn hardware_honors_configured_target_up_to_maximum() {
    assert_eq!(warm_window_pool_target(1, false), 1);
    assert_eq!(warm_window_pool_target(2, false), 2);
    assert_eq!(warm_window_pool_target(99, false), WARM_WINDOW_POOL_MAX);
    assert!(warm_window_pool_should_spawn(0, 2, false));
    assert!(warm_window_pool_should_spawn(1, 2, false));
    assert!(!warm_window_pool_should_spawn(2, 2, false));
}

#[test]
fn software_adapter_caps_nonzero_target_at_one() {
    assert_eq!(warm_window_pool_target(1, true), 1);
    assert_eq!(warm_window_pool_target(5, true), 1);
    assert_eq!(warm_window_pool_target(99, true), 1);
    assert!(warm_window_pool_should_spawn(0, 5, true));
    assert!(!warm_window_pool_should_spawn(1, 5, true));
}
