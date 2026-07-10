use super::{warm_window_pool_should_spawn, warm_window_pool_target, WARM_WINDOW_POOL_MAX};

#[test]
fn warm_window_pool_keeps_one_spare_after_consuming_one() {
    assert_eq!(warm_window_pool_target(0), 2);
    assert_eq!(warm_window_pool_target(1), 2);
    assert_eq!(warm_window_pool_target(2), 2);
    assert_eq!(warm_window_pool_target(99), WARM_WINDOW_POOL_MAX);
}

#[test]
fn warm_window_pool_spawns_until_target_is_reached() {
    assert!(warm_window_pool_should_spawn(0, 2));
    assert!(warm_window_pool_should_spawn(1, 2));
    assert!(!warm_window_pool_should_spawn(2, 2));
}

