use std::sync::Arc;

use parking_lot::Mutex;

use super::dispatch;

fn assert_callback_runs_without_target_guard(target: Arc<str>, expected: &str) {
    let redraw_target = Mutex::new(Some(target));
    let callback_target = &redraw_target;
    let mut observed = None;

    dispatch(&redraw_target, |target| {
        let mut guard = callback_target
            .try_lock()
            .expect("redraw callback must run after the target guard is released");
        *guard = None;
        observed = Some(target);
    });

    assert_eq!(observed.as_deref(), Some(expected));
    assert!(redraw_target.lock().is_none());
}

#[test]
fn main_target_guard_is_released_before_redraw_callback() {
    assert_callback_runs_without_target_guard(Arc::from("main"), "main");
}

#[test]
fn child_target_guard_is_released_before_redraw_callback() {
    assert_callback_runs_without_target_guard(Arc::from("child"), "child");
}

#[test]
fn missing_target_does_not_invoke_redraw_callback() {
    let redraw_target = Mutex::<Option<Arc<str>>>::new(None);
    let mut invoked = false;

    dispatch(&redraw_target, |_| invoked = true);

    assert!(!invoked);
}
